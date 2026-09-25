use super::observation::{Candidate, MAX_CANDIDATES, SigningIdentity, correspondence, summarize};
use super::{
    ApplicationObservation, ResolvedLocation, TargetConfiguration, TargetDeclaration,
    TargetResolution, bundle_metadata, canonical, executable, metadata_fault,
    observation_checkpoint, path_string, read_metadata,
};
use mado_runtime_comparison::model::Fault;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{
    CFBundle, CFDictionary, CFString, CFType, CFURL, kCFBundleExecutableKey, kCFBundleIdentifierKey,
};
use objc2_foundation::{NSString, NSURL};
use serde_json::json;
use std::fs;
use std::mem::{MaybeUninit, size_of};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[path = "signing.rs"]
mod signing;

pub(super) struct Guard<'a> {
    cancelled: &'a AtomicBool,
    deadline: Instant,
}

impl Guard<'_> {
    fn check(&self) -> Result<(), Fault> {
        observation_checkpoint(self.cancelled, self.deadline)
    }

    fn call<T>(&self, call: impl FnOnce() -> T) -> Result<T, Fault> {
        self.check()?;
        let result = call();
        self.check()?;
        Ok(result)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileIdentity {
    fn read(path: &Path, field: &str) -> Result<Self, Fault> {
        let metadata = fs::symlink_metadata(path).map_err(|_| metadata_fault(field, "identity"))?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mode: metadata.mode(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct InstalledBundle {
    pub resolution: ResolvedLocation,
    pub bundle_id: Option<String>,
    identities: Vec<(PathBuf, FileIdentity)>,
}

fn contained(root: &Path, path: &Path, field: &str, stage: &str) -> Result<PathBuf, Fault> {
    let resolved = canonical(path, field)?;
    if resolved == root || !resolved.starts_with(root) {
        return Err(metadata_fault(field, stage));
    }
    Ok(resolved)
}

pub(super) fn resolve_bundle(path: &str, field: &str) -> Result<InstalledBundle, Fault> {
    inspect_bundle(path, field, &|| Ok(()))
}

fn require_absent(path: &Path, field: &str, stage: &str) -> Result<(), Fault> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(metadata_fault(field, stage)),
    }
}

// Every Info plist in CoreFoundation's layout table, relative to the outer root
// (CFBundle_InfoPlist.c). Whatever layout it detects, it prefers Info-macos.plist
// and opens the exact name with a blocking, unbounded read. Its case-insensitive
// directory match only chooses which name to open, so lstat of these names sees
// what Foundation would open on case-sensitive and case-insensitive volumes.
const FOUNDATION_INFO: [&str; 12] = [
    "Info.plist",
    "Info-macos.plist",
    "Contents/Info.plist",
    "Contents/Info-macos.plist",
    "Resources/Info.plist",
    "Resources/Info-macos.plist",
    "Support Files/Info.plist",
    "Support Files/Info-macos.plist",
    "WrappedBundle/Info.plist",
    "WrappedBundle/Info-macos.plist",
    "WrappedBundle/Contents/Info.plist",
    "WrappedBundle/Contents/Info-macos.plist",
];

/// Refuses any Foundation-readable Info plist except the validated one.
fn require_only_validated_info(
    selected: &Path,
    validated: &str,
    field: &str,
    stage: &str,
    checkpoint: &impl Fn() -> Result<(), Fault>,
) -> Result<(), Fault> {
    for location in FOUNDATION_INFO {
        if location != validated {
            checkpoint()?;
            require_absent(&selected.join(location), field, stage)?;
        }
    }
    Ok(())
}

fn foundation_string<'a>(
    value: &'a CFType,
    expected: &str,
    field: &str,
) -> Result<&'a CFString, Fault> {
    let value = value
        .downcast_ref::<CFString>()
        .ok_or_else(|| metadata_fault(field, "foundation_freshness"))?;
    if !usize::try_from(value.length()).is_ok_and(|length| length <= expected.len())
        || value.to_string() != expected
    {
        return Err(metadata_fault(field, "foundation_freshness"));
    }
    Ok(value)
}

fn inspect_bundle(
    path: &str,
    field: &str,
    checkpoint: &impl Fn() -> Result<(), Fault>,
) -> Result<InstalledBundle, Fault> {
    checkpoint()?;
    let selected = canonical(path, field)?;
    checkpoint()?;
    if !selected
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        || !fs::metadata(&selected)
            .map_err(|_| metadata_fault(field, "bundle"))?
            .is_dir()
    {
        return Err(metadata_fault(field, "bundle"));
    }
    let mut identities = Vec::with_capacity(10);
    let mut remember = |path: &Path| -> Result<(), Fault> {
        checkpoint()?;
        if identities.iter().any(|(remembered, _)| remembered == path) {
            return Ok(());
        }
        identities.push((path.to_owned(), FileIdentity::read(path, field)?));
        Ok(())
    };
    remember(Path::new(path))?;
    remember(&selected)?;

    // Only inspect supported metadata layouts, never search for an executable.
    checkpoint()?;
    let wrapped = match fs::symlink_metadata(selected.join("WrappedBundle")) {
        Ok(metadata) if metadata.file_type().is_symlink() => true,
        Ok(_) => return Err(metadata_fault(field, "wrapper")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(metadata_fault(field, "wrapper")),
    };
    let (info, executable_directory) = if wrapped {
        checkpoint()?;
        require_absent(&selected.join("Contents"), field, "layout")?;
        let link = selected.join("WrappedBundle");
        remember(&link)?;
        remember(&selected.join("Wrapper"))?;
        checkpoint()?;
        let wrapper = contained(&selected, &selected.join("Wrapper"), field, "wrapper")?;
        remember(&wrapper)?;
        checkpoint()?;
        let inner = contained(&selected, &link, field, "wrapper")?;
        if inner.parent() != Some(wrapper.as_path())
            || !inner
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        {
            return Err(metadata_fault(field, "wrapper"));
        }
        remember(&inner)?;
        checkpoint()?;
        require_absent(&inner.join("Contents"), field, "layout")?;
        for name in ["BundleMetadata.plist", "iTunesMetadata.plist"] {
            let metadata = wrapper.join(name);
            checkpoint()?;
            match fs::symlink_metadata(&metadata) {
                Ok(_) => {
                    remember(&metadata)?;
                    checkpoint()?;
                    let metadata = contained(&selected, &metadata, field, "wrapper")?;
                    remember(&metadata)?;
                    checkpoint()?;
                    bundle_metadata(&read_metadata(&metadata, field)?, field)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(metadata_fault(field, "wrapper")),
            }
        }
        (inner.join("Info.plist"), inner)
    } else {
        let contents = selected.join("Contents");
        remember(&contents)?;
        (contents.join("Info.plist"), contents.join("MacOS"))
    };
    remember(&info)?;
    checkpoint()?;
    let info = contained(&selected, &info, field, "plist")?;
    remember(&info)?;
    checkpoint()?;
    let metadata = bundle_metadata(&read_metadata(&info, field)?, field)?;
    let name = metadata
        .executable
        .ok_or_else(|| metadata_fault(field, "plist"))?;
    remember(&executable_directory)?;
    let expected_path = executable_directory.join(&name);
    remember(&expected_path)?;
    checkpoint()?;
    let expected_executable = contained(&selected, &expected_path, field, "executable")?;
    remember(&expected_executable)?;
    checkpoint()?;
    executable(&expected_executable, field)?;
    let validated = if wrapped {
        "WrappedBundle/Info.plist"
    } else {
        "Contents/Info.plist"
    };
    require_only_validated_info(&selected, validated, field, "layout", checkpoint)?;

    checkpoint()?;
    let url =
        CFURL::from_directory_path(&selected).ok_or_else(|| metadata_fault(field, "foundation"))?;
    checkpoint()?;
    let bundle =
        CFBundle::new(None, Some(&url)).ok_or_else(|| metadata_fault(field, "foundation"))?;
    checkpoint()?;
    // Bundle instance getters cache Info.plist across same-process updates.
    let information = CFBundle::info_dictionary_for_url(Some(&url))
        .ok_or_else(|| metadata_fault(field, "foundation"))?;
    checkpoint()?;
    // SAFETY: The information dictionary has CFString keys and CFType values.
    // Individual value types are checked before use.
    let information: &CFDictionary<CFString, CFType> = unsafe { information.cast_unchecked() };
    // SAFETY: CoreFoundation exports immutable process-lifetime dictionary keys.
    let (executable_key, identifier_key) =
        unsafe { (kCFBundleExecutableKey, kCFBundleIdentifierKey) };
    let native_name = information
        .get(executable_key.ok_or_else(|| metadata_fault(field, "foundation"))?)
        .ok_or_else(|| metadata_fault(field, "foundation_freshness"))?;
    let native_name = foundation_string(&native_name, &name, field)?;
    let native_id =
        information.get(identifier_key.ok_or_else(|| metadata_fault(field, "foundation"))?);
    match (native_id.as_deref(), metadata.bundle_id.as_deref()) {
        (Some(value), Some(expected)) => {
            foundation_string(value, expected, field)?;
        }
        (None, None) => {}
        _ => return Err(metadata_fault(field, "foundation_freshness")),
    }
    checkpoint()?;
    let executable_url = bundle
        .auxiliary_executable_url(Some(native_name))
        .ok_or_else(|| metadata_fault(field, "foundation"))?;
    checkpoint()?;
    let resolved_executable = executable_url
        .to_file_path()
        .ok_or_else(|| metadata_fault(field, "foundation"))?;
    checkpoint()?;
    let resolved_executable = contained(&selected, &resolved_executable, field, "executable")?;
    checkpoint()?;
    let bundle_id = metadata.bundle_id;
    if resolved_executable != expected_executable {
        return Err(metadata_fault(field, "foundation_freshness"));
    }
    let resolution = ResolvedLocation {
        path: path_string(&selected, field)?,
        executable: path_string(&resolved_executable, field)?,
    };
    for (path, before) in &identities {
        checkpoint()?;
        if FileIdentity::read(path, field)? != *before {
            return Err(metadata_fault(field, "changed"));
        }
    }
    require_only_validated_info(&selected, validated, field, "changed", checkpoint)?;
    checkpoint()?;
    if canonical(path, field)? != selected {
        return Err(metadata_fault(field, "changed"));
    }
    Ok(InstalledBundle {
        resolution,
        bundle_id,
        identities,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lifetime {
    pid: i32,
    seconds: u64,
    microseconds: u64,
}

fn lifetime(pid: i32, guard: &Guard<'_>) -> Result<Lifetime, Fault> {
    if pid <= 0 {
        return Err(unavailable("process_lifetime"));
    }
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(size_of::<libc::proc_bsdinfo>())
        .map_err(|_| unavailable("process_lifetime"))?;
    // SAFETY: The writable buffer matches PROC_PIDTBSDINFO's public SDK layout and size.
    let returned = guard.call(|| unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    })?;
    if returned != size {
        return Err(unavailable("process_lifetime"));
    }
    // SAFETY: libproc reported a complete initialized proc_bsdinfo above.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32
        || info.pbi_start_tvsec == 0
        || info.pbi_start_tvusec >= 1_000_000
        || info.pbi_status == libc::SZOMB
    {
        return Err(unavailable("process_lifetime"));
    }
    Ok(Lifetime {
        pid,
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

fn url_path(url: &NSURL) -> Result<String, Fault> {
    if !url.isFileURL() {
        return Err(unavailable("runtime_url"));
    }
    let path = url.path().ok_or_else(|| unavailable("runtime_url"))?;
    if path.length() > super::MAX_PATH_BYTES {
        return Err(unavailable("runtime_url"));
    }
    let path = path.to_string();
    super::absolute_path(&path, "runtime_url")?;
    path_string(&canonical(&path, "runtime_url")?, "runtime_url")
}

pub(super) fn unavailable(stage: &str) -> Fault {
    Fault::new(
        "TargetObservationEvidence",
        "application correspondence could not be established",
    )
    .with_context(json!({"stage": stage}))
}

fn invalidation(fault: &Fault) -> bool {
    matches!(
        fault.category.as_str(),
        "TargetObservationCancelled" | "TargetObservationTimeout"
    )
}

#[derive(Debug, PartialEq, Eq)]
struct CandidateSnapshot {
    lifetime: Lifetime,
    executable: String,
    architecture: i32,
    bundle_id: String,
}

fn candidate_snapshot(
    app: &NSRunningApplication,
    guard: &Guard<'_>,
) -> Result<CandidateSnapshot, Fault> {
    let pid = guard.call(|| app.processIdentifier())?;
    let lifetime = lifetime(pid, guard)?;
    let bundle_id = guard
        .call(|| app.bundleIdentifier())?
        .ok_or_else(|| unavailable("runtime_identifier"))?;
    if bundle_id.length() == 0 || bundle_id.length() > 255 {
        return Err(unavailable("runtime_identifier"));
    }
    let bundle_id = bundle_id.to_string();
    let architecture = i32::try_from(guard.call(|| app.executableArchitecture())?)
        .map_err(|_| unavailable("architecture"))?;
    if architecture == 0 {
        return Err(unavailable("architecture"));
    }
    let url = guard
        .call(|| app.executableURL())?
        .ok_or_else(|| unavailable("runtime_url"))?;
    let executable = guard.call(|| url_path(&url))??;
    let mut process_path = [0_u8; 4096];
    // SAFETY: The buffer is writable and its full capacity is supplied to libproc.
    let length = guard
        .call(|| unsafe { libc::proc_pidpath(pid, process_path.as_mut_ptr().cast(), 4096) })?;
    if length <= 0 {
        return Err(unavailable("process_executable"));
    }
    let process_path = std::ffi::CStr::from_bytes_until_nul(&process_path)
        .map_err(|_| unavailable("process_executable"))?
        .to_str()
        .map_err(|_| unavailable("process_executable"))?;
    if guard.call(|| canonical(process_path, "runtime_url"))?? != Path::new(&executable) {
        return Err(unavailable("process_executable"));
    }
    Ok(CandidateSnapshot {
        lifetime,
        executable,
        architecture,
        bundle_id,
    })
}

fn verify_candidate(
    app: &NSRunningApplication,
    selected: &InstalledBundle,
    selected_codes: &mut std::collections::BTreeMap<i32, signing::SelectedCode>,
    guard: &Guard<'_>,
) -> Result<(Candidate, CandidateSnapshot, SigningIdentity), Fault> {
    let before = candidate_snapshot(app, guard)?;
    if Some(before.bundle_id.as_str()) != selected.bundle_id.as_deref() {
        return Err(unavailable("runtime_identifier"));
    }
    if let std::collections::btree_map::Entry::Vacant(entry) =
        selected_codes.entry(before.architecture)
    {
        entry.insert(signing::selected(
            &selected.resolution.executable,
            before.architecture,
            guard,
        )?);
    }
    let installed = &selected_codes[&before.architecture];
    let running = signing::running(before.lifetime.pid, &before.executable, guard)?;
    let satisfies_requirement = if matches!(installed.identity, SigningIdentity::Signed(_))
        && matches!(running.identity, SigningIdentity::Signed(_))
    {
        signing::requirement(installed, &running, guard)?
    } else {
        true
    };
    let result = if satisfies_requirement {
        correspondence(
            &installed.identity,
            &running.identity,
            before.executable == selected.resolution.executable,
        )
    } else {
        Candidate::Different
    };
    signing::revalidate(&running, guard)?;
    let after = candidate_snapshot(app, guard)?;
    if before != after {
        return Err(unavailable("process_changed"));
    }
    Ok((result, after, running.identity))
}

pub(super) fn observe(
    configuration: &TargetConfiguration,
    declaration: &TargetDeclaration,
    expected_resolution: &TargetResolution,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<ApplicationObservation, Fault> {
    let guard = Guard {
        cancelled,
        deadline,
    };
    configuration.validate_declaration(declaration)?;
    if configuration.game.kind != "bundle" {
        return Err(Fault::new(
            "TargetObservationSelection",
            "application observation requires a saved bundle selection",
        ));
    }
    let result = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
        autoreleasepool(|_| {
            let selected = inspect_bundle(&configuration.game.path, "game", &|| guard.check())?;
            if selected.resolution != expected_resolution.game {
                let resolution = TargetResolution {
                    game: selected.resolution,
                    launcher: expected_resolution.launcher.clone(),
                    working_directory: expected_resolution.working_directory.clone(),
                };
                return Err(Fault::new(
                    "TargetResolutionChanged",
                    "installed application resolution changed; check and review before saving",
                )
                .with_context(
                    json!({"previous_resolution": expected_resolution, "resolution": resolution}),
                ));
            }
            if declaration.macos.as_ref().is_some_and(|constraint| {
                selected.bundle_id.as_deref() != Some(constraint.bundle_id.as_str())
            }) {
                return Err(super::configuration_fault(
                    "game",
                    "application identifier conflicts with the package declaration",
                ));
            }
            let Some(bundle_id) = selected.bundle_id.as_deref() else {
                return observation(
                    "unverifiable",
                    None,
                    json!({"stage": "bundle_identifier"}),
                    &guard,
                );
            };
            let identifier = NSString::from_str(bundle_id);
            let applications = guard.call(|| {
                NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier)
            })?;
            let count = applications.len();
            if count > MAX_CANDIDATES {
                return observation(
                    "unverifiable",
                    None,
                    json!({"stage": "candidate_limit", "candidate_count": count}),
                    &guard,
                );
            }
            let mut candidates = Vec::with_capacity(count);
            let mut snapshots = Vec::with_capacity(count);
            let mut details = Vec::with_capacity(count);
            let mut selected_codes = std::collections::BTreeMap::new();
            let mut discovered_pids = Vec::with_capacity(count);
            for app in applications.iter() {
                guard.check()?;
                discovered_pids.push(guard.call(|| app.processIdentifier())?);
                match verify_candidate(&app, &selected, &mut selected_codes, &guard) {
                    Ok((candidate, snapshot, identity)) => {
                        details.push(json!({"pid": snapshot.lifetime.pid, "architecture": snapshot.architecture,
                        "result": match candidate { Candidate::Verified(kind) => kind.name(), Candidate::Different => "different", Candidate::Unverifiable => "identity_mismatch" }}));
                        candidates.push(candidate);
                        snapshots.push((app, snapshot, identity));
                    }
                    Err(fault) if invalidation(&fault) => return Err(fault),
                    Err(fault) => {
                        details.push(json!({"stage": fault.context["stage"], "status": fault.context["os_status"]}));
                        candidates.push(Candidate::Unverifiable);
                    }
                }
            }
            for (architecture, before) in &selected_codes {
                match signing::selected(&selected.resolution.executable, *architecture, &guard) {
                    Ok(after) if after.identity == before.identity => {}
                    Err(fault) if invalidation(&fault) => return Err(fault),
                    _ => {
                        return observation(
                            "unverifiable",
                            None,
                            json!({"stage": "installation_changed"}),
                            &guard,
                        );
                    }
                }
            }
            for (app, before, identity) in &snapshots {
                match signing::running(before.lifetime.pid, &before.executable, &guard) {
                    Ok(after) if after.identity == *identity => {}
                    Err(fault) if invalidation(&fault) => return Err(fault),
                    _ => {
                        return observation(
                            "unverifiable",
                            None,
                            json!({"stage": "running_changed"}),
                            &guard,
                        );
                    }
                }
                match candidate_snapshot(app, &guard) {
                    Ok(after) if *before == after => {}
                    Err(fault) if invalidation(&fault) => return Err(fault),
                    _ => {
                        return observation(
                            "unverifiable",
                            None,
                            json!({"stage": "process_changed"}),
                            &guard,
                        );
                    }
                }
            }
            let after = inspect_bundle(&configuration.game.path, "game", &|| guard.check())?;
            if after != selected {
                return observation(
                    "unverifiable",
                    None,
                    json!({"stage": "installation_changed"}),
                    &guard,
                );
            }
            let current = guard.call(|| {
                NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier)
            })?;
            if current.len() != count {
                return observation(
                    "unverifiable",
                    None,
                    json!({"stage": "candidates_changed"}),
                    &guard,
                );
            }
            let mut current_pids = Vec::with_capacity(count);
            for app in current.iter() {
                current_pids.push(guard.call(|| app.processIdentifier())?);
            }
            current_pids.sort_unstable();
            discovered_pids.sort_unstable();
            if current_pids != discovered_pids
                || current_pids.windows(2).any(|pair| pair[0] == pair[1])
            {
                return observation(
                    "unverifiable",
                    None,
                    json!({"stage": "candidates_changed"}),
                    &guard,
                );
            }
            for (_, before, _) in &snapshots {
                match lifetime(before.lifetime.pid, &guard) {
                    Ok(after) if after == before.lifetime => {}
                    Err(fault) if invalidation(&fault) => return Err(fault),
                    _ => {
                        return observation(
                            "unverifiable",
                            None,
                            json!({"stage": "process_changed"}),
                            &guard,
                        );
                    }
                }
            }
            let (status, evidence) = summarize(&candidates);
            observation(
                status,
                evidence.map(|kind| kind.name()),
                json!({
                    "candidate_count": count, "candidates": details,
                    "originating_copy": "unavailable", "window_identity": "not_checked",
                    "running_build_identity": if evidence == Some(super::observation::Evidence::UnsignedPath) { "not_established" } else { "signed_code_only" },
                }),
                &guard,
            )
        })
    }));
    guard.check()?;
    result.map_err(|_| unavailable("platform_exception"))?
}

fn observation(
    status: &str,
    evidence: Option<&str>,
    diagnostics: serde_json::Value,
    guard: &Guard<'_>,
) -> Result<ApplicationObservation, Fault> {
    guard.check()?;
    if serde_json::to_vec(&diagnostics)
        .map_err(|_| unavailable("diagnostics"))?
        .len()
        > 64 * 1024
    {
        return Err(unavailable("diagnostics_limit"));
    }
    let observed_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| unavailable("clock"))?
        .as_millis();
    Ok(ApplicationObservation {
        observed_at_ms: u64::try_from(observed_at_ms).map_err(|_| unavailable("clock"))?,
        status: status.into(),
        evidence: evidence.map(str::to_owned),
        diagnostics,
    })
}
