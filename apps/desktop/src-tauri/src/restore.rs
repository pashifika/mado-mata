//! Narrow recoverable configuration transaction. Logs and package payloads never
//! enter the write set; an unresolved journal is an admission barrier owned by
//! bootstrap, not an invitation to load a partially installed tree.
use crate::backup::{MAX_MANIFEST, Manifest};
use crate::configuration::{self, Capture, Kind, MAX_ENUMERATED, capture, io_fault, path_kind};
use crate::storage::{
    self, MAX_OPEN_TABS, MAX_PROFILES, MAX_TABS, MAX_TOTAL_BYTES, Profile, Settings, TabRecord,
    check_directory, checked_file, decode, encode, exists, filesystem_key, read_bytes,
    validate_package_id, validate_profile, validate_settings, validate_tab,
};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

const JOURNAL: &str = ".restore-journal";
const COMPLETION: &str = ".restore-completion";
const MAX_JOURNAL: usize = 2 * MAX_MANIFEST + 1024;

/// Preservation archives can contain malformed or future documents. Only a
/// complete, supported and ownership-consistent set may be installed.
pub fn validate(capture: &Capture) -> Result<(), Fault> {
    capture.check()?;
    let settings = capture.files.get("settings.json").ok_or_else(|| {
        invalid("snapshot has no App settings; it is preservation material, not a complete restore")
    })?;
    validate_settings(&decode::<Settings>(settings)?)?;
    let mut tabs = BTreeMap::new();
    let mut open = 0;
    for (path, bytes) in &capture.files {
        if path_kind(path)? == Kind::Tab {
            let tab: TabRecord = decode(bytes)?;
            validate_tab(&tab)?;
            if path.split('/').nth(1) != Some(tab.internal_name.as_str()) {
                return Err(invalid("Tab identity differs from its containing path"));
            }
            open += usize::from(tab.open);
            tabs.insert(tab.internal_name.clone(), tab);
        }
    }
    if tabs.len() > MAX_TABS || open > MAX_OPEN_TABS {
        return Err(invalid("snapshot exceeds saved or open Tab limits"));
    }
    let mut budgets: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (path, bytes) in &capture.files {
        match path_kind(path)? {
            Kind::Settings | Kind::Tab => {}
            Kind::LegacyProfile => {
                let profile: Profile = decode(bytes)?;
                validate_profile(&profile)?;
                if path != &format!("profiles/{}.json", profile.id) {
                    return Err(invalid("legacy profile identity differs from its filename"));
                }
                budget(&mut budgets, "profiles", bytes.len())?;
            }
            Kind::Package => {
                let parts: Vec<_> = path.split('/').collect();
                let tab = tabs.get(parts[1]).ok_or_else(|| {
                    invalid("orphaned package configuration has no supported Tab record")
                })?;
                validate_package_id(parts[2])?;
                if !tab
                    .packages
                    .iter()
                    .any(|reference| reference.package_id == parts[2])
                {
                    return Err(invalid(
                        "package configuration is not owned by a saved Tab reference",
                    ));
                }
                let profile: Profile = decode(bytes)
                    .map_err(|_| invalid("unsupported or malformed package configuration owner"))?;
                validate_profile(&profile)?;
                if parts[3] != format!("{}.config", profile.id) || profile.package_id != parts[2] {
                    return Err(invalid(
                        "profile identity differs from its Tab/package/file scope",
                    ));
                }
                budget(
                    &mut budgets,
                    &format!("{}/{}", parts[1], parts[2]),
                    bytes.len(),
                )?;
            }
        }
    }
    Ok(())
}

fn budget(
    budgets: &mut BTreeMap<String, (usize, usize)>,
    owner: &str,
    bytes: usize,
) -> Result<(), Fault> {
    let value = budgets.entry(owner.to_owned()).or_default();
    value.0 += 1;
    value.1 += bytes;
    if value.0 > MAX_PROFILES || value.1 > MAX_TOTAL_BYTES {
        return Err(invalid("snapshot exceeds its profile owner budget"));
    }
    Ok(())
}

pub fn pending(root: &Path) -> Result<bool, Fault> {
    if !exists(root)? {
        return Ok(false);
    }
    check_directory(root)?;
    Ok(exists(&root.join(JOURNAL))? || exists(&root.join(COMPLETION))?)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    before: Manifest,
    after: Manifest,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    version: u32,
    rollback: bool,
    target: Manifest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    BeforeJournal,
    AfterJournal,
    Displaced { index: usize, rollback: bool },
    Installed { index: usize, rollback: bool },
    Verified { rollback: bool },
    BeforeRollback,
    BeforeCleanup,
    AfterCleanupCommit,
    CleanupRemoved { index: usize },
    CleanupDirectoryRemoved,
    CompletionMarkerRemoved,
}

/// The receipt's session authority and idle/discard admission are shell-owned.
/// This boundary independently binds the receipt to the actual source bytes.
pub fn install(root: &Path, new: Capture, expected_generation: Option<&str>) -> Result<(), Fault> {
    install_with(root, new, expected_generation, &mut |_| Ok(()))
}

fn install_with(
    root: &Path,
    new: Capture,
    expected_generation: Option<&str>,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    validate(&new)?;
    if pending(root)? {
        return Err(invalid(
            "an interrupted restore must be completed or rolled back first",
        ));
    }
    let observed = capture(root)?;
    if (!observed.files.is_empty() && expected_generation != Some(observed.generation.as_str()))
        || expected_generation.is_some_and(|expected| expected != observed.generation)
    {
        return Err(Fault::new(
            "SnapshotReceiptStale",
            "a separately requested snapshot matching the current configuration is required",
        ));
    }
    // Changing the spelling of an existing directory is not an authorized tree
    // migration, and may alias a retained empty container after replacement.
    let mut aliases = BTreeMap::new();
    for path in observed.files.keys().chain(new.files.keys()) {
        configuration::check_aliases(path, &mut aliases)?;
    }
    storage::private_directory(root)?;
    let before = Capture::from_files(observed.files, true)?;
    let staging = configuration::temporary(root, "restore-stage");
    configuration::create_private_directory(&staging)?;
    let journal = Journal {
        version: 1,
        before: Manifest::new(&before),
        after: Manifest::new(&new),
    };
    let mut published = false;
    let staged = (|| {
        stage_capture(&staging, "old", &before)?;
        stage_capture(&staging, "new", &new)?;
        configuration::write_private(
            &staging.join("journal.json"),
            &encode(&journal, MAX_JOURNAL)?,
        )?;
        configuration::sync_directory(&staging)?;
        // Creating an absent root changes only its presence flag, not source files.
        if capture(root)? != before {
            return Err(invalid("configuration changed while restore was staged"));
        }
        hook(Point::BeforeJournal)?;
        configuration::publish_no_replace(&staging, &root.join(JOURNAL))
            .map_err(|e| io_fault("publish restore journal", e))?;
        published = true;
        configuration::sync_directory(root)?;
        hook(Point::AfterJournal)
    })();
    if let Err(mut fault) = staged {
        if published {
            return rollback_failure(root, &before, &new, fault, hook);
        }
        if let Err(cleanup) = cleanup_owned(&staging) {
            fault.context["staging_cleanup"] = json!({"path": staging, "fault": cleanup});
        }
        return Err(fault);
    }
    match apply(root, &before, &new, false, hook) {
        Ok(()) => finish(root, false, Some(false), hook),
        Err(fault) => rollback_failure(root, &before, &new, fault, hook),
    }
}

fn stage_capture(directory: &Path, prefix: &str, capture: &Capture) -> Result<(), Fault> {
    for (index, bytes) in capture.files.values().enumerate() {
        configuration::write_private(&directory.join(format!("{prefix}-{index}")), bytes)?;
    }
    Ok(())
}

fn load_capture(directory: &Path, prefix: &str, manifest: &Manifest) -> Result<Capture, Fault> {
    manifest.check()?;
    let mut files = BTreeMap::new();
    for (index, entry) in manifest.files.iter().enumerate() {
        files.insert(
            entry.path.clone(),
            read_bytes(&directory.join(format!("{prefix}-{index}")), entry.length)?,
        );
    }
    manifest.capture(files)
}

/// Restart recovery infers per-file progress from current bytes; no recorded
/// counter implies that an individual replacement happened. Uncommitted bytes are
/// reconciled with preimages; committed cleanup verifies its target manifest even
/// after preimages are gone.
pub fn recover(root: &Path, rollback: bool) -> Result<(), Fault> {
    recover_with(root, rollback, &mut |_| Ok(()))
}

fn recover_with(
    root: &Path,
    rollback: bool,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    if !pending(root)? {
        return Err(invalid("there is no pending restore to recover"));
    }
    if exists(&root.join(COMPLETION))? {
        return finish(root, rollback, None, hook);
    }
    let directory = root.join(JOURNAL);
    check_directory(&directory)?;
    let journal: Journal = decode(&read_bytes(&directory.join("journal.json"), MAX_JOURNAL)?)?;
    if journal.version != 1 {
        return Err(invalid("unsupported restore journal version"));
    }
    let before = load_capture(&directory, "old", &journal.before)?;
    let after = load_capture(&directory, "new", &journal.after)?;
    validate(&after)?;
    let mut aliases = BTreeMap::new();
    for path in before.files.keys().chain(after.files.keys()) {
        configuration::check_aliases(path, &mut aliases)?;
    }
    apply(root, &before, &after, rollback, hook).map_err(|mut fault| {
        fault.context["pending_restore"] = json!(true);
        fault
    })?;
    finish(root, rollback, Some(rollback), hook)
}

fn rollback_failure(
    root: &Path,
    before: &Capture,
    after: &Capture,
    mut original: Fault,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    let rollback =
        hook(Point::BeforeRollback).and_then(|()| apply(root, before, after, true, hook));
    match rollback {
        Ok(()) => {
            original.context["rolled_back"] = json!(true);
            if let Err(cleanup) = finish(root, true, Some(true), hook) {
                original.context["rollback_cleanup"] = json!(cleanup);
            }
        }
        Err(fault) => {
            original.context["rollback_failure"] = json!(fault);
            original.context["pending_restore"] = json!(true);
        }
    }
    Err(original)
}

fn apply(
    root: &Path,
    before: &Capture,
    after: &Capture,
    rollback: bool,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    let target = if rollback { before } else { after };
    let live = capture(root)?;
    let paths: BTreeSet<_> = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect();
    for (path, bytes) in &live.files {
        if !paths.contains(path)
            || (before.files.get(path) != Some(bytes) && after.files.get(path) != Some(bytes))
        {
            return Err(invalid(
                "live configuration differs from both journal generations; external repair is required",
            ));
        }
    }
    let directory = root.join(JOURNAL);
    for (index, path) in paths.iter().enumerate() {
        let destination = root.join(path);
        let current = if exists(&destination)? {
            Some(read_bytes(&destination, path_kind(path)?.maximum())?)
        } else {
            None
        };
        let desired = target.files.get(path);
        if current.as_ref() == desired {
            continue;
        }
        if current.as_ref().is_some_and(|bytes| {
            before.files.get(path) != Some(bytes) && after.files.get(path) != Some(bytes)
        }) {
            return Err(invalid("configuration changed during restore"));
        }
        ensure_parents(root, path)?;
        if let Some(bytes) = &current {
            let held = directory.join(format!("held-{}-{index}", configuration::digest(bytes)));
            if exists(&held)? {
                if read_bytes(&held, path_kind(path)?.maximum())? != *bytes {
                    return Err(invalid(
                        "retained displaced configuration differs from live bytes",
                    ));
                }
                // This exact displaced generation is already retained. Remove
                // only its verified live duplicate; rollback preimages remain.
                fs::remove_file(&destination)
                    .map_err(|e| io_fault("remove retained configuration duplicate", e))?;
            } else {
                configuration::publish_no_replace(&destination, &held)
                    .map_err(|e| io_fault("retain displaced configuration", e))?;
                configuration::sync_directory(&directory)?;
            }
            configuration::sync_directory(destination.parent().expect("managed path has parent"))?;
            hook(Point::Displaced { index, rollback })?;
        }
        if let Some(bytes) = desired {
            let temporary = configuration::temporary(&directory, "install");
            configuration::write_private(&temporary, bytes)?;
            if let Err(error) = configuration::publish_no_replace(&temporary, &destination) {
                let mut fault = io_fault("install staged configuration", error);
                if let Err(cleanup) = fs::remove_file(&temporary) {
                    fault.context["temporary_cleanup"] =
                        json!({"path": temporary, "kind": format!("{:?}", cleanup.kind())});
                }
                return Err(fault);
            }
            configuration::sync_directory(destination.parent().expect("managed path has parent"))?;
        }
        hook(Point::Installed { index, rollback })?;
    }
    if capture(root)? != *target {
        return Err(invalid(
            "installed configuration failed full generation verification",
        ));
    }
    hook(Point::Verified { rollback })?;
    remove_emptied_containers(root, &paths, target)
}

fn ensure_parents(root: &Path, relative: &str) -> Result<(), Fault> {
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.split('/').collect();
    for part in &components[..components.len() - 1] {
        check_directory(&current)?;
        // Check actual container spelling, including empty/unregistered containers.
        let mut count = 0;
        for entry in fs::read_dir(&current).map_err(|e| io_fault("inspect restore parent", e))? {
            count += 1;
            if count > MAX_ENUMERATED {
                return Err(invalid("restore parent enumeration exceeds its bound"));
            }
            let entry = entry.map_err(|e| io_fault("read restore parent entry", e))?;
            if let Some(name) = entry.file_name().to_str() {
                if name != *part && filesystem_key(name) == filesystem_key(part) {
                    return Err(invalid("restore parent aliases an existing container"));
                }
            }
        }
        current.push(part);
        storage::private_directory(&current)?;
    }
    Ok(())
}

/// Verification compares managed bytes, but discovery treats a Tab container that
/// holds only an emptied package directory as an orphan that blocks its name and
/// consumes saved/open slots. Remove, without recursion, the containers this
/// generation no longer owns; one still holding unmanaged data is kept and stays
/// attributable. Restart recovery repeats this step, so an absent container is fine.
fn remove_emptied_containers(
    root: &Path,
    paths: &BTreeSet<String>,
    target: &Capture,
) -> Result<(), Fault> {
    let mut containers: BTreeMap<&str, bool> = BTreeMap::new();
    for path in paths {
        if target.files.contains_key(path) {
            continue;
        }
        let kind = path_kind(path)?;
        if !matches!(kind, Kind::Tab | Kind::Package) {
            continue;
        }
        let mut child = path.as_str();
        while let Some((container, _)) = child.rsplit_once('/') {
            if container == "tabs" {
                break;
            }
            *containers.entry(container).or_insert(false) |= kind == Kind::Tab;
            child = container;
        }
    }
    let mut entries_seen = 0;
    // A parent sorts before its children; remove package containers before Tabs.
    for (container, retire_tab) in containers.iter().rev() {
        let directory = root.join(container);
        if *retire_tab && exists(&directory)? {
            check_directory(&directory)?;
            // A deleted last profile leaves an empty package absent from both generations.
            // Finish bounded, non-following enumeration before changing this directory.
            let mut children = Vec::new();
            for entry in fs::read_dir(&directory)
                .map_err(|error| io_fault("enumerate retiring Tab containers", error))?
            {
                entries_seen += 1;
                if entries_seen > MAX_ENUMERATED {
                    return Err(invalid("retiring Tab enumeration exceeds its bound"));
                }
                let entry = entry.map_err(|error| io_fault("read retiring Tab entry", error))?;
                if entry
                    .file_type()
                    .map_err(|error| io_fault("inspect retiring Tab entry", error))?
                    .is_dir()
                {
                    children.push(entry.path());
                }
            }
            for child in children {
                remove_empty_container(&child)?;
            }
        }
        remove_empty_container(&directory)?;
    }
    Ok(())
}

fn remove_empty_container(directory: &Path) -> Result<(), Fault> {
    match fs::remove_dir(directory) {
        Ok(()) => {
            configuration::sync_directory(directory.parent().expect("managed container has parent"))
        }
        // rmdir reports retained content as ENOTEMPTY or EEXIST.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::DirectoryNotEmpty
                    | io::ErrorKind::AlreadyExists
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(io_fault("remove emptied configuration container", error)),
    }
}

fn finish(
    root: &Path,
    rollback: bool,
    mut committed_rollback: Option<bool>,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    let directory = root.join(JOURNAL);
    let marker = root.join(COMPLETION);
    let result = (|| {
        hook(Point::BeforeCleanup)?;
        let completion = if exists(&marker)? {
            let completion: Completion = decode(&read_bytes(&marker, MAX_JOURNAL)?)?;
            if completion.version != 1 {
                return Err(invalid("unsupported restore completion version"));
            }
            committed_rollback = Some(completion.rollback);
            if completion.rollback != rollback {
                return Err(invalid(
                    "configuration is already committed; resume its cleanup in the original completion or rollback direction",
                ));
            }
            completion
        } else {
            let journal: Journal =
                decode(&read_bytes(&directory.join("journal.json"), MAX_JOURNAL)?)?;
            if journal.version != 1 {
                return Err(invalid("unsupported restore journal version"));
            }
            let target = if rollback {
                journal.before
            } else {
                journal.after
            };
            verify_completion(root, &target)?;
            let completion = Completion {
                version: 1,
                rollback,
                target,
            };
            let temporary = configuration::temporary(&directory, "completion");
            configuration::write_private(&temporary, &encode(&completion, MAX_JOURNAL)?)?;
            if let Err(error) = configuration::publish_no_replace(&temporary, &marker) {
                let mut fault = io_fault("publish restore completion marker", error);
                if let Err(cleanup) = fs::remove_file(&temporary) {
                    fault.context["temporary_cleanup"] =
                        json!({"path": temporary, "kind": format!("{:?}", cleanup.kind())});
                }
                return Err(fault);
            }
            // Keep a root-level admission barrier while journal files disappear.
            // The marker survives both partial preimage cleanup and removal of
            // the now-empty journal directory; its target is verified on restart.
            configuration::sync_directory(root)?;
            completion
        };
        verify_completion(root, &completion.target)?;
        hook(Point::AfterCleanupCommit)?;
        if exists(&directory)? {
            cleanup_owned_with(&directory, hook)?;
        }
        configuration::sync_directory(root)?;
        verify_completion(root, &completion.target)?;
        fs::remove_file(&marker).map_err(|e| io_fault("remove completed restore marker", e))?;
        hook(Point::CompletionMarkerRemoved)?;
        configuration::sync_directory(root)
    })();
    result.map_err(|mut fault| {
        fault.context["configuration_installed"] = json!(committed_rollback == Some(false));
        fault.context["rolled_back"] = json!(committed_rollback == Some(true));
        fault.context["cleanup_incomplete"] = json!(true);
        fault.context["pending_restore"] = json!(pending(root).unwrap_or(true));
        fault.context["retained_staging"] = json!(directory);
        fault.context["completion_marker"] = json!(marker);
        fault
    })
}

fn verify_completion(root: &Path, target: &Manifest) -> Result<(), Fault> {
    target.check()?;
    let live = capture(root)?;
    if live.generation != target.generation {
        return Err(invalid(
            "committed configuration changed before restore cleanup; external repair is required",
        ));
    }
    target.capture(live.files)?;
    Ok(())
}

/// Staging has a flat, bounded set of regular files. Never recursively delete a
/// path supplied by an archive, and refuse unexpected content rather than erase it.
fn cleanup_owned(directory: &Path) -> Result<(), Fault> {
    cleanup_owned_with(directory, &mut |_| Ok(()))
}

fn cleanup_owned_with(
    directory: &Path,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    check_directory(directory)?;
    let mut paths = Vec::new();
    for entry in
        fs::read_dir(directory).map_err(|e| io_fault("enumerate restore staging cleanup", e))?
    {
        if paths.len() >= 4 * configuration::MAX_FILES + 64 {
            return Err(invalid("restore staging cleanup exceeds its bound"));
        }
        let entry = entry.map_err(|e| io_fault("read restore staging cleanup entry", e))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| invalid("unrecognized restore staging entry"))?;
        if name != "journal.json"
            && !name.starts_with("old-")
            && !name.starts_with("new-")
            && !name.starts_with("held-")
            && !name.starts_with(".install-")
            && !name.starts_with(".completion-")
        {
            return Err(invalid(
                "unrecognized restore staging entry; cleanup refused",
            ));
        }
        checked_file(&entry.path(), MAX_JOURNAL)?;
        paths.push(entry.path());
    }
    paths.sort();
    for (index, path) in paths.into_iter().enumerate() {
        fs::remove_file(path).map_err(|e| io_fault("remove owned restore staging file", e))?;
        hook(Point::CleanupRemoved { index })?;
    }
    fs::remove_dir(directory).map_err(|e| io_fault("remove owned restore staging directory", e))?;
    hook(Point::CleanupDirectoryRemoved)
}

fn invalid(message: &str) -> Fault {
    Fault::new("Restore", message)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::configuration::tests::Root;
    use crate::storage::{PackageReference, PackageSource, Store};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    pub(crate) fn interrupt_install(root: &Path, new: Capture, expected_generation: &str) {
        let mut hook = |at| {
            if at
                == (Point::Displaced {
                    index: 0,
                    rollback: false,
                })
            {
                panic!("simulated process exit after displacement");
            }
            Ok(())
        };
        assert!(
            catch_unwind(AssertUnwindSafe(|| install_with(
                root,
                new,
                Some(expected_generation),
                &mut hook
            )))
            .is_err()
        );
        assert!(pending(root).unwrap());
    }

    pub(crate) fn fail_recovery_final_sync(root: &Path, rollback: bool) -> Result<(), Fault> {
        recover_with(root, rollback, &mut |at| {
            if at == Point::CompletionMarkerRemoved {
                Err(io_fault(
                    "sync configuration directory",
                    io::Error::other("injected final sync failure"),
                ))
            } else {
                Ok(())
            }
        })
    }

    fn settings(limit: usize) -> Vec<u8> {
        serde_json::to_vec(&Settings {
            gui_log_limit: limit,
            ..Settings::default()
        })
        .unwrap()
    }
    fn replacement(limit: usize) -> Capture {
        Capture::from_files(
            BTreeMap::from([("settings.json".into(), settings(limit))]),
            true,
        )
        .unwrap()
    }
    fn fixture() -> (Root, Capture) {
        let root = Root::new();
        root.put("settings.json", &settings(100));
        root.put("tabs/Old/tab.config", b"malformed preserved preimage");
        root.put("logs/run.log", b"keep logs");
        root.put("payload/model.bin", b"keep payload");
        root.put("backups/app.config.1", b"keep operator snapshot");
        let old = capture(&root.0).unwrap();
        (root, old)
    }
    fn assert_unrelated(root: &Root) {
        assert_eq!(fs::read(root.0.join("logs/run.log")).unwrap(), b"keep logs");
        assert_eq!(
            fs::read(root.0.join("payload/model.bin")).unwrap(),
            b"keep payload"
        );
        assert_eq!(
            fs::read(root.0.join("backups/app.config.1")).unwrap(),
            b"keep operator snapshot"
        );
    }
    fn profile_id() -> String {
        format!("p-{}-{}-{}", "0".repeat(32), "0".repeat(8), "0".repeat(16))
    }
    fn tab(name: &str, open: bool) -> Vec<u8> {
        serde_json::to_vec(&TabRecord {
            version: 1,
            internal_name: name.into(),
            display_name: name.into(),
            open,
            packages: vec![PackageReference {
                package_id: "pkg".into(),
                source: PackageSource::Directory {
                    path: std::env::temp_dir()
                        .join("package")
                        .to_str()
                        .unwrap()
                        .into(),
                },
            }],
            selected_package_id: Some("pkg".into()),
        })
        .unwrap()
    }
    fn profile() -> Vec<u8> {
        serde_json::to_vec(&Profile {
            version: 1,
            id: profile_id(),
            name: "Profile".into(),
            package_id: "pkg".into(),
            schema_identity: "a".repeat(64),
            values: json!({}),
        })
        .unwrap()
    }

    #[test]
    fn requires_current_receipt_and_rejects_invalid_before_mutation() {
        let (root, old) = fixture();
        assert!(install(&root.0, replacement(200), None).is_err());
        assert!(install(&root.0, replacement(200), Some("stale")).is_err());
        let invalid = Capture::from_files(
            BTreeMap::from([("settings.json".into(), b"broken".to_vec())]),
            true,
        )
        .unwrap();
        assert!(install(&root.0, invalid, Some(&old.generation)).is_err());
        assert_eq!(capture(&root.0).unwrap(), old);
        assert!(!pending(&root.0).unwrap());
    }

    #[test]
    fn successful_install_replaces_only_managed_configuration() {
        let (root, old) = fixture();
        let new = replacement(200);
        install(&root.0, new.clone(), Some(&old.generation)).unwrap();
        assert_eq!(capture(&root.0).unwrap(), new);
        assert!(!pending(&root.0).unwrap());
        assert_unrelated(&root);
    }

    #[test]
    fn each_publication_and_file_failure_restores_original_generation() {
        for point in [
            Point::BeforeJournal,
            Point::AfterJournal,
            Point::Displaced {
                index: 0,
                rollback: false,
            },
            Point::Installed {
                index: 0,
                rollback: false,
            },
            Point::Displaced {
                index: 1,
                rollback: false,
            },
            Point::Installed {
                index: 1,
                rollback: false,
            },
        ] {
            let (root, old) = fixture();
            let mut fired = false;
            let mut hook = |at| {
                if at == point && !fired {
                    fired = true;
                    Err(invalid("injected restore failure"))
                } else {
                    Ok(())
                }
            };
            assert!(
                install_with(&root.0, replacement(200), Some(&old.generation), &mut hook).is_err()
            );
            assert!(fired);
            assert_eq!(capture(&root.0).unwrap(), old);
            assert!(!pending(&root.0).unwrap());
            assert_unrelated(&root);
        }
    }

    #[test]
    fn interrupted_replacement_and_rollback_recover_after_restart() {
        for rollback in [false, true] {
            let (root, old) = fixture();
            let new = replacement(200);
            let mut hook = |at| {
                if at
                    == (Point::Displaced {
                        index: 0,
                        rollback: false,
                    })
                {
                    panic!("simulated process exit");
                }
                Ok(())
            };
            assert!(
                catch_unwind(AssertUnwindSafe(|| install_with(
                    &root.0,
                    new.clone(),
                    Some(&old.generation),
                    &mut hook
                )))
                .is_err()
            );
            assert!(pending(&root.0).unwrap());
            recover(&root.0, rollback).unwrap();
            assert_eq!(capture(&root.0).unwrap(), if rollback { old } else { new });
            assert_unrelated(&root);
        }
        let (root, old) = fixture();
        let mut hook = |at| match at {
            Point::Installed {
                index: 0,
                rollback: false,
            }
            | Point::Displaced {
                index: 0,
                rollback: true,
            } => Err(invalid("injected rollback failure")),
            _ => Ok(()),
        };
        assert!(install_with(&root.0, replacement(200), Some(&old.generation), &mut hook).is_err());
        assert!(pending(&root.0).unwrap());
        recover(&root.0, true).unwrap();
        assert_eq!(capture(&root.0).unwrap(), old);
    }

    #[test]
    fn recovery_preserves_unexpected_external_changes_and_preimages() {
        let (root, old) = fixture();
        let mut hook = |at| {
            if at == Point::AfterJournal {
                panic!("exit");
            }
            Ok(())
        };
        let _ = catch_unwind(AssertUnwindSafe(|| {
            install_with(&root.0, replacement(200), Some(&old.generation), &mut hook)
        }));
        fs::write(root.0.join("settings.json"), settings(333)).unwrap();
        assert!(recover(&root.0, false).is_err());
        assert!(recover(&root.0, true).is_err());
        assert_eq!(
            fs::read(root.0.join("settings.json")).unwrap(),
            settings(333)
        );
        assert!(pending(&root.0).unwrap());
        assert_eq!(
            read_bytes(&root.0.join(JOURNAL).join("old-0"), 32 * 1024).unwrap(),
            settings(100)
        );
    }

    #[test]
    fn cleanup_failure_distinguishes_installed_from_unresolved_bytes() {
        let (root, old) = fixture();
        let new = replacement(200);
        let mut hook = |at| {
            if at == Point::BeforeCleanup {
                Err(invalid("injected cleanup failure"))
            } else {
                Ok(())
            }
        };
        let fault =
            install_with(&root.0, new.clone(), Some(&old.generation), &mut hook).unwrap_err();
        assert_eq!(fault.context["configuration_installed"], true);
        assert_eq!(capture(&root.0).unwrap(), new);
        assert!(pending(&root.0).unwrap());
        recover(&root.0, false).unwrap();
        assert!(!pending(&root.0).unwrap());
    }

    #[test]
    fn interrupted_committed_cleanup_remains_pending_until_restart_recovery_finishes() {
        for point in [
            Point::AfterCleanupCommit,
            Point::CleanupRemoved { index: 4 },
            Point::CleanupDirectoryRemoved,
        ] {
            let (root, old) = fixture();
            let new = replacement(200);
            let mut hook = |at| {
                if at == point {
                    panic!("simulated exit during committed cleanup");
                }
                Ok(())
            };
            assert!(
                catch_unwind(AssertUnwindSafe(|| install_with(
                    &root.0,
                    new.clone(),
                    Some(&old.generation),
                    &mut hook
                )))
                .is_err()
            );
            assert_eq!(capture(&root.0).unwrap(), new);
            assert!(pending(&root.0).unwrap());
            assert!(root.0.join(COMPLETION).is_file());
            if point == Point::CleanupDirectoryRemoved {
                assert!(!root.0.join(JOURNAL).exists());
            }
            assert!(recover(&root.0, true).is_err());
            assert!(pending(&root.0).unwrap());
            recover(&root.0, false).unwrap();
            assert!(!pending(&root.0).unwrap());
            assert!(!root.0.join(JOURNAL).exists());
            assert!(!root.0.join(COMPLETION).exists());
            assert_eq!(capture(&root.0).unwrap(), new);
            assert_unrelated(&root);
        }
    }

    #[test]
    fn actual_cleanup_refusal_retains_discoverable_completion_and_preserves_unknown_data() {
        let (root, old) = fixture();
        let new = replacement(200);
        let mut hook = |at| {
            if at == Point::AfterCleanupCommit {
                root.put(".restore-journal/operator.txt", b"must not delete");
            }
            Ok(())
        };
        let fault =
            install_with(&root.0, new.clone(), Some(&old.generation), &mut hook).unwrap_err();
        assert_eq!(fault.context["configuration_installed"], true);
        assert_eq!(fault.context["cleanup_incomplete"], true);
        assert!(pending(&root.0).unwrap());
        assert!(recover(&root.0, false).is_err());
        assert_eq!(
            fs::read(root.0.join(JOURNAL).join("operator.txt")).unwrap(),
            b"must not delete"
        );
        assert_eq!(capture(&root.0).unwrap(), new);
        fs::remove_file(root.0.join(JOURNAL).join("operator.txt")).unwrap();
        recover(&root.0, false).unwrap();
        assert!(!pending(&root.0).unwrap());
        assert_unrelated(&root);
    }

    #[test]
    fn committed_cleanup_revalidates_live_generation_before_removing_evidence() {
        let (root, old) = fixture();
        let mut hook = |at| {
            if at == Point::AfterCleanupCommit {
                panic!("exit after cleanup commitment");
            }
            Ok(())
        };
        let _ = catch_unwind(AssertUnwindSafe(|| {
            install_with(&root.0, replacement(200), Some(&old.generation), &mut hook)
        }));
        fs::write(root.0.join("settings.json"), settings(333)).unwrap();
        assert!(recover(&root.0, false).is_err());
        assert!(pending(&root.0).unwrap());
        assert_eq!(
            read_bytes(&root.0.join(JOURNAL).join("old-0"), 32 * 1024).unwrap(),
            settings(100)
        );
        fs::write(root.0.join("settings.json"), settings(200)).unwrap();
        recover(&root.0, false).unwrap();
        assert!(!pending(&root.0).unwrap());
    }

    #[test]
    fn unreadable_completion_never_claims_the_requested_direction() {
        let (root, old) = fixture();
        let new = replacement(200);
        let mut hook = |at| {
            if at == Point::AfterCleanupCommit {
                panic!("exit after cleanup commitment");
            }
            Ok(())
        };
        assert!(
            catch_unwind(AssertUnwindSafe(|| install_with(
                &root.0,
                new.clone(),
                Some(&old.generation),
                &mut hook
            )))
            .is_err()
        );
        let committed = fs::read(root.0.join(COMPLETION)).unwrap();
        let mut unsupported: serde_json::Value = serde_json::from_slice(&committed).unwrap();
        unsupported["version"] = json!(2);
        for marker in [b"{".to_vec(), serde_json::to_vec(&unsupported).unwrap()] {
            fs::write(root.0.join(COMPLETION), &marker).unwrap();
            for rollback in [false, true] {
                let fault = recover(&root.0, rollback).unwrap_err();
                assert_eq!(fault.context["configuration_installed"], false);
                assert_eq!(fault.context["rolled_back"], false);
                assert_eq!(fault.context["cleanup_incomplete"], true);
                assert_eq!(fault.context["pending_restore"], true);
                assert!(pending(&root.0).unwrap());
                assert_eq!(capture(&root.0).unwrap(), new);
                assert_eq!(fs::read(root.0.join(COMPLETION)).unwrap(), marker);
                assert_eq!(
                    fs::read(root.0.join(JOURNAL).join("old-0")).unwrap(),
                    settings(100)
                );
            }
        }
        fs::write(root.0.join(COMPLETION), committed).unwrap();
        recover(&root.0, false).unwrap();
        assert!(!pending(&root.0).unwrap());
        assert_eq!(capture(&root.0).unwrap(), new);
        assert_unrelated(&root);
    }

    #[test]
    fn interrupted_rollback_cleanup_retains_rollback_truth_on_restart() {
        let (root, old) = fixture();
        let mut hook = |at| match at {
            Point::Installed {
                index: 0,
                rollback: false,
            } => Err(invalid("injected install failure")),
            Point::AfterCleanupCommit => panic!("exit during rollback cleanup"),
            _ => Ok(()),
        };
        assert!(
            catch_unwind(AssertUnwindSafe(|| install_with(
                &root.0,
                replacement(200),
                Some(&old.generation),
                &mut hook
            )))
            .is_err()
        );
        assert_eq!(capture(&root.0).unwrap(), old);
        assert!(pending(&root.0).unwrap());
        let wrong_direction = recover(&root.0, false).unwrap_err();
        assert_eq!(wrong_direction.context["rolled_back"], true);
        assert_eq!(wrong_direction.context["configuration_installed"], false);
        recover(&root.0, true).unwrap();
        assert!(!pending(&root.0).unwrap());
        assert_eq!(capture(&root.0).unwrap(), old);
        assert_unrelated(&root);
    }

    /// Alpha stays; Beta is removed and emptied; Gamma is removed but keeps an
    /// unmanaged file, so it must remain an attributable orphan.
    fn tabbed_fixture() -> (Root, Capture, Capture) {
        let root = Root::new();
        root.put("settings.json", &settings(100));
        root.put("tabs/Alpha/tab.config", &tab("Alpha", true));
        root.put("tabs/Beta/tab.config", &tab("Beta", false));
        root.put(
            &format!("tabs/Beta/pkg/{}.config", profile_id()),
            &profile(),
        );
        root.put("tabs/Gamma/tab.config", &tab("Gamma", false));
        root.put(
            &format!("tabs/Gamma/pkg/{}.config", profile_id()),
            &profile(),
        );
        root.put("tabs/Gamma/pkg/notes.txt", b"unmanaged operator data");
        root.put("payload/model.bin", b"keep payload");
        let old = capture(&root.0).unwrap();
        let mut files = replacement(200).files;
        files.insert("tabs/Alpha/tab.config".into(), tab("Alpha", true));
        (root, old, Capture::from_files(files, true).unwrap())
    }
    fn assert_converged_catalog(root: &Root, new: &Capture) {
        assert_eq!(capture(&root.0).unwrap(), *new);
        assert!(!pending(&root.0).unwrap());
        assert_eq!(
            fs::read(root.0.join("tabs/Gamma/pkg/notes.txt")).unwrap(),
            b"unmanaged operator data"
        );
        assert_eq!(
            fs::read(root.0.join("payload/model.bin")).unwrap(),
            b"keep payload"
        );
        let store = Store::new(root.0.clone()).unwrap();
        let listing = store.tabs().unwrap();
        assert_eq!(
            listing
                .tabs
                .iter()
                .map(|tab| tab.internal_name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha"]
        );
        assert_eq!(listing.faults.len(), 1, "{:?}", listing.faults);
        assert_eq!(listing.faults[0].category, "TabOrphan");
        assert_eq!(listing.faults[0].context["internal_name"], "Gamma");
        store.create_tab("Beta", "Beta").unwrap();
        assert_eq!(
            store.create_tab("Gamma", "Gamma").unwrap_err().category,
            "TabExists"
        );
    }

    #[test]
    fn restore_removing_a_tab_with_package_configuration_frees_its_name_and_slots() {
        let (root, old, new) = tabbed_fixture();
        install(&root.0, new.clone(), Some(&old.generation)).unwrap();
        assert_converged_catalog(&root, &new);
    }

    #[test]
    fn restore_after_deleting_the_last_profile_does_not_invent_an_orphan() {
        let (root, _, new) = tabbed_fixture();
        let store = Store::new(root.0.clone()).unwrap();
        store.set_tab_open("Beta", true).unwrap();
        store
            .profile_store("Beta", "pkg")
            .unwrap()
            .delete(&profile_id())
            .unwrap();
        let old = capture(&root.0).unwrap();
        install(&root.0, new.clone(), Some(&old.generation)).unwrap();
        assert_converged_catalog(&root, &new);
    }

    #[test]
    fn container_reconciliation_interrupted_after_verification_converges_on_restart() {
        let (root, _, new) = tabbed_fixture();
        let store = Store::new(root.0.clone()).unwrap();
        store.set_tab_open("Beta", true).unwrap();
        store
            .profile_store("Beta", "pkg")
            .unwrap()
            .delete(&profile_id())
            .unwrap();
        let old = capture(&root.0).unwrap();
        let mut hook = |at| {
            if at == (Point::Verified { rollback: false }) {
                panic!("simulated exit after verification");
            }
            Ok(())
        };
        assert!(
            catch_unwind(AssertUnwindSafe(|| install_with(
                &root.0,
                new.clone(),
                Some(&old.generation),
                &mut hook
            )))
            .is_err()
        );
        assert!(pending(&root.0).unwrap());
        assert!(root.0.join("tabs/Beta/pkg").is_dir());
        recover(&root.0, false).unwrap();
        assert_converged_catalog(&root, &new);
    }

    #[test]
    fn rolled_back_restore_that_adds_a_tab_with_a_profile_leaves_no_orphan() {
        let root = Root::new();
        root.put("settings.json", &settings(100));
        root.put("tabs/Alpha/tab.config", &tab("Alpha", true));
        let old = capture(&root.0).unwrap();
        let mut files = old.files.clone();
        files.insert("tabs/Gamma/tab.config".into(), tab("Gamma", false));
        files.insert(format!("tabs/Gamma/pkg/{}.config", profile_id()), profile());
        let new = Capture::from_files(files, true).unwrap();
        // Sorted paths install the Gamma profile (index 2) before its tab.config,
        // so the failure leaves a freshly created Tab and package container behind.
        let mut fired = false;
        let mut hook = |at| {
            if at
                == (Point::Installed {
                    index: 2,
                    rollback: false,
                })
            {
                fired = true;
                Err(invalid("injected restore failure"))
            } else {
                Ok(())
            }
        };
        let fault = install_with(&root.0, new, Some(&old.generation), &mut hook).unwrap_err();
        assert!(fired);
        assert_eq!(fault.context["rolled_back"], true);
        assert_eq!(capture(&root.0).unwrap(), old);
        assert!(!pending(&root.0).unwrap());
        let store = Store::new(root.0.clone()).unwrap();
        let listing = store.tabs().unwrap();
        assert!(listing.faults.is_empty(), "{:?}", listing.faults);
        assert_eq!(listing.tabs.len(), 1);
        store.create_tab("Gamma", "Gamma").unwrap();
    }

    #[test]
    fn typed_validation_rejects_unknown_owners_and_wrong_scopes() {
        let tab = TabRecord {
            version: 1,
            internal_name: "One".into(),
            display_name: "One".into(),
            open: false,
            packages: vec![PackageReference {
                package_id: "pkg".into(),
                source: PackageSource::Directory {
                    path: std::env::temp_dir()
                        .join("package")
                        .to_str()
                        .unwrap()
                        .into(),
                },
            }],
            selected_package_id: Some("pkg".into()),
        };
        let profile = Profile {
            version: 1,
            id: format!("p-{}-{}-{}", "0".repeat(32), "0".repeat(8), "0".repeat(16)),
            name: "Profile".into(),
            package_id: "pkg".into(),
            schema_identity: "a".repeat(64),
            values: json!({}),
        };
        let profile_path = format!("tabs/One/pkg/{}.config", profile.id);
        let mut files = replacement(200).files;
        files.insert(
            "tabs/One/tab.config".into(),
            serde_json::to_vec(&tab).unwrap(),
        );
        files.insert(profile_path.clone(), serde_json::to_vec(&profile).unwrap());
        validate(&Capture::from_files(files.clone(), true).unwrap()).unwrap();
        files.insert("tabs/One/pkg/target.config".into(), b"{}".to_vec());
        assert!(validate(&Capture::from_files(files.clone(), true).unwrap()).is_err());
        files.remove("tabs/One/pkg/target.config");
        files.remove("tabs/One/tab.config");
        assert!(validate(&Capture::from_files(files.clone(), true).unwrap()).is_err());
        files.insert(
            "tabs/One/tab.config".into(),
            serde_json::to_vec(&tab).unwrap(),
        );
        let mut wrong = profile;
        wrong.package_id = "sibling".into();
        files.insert(profile_path, serde_json::to_vec(&wrong).unwrap());
        assert!(validate(&Capture::from_files(files.clone(), true).unwrap()).is_err());
        files.remove("settings.json");
        assert!(validate(&Capture::from_files(files, true).unwrap()).is_err());
    }
}
