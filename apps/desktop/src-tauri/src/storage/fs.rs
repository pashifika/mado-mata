use super::{MAX_DIRECTORY_ENTRIES, MAX_PROFILE_BYTES, MAX_SETTINGS_BYTES, MAX_TAB_BYTES};
use crate::target::MAX_TARGET_BYTES;
use mado_runtime_comparison::model::Fault;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

pub(super) fn bounded_entries(directory: &Path) -> Result<Vec<fs::DirEntry>, Fault> {
    check_directory(directory)?;
    let mut entries = Vec::new();
    for entry in
        fs::read_dir(directory).map_err(|error| storage("list storage directory", error))?
    {
        if entries.len() >= MAX_DIRECTORY_ENTRIES - 1 {
            return Err(limit(
                "storage directory has too many entries; retain space for an atomic write",
            ));
        }
        entries.push(entry.map_err(|error| storage("read storage entry", error))?);
    }
    Ok(entries)
}

pub(crate) fn filesystem_key(name: &str) -> String {
    name.nfd()
        .flat_map(char::to_lowercase)
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .nfd()
        .collect()
}

pub(super) fn check_alias(directory: &Path, name: &str) -> Result<(), Fault> {
    let key = filesystem_key(name);
    #[cfg(unix)]
    let target = match fs::symlink_metadata(directory.join(name)) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(storage("inspect component identity", error)),
    };
    for entry in bounded_entries(directory)? {
        let filename = entry.file_name();
        if let Some(existing) = filename.to_str() {
            if existing != name && filesystem_key(existing) == key {
                return Err(Fault::new(
                    "StorageAlias",
                    "component aliases another retained filesystem identity",
                ));
            }
            #[cfg(unix)]
            if existing != name {
                if let Some(target) = &target {
                    let metadata = fs::symlink_metadata(entry.path())
                        .map_err(|error| storage("inspect component identity", error))?;
                    if target.dev() == metadata.dev() && target.ino() == metadata.ino() {
                        return Err(Fault::new(
                            "StorageAlias",
                            "component resolves to another retained filesystem identity",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn private_directory(path: &Path) -> Result<(), Fault> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(Fault::new(
                "Storage",
                "application storage must be a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder
                .create(path)
                .map_err(|error| storage("create storage directory", error))?;
        }
        Err(error) => return Err(storage("inspect storage directory", error)),
    }
    check_directory(path)
}

pub(crate) fn check_directory(path: &Path) -> Result<(), Fault> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| storage("inspect storage directory", error))?;
    if !metadata.file_type().is_dir() {
        return Err(Fault::new(
            "Storage",
            "application storage must be a real directory",
        ));
    }
    #[cfg(unix)]
    if metadata.mode() & 0o077 != 0 {
        return Err(Fault::new(
            "Storage",
            "application storage directory is not private",
        ));
    }
    Ok(())
}

pub(crate) fn checked_file(path: &Path, maximum: usize) -> Result<fs::Metadata, Fault> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| storage("inspect stored file", error))?;
    if !metadata.file_type().is_file() {
        return Err(Fault::new(
            "Storage",
            "stored data must be a regular file, not a link or directory",
        ));
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
        return Err(Fault::new(
            "Storage",
            "stored data must be private and have no hard links",
        ));
    }
    if metadata.len() > maximum as u64 {
        return Err(limit("stored file exceeds its byte bound"));
    }
    Ok(metadata)
}

/// Reads a private regular file of at most `maximum` bytes, refusing one that changes between
/// the metadata check and the open. Decoding is separate so a caller can parse one capture
/// more than once without reading the file again.
pub(crate) fn read_bytes(path: &Path, maximum: usize) -> Result<Vec<u8>, Fault> {
    let before = checked_file(path, maximum)?;
    let mut file = File::open(path).map_err(|error| storage("open stored file", error))?;
    let opened = file
        .metadata()
        .map_err(|error| storage("inspect opened file", error))?;
    #[cfg(unix)]
    if before.dev() != opened.dev() || before.ino() != opened.ino() || opened.nlink() != 1 {
        return Err(Fault::new(
            "Storage",
            "stored file changed while being opened",
        ));
    }
    if !opened.is_file() || opened.len() != before.len() {
        return Err(Fault::new(
            "Storage",
            "stored file changed while being opened",
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| storage("read stored file", error))?;
    if bytes.len() > maximum {
        return Err(limit("stored file exceeds its byte bound"));
    }
    let after = checked_file(path, maximum)?;
    if bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
    {
        return Err(Fault::new(
            "StorageChanged",
            "stored file changed while being read",
        ));
    }
    #[cfg(unix)]
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(Fault::new(
            "StorageChanged",
            "stored file changed while being read",
        ));
    }
    Ok(bytes)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Fault> {
    if bytes
        .iter()
        .copied()
        .find(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        != Some(b'{')
    {
        return Err(malformed());
    }
    serde_json::from_slice(bytes).map_err(|_| malformed())
}

pub(crate) fn exists(path: &Path) -> Result<bool, Fault> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage("inspect storage destination", error)),
    }
}

pub(crate) fn encode<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>, Fault> {
    struct Bounded {
        bytes: Vec<u8>,
        maximum: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("stored JSON exceeds its byte bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| limit("stored JSON cannot be encoded within its byte bound"))?;
    Ok(output.bytes)
}

pub(crate) fn write_atomic(
    destination: &Path,
    bytes: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), Fault> {
    let directory = destination
        .parent()
        .ok_or_else(|| Fault::new("Storage", "storage destination has no parent"))?;
    check_directory(directory)?;
    let maximum = match destination.file_name().and_then(|name| name.to_str()) {
        Some("settings.json") => MAX_SETTINGS_BYTES,
        Some("tab.config") => MAX_TAB_BYTES,
        Some("target.config") => MAX_TARGET_BYTES,
        _ => MAX_PROFILE_BYTES,
    };
    if bytes.len() > maximum {
        return Err(limit("atomic write exceeds its destination byte bound"));
    }
    if exists(destination)? {
        checked_file(destination, maximum)?;
    }
    let temporary = destination.with_extension("pending");
    // The exclusive create below refuses as well; name the cause rather than an I/O code.
    if exists(&temporary)? {
        return Err(Fault::new(
            "StoragePending",
            "an unresolved write to this destination must be preserved or repaired first",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|error| storage("create atomic write", error))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| storage("write atomic data", error))?;
        file.flush()
            .map_err(|error| storage("flush atomic data", error))?;
        file.sync_all()
            .map_err(|error| storage("sync atomic data", error))?;
        drop(file);
        replace(&temporary, destination).map_err(|error| storage("replace stored file", error))
    })();
    match result {
        Ok(()) => Ok(()),
        Err(mut fault) => {
            // Only remove the temporary file created by this call, never the original.
            if fs::remove_file(&temporary).is_err() {
                fault.context["temporary_cleanup"] = json!("failed");
            }
            Err(fault)
        }
    }
}

pub(super) fn limit(message: &str) -> Fault {
    Fault::new("StorageLimit", message)
}

pub(super) fn malformed() -> Fault {
    Fault::new(
        "StorageFormat",
        "stored JSON is malformed or incompatible; original data was preserved",
    )
}

pub(super) fn storage(operation: &str, error: io::Error) -> Fault {
    Fault::new("Storage", format!("could not {operation}"))
        .with_context(json!({"operation": operation, "kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}))
}

#[cfg(test)]
mod tests;
