//! Raw, bounded configuration capture. No typed document loading or package traversal.
use crate::storage::{
    MAX_PROFILE_BYTES, MAX_SETTINGS_BYTES, MAX_TAB_BYTES, check_directory, checked_file, exists,
    filesystem_key, read_bytes,
};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

pub const MAX_FILES: usize = 4096;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_ENUMERATED: usize = 16_384;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub files: BTreeMap<String, Vec<u8>>,
    pub generation: String,
    pub root_present: bool,
    pub settings_present: bool,
}

impl Capture {
    pub fn from_files(files: BTreeMap<String, Vec<u8>>, root_present: bool) -> Result<Self, Fault> {
        let generation = generation(&files, root_present)?;
        Ok(Self {
            settings_present: files.contains_key("settings.json"),
            files,
            generation,
            root_present,
        })
    }

    pub(crate) fn check(&self) -> Result<(), Fault> {
        if self.generation != generation(&self.files, self.root_present)?
            || self.settings_present != self.files.contains_key("settings.json")
        {
            return Err(fault("configuration generation does not match its bytes"));
        }
        Ok(())
    }
}

fn generation(files: &BTreeMap<String, Vec<u8>>, root_present: bool) -> Result<String, Fault> {
    if files.len() > MAX_FILES || (!root_present && !files.is_empty()) {
        return Err(fault("invalid configuration file count or root presence"));
    }
    let mut total = 0usize;
    let mut aliases = BTreeMap::new();
    let mut hash = Sha256::new();
    hash.update(b"mado-mata-configuration-v1\0");
    hash.update([
        u8::from(root_present),
        u8::from(files.contains_key("settings.json")),
    ]);
    for (path, bytes) in files {
        let kind = path_kind(path)?;
        check_aliases(path, &mut aliases)?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| fault("configuration size overflow"))?;
        if bytes.len() > kind.maximum() || total > MAX_BYTES {
            return Err(fault("configuration exceeds its byte budget"));
        }
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Settings,
    Tab,
    Package,
    LegacyProfile,
}

impl Kind {
    pub(crate) fn maximum(self) -> usize {
        match self {
            Self::Settings => MAX_SETTINGS_BYTES,
            Self::Tab => MAX_TAB_BYTES,
            Self::Package | Self::LegacyProfile => MAX_PROFILE_BYTES,
        }
    }
}

pub(crate) fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && value.trim() == value
        && !value.ends_with('.')
        && !value.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
        && (!cfg!(windows)
            || !matches!(
                value
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_ascii_uppercase()
                    .as_str(),
                "CON"
                    | "PRN"
                    | "AUX"
                    | "NUL"
                    | "COM1"
                    | "COM2"
                    | "COM3"
                    | "COM4"
                    | "COM5"
                    | "COM6"
                    | "COM7"
                    | "COM8"
                    | "COM9"
                    | "LPT1"
                    | "LPT2"
                    | "LPT3"
                    | "LPT4"
                    | "LPT5"
                    | "LPT6"
                    | "LPT7"
                    | "LPT8"
                    | "LPT9"
            ))
}

pub(crate) fn path_kind(path: &str) -> Result<Kind, Fault> {
    if path.len() > 1024 || !path.split('/').all(safe_component) {
        return Err(fault("unsafe configuration path"));
    }
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["settings.json"] => Ok(Kind::Settings),
        ["profiles", file] if file.ends_with(".json") => Ok(Kind::LegacyProfile),
        ["tabs", _, "tab.config"] => Ok(Kind::Tab),
        ["tabs", _, package, file]
            if filesystem_key(package) != "tab.config" && file.ends_with(".config") =>
        {
            Ok(Kind::Package)
        }
        _ => Err(fault("path is outside the managed configuration set")),
    }
}

/// Check every prefix, not only full filenames: two differently spelled package
/// directories must not become one directory on a case-insensitive filesystem.
pub(crate) fn check_aliases(
    path: &str,
    aliases: &mut BTreeMap<String, String>,
) -> Result<(), Fault> {
    let mut prefix = String::new();
    for part in path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        let key = filesystem_key(&prefix);
        if aliases
            .get(&key)
            .is_some_and(|previous| previous != &prefix)
        {
            return Err(fault("configuration contains filesystem aliases"));
        }
        aliases.insert(key, prefix.clone());
    }
    Ok(())
}

/// Caller serializes with application configuration writers. Two complete byte
/// observations detect external changes; this is not a cross-process lock.
pub fn capture(root: &Path) -> Result<Capture, Fault> {
    capture_between(root, || {})
}

fn capture_between(root: &Path, between: impl FnOnce()) -> Result<Capture, Fault> {
    let first = capture_once(root)?;
    between();
    let second = capture_once(root)?;
    if first != second {
        return Err(fault(
            "configuration changed during capture; nothing was published",
        ));
    }
    Ok(first)
}

fn capture_once(root: &Path) -> Result<Capture, Fault> {
    if !exists(root)? {
        return Capture::from_files(BTreeMap::new(), false);
    }
    check_directory(root)?;
    let mut files = BTreeMap::new();
    let mut remaining = MAX_BYTES;
    let mut enumerated = 0;
    for entry in entries(root, &mut enumerated)? {
        if let Some(name) = entry.file_name().to_str() {
            let key = filesystem_key(name);
            if key == "settings.pending" {
                return Err(fault(
                    "an unresolved App settings write must be preserved or repaired first",
                ));
            }
            if matches!(key.as_str(), "settings.json" | "profiles" | "tabs") && name != key {
                return Err(fault("managed root entry has an alias spelling"));
            }
        }
    }
    if exists(&root.join("settings.json"))? {
        add_file(root, "settings.json", &mut files, &mut remaining)?;
    }
    let legacy = root.join("profiles");
    if exists(&legacy)? {
        for entry in entries(&legacy, &mut enumerated)? {
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| fault("non-UTF-8 legacy configuration entry"))?;
            refuse_pending(&entry, name)?;
            if filesystem_key(name).ends_with(".json") && !name.ends_with(".json") {
                return Err(fault("legacy configuration has an alias extension"));
            }
            if name.ends_with(".json") {
                add_file(
                    root,
                    &format!("profiles/{name}"),
                    &mut files,
                    &mut remaining,
                )?;
            }
        }
    }
    let tabs = root.join("tabs");
    if exists(&tabs)? {
        for tab in entries(&tabs, &mut enumerated)? {
            let name = tab.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| fault("non-UTF-8 Tab container"))?;
            let metadata = fs::symlink_metadata(tab.path())
                .map_err(|e| io_fault("inspect Tab container", e))?;
            if metadata.is_file() {
                continue;
            }
            if !safe_component(name) {
                return Err(fault("unsafe Tab container name"));
            }
            for entry in entries(&tab.path(), &mut enumerated)? {
                let child = entry.file_name();
                let child = child
                    .to_str()
                    .ok_or_else(|| fault("non-UTF-8 package container"))?;
                if filesystem_key(child) == "tab.pending" {
                    return Err(fault(
                        "an unresolved Tab write must be preserved or repaired first",
                    ));
                }
                if filesystem_key(child) == "tab.config" && child != "tab.config" {
                    return Err(fault("Tab document has an alias spelling"));
                }
                if child == "tab.config" {
                    add_file(
                        root,
                        &format!("tabs/{name}/tab.config"),
                        &mut files,
                        &mut remaining,
                    )?;
                } else {
                    let metadata = fs::symlink_metadata(entry.path())
                        .map_err(|e| io_fault("inspect package container", e))?;
                    if metadata.is_file() {
                        continue;
                    }
                    if !safe_component(child) {
                        return Err(fault("unsafe package container name"));
                    }
                    for file in entries(&entry.path(), &mut enumerated)? {
                        let filename = file.file_name();
                        let filename = filename
                            .to_str()
                            .ok_or_else(|| fault("non-UTF-8 package configuration entry"))?;
                        refuse_pending(&file, filename)?;
                        if filesystem_key(filename).ends_with(".config")
                            && !filename.ends_with(".config")
                        {
                            return Err(fault("package configuration has an alias extension"));
                        }
                        if filename.ends_with(".config") {
                            add_file(
                                root,
                                &format!("tabs/{name}/{child}/{filename}"),
                                &mut files,
                                &mut remaining,
                            )?;
                        }
                    }
                }
            }
        }
    }
    Capture::from_files(files, true)
}

fn refuse_pending(entry: &fs::DirEntry, name: &str) -> Result<(), Fault> {
    if filesystem_key(name).ends_with(".pending")
        && !entry
            .file_type()
            .map_err(|e| io_fault("inspect pending configuration", e))?
            .is_dir()
    {
        return Err(fault(
            "an unresolved configuration write must be preserved or repaired first",
        ));
    }
    Ok(())
}

fn entries(path: &Path, count: &mut usize) -> Result<Vec<fs::DirEntry>, Fault> {
    check_directory(path)?;
    let mut output = Vec::new();
    for entry in fs::read_dir(path).map_err(|e| io_fault("enumerate configuration", e))? {
        *count += 1;
        if *count > MAX_ENUMERATED {
            return Err(fault("configuration enumeration exceeds its bound"));
        }
        output.push(entry.map_err(|e| io_fault("read configuration entry", e))?);
    }
    Ok(output)
}

fn add_file(
    root: &Path,
    relative: &str,
    files: &mut BTreeMap<String, Vec<u8>>,
    remaining: &mut usize,
) -> Result<(), Fault> {
    if files.len() >= MAX_FILES {
        return Err(fault("configuration exceeds its file budget"));
    }
    let maximum = path_kind(relative)?.maximum().min(*remaining);
    let path = root.join(relative);
    let before = checked_file(&path, maximum)?;
    let bytes = read_bytes(&path, maximum)?;
    let after = checked_file(&path, maximum)?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(fault("configuration changed while being read"));
    }
    #[cfg(unix)]
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(fault("configuration changed while being read"));
    }
    *remaining -= bytes.len();
    files.insert(relative.to_owned(), bytes);
    Ok(())
}

/// Atomic no-replace publication for both files and directories. An unsupported
/// platform/filesystem is an error, never an overwriting rename fallback.
pub fn publish_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        #[cfg(target_os = "macos")]
        // SAFETY: both pointers reference live NUL-terminated strings consumed
        // synchronously. RENAME_EXCL enforces no-replace publication.
        #[expect(unsafe_code, reason = "audited macOS atomic no-replace rename")]
        let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        // SAFETY: both pointers reference live NUL-terminated strings consumed
        // synchronously. AT_FDCWD and RENAME_NOREPLACE are valid renameat2 arguments.
        #[expect(unsafe_code, reason = "audited Linux atomic no-replace rename")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        if from[..from.len() - 1].contains(&0) || to[..to.len() - 1].contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains NUL",
            ));
        }
        // SAFETY: both pointers reference live NUL-terminated UTF-16 buffers consumed
        // synchronously. Zero flags omit REPLACE_EXISTING and COPY_ALLOWED.
        #[expect(unsafe_code, reason = "audited Windows atomic no-replace move")]
        let result = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
        };
        if result != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no atomic no-replace publication primitive",
        ))
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn temporary(parent: &Path, label: &str) -> PathBuf {
    parent.join(format!(
        ".{label}-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ))
}

pub(crate) fn create_private_file(path: &Path) -> Result<File, Fault> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .map_err(|e| io_fault("create private staging file", e))
}

pub(crate) fn create_private_directory(path: &Path) -> Result<(), Fault> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|e| io_fault("create private staging directory", e))
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Fault> {
    let mut file = create_private_file(path)?;
    file.write_all(bytes)
        .map_err(|e| io_fault("write staged configuration", e))?;
    file.sync_all()
        .map_err(|e| io_fault("sync staged configuration", e))
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), Fault> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|e| io_fault("sync configuration directory", e))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn fault(message: &str) -> Fault {
    Fault::new("Configuration", message)
}

pub(crate) fn io_fault(operation: &str, error: io::Error) -> Fault {
    Fault::new("ConfigurationIo", format!("could not {operation}"))
        .with_context(json!({"operation": operation, "kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct Root(pub PathBuf);
    impl Root {
        pub(crate) fn new() -> Self {
            let path = temporary(&std::env::temp_dir(), "mado-configuration-test");
            create_private_directory(&path).unwrap();
            Self(path)
        }
        pub(crate) fn put(&self, path: &str, bytes: &[u8]) {
            let destination = self.0.join(path);
            crate::storage::private_directory(destination.parent().unwrap()).unwrap();
            write_private(&destination, bytes).unwrap();
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn captures_malformed_orphaned_and_closed_scope_without_payloads() {
        let root = Root::new();
        root.put("settings.json", b"invalid JSON\0");
        root.put("tabs/Closed/tab.config", br#"{"open":false}"#);
        root.put("tabs/Orphan/pkg/target.config", b"future owner");
        root.put("profiles/old.json", b"malformed legacy");
        root.put("tabs/Closed/pkg/source.lua", b"excluded");
        root.put("logs/run.log", b"excluded");
        root.put("backups/app.config.1", b"excluded");
        root.put(".unrelated-temporary", b"excluded");
        let captured = capture(&root.0).unwrap();
        assert_eq!(captured.files.len(), 4);
        assert_eq!(captured.files["settings.json"], b"invalid JSON\0");
        assert_eq!(captured, capture(&root.0).unwrap());
        assert!(captured.files.contains_key("tabs/Orphan/pkg/target.config"));
    }

    #[test]
    fn packages_subtree_is_not_read_or_admitted_to_configuration_snapshots() {
        let root = Root::new();
        root.put("settings.json", b"retained settings");
        for area in ["sources", "pkgs"] {
            root.put(&format!("{area}/sample/main.ts"), b"source before");
            root.put(
                &format!("{area}/sample/profiles/default.json"),
                b"portable preset",
            );
        }
        let before = capture(&root.0).unwrap();
        let observed = capture_between(&root.0, || {
            for area in ["sources", "pkgs"] {
                fs::write(root.0.join(area).join("sample/main.ts"), b"source after").unwrap();
            }
        })
        .unwrap();
        assert_eq!(observed, before);
        assert_eq!(
            observed.files,
            BTreeMap::from([("settings.json".into(), b"retained settings".to_vec())])
        );
        for area in ["sources", "pkgs"] {
            assert!(
                Capture::from_files(
                    BTreeMap::from([(format!("{area}/sample/main.ts"), b"overwrite".to_vec())]),
                    true,
                )
                .is_err()
            );
            assert_eq!(
                fs::read(root.0.join(area).join("sample/main.ts")).unwrap(),
                b"source after"
            );
            assert_eq!(
                fs::read(root.0.join(area).join("sample/profiles/default.json")).unwrap(),
                b"portable preset"
            );
        }
    }

    #[test]
    fn rejects_aggregate_and_aliasing_paths() {
        let files = BTreeMap::from([
            ("tabs/One/tab.config".into(), vec![]),
            ("tabs/one/pkg/a.config".into(), vec![]),
        ]);
        assert!(Capture::from_files(files, true).is_err());
        let files = (0..=256)
            .map(|i| (format!("profiles/{i}.json"), vec![0; 64 * 1024]))
            .collect();
        assert!(Capture::from_files(files, true).is_err());
    }

    #[test]
    fn publication_never_replaces_files_or_directories() {
        let root = Root::new();
        root.put("first", b"first");
        root.put("second", b"second");
        assert!(publish_no_replace(&root.0.join("first"), &root.0.join("second")).is_err());
        assert_eq!(fs::read(root.0.join("second")).unwrap(), b"second");
        create_private_directory(&root.0.join("a")).unwrap();
        create_private_directory(&root.0.join("b")).unwrap();
        assert!(publish_no_replace(&root.0.join("a"), &root.0.join("b")).is_err());
        assert!(root.0.join("a").is_dir());
    }

    #[test]
    fn changing_source_set_refuses_a_mixed_capture() {
        let root = Root::new();
        root.put("settings.json", b"before");
        assert!(
            capture_between(&root.0, || {
                fs::write(root.0.join("settings.json"), b"after").unwrap();
                root.put("profiles/new.json", b"new");
            })
            .is_err()
        );
    }

    #[test]
    fn concurrent_publications_have_exactly_one_winner() {
        use std::sync::{Arc, Barrier};
        let root = Root::new();
        root.put("a", b"one");
        root.put("b", b"two");
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let barrier = Arc::clone(&barrier);
                let from = root.0.join(name);
                let to = root.0.join("published");
                std::thread::spawn(move || {
                    barrier.wait();
                    publish_no_replace(&from, &to).is_ok()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|handle| usize::from(handle.join().unwrap()))
                .sum::<usize>(),
            1
        );
        let published = fs::read(root.0.join("published")).unwrap();
        assert!(published == b"one" || published == b"two");
        assert_ne!(root.0.join("a").exists(), root.0.join("b").exists());
    }

    #[test]
    fn pending_files_refuse_capture_without_omitting_valid_pending_named_packages() {
        for path in [
            "settings.pending",
            "profiles/old.pending",
            "tabs/One/tab.pending",
            "tabs/One/pkg/profile.pending",
        ] {
            let root = Root::new();
            root.put(path, b"pending");
            assert!(capture(&root.0).is_err(), "{path}");
        }
        let root = Root::new();
        root.put("tabs/Orphan/pkg.pending/target.config", b"preserved");
        assert_eq!(
            capture(&root.0).unwrap().files["tabs/Orphan/pkg.pending/target.config"],
            b"preserved"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_linked_configuration_and_unsafe_directories() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = Root::new();
        root.put("original", b"{}");
        symlink(root.0.join("original"), root.0.join("settings.json")).unwrap();
        assert!(capture(&root.0).is_err());
        fs::remove_file(root.0.join("settings.json")).unwrap();
        fs::hard_link(root.0.join("original"), root.0.join("settings.json")).unwrap();
        assert!(capture(&root.0).is_err());
        fs::remove_file(root.0.join("settings.json")).unwrap();
        create_private_directory(&root.0.join("tabs")).unwrap();
        fs::set_permissions(root.0.join("tabs"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(capture(&root.0).is_err());
    }
}
