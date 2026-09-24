//! Narrow recoverable configuration transaction. Logs and package payloads never
//! enter the write set; an unresolved journal is an admission barrier owned by
//! bootstrap, not an invitation to load a partially installed tree.
use crate::backup::{MAX_MANIFEST, Manifest};
use crate::configuration::{self, Capture, Kind, capture, io_fault, path_kind};
use crate::storage::{
    self, Profile, Settings, TabRecord, check_directory, checked_file, decode, encode, exists,
    filesystem_key, read_bytes, validate_package_id, validate_profile, validate_settings,
    validate_tab,
};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
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
    if tabs.len() > 64 || open > 8 {
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
    if value.0 > 64 || value.1 > 1024 * 1024 {
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
struct Progress {
    rollback: bool,
    completed_paths: usize,
    verified: bool,
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
    BeforeRollback,
    BeforeCleanup,
    AfterCleanupCommit,
    CleanupRemoved { index: usize },
    CleanupDirectoryRemoved,
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
        configuration::write_private(
            &staging.join("progress.json"),
            &encode(
                &Progress {
                    rollback: false,
                    completed_paths: 0,
                    verified: false,
                },
                1024,
            )?,
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
            return rollback_failure(root, &journal, &before, &new, fault, hook);
        }
        if let Err(cleanup) = cleanup_owned(&staging) {
            fault.context["staging_cleanup"] = json!({"path": staging, "fault": cleanup});
        }
        return Err(fault);
    }
    match apply(root, &journal, &before, &new, false, hook) {
        Ok(()) => finish(root, false, hook),
        Err(fault) => rollback_failure(root, &journal, &before, &new, fault, hook),
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

/// Restart recovery never trusts a progress counter to imply that an individual
/// replacement happened. Uncommitted bytes are reconciled with preimages;
/// committed cleanup verifies its target manifest even after preimages are gone.
pub fn recover(root: &Path, rollback: bool) -> Result<(), Fault> {
    if !pending(root)? {
        return Err(invalid("there is no pending restore to recover"));
    }
    if exists(&root.join(COMPLETION))? {
        return finish(root, rollback, &mut |_| Ok(()));
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
    let mut hook = |_| Ok(());
    apply(root, &journal, &before, &after, rollback, &mut hook).map_err(|mut fault| {
        fault.context["pending_restore"] = json!(true);
        fault
    })?;
    finish(root, rollback, &mut hook)
}

fn rollback_failure(
    root: &Path,
    journal: &Journal,
    before: &Capture,
    after: &Capture,
    mut original: Fault,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    let rollback =
        hook(Point::BeforeRollback).and_then(|()| apply(root, journal, before, after, true, hook));
    match rollback {
        Ok(()) => {
            original.context["rolled_back"] = json!(true);
            if let Err(cleanup) = finish(root, true, hook) {
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
    _journal: &Journal,
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
    write_progress(
        &directory,
        &Progress {
            rollback,
            completed_paths: 0,
            verified: false,
        },
    )?;
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
        write_progress(
            &directory,
            &Progress {
                rollback,
                completed_paths: index + 1,
                verified: false,
            },
        )?;
    }
    if capture(root)? != *target {
        return Err(invalid(
            "installed configuration failed full generation verification",
        ));
    }
    write_progress(
        &directory,
        &Progress {
            rollback,
            completed_paths: paths.len(),
            verified: true,
        },
    )
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
            if count > 16_384 {
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

fn write_progress(directory: &Path, progress: &Progress) -> Result<(), Fault> {
    let temporary = configuration::temporary(directory, "progress");
    configuration::write_private(&temporary, &encode(progress, 1024)?)?;
    let destination = directory.join("progress.json");
    checked_file(&destination, 1024)?;
    // Replacing this transaction-owned small record is intentional; never a
    // publication primitive for an operator file or another transaction.
    fs::rename(&temporary, &destination).map_err(|e| io_fault("record restore progress", e))?;
    configuration::sync_directory(directory)
}

fn finish(
    root: &Path,
    rollback: bool,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    let directory = root.join(JOURNAL);
    let marker = root.join(COMPLETION);
    let mut committed_rollback = rollback;
    let result = (|| {
        hook(Point::BeforeCleanup)?;
        let completion = if exists(&marker)? {
            let completion: Completion = decode(&read_bytes(&marker, MAX_JOURNAL)?)?;
            committed_rollback = completion.rollback;
            if completion.version != 1 {
                return Err(invalid("unsupported restore completion version"));
            }
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
        configuration::sync_directory(root)
    })();
    result.map_err(|mut fault| {
        fault.context["configuration_installed"] = json!(!committed_rollback);
        fault.context["rolled_back"] = json!(committed_rollback);
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
        if !matches!(name, "journal.json" | "progress.json")
            && !name.starts_with("old-")
            && !name.starts_with("new-")
            && !name.starts_with("held-")
            && !name.starts_with(".progress-")
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
mod tests {
    use super::*;
    use crate::configuration::tests::Root;
    use std::panic::{AssertUnwindSafe, catch_unwind};

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

    #[test]
    fn typed_validation_rejects_unknown_owners_and_wrong_scopes() {
        use crate::storage::{PackageReference, PackageSource};
        let tab = TabRecord {
            version: 1,
            internal_name: "One".into(),
            display_name: "One".into(),
            open: false,
            packages: vec![PackageReference {
                package_id: "pkg".into(),
                source: PackageSource::Directory {
                    path: "/package".into(),
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
