use super::{Candidate, MAX_BYTES, MAX_FILES, Publisher, io_fault, limits, stale};
use crate::configuration::{
    create_private_directory as create_directory, create_private_file, digest, publish_no_replace,
    sync_directory, temporary, write_private as write_new,
};
use crate::storage::{
    check_directory, encode, exists, filesystem_key, private_directory, read_bytes,
};
use mado_runtime_comparison::inventory::PackageDraft;
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const JOURNAL_BYTES: usize = 9 * MAX_BYTES;
const PARENT_ENTRIES: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    path: String,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    root: PathBuf,
    root_identity: String,
    scratch: String,
    before: BTreeMap<String, String>,
    after: BTreeMap<String, String>,
    changes: Vec<Change>,
}

fn hashes(files: &BTreeMap<String, Arc<[u8]>>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(path, bytes)| (path.clone(), digest(bytes)))
        .collect()
}

impl Publisher {
    fn directory(&self) -> PathBuf {
        self.data_root.join("authoring")
    }
    fn journal_path(&self) -> PathBuf {
        self.directory().join("pending.json")
    }

    /// Called before restoring any source. Recovery is explicit: startup never
    /// silently overwrites source or chooses a different configuration root.
    pub fn recover_pending(&self) -> Result<(), Fault> {
        if !exists(&self.data_root)? {
            return Ok(());
        }
        check_directory(&self.data_root)?;
        let directory = self.directory();
        if !exists(&directory)? {
            return Ok(());
        }
        check_directory(&directory)?;
        if !exists(&self.journal_path())? {
            return Ok(());
        }
        match self.read_journal() {
            Ok(journal) => Err(recovery(&journal.root, "a pending source publication requires explicit recovery")),
            Err(fault) => Err(Fault::new("AuthoringRecoveryRequired", "private publication journal is unreadable or invalid; preserve it for manual repair").with_context(json!({"cause":fault}))),
        }
    }

    pub fn check_admission(&self, _root: &Path) -> Result<(), Fault> {
        // There is only one publication slot. A malformed journal cannot safely
        // establish which package it owns, so refuse all package admission.
        self.recover_pending()
    }

    pub fn recover(&self, root: &Path) -> Result<(), Fault> {
        let journal = self.read_journal()?;
        let canonical = PackageDraft::canonical_root(root)?;
        if canonical != journal.root {
            return Err(recovery(
                &journal.root,
                "recovery request does not own the pending package",
            ));
        }
        self.separate_root(&canonical)?;
        self.finish(&journal, |_| Ok(()))?;
        self.retire(&journal)
    }

    pub(super) fn create_files(
        &self,
        root: &Path,
        draft: PackageDraft,
    ) -> Result<Candidate, Fault> {
        self.check_admission(root)?;
        let destination = missing_destination(root, Some(self))?;
        self.separate_root(&destination)?;
        let parent = destination.parent().ok_or_else(stale)?;
        let stage = temporary(parent, "mado-authoring-create");
        create_directory(&stage)?;
        let result = (|| {
            for (path, bytes) in draft.files() {
                let destination = checked_destination(&stage, path, true)?;
                write_new(&destination, bytes)?;
                sync_directory(destination.parent().ok_or_else(stale)?)?;
            }
            sync_directory(&stage)?;
            missing_destination(&destination, None)?;
            publish_no_replace(&stage, &destination)
                .map_err(|error| io_fault("publish missing package directory", error))?;
            sync_directory(parent)?;
            self.open(&destination)
        })();
        if result.is_err() && exists(&stage).unwrap_or(false) {
            // This call created the entire private staging tree, never the destination.
            let _ = fs::remove_dir_all(&stage);
        }
        result
    }

    pub(super) fn publish_files(
        &self,
        candidate: &Candidate,
        next: &PackageDraft,
        after_change: impl FnMut(usize) -> Result<(), Fault>,
    ) -> Result<Option<Fault>, Fault> {
        if candidate.revision() == next.revision() {
            return Ok(None);
        }
        let mut paths: BTreeSet<_> = candidate
            .draft
            .files()
            .keys()
            .chain(next.files().keys())
            .cloned()
            .collect();
        let mut changes = Vec::new();
        // Keep the declaration replacement last, while admission stays closed.
        paths.remove("package.json");
        for path in paths
            .into_iter()
            .chain(std::iter::once("package.json".into()))
        {
            let before = candidate.draft.files().get(&path);
            let after = next.files().get(&path);
            if before == after {
                continue;
            }
            let destination = checked_destination(&candidate.root, &path, false)?;
            if read_package_file(&destination)?.as_deref() != before.map(AsRef::as_ref) {
                return Err(stale());
            }
            changes.push(Change {
                path,
                before: before.map(|bytes| bytes.to_vec()),
                after: after.map(|bytes| bytes.to_vec()),
            });
        }
        let parent = candidate.root.parent().ok_or_else(stale)?;
        let scratch = temporary(parent, "mado-authoring-write");
        if exists(&scratch)? {
            return Err(Fault::new(
                "AuthoringDestination",
                "publication staging name is occupied; retained bytes were not touched",
            ));
        }
        let journal = Journal {
            version: 1,
            root: candidate.root.clone(),
            root_identity: candidate.root_identity.clone(),
            scratch: scratch
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(stale)?
                .to_owned(),
            before: hashes(candidate.draft.files()),
            after: hashes(next.files()),
            changes,
        };
        private_directory(&self.data_root)?;
        private_directory(&self.directory())?;
        sync_directory(&self.data_root)?;
        if let Some(parent) = self.data_root.parent() {
            sync_directory(parent)?;
        }
        // Only a complete durable journal may become the admission barrier.
        self.publish_journal(&journal)?;
        self.finish(&journal, after_change)?;
        // Persistence is already complete. Failure to retire is a refresh/recovery
        // problem, not a request to repeat the mutation; open reports the journal.
        Ok(self.retire(&journal).err())
    }

    fn publish_journal(&self, journal: &Journal) -> Result<(), Fault> {
        let bytes = encode(journal, JOURNAL_BYTES)?;
        let directory = self.directory();
        // A single unpublished slot bounds crash leftovers. Source writes begin
        // only after this file has moved to pending.json.
        let stage = directory.join("pending.tmp");
        if exists(&stage)? {
            crate::storage::checked_file(&stage, JOURNAL_BYTES)?;
            fs::remove_file(&stage)
                .map_err(|error| io_fault("discard unpublished journal stage", error))?;
        }
        let mut file = create_private_file(&stage)?;
        let staged = file
            .write_all(&bytes)
            .map_err(|error| crate::configuration::io_fault("write authoring journal", error))
            .and_then(|()| {
                file.sync_all()
                    .map_err(|error| crate::configuration::io_fault("sync authoring journal", error))
            });
        drop(file);
        let result = staged.and_then(|()| {
            publish_no_replace(&stage, &self.journal_path())
                .map_err(|error| io_fault("publish authoring journal", error))?;
            sync_directory(&directory)
        });
        if let Err(mut fault) = result {
            if let Err(error) = fs::remove_file(&stage)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                fault.context["temporary_cleanup"] =
                    json!({"path": stage, "kind": format!("{:?}", error.kind())});
            }
            return Err(fault);
        }
        Ok(())
    }

    fn read_journal(&self) -> Result<Journal, Fault> {
        check_directory(&self.data_root)?;
        check_directory(&self.directory())?;
        let bytes = read_bytes(&self.journal_path(), JOURNAL_BYTES)?;
        let journal: Journal = serde_json::from_slice(&bytes).map_err(|error| {
            Fault::new("AuthoringRecoveryRequired", "invalid publication journal")
                .with_context(json!({"cause":error.to_string()}))
        })?;
        journal.validate()?;
        Ok(journal)
    }

    fn finish(
        &self,
        journal: &Journal,
        mut after_change: impl FnMut(usize) -> Result<(), Fault>,
    ) -> Result<(), Fault> {
        journal.validate()?;
        let result = (|| {
            if PackageDraft::canonical_root(&journal.root)? != journal.root
                || root_identity(&journal.root)? != journal.root_identity
            {
                return Err(stale());
            }
            self.separate_root(&journal.root)?;
            let mut recovery_limits = limits()?;
            recovery_limits.snapshot_files = MAX_FILES * 2;
            recovery_limits.snapshot_bytes = MAX_BYTES * 2;
            let current = PackageDraft::capture_bytes(&journal.root, &recovery_limits)?;
            journal.check_current(&current)?;
            // Validate the whole prospective declaration before any recovery write.
            let mut prospective = current;
            for change in &journal.changes {
                if let Some(bytes) = &change.after {
                    prospective.insert(change.path.clone(), Arc::from(bytes.as_slice()));
                } else {
                    prospective.remove(&change.path);
                }
            }
            if hashes(&prospective) != journal.after {
                return Err(stale());
            }
            PackageDraft::from_files(prospective, &limits()?)?;
            let scratch = journal.scratch_path()?;
            if exists(&scratch)? {
                check_scratch(&scratch, journal.changes.len())?;
            } else {
                create_directory(&scratch)?;
                sync_directory(scratch.parent().ok_or_else(stale)?)?;
            }
            for (index, change) in journal.changes.iter().enumerate() {
                if root_identity(&journal.root)? != journal.root_identity {
                    return Err(stale());
                }
                let destination = checked_destination(&journal.root, &change.path, false)?;
                let now = read_package_file(&destination)?;
                if now.as_deref() == change.after.as_deref() {
                    continue;
                }
                if now.as_deref() != change.before.as_deref() {
                    return Err(stale());
                }
                if let Some(bytes) = &change.after {
                    let destination = checked_destination(&journal.root, &change.path, true)?;
                    let stage = scratch.join(format!("next-{index}"));
                    let staged = if exists(&stage)? {
                        let written = read_bytes(&stage, MAX_BYTES)?;
                        if written == *bytes {
                            // A prior write may have completed before sync failed.
                            OpenOptions::new()
                                .read(true)
                                .write(true)
                                .open(&stage)
                                .and_then(|file| file.sync_all())
                                .map_err(|error| {
                                    io_fault("sync recovered publication stage", error)
                                })?;
                            true
                        } else if bytes.starts_with(&written) {
                            // A partial private write is rebuildable from the
                            // journal; unrelated bytes still require repair.
                            fs::remove_file(&stage).map_err(|error| {
                                io_fault("discard partial publication stage", error)
                            })?;
                            false
                        } else {
                            return Err(stale());
                        }
                    } else {
                        false
                    };
                    if !staged {
                        write_new(&stage, bytes)?;
                        sync_directory(&scratch)?;
                    }
                    // Recheck the affected source immediately before replacement.
                    if read_package_file(&destination)?.as_deref() != change.before.as_deref() {
                        return Err(stale());
                    }
                    if change.before.is_none() {
                        publish_no_replace(&stage, &destination)
                            .map_err(|error| io_fault("publish added package file", error))?;
                    } else {
                        fs::rename(&stage, &destination)
                            .map_err(|error| io_fault("replace package file", error))?;
                    }
                    sync_directory(&scratch)?;
                } else {
                    fs::remove_file(&destination)
                        .map_err(|error| io_fault("remove declared package file", error))?;
                }
                sync_directory(destination.parent().ok_or_else(stale)?)?;
                after_change(index)?;
            }
            let complete = PackageDraft::capture(&journal.root, &limits()?)?;
            if hashes(complete.files()) != journal.after {
                return Err(stale());
            }
            Ok(())
        })();
        result.map_err(|fault: Fault| recovery(&journal.root, "publication is incomplete or conflicts with external source; original bytes and preimages were retained").with_context(json!({"package_path":journal.root,"cause":fault})))
    }

    fn retire(&self, journal: &Journal) -> Result<(), Fault> {
        let scratch = journal.scratch_path()?;
        if exists(&scratch)? {
            check_scratch(&scratch, journal.changes.len())?;
            // A crash after the package rename can leave a redundant staged copy.
            for index in 0..journal.changes.len() {
                let path = scratch.join(format!("next-{index}"));
                if exists(&path)? {
                    let expected = journal.changes[index].after.as_deref().ok_or_else(stale)?;
                    if read_bytes(&path, MAX_BYTES)? != expected {
                        return Err(recovery(
                            &journal.root,
                            "staging bytes changed externally; preserve them for repair",
                        ));
                    }
                    fs::remove_file(&path)
                        .map_err(|error| io_fault("retire publication stage", error))?;
                }
            }
            fs::remove_dir(&scratch)
                .map_err(|error| io_fault("retire publication staging directory", error))?;
            sync_directory(scratch.parent().ok_or_else(stale)?)?;
        }
        fs::remove_file(self.journal_path())
            .map_err(|error| io_fault("retire publication journal", error))?;
        sync_directory(&self.directory())
    }
}

impl Journal {
    fn validate(&self) -> Result<(), Fault> {
        if self.version != 1
            || !self.root.is_absolute()
            || self.root.as_os_str().len() > 4096
            || self
                .root
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            || !self.scratch.starts_with(".mado-authoring-write-")
            || self.scratch.len() > 96
            || !self
                .scratch
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            || self.before.len() > MAX_FILES
            || self.after.len() > MAX_FILES
            || self.changes.len() > MAX_FILES * 2
        {
            return Err(recovery(
                &self.root,
                "unsupported or unbounded publication journal",
            ));
        }
        for (path, hash) in self.before.iter().chain(&self.after) {
            PackageDraft::check_path(path)?;
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(stale());
            }
        }
        let mut changed = BTreeSet::new();
        let mut preimage_bytes = 0usize;
        let mut postimage_bytes = 0usize;
        for change in &self.changes {
            PackageDraft::check_path(&change.path)?;
            if !changed.insert(&change.path) || change.before == change.after {
                return Err(stale());
            }
            preimage_bytes += change.before.as_ref().map_or(0, Vec::len);
            postimage_bytes += change.after.as_ref().map_or(0, Vec::len);
            if preimage_bytes > MAX_BYTES
                || postimage_bytes > MAX_BYTES
                || change.before.as_ref().map(|bytes| digest(bytes)).as_ref()
                    != self.before.get(&change.path)
                || change.after.as_ref().map(|bytes| digest(bytes)).as_ref()
                    != self.after.get(&change.path)
            {
                return Err(stale());
            }
        }
        for path in self.before.keys().chain(self.after.keys()) {
            if self.before.get(path) != self.after.get(path) && !changed.contains(path) {
                return Err(stale());
            }
        }
        Ok(())
    }

    fn scratch_path(&self) -> Result<PathBuf, Fault> {
        Ok(self.root.parent().ok_or_else(stale)?.join(&self.scratch))
    }

    fn check_current(&self, current: &BTreeMap<String, Arc<[u8]>>) -> Result<(), Fault> {
        let actual = hashes(current);
        for path in self
            .before
            .keys()
            .chain(self.after.keys())
            .chain(actual.keys())
        {
            if !self.before.contains_key(path) && !self.after.contains_key(path) {
                return Err(stale());
            }
            let observed = actual.get(path);
            if observed != self.before.get(path) && observed != self.after.get(path) {
                return Err(stale());
            }
        }
        Ok(())
    }
}

fn recovery(root: &Path, message: &str) -> Fault {
    Fault::new("AuthoringRecoveryRequired", message).with_context(json!({"package_path":root}))
}

pub(super) fn root_identity(root: &Path) -> Result<String, Fault> {
    let metadata =
        fs::symlink_metadata(root).map_err(|error| io_fault("inspect package root", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(stale());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(stale());
        }
        Ok(format!("{}", metadata.creation_time()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(Fault::new(
            "AuthoringStorage",
            "package root identity is unsupported on this platform",
        ))
    }
}
/// Resolve the existing ancestor before checking private-root separation. Missing
/// components remain inert until an explicit authoring command creates them.
pub(super) fn resolve_packages_root(root: &Path) -> Result<PathBuf, Fault> {
    if !root.is_absolute()
        || root.as_os_str().len() > crate::storage::MAX_PATH_BYTES
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || root
            .to_str()
            .is_none_or(|path| path.chars().any(char::is_control))
    {
        return Err(Fault::new(
            "AuthoringPath",
            "packages root must be a bounded absolute path without parent traversal",
        ));
    }
    let mut ancestor = root.to_path_buf();
    let mut missing = Vec::new();
    while !exists(&ancestor)? {
        let name = ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(stale)?;
        if !crate::configuration::safe_component(name) || missing.len() >= MAX_FILES {
            return Err(Fault::new(
                "AuthoringPath",
                "packages root contains an unsafe or excessive path component",
            ));
        }
        missing.push(name.to_owned());
        if !ancestor.pop() {
            return Err(stale());
        }
    }
    let mut resolved = PackageDraft::canonical_root(&ancestor)?;
    if let (Some(parent), Some(name)) = (
        ancestor.parent(),
        ancestor.file_name().and_then(|name| name.to_str()),
    ) {
        check_alias(parent, name)?;
    }
    check_package_ancestors(&resolved)?;
    for name in missing.into_iter().rev() {
        if exists(&resolved)? {
            check_alias(&resolved, &name)?;
        }
        resolved.push(name);
    }
    Ok(resolved)
}

pub(super) fn create_packages_root(root: &Path) -> Result<(), Fault> {
    let mut ancestor = root.to_path_buf();
    let mut missing = Vec::new();
    while !exists(&ancestor)? {
        missing.push(ancestor.clone());
        if !ancestor.pop() {
            return Err(stale());
        }
    }
    PackageDraft::canonical_root(&ancestor)?;
    for directory in missing.into_iter().rev() {
        let parent = directory.parent().ok_or_else(stale)?;
        PackageDraft::canonical_root(parent)?;
        check_alias(
            parent,
            directory
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(stale)?,
        )?;
        create_directory(&directory)?;
        sync_directory(parent)?;
    }
    Ok(())
}

fn missing_destination(root: &Path, publisher: Option<&Publisher>) -> Result<PathBuf, Fault> {
    if root
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(Fault::new(
            "AuthoringPath",
            "destination may not contain parent traversal",
        ));
    }
    let absolute = std::path::absolute(root)
        .map_err(|error| io_fault("resolve package destination", error))?;
    let name = absolute
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(stale)?;
    if absolute.as_os_str().len() > 4096 || !crate::configuration::safe_component(name) {
        return Err(Fault::new(
            "AuthoringPath",
            "package destination is not a safe bounded filesystem component",
        ));
    }
    let parent = absolute.parent().ok_or_else(stale)?;
    if let Some(publisher) = publisher {
        if !exists(parent)? {
            // The complete starter/copy is validated before create_files reaches here.
            let resolved = publisher.packages_root(parent)?;
            publisher.separate_root(&resolved.join(name))?;
            create_packages_root(&resolved)?;
        }
    }
    let parent = PackageDraft::canonical_root(parent)?;
    check_package_ancestors(&parent)?;
    check_alias(&parent, name)?;
    let destination = parent.join(name);
    if exists(&destination)? {
        return Err(Fault::new(
            "AuthoringDestination",
            "Create and Duplicate require a missing destination",
        ));
    }
    Ok(destination)
}

pub(crate) fn check_package_ancestors(parent: &Path) -> Result<(), Fault> {
    let mut manifest_bytes = 0;
    for (depth, ancestor) in parent.ancestors().enumerate() {
        if depth >= MAX_FILES {
            return Err(Fault::new(
                "AuthoringPath",
                "destination ancestry exceeds the inspection bound",
            ));
        }
        if let Some(bytes) = read_package_file(&ancestor.join("package.json"))? {
            manifest_bytes += bytes.len();
            if manifest_bytes > MAX_BYTES {
                return Err(Fault::new(
                    "AuthoringPath",
                    "ancestor manifests exceed the inspection byte bound",
                ));
            }
            if serde_json::from_slice::<serde_json::Value>(&bytes)
                .is_ok_and(|manifest| manifest["sdk"] == "mado-host-v1")
            {
                return Err(Fault::new(
                    "AuthoringDestination",
                    "destination cannot be inside an existing package",
                ));
            }
        }
    }
    Ok(())
}

fn check_alias(parent: &Path, name: &str) -> Result<(), Fault> {
    let key = filesystem_key(name);
    for (count, entry) in fs::read_dir(parent)
        .map_err(|error| io_fault("inspect destination parent", error))?
        .enumerate()
    {
        if count >= PARENT_ENTRIES {
            return Err(Fault::new(
                "AuthoringPath",
                "destination parent exceeds the enumeration bound",
            ));
        }
        let entry = entry.map_err(|error| io_fault("inspect destination entry", error))?;
        if let Some(existing) = entry.file_name().to_str() {
            if existing != name && filesystem_key(existing) == key {
                return Err(Fault::new(
                    "AuthoringPath",
                    "destination aliases an existing case-folded path",
                ));
            }
        }
    }
    Ok(())
}

fn checked_destination(root: &Path, path: &str, create: bool) -> Result<PathBuf, Fault> {
    PackageDraft::check_path(path)?;
    PackageDraft::canonical_root(root)?;
    let mut current = root.to_owned();
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        if exists(&current)? {
            check_alias(&current, component)?;
        }
        current.push(component);
        if components.peek().is_none() {
            break;
        }
        if exists(&current)? {
            let metadata = fs::symlink_metadata(&current)
                .map_err(|error| io_fault("inspect package directory", error))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(stale());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(stale());
                }
            }
        } else if create {
            create_directory(&current)?;
            sync_directory(current.parent().ok_or_else(stale)?)?;
        }
    }
    Ok(current)
}

fn read_package_file(path: &Path) -> Result<Option<Vec<u8>>, Fault> {
    if !exists(path)? {
        return Ok(None);
    }
    let before =
        fs::symlink_metadata(path).map_err(|error| io_fault("inspect publication file", error))?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > MAX_BYTES as u64 {
        return Err(stale());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.nlink() != 1 {
            return Err(stale());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if before.file_attributes() & 0x400 != 0 {
            return Err(stale());
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| io_fault("open publication file", error))?;
    let opened = file
        .metadata()
        .map_err(|error| io_fault("inspect opened publication file", error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != opened.dev() || before.ino() != opened.ino() || opened.nlink() != 1 {
            return Err(stale());
        }
    }
    if !opened.is_file() {
        return Err(stale());
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_fault("read publication file", error))?;
    if bytes.len() != before.len() as usize {
        return Err(stale());
    }
    Ok(Some(bytes))
}

fn check_scratch(path: &Path, count: usize) -> Result<(), Fault> {
    check_directory(path)?;
    for (index, entry) in fs::read_dir(path)
        .map_err(|error| io_fault("inspect publication stage", error))?
        .enumerate()
    {
        if index >= count {
            return Err(stale());
        }
        let entry = entry.map_err(|error| io_fault("inspect staged entry", error))?;
        let name = entry.file_name();
        let number = name
            .to_str()
            .and_then(|name| name.strip_prefix("next-"))
            .and_then(|number| number.parse::<usize>().ok())
            .filter(|number| *number < count)
            .ok_or_else(stale)?;
        if name != format!("next-{number}").as_str() {
            return Err(stale());
        }
        crate::storage::checked_file(&entry.path(), MAX_BYTES)?;
    }
    Ok(())
}
