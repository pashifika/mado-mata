//! Installed releases at `<data>/pkgs/<package_id>/<artifact_id>/`: the exact signed
//! `index.json` and `signature.json`, a bounded host `receipt.json` and the
//! `package/` payload. Every consumer reverifies the container; nothing is cached.
//!
//! Imports into one data root serialize on an exclusive OS file lock held on
//! `pkgs/.import.lock` from the conflict check through publication, so concurrent
//! importers using this module, in any process, cannot both install different
//! artifacts for one package release. The lock is released when its handle closes,
//! including after a crash. None of this protects against direct filesystem edits.

use super::archive::{INDEX, SIGNATURE};
use super::format::{self, Envelope};
use super::{
    Hook, MAX_SIGNATURE_BYTES, PreparedPackage, Publication, ReleaseFacts, Stage, cancelled,
    integrity_fault, io_fault, limit_fault, matches_index, note,
};
use crate::author_signing::MAX_INDEX_BYTES;
use crate::configuration::{
    create_private_directory, publish_no_replace, sync_directory, temporary, write_private,
};
use mado_runtime_comparison::inventory::{PackageDraft, is_os_metadata_entry};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions, TryLockError};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

const PACKAGES: &str = "pkgs";
const PAYLOAD: &str = "package";
const RECEIPT: &str = "receipt.json";
const LOCK: &str = ".import.lock";
const STAGING: &str = "install";
const MEMBERS: [&str; 4] = [PAYLOAD, INDEX, SIGNATURE, RECEIPT];
const RECEIPT_VERSION: u64 = 1;
const MAX_RECEIPT_BYTES: usize = 4 * 1024;
const MAX_INSTALLED: usize = 256;
const MAX_ENTRIES: usize = 4096;

/// The persisted reference to one installed artifact.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstalledIdentity {
    pub package_id: String,
    pub artifact_id: String,
}

impl InstalledIdentity {
    fn checked(package_id: &str, artifact_id: &str) -> Result<Self, Fault> {
        if PackageDraft::check_id(package_id).is_err() || !format::is_digest(artifact_id) {
            return Err(Fault::new(
                "InstalledReference",
                "installed release reference is malformed",
            ));
        }
        Ok(Self {
            package_id: package_id.to_owned(),
            artifact_id: artifact_id.to_owned(),
        })
    }

    fn fault(&self, category: &str, message: &str) -> Fault {
        Fault::new(category, message).with_context(json!({
            "package_id":self.package_id,
            "artifact_id":self.artifact_id
        }))
    }

    fn caused(&self, category: &str, message: &str, cause: Fault) -> Fault {
        let mut fault = self.fault(category, message);
        note(&mut fault, "cause", json!(cause));
        fault
    }
}

/// One installation verified on this call: its exact payload capture and facts.
#[derive(Clone, Debug)]
pub struct VerifiedInstalled {
    identity: InstalledIdentity,
    draft: PackageDraft,
    payload_path: PathBuf,
    facts: ReleaseFacts,
    envelope: Envelope,
}

impl VerifiedInstalled {
    pub fn identity(&self) -> &InstalledIdentity {
        &self.identity
    }
    /// The verified payload bytes; consumers build their inventory from this capture.
    pub fn draft(&self) -> &PackageDraft {
        &self.draft
    }
    pub fn payload_path(&self) -> &Path {
        &self.payload_path
    }
    pub fn facts(&self) -> &ReleaseFacts {
        &self.facts
    }
}

/// One listed container; exactly one of `facts` and `fault` is present.
#[derive(Clone, Debug, Serialize)]
pub struct InstalledRelease {
    pub package_id: String,
    pub artifact_id: String,
    pub facts: Option<ReleaseFacts>,
    pub fault: Option<Fault>,
}

#[derive(Serialize)]
struct ReceiptDocument<'a> {
    version: u64,
    artifact_id: &'a str,
    package_id: &'a str,
    release_version: &'a str,
    key_id: &'a str,
    index_sha256: String,
}

struct Receipt {
    artifact_id: String,
    package_id: String,
    release_version: String,
    key_id: String,
    index_sha256: String,
}

/// Reverifies receipt, signed envelope and payload bytes of one installed release.
pub fn verify_installed(
    root: &Path,
    package_id: &str,
    artifact_id: &str,
) -> Result<VerifiedInstalled, Fault> {
    let identity = InstalledIdentity::checked(package_id, artifact_id)?;
    let root = root.canonicalize().map_err(|error| {
        identity.caused(
            missing_or_integrity(&error),
            "the data root of the installed release is unavailable",
            io_fault("resolve the data root", root, &error),
        )
    })?;
    let mut container = root.join(PACKAGES);
    for component in [identity.package_id.as_str(), identity.artifact_id.as_str()] {
        real_directory(&container, &identity)?;
        container.push(component);
    }
    real_directory(&container, &identity)?;
    verify_container(&container, &identity)
}

/// Identifies a managed container from its location and any host-owned member, valid
/// or not, for the container itself, its payload or any descendant. Ordinary
/// directories under `pkgs` and unresolvable paths are not managed.
pub fn managed_identity(root: &Path, path: &Path) -> Result<Option<InstalledIdentity>, Fault> {
    let Ok(packages) = root.join(PACKAGES).canonicalize() else {
        return Ok(None);
    };
    let Some(resolved) = resolve(path) else {
        return Ok(None);
    };
    let Ok(relative) = resolved.strip_prefix(&packages) else {
        return Ok(None);
    };
    let mut parts = relative.components();
    let (Some(Component::Normal(package_id)), Some(Component::Normal(artifact_id))) =
        (parts.next(), parts.next())
    else {
        return Ok(None);
    };
    let (Some(package_id), Some(artifact_id)) = (package_id.to_str(), artifact_id.to_str()) else {
        return Ok(None);
    };
    let Ok(identity) = InstalledIdentity::checked(package_id, artifact_id) else {
        return Ok(None);
    };
    let container = packages.join(package_id).join(artifact_id);
    if !fs::symlink_metadata(&container).is_ok_and(|metadata| metadata.file_type().is_dir())
        || !has_member(&container)?
    {
        return Ok(None);
    }
    Ok(Some(identity))
}

/// A bounded filesystem lookup for explicit selection, reverifying every container.
pub fn installed_releases(root: &Path) -> Result<Vec<InstalledRelease>, Fault> {
    let packages = root.join(PACKAGES);
    match fs::symlink_metadata(&packages) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Ok(Vec::new()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_fault("inspect installed releases", &packages, &error)),
    }
    let mut found = Vec::new();
    for package in listing(&packages)? {
        let Some(package_id) = directory_name(&package, &packages)? else {
            continue;
        };
        if PackageDraft::check_id(&package_id).is_err() {
            continue;
        }
        for container in listing(&package.path())? {
            let Some(artifact_id) = directory_name(&container, &packages)? else {
                continue;
            };
            if !format::is_digest(&artifact_id) || !has_member(&container.path())? {
                continue;
            }
            if found.len() >= MAX_INSTALLED {
                return Err(limit_fault("more than 256 installed releases"));
            }
            found.push(InstalledIdentity {
                package_id: package_id.clone(),
                artifact_id,
            });
        }
    }
    found.sort();
    Ok(found
        .into_iter()
        .map(|identity| {
            let verified = verify_installed(root, &identity.package_id, &identity.artifact_id);
            let (facts, fault) = match verified {
                Ok(verified) => (Some(verified.facts), None),
                Err(fault) => (None, Some(fault)),
            };
            InstalledRelease {
                package_id: identity.package_id,
                artifact_id: identity.artifact_id,
                facts,
                fault,
            }
        })
        .collect())
}

pub(super) fn publish(
    root: &Path,
    prepared: &PreparedPackage,
    cancel: &AtomicBool,
    hook: Hook<'_>,
) -> Result<Publication, Fault> {
    cancelled(cancel)?;
    let facts = prepared.facts();
    let identity = InstalledIdentity::checked(&facts.package_id, &facts.artifact_id)?;
    let root = root
        .canonicalize()
        .map_err(|error| io_fault("resolve the data root", root, &error))?;
    let packages = root.join(PACKAGES);
    owned_directory(&packages)?;
    // A legacy package rooted at `pkgs` itself would absorb installed containers.
    if crate::storage::exists(&packages.join("package.json"))? {
        return Err(occupied(&packages));
    }
    let _lock = lock(&packages)?;
    let package = packages.join(&identity.package_id);
    owned_directory(&package)?;
    let destination = package.join(&identity.artifact_id);
    let installed = containers(&package)?;
    let publication = |unchanged, cleanup_error| Publication {
        path: destination.to_string_lossy().into_owned(),
        package_id: facts.package_id.clone(),
        release_version: facts.release_version.clone(),
        artifact_id: facts.artifact_id.clone(),
        unchanged,
        committed: !unchanged,
        cleanup_error,
    };
    if installed.contains(&identity.artifact_id) {
        let existing = verify_container(&destination, &identity).map_err(|cause| {
            identity.caused(
                "InstalledIntegrity",
                "the existing installation of this artifact no longer verifies; it was preserved",
                cause,
            )
        })?;
        same_artifact(&existing, prepared).map_err(|cause| {
            identity.caused(
                "InstalledIntegrity",
                "the existing installation differs from this artifact; it was preserved",
                cause,
            )
        })?;
        return Ok(publication(true, None));
    }
    for other in &installed {
        let receipt = read_receipt(&package.join(other)).map_err(|cause| {
            let mut fault = conflict(
                facts,
                other,
                "an installed release of this package cannot be read; resolve it before importing another",
            );
            note(&mut fault, "cause", json!(cause));
            fault
        })?;
        if receipt.release_version == facts.release_version {
            return Err(conflict(
                facts,
                other,
                "a different artifact is already installed for this package release; it was preserved",
            ));
        }
    }
    cancelled(cancel)?;
    let staging = temporary(&package, STAGING);
    create_private_directory(&staging)?;
    let staged = (|| -> Result<(), Fault> {
        write_container(&staging, prepared, cancel)?;
        verify_container(&staging, &identity)
            .and_then(|verified| same_artifact(&verified, prepared))
            .map_err(|cause| {
                identity.caused(
                    "PackageIntegrity",
                    "the staged installation does not verify as the reviewed artifact",
                    cause,
                )
            })?;
        hook(Stage::Staged, &staging)?;
        cancelled(cancel)?;
        hook(Stage::Publish, &destination)?;
        publish_no_replace(&staging, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                identity.fault(
                    "PackageConflict",
                    "another installation appeared at the destination; it was preserved",
                )
            } else {
                io_fault(
                    "publish the installed release without replacement",
                    &destination,
                    &error,
                )
            }
        })
    })();
    if let Err(mut fault) = staged {
        // Only this call's staging is removed; a failure retains it as evidence.
        if let Err(error) = fs::remove_dir_all(&staging) {
            note(
                &mut fault,
                "cleanup_error",
                json!(io_fault("remove import staging", &staging, &error)),
            );
        }
        return Err(fault);
    }
    let cleanup_error = hook(Stage::Committed, &destination)
        .and_then(|()| sync_directory(&package))
        .err();
    Ok(publication(false, cleanup_error))
}

fn conflict(facts: &ReleaseFacts, installed: &str, message: &str) -> Fault {
    Fault::new("PackageConflict", message).with_context(json!({
        "package_id":facts.package_id,
        "release_version":facts.release_version,
        "artifact_id":facts.artifact_id,
        "installed_artifact_id":installed
    }))
}

fn occupied(path: &Path) -> Fault {
    Fault::new(
        "PackageConflict",
        "an ordinary package, file or link occupies the installed-release location; it was preserved",
    )
    .with_context(json!({"path":path.to_string_lossy()}))
}

/// Envelope bytes and payload revision must equal the prepared capture.
fn same_artifact(verified: &VerifiedInstalled, prepared: &PreparedPackage) -> Result<(), Fault> {
    if verified.envelope.index != prepared.index
        || verified.envelope.signature != prepared.signature
        || verified.draft.revision() != prepared.draft.revision()
    {
        return Err(integrity_fault(
            "installed envelope or payload differs from the artifact",
        ));
    }
    Ok(())
}

fn verify_container(
    container: &Path,
    identity: &InstalledIdentity,
) -> Result<VerifiedInstalled, Fault> {
    let integrity = |message: &str| identity.fault("InstalledIntegrity", message);
    let caused =
        |message: &str, cause: Fault| identity.caused("InstalledIntegrity", message, cause);
    crate::storage::check_directory(container)
        .map_err(|cause| caused("installed release is not a private real directory", cause))?;
    let mut names = BTreeSet::new();
    for entry in listing(container)? {
        let name = entry.file_name();
        let name = name
            .to_str()
            .filter(|name| MEMBERS.contains(name))
            .ok_or_else(|| integrity("installed release has unexpected entries"))?;
        names.insert(name.to_owned());
    }
    if let Some(member) = MEMBERS.iter().find(|member| !names.contains(**member)) {
        let mut fault = identity.fault("InstalledMissing", "installed release member is missing");
        note(&mut fault, "member", json!(member));
        return Err(fault);
    }
    let receipt = read_receipt(container)
        .map_err(|cause| caused("installed receipt is unreadable or malformed", cause))?;
    if receipt.artifact_id != identity.artifact_id || receipt.package_id != identity.package_id {
        return Err(integrity("installed receipt does not match its reference"));
    }
    let read = |name: &str, maximum: usize| {
        crate::storage::read_bytes(&container.join(name), maximum)
            .map_err(|cause| caused("installed signed envelope is unreadable", cause))
    };
    let index = read(INDEX, MAX_INDEX_BYTES)?;
    let signature = read(SIGNATURE, MAX_SIGNATURE_BYTES)?;
    let index_sha256 = format::sha256_hex(&index);
    let envelope = format::verify(index, signature)
        .map_err(|cause| caused("installed signed envelope no longer verifies", cause))?;
    if receipt.key_id != envelope.key_id || receipt.index_sha256 != index_sha256 {
        return Err(integrity(
            "installed receipt does not match its signed envelope",
        ));
    }
    let payload_path = container.join(PAYLOAD);
    let draft = PackageDraft::capture(&payload_path, &crate::authoring::limits()?)
        .map_err(|cause| caused("installed payload cannot be captured", cause))?;
    matches_index(draft.files(), &envelope.files)
        .map_err(|cause| caused("installed payload differs from its signed index", cause))?;
    let facts = ReleaseFacts::new(&draft, &envelope.key_id, identity.artifact_id.clone())
        .map_err(|cause| caused("installed payload lacks release metadata", cause))?;
    if facts.package_id != identity.package_id || facts.release_version != receipt.release_version {
        return Err(integrity(
            "installed receipt does not match its signed release",
        ));
    }
    Ok(VerifiedInstalled {
        identity: identity.clone(),
        draft,
        payload_path,
        facts,
        envelope,
    })
}

fn write_container(
    staging: &Path,
    prepared: &PreparedPackage,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    let payload = staging.join(PAYLOAD);
    create_private_directory(&payload)?;
    let mut directories = vec![payload.clone()];
    for (path, bytes) in prepared.draft().files() {
        cancelled(cancel)?;
        let mut target = payload.clone();
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            target.push(component);
            // Paths are portable and case-unique, so a missing parent is ours to create.
            if components.peek().is_some() && !directories.contains(&target) {
                create_private_directory(&target)?;
                directories.push(target.clone());
            }
        }
        write_private(&target, bytes)?;
    }
    let facts = prepared.facts();
    let receipt = crate::storage::encode(
        &ReceiptDocument {
            version: RECEIPT_VERSION,
            artifact_id: &facts.artifact_id,
            package_id: &facts.package_id,
            release_version: &facts.release_version,
            key_id: &facts.key_id,
            index_sha256: format::sha256_hex(&prepared.index),
        },
        MAX_RECEIPT_BYTES,
    )?;
    write_private(&staging.join(INDEX), &prepared.index)?;
    write_private(&staging.join(SIGNATURE), &prepared.signature)?;
    write_private(&staging.join(RECEIPT), &receipt)?;
    for directory in directories.iter().rev() {
        sync_directory(directory)?;
    }
    sync_directory(staging)
}

fn read_receipt(container: &Path) -> Result<Receipt, Fault> {
    let bytes = crate::storage::read_bytes(&container.join(RECEIPT), MAX_RECEIPT_BYTES)?;
    let malformed = |cause: &str| {
        integrity_fault("installed receipt is malformed").with_context(json!({"cause":cause}))
    };
    let document = format::strict_json(&bytes).map_err(|cause| malformed(&cause))?;
    let fields = format::members(
        &document,
        &[
            "artifact_id",
            "index_sha256",
            "key_id",
            "package_id",
            "release_version",
            "version",
        ],
    )
    .map_err(malformed)?;
    if fields["version"].as_u64() != Some(RECEIPT_VERSION) {
        return Err(malformed("unsupported receipt version"));
    }
    let text = |name: &str| {
        fields[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| malformed("receipt members must be strings"))
    };
    Ok(Receipt {
        artifact_id: text("artifact_id")?,
        package_id: text("package_id")?,
        release_version: text("release_version")?,
        key_id: text("key_id")?,
        index_sha256: text("index_sha256")?,
    })
}

/// Exclusive import lock; a link, special file or hard-linked alias refuses.
fn lock(packages: &Path) -> Result<fs::File, Fault> {
    let path = packages.join(LOCK);
    let alias = || {
        Fault::new(
            "PackageStorage",
            "the package import lock must be a private regular file",
        )
        .with_context(json!({"path":path.to_string_lossy()}))
    };
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file() => return Err(alias()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_fault("inspect the package import lock", &path, &error)),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|error| io_fault("open the package import lock", &path, &error))?;
    let metadata = file
        .metadata()
        .map_err(|error| io_fault("inspect the package import lock", &path, &error))?;
    if !metadata.is_file() {
        return Err(alias());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
            return Err(alias());
        }
    }
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Fault::new(
            "PackageBusy",
            "another package import into this data root is in progress",
        )),
        Err(TryLockError::Error(error)) => Err(io_fault("lock package imports", &path, &error)),
    }
}

/// Artifact directories of a package; any other entry is an ordinary directory the
/// import must not adopt. Another call's hidden staging is tolerated.
fn containers(package: &Path) -> Result<Vec<String>, Fault> {
    let mut found = Vec::new();
    for entry in listing(package)? {
        let name = entry.file_name();
        let directory = entry
            .file_type()
            .map_err(|error| io_fault("inspect installed releases", &entry.path(), &error))?
            .is_dir();
        match name.to_str() {
            Some(name) if directory && format::is_digest(name) => found.push(name.to_owned()),
            Some(name) if directory && name.starts_with(&format!(".{STAGING}-")) => {}
            _ => return Err(occupied(package)),
        }
    }
    Ok(found)
}

/// Creates a missing private directory; an existing one must be a real directory.
fn owned_directory(path: &Path) -> Result<(), Fault> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => return Err(occupied(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_fault("inspect installed releases", path, &error)),
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) =>
        {
            Ok(())
        }
        Err(error) => Err(io_fault(
            "create the installed-release folder",
            path,
            &error,
        )),
    }
}

fn real_directory(path: &Path, identity: &InstalledIdentity) -> Result<(), Fault> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(identity.fault(
            "InstalledIntegrity",
            "installed release location is not a real directory",
        )),
        Err(error) => Err(identity.caused(
            missing_or_integrity(&error),
            "installed release is unavailable",
            io_fault("inspect the installed release", path, &error),
        )),
    }
}

fn missing_or_integrity(error: &io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::NotFound {
        "InstalledMissing"
    } else {
        "InstalledIntegrity"
    }
}

fn has_member(container: &Path) -> Result<bool, Fault> {
    for member in MEMBERS {
        if crate::storage::exists(&container.join(member))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn directory_name(entry: &fs::DirEntry, packages: &Path) -> Result<Option<String>, Fault> {
    let directory = entry
        .file_type()
        .map_err(|error| io_fault("inspect installed releases", packages, &error))?
        .is_dir();
    Ok(entry
        .file_name()
        .to_str()
        .filter(|_| directory)
        .map(str::to_owned))
}

/// Bounded directory entries, excluding OS metadata files.
fn listing(directory: &Path) -> Result<Vec<fs::DirEntry>, Fault> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| io_fault("list installed releases", directory, &error))?
    {
        let entry =
            entry.map_err(|error| io_fault("list installed releases", directory, &error))?;
        if is_os_metadata_entry(&entry.file_name(), &entry)
            .map_err(|error| io_fault("inspect installed releases", directory, &error))?
        {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            return Err(limit_fault(
                "installed release directory has too many entries",
            ));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Canonicalizes the longest existing ancestor and appends the missing remainder.
fn resolve(path: &Path) -> Option<PathBuf> {
    let mut existing = std::path::absolute(path).ok()?;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut resolved) => {
                resolved.extend(missing.iter().rev());
                return Some(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(existing.file_name()?.to_owned());
                if !existing.pop() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}
