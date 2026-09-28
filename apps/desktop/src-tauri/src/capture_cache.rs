//! Machine-local originals; callers provide only accepted, detached PNG payloads.
use crate::storage::{validate_id, validate_package_id};
use mado_runtime_comparison::images::INPUT_MAX_BYTES;
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

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
    /// Derives private originals from the selected configuration root, never an IPC path.
    pub fn new(config_root: &Path) -> Result<Self, Fault> {
        let root = std::path::absolute(config_root)
            .map_err(|error| io_fault("resolve configuration root", error))?;
        if root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(refused("configuration root is not normalized"));
        }
        Ok(Self {
            folder: root.join("caches"),
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

    /// Creates missing managed folders and validates the path before shell dispatch.
    /// No file contents are accessed here; the shell resolves the pathname separately.
    pub fn folder_for_open(&self) -> Result<PathBuf, Fault> {
        ensure_directories(&self.folder)?;
        check_ancestors(&self.folder)?;
        Ok(self.folder.clone())
    }

    /// Opens this capture's managed original as a verified handle; `None` when absent.
    /// The package directory is reached without following links and the entry is opened
    /// relative to that retained identity; the handle is what the caller reads.
    pub fn open_image(&self, package_id: &str, capture_id: &str) -> Result<Option<File>, Fault> {
        validate_id(capture_id)?;
        let Some(directory) = self.package_directory(package_id)? else {
            return Ok(None);
        };
        let Some(file) = directory.open_image(&format!("{capture_id}.png"))? else {
            return Ok(None);
        };
        check_regular(
            &file
                .metadata()
                .map_err(|error| io_fault("inspect cache image", error))?,
        )?;
        Ok(Some(file))
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
            validate_id(capture_id)?;
            let directory = self.create_package_directory(package_id)?;
            let pending = format!("{capture_id}.pending");
            let mut file = directory.create_pending(&pending)?;
            let publication: Result<(), Fault> = (|| {
                file.write_all(png)
                    .map_err(|error| io_fault("write cache image", error))?;
                file.sync_all()
                    .map_err(|error| io_fault("sync cache image", error))?;
                drop(file);
                // The temporary is complete and synced before same-directory replacement.
                // Only this private cache key may be replaced; package assets are unrelated.
                directory.publish(&pending, &format!("{capture_id}.png"))?;
                cached = true;
                Ok(())
            })();
            let cleanup = if cached {
                Ok(())
            } else {
                directory.remove(&pending)
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

    fn package_directory(&self, package_id: &str) -> Result<Option<PackageDirectory>, Fault> {
        validate_package_id(package_id)?;
        let directory = self.folder.join(package_id);
        check_ancestors(&directory)?;
        if optional_metadata(&directory)?.is_none() {
            return Ok(None);
        }
        PackageDirectory::open(&directory).map(Some)
    }

    fn create_package_directory(&self, package_id: &str) -> Result<PackageDirectory, Fault> {
        validate_package_id(package_id)?;
        let directory = self.folder.join(package_id);
        ensure_directories(&directory)?;
        PackageDirectory::open(&directory)
    }

    /// A refused entry or an observed folder change fails the whole measurement; no
    /// partial total is ever reported as complete.
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
                // Entry metadata comes from the opened directory, not a re-resolved pathname.
                let metadata = entry
                    .metadata()
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
            let replaced = before.dev() != after.dev() || before.ino() != after.ino();
            // Windows exposes no stable directory identity in std; creation time is the
            // strongest observable property beside the modification check above.
            #[cfg(windows)]
            let replaced = before.creation_time() != after.creation_time();
            if replaced {
                return Err(refused("cache folder was replaced during measurement"));
            }
        }
        Ok(total)
    }
}

/// A `caches/<package_id>` directory reached without following any link and retained
/// for the duration of one entry operation, so a pathname substituted afterwards cannot
/// redirect that operation outside the managed cache.
struct PackageDirectory {
    /// The leaf descriptor; every entry operation is relative to it.
    #[cfg(unix)]
    handle: File,
    /// Root first, then descendants, each held without delete sharing so no pinned level
    /// can be renamed or replaced while the pathname operations below use it.
    #[cfg(windows)]
    _pinned: Vec<File>,
    #[cfg(windows)]
    path: PathBuf,
}

#[cfg(unix)]
impl PackageDirectory {
    /// Walks every component from `/` with directory-only, no-follow opens.
    fn open(path: &Path) -> Result<Self, Fault> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC);
        let mut handle = options
            .open("/")
            .map_err(|error| io_fault("open filesystem root", error))?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    handle = at::open(&handle, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                        .map_err(|error| io_fault("open cache folder", error))?;
                }
                Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                    return Err(refused("cache path is not normalized"));
                }
            }
        }
        Ok(Self { handle })
    }

    fn open_image(&self, name: &str) -> Result<Option<File>, Fault> {
        match at::open(
            &self.handle,
            name.as_ref(),
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        ) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_fault("open cache image", error)),
        }
    }

    fn create_pending(&self, name: &str) -> Result<File, Fault> {
        at::open(
            &self.handle,
            name.as_ref(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
        .map_err(|error| io_fault("create cache write", error))
    }

    fn publish(&self, pending: &str, destination: &str) -> Result<(), Fault> {
        at::rename(&self.handle, pending.as_ref(), destination.as_ref())
            .map_err(|error| io_fault("publish cache image", error))
    }

    fn remove(&self, name: &str) -> Result<(), Fault> {
        at::unlink(&self.handle, name.as_ref())
            .map_err(|error| io_fault("remove own cache temporary", error))
    }
}

#[cfg(windows)]
impl PackageDirectory {
    /// Pins every directory below the drive root, each opened on its own reparse point.
    fn open(path: &Path) -> Result<Self, Fault> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        let mut pinned = Vec::new();
        let mut current = PathBuf::with_capacity(path.as_os_str().len());
        for component in path.components() {
            current.push(component.as_os_str());
            match component {
                Component::Prefix(_) | Component::RootDir => {}
                Component::Normal(_) => {
                    let handle = OpenOptions::new()
                        .read(true)
                        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                        .open(&current)
                        .map_err(|error| io_fault("open cache folder", error))?;
                    let metadata = handle
                        .metadata()
                        .map_err(|error| io_fault("inspect cache folder", error))?;
                    check_not_link(&metadata)?;
                    if !metadata.is_dir() {
                        return Err(refused("cache ancestor is not a directory"));
                    }
                    pinned.push(handle);
                }
                Component::CurDir | Component::ParentDir => {
                    return Err(refused("cache path is not normalized"));
                }
            }
        }
        Ok(Self {
            _pinned: pinned,
            path: current,
        })
    }

    fn open_image(&self, name: &str) -> Result<Option<File>, Fault> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        let mut options = OpenOptions::new();
        options
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        match options.open(self.path.join(name)) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_fault("open cache image", error)),
        }
    }

    fn create_pending(&self, name: &str) -> Result<File, Fault> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path.join(name))
            .map_err(|error| io_fault("create cache write", error))
    }

    fn publish(&self, pending: &str, destination: &str) -> Result<(), Fault> {
        fs::rename(self.path.join(pending), self.path.join(destination))
            .map_err(|error| io_fault("publish cache image", error))
    }

    fn remove(&self, name: &str) -> Result<(), Fault> {
        fs::remove_file(self.path.join(name))
            .map_err(|error| io_fault("remove own cache temporary", error))
    }
}

/// Directory-relative entry operations; each name is one validated component.
#[cfg(unix)]
mod at {
    use std::ffi::{CString, OsStr, c_int, c_uint};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    fn component(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    /// Opens `name` inside `directory`; a link entry is refused, never followed.
    #[expect(
        unsafe_code,
        reason = "audited directory-relative open of one validated entry"
    )]
    pub fn open(directory: &File, name: &OsStr, flags: c_int, mode: c_uint) -> io::Result<File> {
        let name = component(name)?;
        // SAFETY: The directory descriptor stays open for this borrow and the name is a
        // live NUL-terminated string consumed synchronously; mode is only read with O_CREAT.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat just created this descriptor and nothing else owns it.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    #[expect(
        unsafe_code,
        reason = "audited directory-relative rename between two validated entries"
    )]
    pub fn rename(directory: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
        let from = component(from)?;
        let to = component(to)?;
        // SAFETY: Both names are live NUL-terminated strings and the descriptor stays
        // open for this synchronous call.
        let result = unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                to.as_ptr(),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[expect(
        unsafe_code,
        reason = "audited directory-relative unlink of one validated entry"
    )]
    pub fn unlink(directory: &File, name: &OsStr) -> io::Result<()> {
        let name = component(name)?;
        // SAFETY: The name is a live NUL-terminated string and the descriptor stays open
        // for this synchronous call; zero flags unlink a non-directory entry.
        let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
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
