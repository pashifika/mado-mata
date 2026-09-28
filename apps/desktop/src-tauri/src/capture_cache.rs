//! Machine-local originals; callers provide only accepted, detached PNG payloads.
use crate::storage::{validate_id, validate_package_id};
use mado_runtime_comparison::images::INPUT_MAX_BYTES;
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

pub struct CaptureCache {
    folder: PathBuf,
}

#[derive(Serialize)]
pub struct CacheInfo {
    pub folder: String,
    pub bytes: Option<u64>,
    pub error: Option<Fault>,
}

#[derive(Serialize)]
pub struct CacheWrite {
    pub cached: bool,
    pub error: Option<Fault>,
}

impl CaptureCache {
    /// The shell supplies its platform-resolved application cache root, never an IPC path.
    pub fn new(app_cache_root: PathBuf) -> Result<Self, Fault> {
        if !app_cache_root.is_absolute()
            || app_cache_root
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(refused(
                "application cache root is not absolute and normalized",
            ));
        }
        Ok(Self {
            folder: app_cache_root.join("captures"),
        })
    }

    pub fn info(&self) -> CacheInfo {
        let result = self.measure();
        CacheInfo {
            folder: self.folder.to_string_lossy().into_owned(),
            bytes: result.as_ref().ok().copied(),
            error: result.err(),
        }
    }

    pub fn folder_for_open(&self) -> Result<PathBuf, Fault> {
        ensure_directories(&self.folder)?;
        check_ancestors(&self.folder)?;
        Ok(self.folder.clone())
    }

    /// Missing files pass through to the ordinary bounded PNG loader.
    pub fn image_path(&self, package_id: &str, capture_id: &str) -> Result<PathBuf, Fault> {
        validate_package_id(package_id)?;
        validate_id(capture_id)?;
        let directory = self.folder.join(package_id);
        check_ancestors(&directory)?;
        let path = directory.join(format!("{capture_id}.png"));
        if let Some(metadata) = optional_metadata(&path)? {
            check_regular(&metadata)?;
        }
        Ok(path)
    }

    /// Atomically replaces this capture's private original; never publishes a partial PNG.
    /// No image copy or second encoding; the caller retains its payload reservation.
    pub fn persist(&self, package_id: &str, capture_id: &str, png: &[u8]) -> CacheWrite {
        let mut cached = false;
        let result = (|| {
            if png.len() > INPUT_MAX_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(refused(
                    "cache PNG exceeds the image bound or has an invalid signature",
                ));
            }
            let destination = self.image_path(package_id, capture_id)?;
            let directory = destination.parent().expect("cache image has a parent");
            ensure_directories(directory)?;
            let pending = destination.with_extension("pending");
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options
                .open(&pending)
                .map_err(|error| io_fault("create cache write", error))?;
            let publication: Result<(), Fault> = (|| {
                file.write_all(png)
                    .map_err(|error| io_fault("write cache image", error))?;
                file.sync_all()
                    .map_err(|error| io_fault("sync cache image", error))?;
                drop(file);
                check_ancestors(directory)?;
                if let Some(metadata) = optional_metadata(&destination)? {
                    check_regular(&metadata)?;
                }
                // The temporary is complete and synced before same-directory replacement.
                // Only this private cache key may be replaced; package assets are unrelated.
                fs::rename(&pending, &destination)
                    .map_err(|error| io_fault("publish cache image", error))?;
                cached = true;
                Ok(())
            })();
            let cleanup = if cached {
                Ok(())
            } else {
                fs::remove_file(&pending)
                    .map_err(|error| io_fault("remove own cache temporary", error))
            };
            match (publication, cleanup) {
                (Err(mut primary), Err(cleanup)) => {
                    primary.context["cleanup"] = serde_json::json!(cleanup);
                    Err(primary)
                }
                (Err(primary), Ok(())) => Err(primary),
                (Ok(()), cleanup) => cleanup,
            }
        })();
        CacheWrite {
            cached,
            error: result.err(),
        }
    }

    fn measure(&self) -> Result<u64, Fault> {
        check_ancestors(&self.folder)?;
        if optional_metadata(&self.folder)?.is_none() {
            return Ok(0);
        }
        let mut directories = vec![self.folder.clone()];
        let mut total = 0u64;
        while let Some(directory) = directories.pop() {
            check_ancestors(&directory)?;
            let before = fs::symlink_metadata(&directory)
                .map_err(|error| io_fault("inspect cache folder", error))?;
            for entry in
                fs::read_dir(&directory).map_err(|error| io_fault("measure cache folder", error))?
            {
                let entry = entry.map_err(|error| io_fault("read cache entry", error))?;
                let metadata = fs::symlink_metadata(entry.path())
                    .map_err(|error| io_fault("inspect cache entry", error))?;
                check_not_link(&metadata)?;
                if metadata.is_dir() {
                    directories.push(entry.path());
                } else {
                    check_regular(&metadata)?;
                    total = total
                        .checked_add(metadata.len())
                        .ok_or_else(|| refused("cache size cannot be represented"))?;
                }
            }
            let after = fs::symlink_metadata(&directory)
                .map_err(|error| io_fault("recheck cache folder", error))?;
            check_not_link(&after)?;
            if !after.is_dir() || before.modified().ok() != after.modified().ok() {
                return Err(refused("cache folder changed during measurement"));
            }
            #[cfg(unix)]
            if before.dev() != after.dev() || before.ino() != after.ino() {
                return Err(refused("cache folder was replaced during measurement"));
            }
        }
        Ok(total)
    }
}

fn optional_metadata(path: &Path) -> Result<Option<Metadata>, Fault> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_fault("inspect cache path", error)),
    }
}

fn check_not_link(metadata: &Metadata) -> Result<(), Fault> {
    #[cfg(windows)]
    if metadata.file_attributes() & 0x400 != 0 {
        return Err(refused("cache paths cannot contain reparse points"));
    }
    if metadata.file_type().is_symlink() {
        return Err(refused("cache paths cannot contain symbolic links"));
    }
    Ok(())
}

fn check_regular(metadata: &Metadata) -> Result<(), Fault> {
    check_not_link(metadata)?;
    if !metadata.is_file() {
        return Err(refused("cache image is not a regular file"));
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(refused("cache image has additional hard links"));
    }
    Ok(())
}

fn check_ancestors(path: &Path) -> Result<(), Fault> {
    for ancestor in path.ancestors() {
        if let Some(metadata) = optional_metadata(ancestor)? {
            check_not_link(&metadata)?;
            if !metadata.is_dir() {
                return Err(refused("cache ancestor is not a directory"));
            }
        }
    }
    Ok(())
}

fn ensure_directories(path: &Path) -> Result<(), Fault> {
    check_ancestors(path)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|error| io_fault("create cache folder", error))?;
    check_ancestors(path)
}

fn refused(message: &str) -> Fault {
    Fault::new("CaptureCache", message)
}
fn io_fault(operation: &str, error: io::Error) -> Fault {
    // ErrorKind is actionable without embedding machine-local paths in a diagnostic.
    refused(&format!("{operation}: {:?}", error.kind()))
}

#[cfg(test)]
mod tests;
