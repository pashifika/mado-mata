//! Narrow recoverable configuration transaction. Logs and package payloads never
//! enter the write set; an unresolved journal is an admission barrier owned by
//! bootstrap, not an invitation to load a partially installed tree.
use crate::backup::{MAX_MANIFEST, Manifest};
use crate::configuration::{self, Capture, Kind, MAX_ENUMERATED, capture, io_fault, path_kind};
use crate::storage::{
    self, MAX_OPEN_TABS, MAX_PROFILES, MAX_TABS, MAX_TOTAL_BYTES, Profile, Settings, TabRecord,
    check_directory, checked_file, decode, encode, exists, filesystem_key, read_bytes,
    validate_package_id, validate_settings, validate_tab,
};
use crate::target::{MAX_TARGET_BYTES, TargetRecord};
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
        let result = (|| {
            match path_kind(path)? {
                Kind::Settings | Kind::Tab | Kind::IdentityMigrations => {}
                Kind::LegacyProfile => {
                    // Historical sources are bounded preservation bytes, not active records.
                    budget(&mut budgets, "profiles", bytes.len())?;
                }
                Kind::Package => {
                    let parts: Vec<_> = path.split('/').collect();
                    let tab = tabs.get(parts[1]).ok_or_else(|| {
                        invalid("orphaned package configuration has no supported Tab record")
                    })?;
                    validate_package_id(parts[2])?;
                    if parts[3] == "target.config" {
                        if bytes.len() > MAX_TARGET_BYTES {
                            return Err(invalid("target configuration exceeds its byte bound"));
                        }
                        let target: TargetRecord = decode(bytes)?;
                        target.validate_owned(parts[1], parts[2])?;
                        // Target has its own per-file limit; Capture enforces the shared
                        // file/byte budget without consuming a portable-profile slot.
                        return Ok(());
                    }
                    if !tab
                        .packages
                        .iter()
                        .any(|reference| reference.package_id == parts[2])
                    {
                        return Err(invalid(
                            "package configuration is not owned by a saved Tab reference",
                        ));
                    }
                    let profile: Profile = decode(bytes).map_err(|_| {
                        invalid("unsupported or malformed package configuration owner")
                    })?;
                    storage::validate_profile(&profile)?;
                    if parts[3] != format!("{}.config", profile.id)
                        || profile.package_id != parts[2]
                    {
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
            Ok(())
        })();
        result.map_err(|mut fault: Fault| {
            fault.context["path"] = json!(path);
            fault
        })?;
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
    Ok(exists(&root.join(JOURNAL))?
        || exists(&root.join(COMPLETION))?
        || exists(&root.join("identity-migrations.pending"))?)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Restore,
    ProfileImport {
        internal_name: String,
        package_id: String,
        source_id: String,
    },
}

impl Operation {
    fn check(&self) -> Result<(), Fault> {
        if let Self::ProfileImport {
            internal_name,
            package_id,
            source_id,
        } = self
        {
            storage::validate_internal_name(internal_name)?;
            storage::validate_package_id(package_id)?;
            storage::validate_id(source_id)?;
        }
        Ok(())
    }
}

fn validate_transition(
    before: &Capture,
    after: &Capture,
    operation: &Operation,
) -> Result<(), Fault> {
    // Exact rollback uses the already bounded preimage, with no metadata merge.
    before.check()?;
    after.check()?;
    operation.check()?;
    match operation {
        Operation::Restore => validate(after),
        Operation::ProfileImport {
            internal_name,
            package_id,
            source_id,
        } => {
            storage::validate_import_transition(before, after, internal_name, package_id, source_id)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    operation: Operation,
    before: Manifest,
    after: Manifest,
}

impl Journal {
    fn check(&self) -> Result<(), Fault> {
        if self.version != 3 {
            return Err(unsupported());
        }
        self.before.check()?;
        self.after.check()?;
        self.operation.check()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    version: u32,
    operation: Operation,
    rollback: bool,
    target: Manifest,
}

fn unsupported() -> Fault {
    Fault::new(
        "RestoreUnsupported",
        "Unsupported pending configuration evidence was preserved. Use checkout 824d1b7bd001efb025e53a3becb9e5521af677cf to settle a preserved pre-upgrade copy; do not downgrade this root.",
    )
}

fn read_protocol<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Fault> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let bytes = read_bytes(path, MAX_JOURNAL)?;
    let header: Version = decode(&bytes)?;
    if header.version != 3 {
        return Err(unsupported());
    }
    decode(&bytes)
}

fn refuse_pending_ledger(root: &Path) -> Result<(), Fault> {
    if exists(&root.join("identity-migrations.pending"))? {
        return Err(unsupported().with_context(json!({"path": "identity-migrations.pending"})));
    }
    Ok(())
}

/// Read-only eligibility shared by Bootstrap and recovery command admission.
pub(crate) fn check_recovery(root: &Path) -> Result<(), Fault> {
    if !pending(root)? {
        return Err(invalid(
            "there is no pending configuration operation to recover",
        ));
    }
    refuse_pending_ledger(root)?;
    let directory = root.join(JOURNAL);
    if exists(&root.join(COMPLETION))? {
        let completion = read_completion(root)?;
        verify_completion(root, &completion.target)
    } else {
        check_directory(&directory)?;
        let journal: Journal = read_protocol(&directory.join("journal.json"))?;
        journal.check()?;
        let before = load_capture(&directory, "old", &journal.before)?;
        let after = load_capture(&directory, "new", &journal.after)?;
        validate_transition(&before, &after, &journal.operation)?;
        let mut aliases = BTreeMap::new();
        for path in before.files.keys().chain(after.files.keys()) {
            configuration::check_aliases(path, &mut aliases)?;
        }
        let live = capture(root)?;
        for (path, bytes) in &live.files {
            if before.files.get(path) != Some(bytes) && after.files.get(path) != Some(bytes) {
                return Err(invalid(
                    "live configuration differs from both journal generations",
                ));
            }
        }
        Ok(())
    }
}

fn read_completion(root: &Path) -> Result<Completion, Fault> {
    let completion: Completion = read_protocol(&root.join(COMPLETION))?;
    completion.operation.check()?;
    // The journal may already be partly removed by committed cleanup. A
    // retained older journal still blocks cleanup, even beside a current marker.
    let directory = root.join(JOURNAL);
    if exists(&directory)? {
        check_directory(&directory)?;
        if exists(&directory.join("journal.json"))? {
            let journal: Journal = read_protocol(&directory.join("journal.json"))?;
            journal.check()?;
            let target = if completion.rollback {
                &journal.before
            } else {
                &journal.after
            };
            if journal.operation != completion.operation
                || target.generation != completion.target.generation
            {
                return Err(invalid("completion differs from its retained journal"));
            }
        }
    }
    Ok(completion)
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

/// A Restore generation whose deterministic publication checks passed.
/// Only Bootstrap supplies session receipt, confirmation, and discard authority.
pub(crate) struct PreparedRestore(PreparedPublication);

struct PreparedPublication {
    observed: Capture,
    new: Capture,
    operation: Operation,
}

/// Bind the preparation to the preserved live generation without retiring its session.
pub(crate) fn prepare(
    new: Capture,
    observed: Capture,
    expected_generation: Option<&str>,
) -> Result<PreparedRestore, Fault> {
    if (!observed.files.is_empty() && expected_generation != Some(observed.generation.as_str()))
        || expected_generation.is_some_and(|expected| expected != observed.generation)
    {
        return Err(Fault::new(
            "SnapshotReceiptStale",
            "a separately requested snapshot matching the current configuration is required",
        ));
    }
    Ok(PreparedRestore(prepare_publication(
        observed,
        new,
        Operation::Restore,
    )?))
}

pub(crate) fn install(root: &Path, plan: PreparedRestore) -> Result<(), Fault> {
    install_with(root, plan, &mut |_| Ok(()))
}

fn install_with(
    root: &Path,
    plan: PreparedRestore,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    publish(root, plan.0, hook)
}

pub(crate) fn install_profile_import(
    root: &Path,
    before: Capture,
    after: Capture,
    tab: &str,
    package: &str,
    source_id: &str,
) -> Result<(), Fault> {
    let operation = Operation::ProfileImport {
        internal_name: tab.into(),
        package_id: package.into(),
        source_id: source_id.into(),
    };
    let prepared = prepare_publication(before, after, operation)?;
    publish(root, prepared, &mut |_point| {
        #[cfg(test)]
        tests::import_hook(root, _point);
        Ok(())
    })
}

fn prepare_publication(
    observed: Capture,
    new: Capture,
    operation: Operation,
) -> Result<PreparedPublication, Fault> {
    validate_transition(&observed, &new, &operation)?;
    // Changing the spelling of an existing directory is not an authorized tree
    // migration, and may alias a retained empty container after replacement.
    let mut aliases = BTreeMap::new();
    for path in observed.files.keys().chain(new.files.keys()) {
        configuration::check_aliases(path, &mut aliases)?;
    }
    Ok(PreparedPublication {
        observed,
        new,
        operation,
    })
}

fn publish(
    root: &Path,
    prepared: PreparedPublication,
    hook: &mut impl FnMut(Point) -> Result<(), Fault>,
) -> Result<(), Fault> {
    let PreparedPublication {
        observed,
        new,
        operation,
    } = prepared;
    if pending(root)? {
        return Err(invalid(
            "an interrupted configuration transaction must be recovered first",
        ));
    }
    if capture(root)? != observed {
        return Err(Fault::new(
            "ConfigurationChanged",
            "configuration changed after transaction admission",
        ));
    }
    storage::private_directory(root)?;
    let before = Capture::from_files(observed.files, true)?;
    let staging = configuration::temporary(root, "restore-stage");
    configuration::create_private_directory(&staging)?;
    let journal = Journal {
        version: 3,
        operation,
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
    refuse_pending_ledger(root)?;
    if exists(&root.join(COMPLETION))? {
        return finish(root, rollback, None, hook);
    }
    let directory = root.join(JOURNAL);
    check_directory(&directory)?;
    let journal: Journal = read_protocol(&directory.join("journal.json"))?;
    journal.check()?;
    let before = load_capture(&directory, "old", &journal.before)?;
    let after = load_capture(&directory, "new", &journal.after)?;
    validate_transition(&before, &after, &journal.operation)?;
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
            let completion = read_completion(root)?;
            committed_rollback = Some(completion.rollback);
            if completion.rollback != rollback {
                return Err(invalid(
                    "configuration is already committed; resume its cleanup in the original completion or rollback direction",
                ));
            }
            completion
        } else {
            let journal: Journal = read_protocol(&directory.join("journal.json"))?;
            journal.check()?;
            let target = if rollback {
                journal.before
            } else {
                journal.after
            };
            verify_completion(root, &target)?;
            let completion = Completion {
                version: 3,
                operation: journal.operation,
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

    type ImportHook = (std::path::PathBuf, fn(&Path, Point));
    thread_local! {
        static IMPORT_HOOK: std::cell::RefCell<Option<ImportHook>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn import_hook(root: &Path, point: Point) {
        IMPORT_HOOK.with(|slot| {
            if let Some((owner, hook)) = slot.borrow().as_ref() {
                if owner == root {
                    hook(root, point);
                }
            }
        });
    }

    fn with_import_hook<T>(root: &Path, hook: fn(&Path, Point), action: impl FnOnce() -> T) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                IMPORT_HOOK.with(|slot| *slot.borrow_mut() = None);
            }
        }
        IMPORT_HOOK.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some((root.to_path_buf(), hook));
        });
        let _reset = Reset;
        action()
    }

    pub(crate) fn with_import_cleanup_failure<T>(root: &Path, action: impl FnOnce() -> T) -> T {
        with_import_hook(
            root,
            |root, point| {
                if point == (Point::CleanupRemoved { index: 0 }) {
                    // Enumeration has validated every staged file and journal.json has
                    // been removed. The next actual remove_file must fail on a directory.
                    let obstacle = root.join(JOURNAL).join("new-0");
                    fs::remove_file(&obstacle).unwrap();
                    configuration::create_private_directory(&obstacle).unwrap();
                }
            },
            action,
        )
    }

    pub(crate) fn interrupt_install(root: &Path, new: Capture, expected_generation: &str) {
        let plan = prepare(new, capture(root).unwrap(), Some(expected_generation)).unwrap();
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
        assert!(catch_unwind(AssertUnwindSafe(|| install_with(root, plan, &mut hook))).is_err());
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
        for area in ["sources", "pkgs"] {
            root.put(
                &format!("{area}/sample/main.ts"),
                b"export const retained = true;",
            );
            root.put(&format!("{area}/sample/assets/pixel.rgba"), &[1, 2, 3, 255]);
        }
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
        for area in ["sources", "pkgs"] {
            assert_eq!(
                fs::read(root.0.join(area).join("sample/main.ts")).unwrap(),
                b"export const retained = true;"
            );
            assert_eq!(
                fs::read(root.0.join(area).join("sample/assets/pixel.rgba")).unwrap(),
                [1, 2, 3, 255]
            );
        }
    }
    fn profile_id() -> String {
        "00000000000000000000".into()
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
        assert!(prepare(replacement(200), old.clone(), None).is_err());
        assert!(prepare(replacement(200), old.clone(), Some("stale")).is_err());
        let invalid = Capture::from_files(
            BTreeMap::from([("settings.json".into(), b"broken".to_vec())]),
            true,
        )
        .unwrap();
        assert!(prepare(invalid, old.clone(), Some(&old.generation)).is_err());
        assert_eq!(capture(&root.0).unwrap(), old);
        assert!(!pending(&root.0).unwrap());
    }

    #[test]
    fn successful_install_replaces_only_managed_configuration() {
        let (root, old) = fixture();
        let new = replacement(200);
        let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
        install(&root.0, plan).unwrap();
        assert_eq!(capture(&root.0).unwrap(), new);
        assert!(!pending(&root.0).unwrap());
        assert_unrelated(&root);
    }

    #[test]
    fn prepared_restore_rechecks_live_generation_and_pending_recovery_before_writes() {
        for interrupted in [false, true] {
            let (root, old) = fixture();
            let plan = prepare(replacement(200), old.clone(), Some(&old.generation)).unwrap();
            if interrupted {
                root.put(COMPLETION, b"pending recovery evidence");
            } else {
                fs::write(root.0.join("settings.json"), settings(333)).unwrap();
            }
            let current = capture(&root.0).unwrap();
            let error = install(&root.0, plan).unwrap_err();
            assert_eq!(
                error.category,
                if interrupted {
                    "Restore"
                } else {
                    "ConfigurationChanged"
                }
            );
            assert_eq!(capture(&root.0).unwrap(), current);
            assert_eq!(pending(&root.0).unwrap(), interrupted);
            assert!(!root.0.join(JOURNAL).exists());
            if interrupted {
                assert_eq!(
                    fs::read(root.0.join(COMPLETION)).unwrap(),
                    b"pending recovery evidence"
                );
            }
        }
    }

    #[test]
    fn os_metadata_is_preserved_but_never_archived_or_installed_as_configuration() {
        let source = Root::new();
        let archive_directory = Root::new();
        let destination = Root::new();
        let id = profile_id();
        source.put("settings.json", &settings(100));
        source.put("tabs/One/tab.config", &tab("One", true));
        source.put(&format!("tabs/One/pkg/{id}.config"), &profile());
        source.put(&format!("profiles/{id}.json"), &profile());
        let expected = capture(&source.0).unwrap();
        validate(&expected).unwrap();
        let metadata_paths = [
            ".DS_Store".to_owned(),
            "profiles/._legacy.pending".to_owned(),
            format!("profiles/._{id}.json"),
            "tabs/One/._tab.pending".to_owned(),
            format!("tabs/One/pkg/._{id}.config"),
            format!("tabs/One/pkg/._{id}.pending"),
            "tabs/One/pkg/desktop.ini".to_owned(),
        ];
        for path in &metadata_paths {
            source.put(path, b"source OS metadata");
            destination.put(path, b"destination OS metadata");
        }
        let captured = capture(&source.0).unwrap();
        assert_eq!(captured, expected);
        let receipt =
            crate::backup::write(&source.0, captured, Some(&archive_directory.0)).unwrap();
        let archived = crate::backup::read(Path::new(&receipt.path)).unwrap();
        assert_eq!(archived, expected);
        validate(&archived).unwrap();

        destination.put("settings.json", &settings(200));
        let before = capture(&destination.0).unwrap();
        // New metadata after the preservation receipt must not invalidate it.
        destination.put("tabs/One/pkg/Thumbs.db", b"new thumbnail cache");
        let expected_generation = before.generation.clone();
        let plan = prepare(archived, before, Some(&expected_generation)).unwrap();
        install(&destination.0, plan).unwrap();
        assert_eq!(capture(&destination.0).unwrap(), expected);
        assert!(!pending(&destination.0).unwrap());
        let store = Store::new(destination.0.clone()).unwrap();
        let tabs = store.tabs().unwrap();
        assert!(tabs.faults.is_empty());
        assert_eq!(tabs.tabs.len(), 1);
        assert_eq!(tabs.tabs[0].internal_name, "One");
        let profiles = store
            .profile_store("One", "pkg")
            .unwrap()
            .list("pkg", &"a".repeat(64))
            .unwrap();
        assert!(profiles.rejected.is_empty());
        assert_eq!(profiles.profiles.len(), 1);
        assert_eq!(profiles.profiles[0].id, id);
        for path in metadata_paths {
            assert_eq!(
                fs::read(source.0.join(&path)).unwrap(),
                b"source OS metadata"
            );
            assert_eq!(
                fs::read(destination.0.join(path)).unwrap(),
                b"destination OS metadata"
            );
        }
        assert_eq!(
            fs::read(destination.0.join("tabs/One/pkg/Thumbs.db")).unwrap(),
            b"new thumbnail cache"
        );
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
            let plan = prepare(replacement(200), old.clone(), Some(&old.generation)).unwrap();
            assert!(install_with(&root.0, plan, &mut hook).is_err());
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
            let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
            assert!(
                catch_unwind(AssertUnwindSafe(|| install_with(&root.0, plan, &mut hook))).is_err()
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
        let plan = prepare(replacement(200), old.clone(), Some(&old.generation)).unwrap();
        assert!(install_with(&root.0, plan, &mut hook).is_err());
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
        let plan = prepare(replacement(200), old.clone(), Some(&old.generation)).unwrap();
        let _ = catch_unwind(AssertUnwindSafe(|| install_with(&root.0, plan, &mut hook)));
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
        let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
        let fault = install_with(&root.0, plan, &mut hook).unwrap_err();
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
            let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
            assert!(
                catch_unwind(AssertUnwindSafe(|| install_with(&root.0, plan, &mut hook))).is_err()
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
        let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
        let fault = install_with(&root.0, plan, &mut hook).unwrap_err();
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
        let plan = prepare(replacement(200), old.clone(), Some(&old.generation)).unwrap();
        let _ = catch_unwind(AssertUnwindSafe(|| install_with(&root.0, plan, &mut hook)));
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
        let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
        assert!(catch_unwind(AssertUnwindSafe(|| install_with(&root.0, plan, &mut hook))).is_err());
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
        let plan = prepare(replacement(200), old.clone(), Some(&old.generation)).unwrap();
        assert!(catch_unwind(AssertUnwindSafe(|| install_with(&root.0, plan, &mut hook))).is_err());
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
        let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
        install(&root.0, plan).unwrap();
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
        let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
        install(&root.0, plan).unwrap();
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
        let plan = prepare(new.clone(), old.clone(), Some(&old.generation)).unwrap();
        assert!(catch_unwind(AssertUnwindSafe(|| install_with(&root.0, plan, &mut hook))).is_err());
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
        let plan = prepare(new, old.clone(), Some(&old.generation)).unwrap();
        let fault = install_with(&root.0, plan, &mut hook).unwrap_err();
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
            id: profile_id(),
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

    fn target_record() -> TargetRecord {
        let declaration = crate::target::tests::declaration();
        TargetRecord {
            version: 1,
            internal_name: "Owner".into(),
            package_id: "pkg".into(),
            revision: 7,
            binding: Some(crate::target::TargetBinding {
                id: profile_id(),
                package_id: "pkg".into(),
                target_id: declaration.id.clone(),
                declaration_identity: declaration.identity().unwrap(),
                configuration: crate::target::tests::configuration("/offline/not-installed/game"),
                resolution: crate::target::TargetResolution {
                    game: crate::target::ResolvedLocation {
                        path: "/offline/not-installed/game".into(),
                        executable: "/offline/not-installed/game".into(),
                    },
                    launcher: None,
                    working_directory: None,
                },
            }),
        }
    }

    #[test]
    fn complete_snapshot_restores_target_intent_without_installation_metadata() {
        let source = Root::new();
        source.put("settings.json", &settings(200));
        source.put("tabs/Owner/tab.config", &tab("Owner", true));
        source.put(
            &format!("tabs/Owner/pkg/{}.config", profile_id()),
            &profile(),
        );
        let record = target_record();
        let bytes = serde_json::to_vec(&record).unwrap();
        source.put("tabs/Owner/pkg/target.config", &bytes);
        let snapshot = capture(&source.0).unwrap();
        assert_eq!(snapshot.files["tabs/Owner/pkg/target.config"], bytes);
        validate(&snapshot).unwrap();
        let destination = Root::new();
        destination.put("settings.json", &settings(100));
        destination.put("logs/retained.log", b"unrelated");
        let before = capture(&destination.0).unwrap();
        let expected_generation = before.generation.clone();
        let plan = prepare(snapshot.clone(), before, Some(&expected_generation)).unwrap();
        install(&destination.0, plan).unwrap();
        assert_eq!(capture(&destination.0).unwrap(), snapshot);
        assert_eq!(
            Store::new(destination.0.clone())
                .unwrap()
                .read_target("Owner", "pkg")
                .unwrap(),
            record
        );
        assert_eq!(
            fs::read(destination.0.join("logs/retained.log")).unwrap(),
            b"unrelated"
        );
    }

    #[test]
    fn target_only_snapshot_restores_before_inspection_but_refuses_unbound_profiles() {
        let source = Root::new();
        source.put("settings.json", &settings(200));
        let mut owner: TabRecord = decode(&tab("Owner", true)).unwrap();
        owner.packages.clear();
        owner.selected_package_id = None;
        source.put(
            "tabs/Owner/tab.config",
            &serde_json::to_vec(&owner).unwrap(),
        );
        let record = target_record();
        source.put(
            "tabs/Owner/pkg/target.config",
            &serde_json::to_vec(&record).unwrap(),
        );
        let snapshot = capture(&source.0).unwrap();
        validate(&snapshot).unwrap();

        let destination = Root::new();
        destination.put("settings.json", &settings(100));
        let before = capture(&destination.0).unwrap();
        let generation = before.generation.clone();
        let plan = prepare(snapshot.clone(), before, Some(&generation)).unwrap();
        install(&destination.0, plan).unwrap();
        let restored = Store::new(destination.0.clone()).unwrap();
        assert_eq!(restored.read_target("Owner", "pkg").unwrap(), record);
        assert_eq!(restored.tab("Owner").unwrap(), owner);
        assert!(matches!(
            restored.profile_store("Owner", "pkg"),
            Err(fault) if fault.category == "ProfileIdentity"
        ));

        let mut files = snapshot.files;
        files.insert(format!("tabs/Owner/pkg/{}.config", profile_id()), profile());
        assert!(validate(&Capture::from_files(files, true).unwrap()).is_err());
    }

    #[test]
    fn restore_target_owner_schema_and_shared_budget_are_strict_without_using_profile_slots() {
        let mut files = replacement(200).files;
        files.insert("tabs/Owner/tab.config".into(), tab("Owner", true));
        for index in 0..MAX_PROFILES {
            let mut profile: Profile = decode(&profile()).unwrap();
            profile.id = format!("{index:019x}0");
            files.insert(
                format!("tabs/Owner/pkg/{}.config", profile.id),
                serde_json::to_vec(&profile).unwrap(),
            );
        }
        let record = target_record();
        let bytes = serde_json::to_vec(&record).unwrap();
        let path = "tabs/Owner/pkg/target.config";
        files.insert(path.into(), bytes.clone());
        validate(&Capture::from_files(files.clone(), true).unwrap()).unwrap();
        for (field, value) in [
            ("version", json!(2)),
            ("internal_name", json!("Other")),
            ("package_id", json!("other")),
            ("revision", json!(0)),
            ("revision", json!(crate::target::MAX_TARGET_REVISION + 1)),
            ("binding", json!(["not", "an", "object"])),
            ("approval", json!(true)),
        ] {
            let mut invalid = serde_json::to_value(&record).unwrap();
            invalid[field] = value;
            files.insert(path.into(), serde_json::to_vec(&invalid).unwrap());
            assert!(
                validate(&Capture::from_files(files.clone(), true).unwrap()).is_err(),
                "{field}"
            );
        }
        let mut at_bound = bytes;
        at_bound.resize(MAX_TARGET_BYTES, b' ');
        files.insert(path.into(), at_bound.clone());
        validate(&Capture::from_files(files.clone(), true).unwrap()).unwrap();
        at_bound.push(b' ');
        files.insert(path.into(), at_bound);
        assert!(Capture::from_files(files, true).is_err());

        // Each target file participates in the same 16 MiB captured-byte budget.
        let mut aggregate = BTreeMap::new();
        for index in 0..configuration::MAX_BYTES / MAX_TARGET_BYTES {
            aggregate.insert(
                format!("tabs/T{index}/pkg/target.config"),
                vec![b' '; MAX_TARGET_BYTES],
            );
        }
        assert!(Capture::from_files(aggregate.clone(), true).is_ok());
        aggregate.insert("tabs/Overflow/pkg/target.config".into(), vec![b' ']);
        assert!(Capture::from_files(aggregate, true).is_err());
    }
}

#[cfg(test)]
pub(crate) mod cutover_tests;
