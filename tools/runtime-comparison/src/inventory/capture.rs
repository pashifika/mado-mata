use super::validation::{
    ImageBudget, catalog, declare, json_file, portable_component, portable_path, text_file,
    validate_manifest, validate_map, validate_source,
};
use super::{Inventory, MAX_BYTES, MAX_DEPTH, MAX_FILES, Manifest, invalid};
use crate::host::resolve_options;
use crate::images::{PayloadBytes, reserve_payload};
use crate::model::{Fault, Limits};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

impl Inventory {
    pub fn capture(root: &Path, limits: &Limits) -> Result<Self, Fault> {
        Self::capture_with_stop(root, limits, None)
    }

    pub(crate) fn capture_with_stop(
        root: &Path,
        limits: &Limits,
        stop: Option<&AtomicBool>,
    ) -> Result<Self, Fault> {
        let capture = capture_files(root, limits, stop)?;
        inventory_from_capture(capture, limits)
    }
}

pub(super) fn capture_files<'a>(
    root: &Path,
    limits: &Limits,
    stop: Option<&'a AtomicBool>,
) -> Result<Capture<'a>, Fault> {
    capture_files_bounded(root, limits, stop, 1)
}

pub(super) fn capture_recovery_files<'a>(
    root: &Path,
    limits: &Limits,
) -> Result<Capture<'a>, Fault> {
    // A recovery read can contain the union of two otherwise valid revisions.
    // This does not change ordinary Inventory, Plan or draft admission ceilings.
    capture_files_bounded(root, limits, None, 2)
}

fn capture_files_bounded<'a>(
    root: &Path,
    limits: &Limits,
    stop: Option<&'a AtomicBool>,
    revisions: usize,
) -> Result<Capture<'a>, Fault> {
    if limits.snapshot_files == 0
        || limits.snapshot_files > MAX_FILES
        || limits.snapshot_bytes == 0
        || limits.snapshot_bytes > MAX_BYTES
        || limits.duration_ms == 0
    {
        return Err(invalid(
            "snapshot bounds must be positive and within application ceilings",
        ));
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(limits.duration_ms))
        .ok_or_else(|| invalid("snapshot deadline overflows"))?;
    let root = checked_root(root)?;
    let mut capture = Capture {
        files: BTreeMap::new(),
        stamps: BTreeMap::new(),
        bytes: 0,
        file_limit: limits.snapshot_files * revisions,
        byte_limit: limits.snapshot_bytes * revisions,
        deadline,
        stop,
    };
    capture.collect(&root, "", 0)?;
    // A second bounded pass compares file identities, contents and directory membership.
    // Execution never reopens these paths after the capture completes.
    capture.verify(&root)?;
    Ok(capture)
}

pub(super) fn inventory_from_files(
    files: BTreeMap<String, PayloadBytes>,
    limits: &Limits,
) -> Result<Inventory, Fault> {
    let capture = Capture {
        bytes: files.values().map(|bytes| bytes.len()).sum(),
        files,
        stamps: BTreeMap::new(),
        file_limit: limits.snapshot_files,
        byte_limit: limits.snapshot_bytes,
        deadline: Instant::now() + Duration::from_millis(limits.duration_ms),
        stop: None,
    };
    inventory_from_capture(capture, limits)
}

fn inventory_from_capture(mut capture: Capture<'_>, limits: &Limits) -> Result<Inventory, Fault> {
    let raw_manifest = capture
        .files
        .get("package.json")
        .ok_or_else(|| invalid("package.json is required"))?;
    if raw_manifest.len() > crate::images::PACKAGE_NON_IMAGE_BYTES {
        return Err(invalid("package non-image byte limit exceeded"));
    }
    let manifest: Manifest = serde_json::from_slice(raw_manifest)
        .map_err(|error| invalid(format!("invalid package.json: {error}")))?;
    validate_manifest(&manifest)?;
    let mut image_budget = ImageBudget::default();
    for (id, asset) in &manifest.assets {
        let bytes = capture
            .files
            .get(&asset.path)
            .ok_or_else(|| invalid(format!("asset {id} is missing: {}", asset.path)))?;
        image_budget.add(id, asset, bytes.len())?;
    }
    image_budget.check_total(capture.bytes)?;
    let files: BTreeMap<_, _> = capture
        .files
        .iter()
        .map(|(path, bytes)| {
            (
                path.clone(),
                json!({"sha256": format!("{:x}", bytes.digest()), "bytes": bytes.len()}),
            )
        })
        .collect();
    let mut declared = BTreeSet::from(["package.json".to_owned()]);
    let mut sources = BTreeMap::new();
    for path in &manifest.sources {
        declare(&mut declared, path)?;
        let source = text_file(&mut capture.files, path)?;
        validate_source(path, &source, &manifest.runtime)?;
        sources.insert(path.clone(), source);
    }
    declare(&mut declared, &manifest.schema)?;
    let schema = json_file(&mut capture.files, &manifest.schema)?;
    let mut profiles = BTreeMap::new();
    for (id, path) in &manifest.profiles {
        portable_component(id)?;
        declare(&mut declared, path)?;
        let profile = json_file(&mut capture.files, path)?;
        resolve_options(&schema, &profile, &manifest.package_id)?;
        profiles.insert(id.clone(), profile);
    }
    let mut assets = BTreeMap::new();
    for (id, asset) in &manifest.assets {
        portable_component(id)?;
        declare(&mut declared, &asset.path)?;
        let bytes = capture
            .files
            .remove(&asset.path)
            .ok_or_else(|| invalid(format!("asset {id} is missing: {}", asset.path)))?;
        assets.insert(id.clone(), bytes);
    }
    let mut source_maps = BTreeMap::new();
    for (module, path) in &manifest.source_maps {
        if !sources.contains_key(module) {
            return Err(invalid(format!(
                "source map refers to undeclared source: {module}"
            )));
        }
        declare(&mut declared, path)?;
        let map = text_file(&mut capture.files, path)?;
        validate_map(&map)?;
        source_maps.insert(module.clone(), map);
    }
    for path in capture.files.keys() {
        if !declared.contains(path) {
            return Err(invalid(format!("undeclared package file: {path}")));
        }
    }
    let (approved_sources, catalog) = catalog(&manifest)?;
    for (id, source) in approved_sources {
        capture.reserve(source.len())?;
        sources.insert(id, source);
    }
    image_budget.check_total(capture.bytes)?;
    if sources.len() + declared.len() - manifest.sources.len() > limits.snapshot_files {
        return Err(invalid(
            "approved dependency closure exceeds snapshot file bound",
        ));
    }
    let metadata = json!({
        "manifest": manifest,
        "runtime": manifest.runtime,
        "files": files,
        "catalog": catalog,
        "capture_limits": {"files": limits.snapshot_files, "bytes": limits.snapshot_bytes}
    });
    let mut inventory = Inventory {
        identity: String::new(),
        package_id: manifest.package_id,
        sources,
        assets,
        schema,
        profiles,
        entries: manifest.entries,
        metadata,
        source_maps,
    };
    inventory.refresh_identity()?;
    capture.check()?;
    Ok(inventory)
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    directory: bool,
    length: u64,
    modified: SystemTime,
    identity: (u64, u64, u64, u64),
}

pub(super) struct Capture<'a> {
    pub(super) files: BTreeMap<String, PayloadBytes>,
    stamps: BTreeMap<String, Stamp>,
    bytes: usize,
    file_limit: usize,
    byte_limit: usize,
    deadline: Instant,
    stop: Option<&'a AtomicBool>,
}

impl Capture<'_> {
    pub(super) fn directories(&self) -> impl Iterator<Item = &str> {
        self.stamps
            .iter()
            .filter(|(path, stamp)| !path.is_empty() && stamp.directory)
            .map(|(path, _)| path.as_str())
    }

    fn check(&self) -> Result<(), Fault> {
        if self.stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err(Fault::new(
                "Cancelled",
                "Stop requested during inventory capture",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(Fault::new("Timeout", "inventory capture deadline exceeded"));
        }
        Ok(())
    }

    fn reserve(&mut self, bytes: usize) -> Result<(), Fault> {
        self.check()?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| invalid("snapshot size overflows"))?;
        if self.bytes > self.byte_limit {
            return Err(invalid("snapshot byte limit exceeded"));
        }
        Ok(())
    }

    fn collect(&mut self, root: &Path, id: &str, depth: usize) -> Result<(), Fault> {
        self.check()?;
        if depth > MAX_DEPTH
            || self.stamps.len() >= self.file_limit.saturating_mul(2).saturating_add(1)
        {
            return Err(invalid("snapshot directory/depth bound exceeded"));
        }
        let path = root.join(id);
        let before = checked_metadata(&path)?;
        let original = stamp(&before)?;
        if before.is_dir() {
            self.stamps.insert(id.to_owned(), original);
            let mut names = BTreeSet::new();
            for entry in fs::read_dir(&path).map_err(|error| io_fault(id, error))? {
                self.check()?;
                let entry = entry.map_err(|error| io_fault(id, error))?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid("non-UTF-8 package path"))?;
                portable_component(&name)?;
                if !names.insert(name.to_ascii_lowercase()) {
                    return Err(invalid(format!(
                        "case-colliding package entry: {id}/{name}"
                    )));
                }
                let child = if id.is_empty() {
                    name
                } else {
                    format!("{id}/{name}")
                };
                portable_path(&child)?;
                self.collect(root, &child, depth + 1)?;
            }
            if self.stamps.get(id) != Some(&stamp(&checked_metadata(&path)?)?) {
                return Err(changed(id));
            }
        } else {
            if self.files.len() >= self.file_limit {
                return Err(invalid("snapshot file limit exceeded"));
            }
            let size = usize::try_from(before.len())
                .map_err(|_| invalid("snapshot file size overflows"))?;
            self.reserve(size)?;
            let bytes = read_stable(&path, id, &original, size, self.deadline)?;
            self.stamps.insert(id.to_owned(), original);
            self.files.insert(id.to_owned(), bytes);
        }
        Ok(())
    }

    fn verify(&self, root: &Path) -> Result<(), Fault> {
        self.check()?;
        checked_root(root)?;
        for (id, original) in &self.stamps {
            self.check()?;
            let path = root.join(id);
            if stamp(&checked_metadata(&path)?)? != *original {
                return Err(changed(id));
            }
            if original.directory {
                let mut count = 0usize;
                for entry in fs::read_dir(&path).map_err(|error| io_fault(id, error))? {
                    self.check()?;
                    let entry = entry.map_err(|error| io_fault(id, error))?;
                    let name = entry.file_name().into_string().map_err(|_| changed(id))?;
                    let child = if id.is_empty() {
                        name
                    } else {
                        format!("{id}/{name}")
                    };
                    if !self.stamps.contains_key(&child) {
                        return Err(changed(&child));
                    }
                    count += 1;
                    if count > self.file_limit.saturating_mul(2) {
                        return Err(invalid("snapshot directory membership exceeds bound"));
                    }
                }
            } else {
                let expected = self.files.get(id).ok_or_else(|| changed(id))?;
                let bytes = read_stable(&path, id, original, expected.len(), self.deadline)?;
                if bytes != *expected {
                    return Err(changed(id));
                }
            }
        }
        for (id, original) in &self.stamps {
            self.check()?;
            if stamp(&checked_metadata(&root.join(id))?)? != *original {
                return Err(changed(id));
            }
        }
        checked_root(root)?;
        Ok(())
    }
}

pub(super) fn checked_root(root: &Path) -> Result<PathBuf, Fault> {
    let absolute = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| io_fault("package root", error))?
            .join(root)
    };
    if absolute
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(invalid("package root may not contain parent traversal"));
    }
    // The operator's root may live below an OS path alias. Resolve that trusted
    // location once; links inside the captured package remain forbidden.
    if !checked_metadata(&absolute)?.is_dir() {
        return Err(invalid("package root is not a directory"));
    }
    let canonical = absolute
        .canonicalize()
        .map_err(|error| io_fault("package root", error))?;
    if !checked_metadata(&canonical)?.is_dir() {
        return Err(invalid("canonical package root is not a directory"));
    }
    Ok(canonical)
}

fn checked_metadata(path: &Path) -> Result<Metadata, Fault> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package root");
    let metadata = fs::symlink_metadata(path).map_err(|error| io_fault(name, error))?;
    if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
        return Err(
            invalid("package paths must be regular files or directories, never links")
                .with_context(json!({"path": name})),
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(
                invalid("package reparse paths are forbidden").with_context(json!({"path": name}))
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.is_file() && metadata.nlink() != 1 {
            return Err(invalid("package hard-linked files are forbidden")
                .with_context(json!({"path": name})));
        }
    }
    Ok(metadata)
}

fn stamp(metadata: &Metadata) -> Result<Stamp, Fault> {
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        )
    };
    #[cfg(windows)]
    let identity = {
        use std::os::windows::fs::MetadataExt;
        (
            metadata.creation_time(),
            metadata.last_write_time(),
            metadata.file_attributes() as u64,
            0,
        )
    };
    #[cfg(not(any(unix, windows)))]
    return Err(invalid(
        "snapshot file identity is unsupported on this platform",
    ));
    #[cfg(any(unix, windows))]
    Ok(Stamp {
        directory: metadata.is_dir(),
        length: metadata.len(),
        modified: metadata
            .modified()
            .map_err(|error| io_fault("file timestamp", error))?,
        identity,
    })
}

fn read_stable(
    path: &Path,
    id: &str,
    original: &Stamp,
    size: usize,
    deadline: Instant,
) -> Result<PayloadBytes, Fault> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x100 | 0x4); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000 | 0x800); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000); // FILE_SHARE_READ, OPEN_REPARSE_POINT
    }
    let mut file: File = options.open(path).map_err(|error| io_fault(id, error))?;
    let opened = file.metadata().map_err(|error| io_fault(id, error))?;
    if !opened.is_file() || stamp(&opened)? != *original {
        return Err(changed(id));
    }
    #[cfg(windows)]
    let windows_identity = windows_file_identity(&file)?;
    let reservation = reserve_payload(size)?;
    let _scratch = reserve_payload(16 * 1024)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| invalid("snapshot allocation refused"))?;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err(Fault::new(
                "Timeout",
                "inventory file read deadline exceeded",
            ));
        }
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_fault(id, error))?;
        if count == 0 {
            break;
        }
        if count > size.saturating_sub(bytes.len()) {
            return Err(changed(id));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if bytes.len() != size
        || stamp(&file.metadata().map_err(|error| io_fault(id, error))?)? != *original
        || stamp(&checked_metadata(path)?)? != *original
    {
        return Err(changed(id));
    }
    #[cfg(windows)]
    if windows_file_identity(&file)? != windows_identity {
        return Err(changed(id));
    }
    PayloadBytes::from_reserved(bytes, reservation)
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<(u32, u32, u32), Fault> {
    use std::os::windows::io::AsRawHandle;
    // BY_HANDLE_FILE_INFORMATION: DWORD fields and three FILETIME pairs.
    #[repr(C)]
    #[derive(Default)]
    struct FileInformation {
        attributes: u32,
        creation: [u32; 2],
        access: [u32; 2],
        write: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            info: *mut FileInformation,
        ) -> i32;
    }
    let mut info = FileInformation::default();
    // SAFETY: file owns a live non-pipe handle; info is a writable, correctly laid-out buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io_fault("file identity", std::io::Error::last_os_error()));
    }
    if info.links != 1 || info.attributes & 0x400 != 0 {
        return Err(invalid(
            "package hard links and reparse points are forbidden",
        ));
    }
    Ok((info.volume, info.index_high, info.index_low))
}

fn changed(id: &str) -> Fault {
    invalid("package changed during bounded capture").with_context(json!({"path": id}))
}
fn io_fault(id: &str, error: std::io::Error) -> Fault {
    invalid("package file operation failed")
        .with_context(json!({"path": id, "cause": error.to_string()}))
}
