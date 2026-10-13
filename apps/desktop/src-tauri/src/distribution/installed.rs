//! Installed releases are the original signed archives at
//! `<data>/pkgs/<key_id>/<package_id>/<artifact_id>.mmpkg`, where `key_id` is the
//! SHA-256 of the signing public key and `artifact_id` the SHA-256 of the exact archive
//! bytes. Nothing is extracted and no host receipt is kept: every consumer rereads the
//! archive, rechecks its digest, signer folder and package, and decodes it through the
//! same verified boundary as an import review. Nothing is cached.
//!
//! The persisted reference names only the package and artifact, so a read finds the
//! signer folder by a bounded scan; the artifact digest fixes the signer, so more than
//! one location for an artifact is malformed and refuses rather than choosing one.
//! Expanded containers of the superseded `pkgs/<package_id>/<artifact_id>/` layout
//! are never read, rebuilt or removed; they are recognized only so authoring and
//! ordinary selection refuse them, and their references report a missing archive
//! until the original `.mmpkg` is imported again.
//!
//! Imports into one data root serialize on an exclusive OS file lock held on
//! `pkgs/.import.lock` from the conflict check through publication, so concurrent
//! importers using this module, in any process, cannot both install different
//! artifacts for one package release. The lock is released when its handle closes,
//! including after a crash. None of this protects against direct filesystem edits.

use super::archive::{INDEX, SIGNATURE};
use super::format;
use super::{
    Hook, PreparedPackage, Publication, ReleaseFacts, Stage, cancelled, io_fault, limit_fault,
    note, read_archive, verified_release, verify_staged, write_staged,
};
use crate::configuration::{create_private_file, publish_no_replace, sync_directory, temporary};
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
const ARCHIVE: &str = ".mmpkg";
const LOCK: &str = ".import.lock";
const STAGING: &str = "install";
/// Members of a superseded expanded container, recognized only for refusal.
const LEGACY_MEMBERS: [&str; 4] = ["package", INDEX, SIGNATURE, "receipt.json"];
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

    fn file_name(&self) -> String {
        format!("{}{ARCHIVE}", self.artifact_id)
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

    /// No archive exists for this reference; only the original `.mmpkg` restores it.
    fn missing(&self) -> Fault {
        let mut fault = self.fault(
            "InstalledMissing",
            "the installed release archive is missing; import its original .mmpkg again",
        );
        note(&mut fault, "reimport_required", json!(true));
        fault
    }
}

/// One installed archive verified on this call: its decoded payload capture and facts.
#[derive(Clone, Debug)]
pub struct VerifiedInstalled {
    identity: InstalledIdentity,
    draft: PackageDraft,
    archive_path: PathBuf,
    facts: ReleaseFacts,
}

impl VerifiedInstalled {
    pub fn identity(&self) -> &InstalledIdentity {
        &self.identity
    }
    /// The verified payload bytes; consumers build their inventory from this capture.
    pub fn draft(&self) -> &PackageDraft {
        &self.draft
    }
    /// The installed `.mmpkg` file these facts were read from; never a payload folder.
    pub fn archive_path(&self) -> &Path {
        &self.archive_path
    }
    pub fn facts(&self) -> &ReleaseFacts {
        &self.facts
    }
}

/// One listed artifact; exactly one of `facts` and `fault` is present.
#[derive(Clone, Debug, Serialize)]
pub struct InstalledRelease {
    pub package_id: String,
    pub artifact_id: String,
    pub facts: Option<ReleaseFacts>,
    pub fault: Option<Fault>,
}

/// Where publication installs these verified facts: the canonical data root's
/// `pkgs/<key_id>/<package_id>/<artifact_id>.mmpkg`.
pub fn installed_destination(root: &Path, facts: &ReleaseFacts) -> Result<PathBuf, Fault> {
    let (packages, identity) = layout(root, facts)?;
    Ok(packages
        .join(&facts.key_id)
        .join(&identity.package_id)
        .join(identity.file_name()))
}

/// Rereads, rehashes and decodes the one installed archive of this reference.
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
    let packages = root.join(PACKAGES);
    let keys = signers(&packages, &identity)?;
    verify_unique(&packages, &identity, &keys)
}

/// Metadata-only lookup for diagnosis. Presence is not archive verification.
pub(crate) fn observe_installed(
    root: &Path,
    package_id: &str,
    artifact_id: &str,
) -> Result<(), Fault> {
    let identity = InstalledIdentity::checked(package_id, artifact_id)?;
    let packages = root.join(PACKAGES);
    if !crate::storage::exists(&packages)? {
        return Err(identity.missing());
    }
    crate::storage::check_directory(&packages).map_err(|cause| {
        identity.caused("InstalledIntegrity", "installed releases are unavailable", cause)
    })?;
    let mut found = false;
    for entry in listing(&packages)? {
        let name = entry.file_name();
        let Some(key) = name.to_str().filter(|key| format::is_digest(key)) else {
            continue;
        };
        let signer = packages.join(key);
        if entry.file_type().map_err(|error| io_fault("inspect signer location", &signer, &error))?.is_file() {
            continue;
        }
        crate::storage::check_directory(&signer).map_err(|cause| {
            identity.caused("InstalledIntegrity", "installed signer location is unsafe", cause)
        })?;
        let package = signer.join(package_id);
        if !crate::storage::exists(&package)? {
            continue;
        }
        crate::storage::check_directory(&package).map_err(|cause| {
            identity.caused("InstalledIntegrity", "installed package location is unsafe", cause)
        })?;
        let archive = package.join(identity.file_name());
        if !crate::storage::exists(&archive)? {
            continue;
        }
        if found {
            return Err(identity.fault("InstalledIntegrity", "installed artifact has ambiguous locations"));
        }
        crate::storage::checked_file(&archive, super::MAX_ARCHIVE_BYTES).map_err(|cause| {
            identity.caused("InstalledIntegrity", "installed archive is unsafe", cause)
        })?;
        found = true;
    }
    if !found {
        return Err(identity.missing());
    }
    Ok(())
}

/// Identifies an installed archive, or a superseded expanded container, from its
/// location: the archive itself and any path beneath its name, or the container and
/// any descendant. Valid or not, such paths are never ordinary packages. Other paths
/// under `pkgs`, including the signer and package folders, and unresolvable paths are
/// not managed.
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
    let mut parts = relative.components().map(|part| match part {
        Component::Normal(name) => name.to_str(),
        _ => None,
    });
    let (first, second, third) = (parts.next().flatten(), parts.next().flatten(), parts.next());
    let (Some(first), Some(second)) = (first, second) else {
        return Ok(None);
    };
    if format::is_digest(first)
        && let Some(Some(name)) = third
        && let Some(artifact_id) = archive_digest(name)
        && let Ok(identity) = InstalledIdentity::checked(second, artifact_id)
        && fs::symlink_metadata(packages.join(first).join(second).join(name)).is_ok()
    {
        return Ok(Some(identity));
    }
    let Ok(identity) = InstalledIdentity::checked(first, second) else {
        return Ok(None);
    };
    let container = packages.join(first).join(second);
    if fs::symlink_metadata(&container).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        for member in LEGACY_MEMBERS {
            if crate::storage::exists(&container.join(member))? {
                return Ok(Some(identity));
            }
        }
    }
    Ok(None)
}

/// A bounded filesystem lookup for explicit selection. Each found artifact is listed
/// once and resolved exactly as [`verify_installed`] would, so an artifact found in
/// more than one signer folder carries one fault instead of a duplicate row.
pub fn installed_releases(root: &Path) -> Result<Vec<InstalledRelease>, Fault> {
    let packages = root.join(PACKAGES);
    match fs::symlink_metadata(&packages) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Ok(Vec::new()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_fault("inspect installed releases", &packages, &error)),
    }
    let mut found = BTreeSet::new();
    catalog(&packages, |package_id, artifact_id| {
        found.insert(InstalledIdentity {
            package_id: package_id.to_owned(),
            artifact_id: artifact_id.to_owned(),
        });
    })?;
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

/// Publishes the prepared archive bytes as one file without replacing anything; an
/// identical verified installation is reported unchanged.
pub(super) fn publish(
    root: &Path,
    prepared: &PreparedPackage,
    cancel: &AtomicBool,
    hook: Hook<'_>,
) -> Result<Publication, Fault> {
    cancelled(cancel)?;
    let facts = prepared.facts();
    let (packages, identity) = layout(root, facts)?;
    let signer = packages.join(&facts.key_id);
    let package = signer.join(&identity.package_id);
    let destination = package.join(identity.file_name());
    owned_directory(&packages)?;
    // A legacy package rooted at `pkgs` itself would absorb installed releases.
    if crate::storage::exists(&packages.join("package.json"))? {
        return Err(occupied(&packages));
    }
    let _lock = lock(&packages)?;
    let publication = |unchanged, cleanup_error| Publication {
        path: destination.to_string_lossy().into_owned(),
        package_id: facts.package_id.clone(),
        release_version: facts.release_version.clone(),
        artifact_id: facts.artifact_id.clone(),
        unchanged,
        committed: !unchanged,
        cleanup_error,
    };
    let holders = signers(&packages, &identity)?;
    if let Some(other) = holders.iter().find(|key| **key != facts.key_id) {
        let mut fault = conflict(
            facts,
            &identity.artifact_id,
            "this artifact already appears in another signer folder; it was preserved",
        );
        note(&mut fault, "installed_key_id", json!(other));
        return Err(fault);
    }
    if !holders.is_empty() {
        verify_at(&packages, &facts.key_id, &identity).map_err(|cause| {
            identity.caused(
                "InstalledIntegrity",
                "the existing installation of this artifact no longer verifies; it was preserved",
                cause,
            )
        })?;
        return Ok(publication(true, None));
    }
    // A new artifact must keep the catalog listable, counted exactly as listing does.
    if catalog(&packages, |_, _| {})? >= MAX_INSTALLED {
        return Err(identity.fault(
            "PackageLimit",
            "256 releases are already installed; this release was not installed",
        ));
    }
    for (key, artifact_id) in releases(&packages, &identity.package_id)? {
        let other = InstalledIdentity {
            package_id: identity.package_id.clone(),
            artifact_id,
        };
        let installed = verify_at(&packages, &key, &other).map_err(|cause| {
            let mut fault = conflict(
                facts,
                &other.artifact_id,
                "an installed release of this package cannot be verified; resolve it before importing another",
            );
            note(&mut fault, "cause", json!(cause));
            fault
        })?;
        if installed.facts.release_version == facts.release_version {
            return Err(conflict(
                facts,
                &other.artifact_id,
                "a different artifact is already installed for this package release; it was preserved",
            ));
        }
    }
    cancelled(cancel)?;
    owned_directory(&signer)?;
    owned_directory(&package)?;
    let staging = temporary(&package, STAGING);
    let file = create_private_file(&staging)?;
    let staged = (|| -> Result<(), Fault> {
        write_staged(file, &prepared.archive, &staging, cancel)?;
        verify_staged(&staging, &prepared.archive)?;
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
        // Only this call's staging file is removed; a failure retains it as evidence.
        if let Err(error) = fs::remove_file(&staging) {
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
        .and_then(|()| sync_directory(&signer))
        .and_then(|()| sync_directory(&packages))
        .err();
    Ok(publication(false, cleanup_error))
}

/// The canonical `pkgs` folder and validated reference shared by review and publication.
fn layout(root: &Path, facts: &ReleaseFacts) -> Result<(PathBuf, InstalledIdentity), Fault> {
    let identity = InstalledIdentity::checked(&facts.package_id, &facts.artifact_id)?;
    if !format::is_digest(&facts.key_id) {
        return Err(identity.fault(
            "InstalledReference",
            "installed release signing key fingerprint is malformed",
        ));
    }
    let root = root
        .canonicalize()
        .map_err(|error| io_fault("resolve the data root", root, &error))?;
    Ok((root.join(PACKAGES), identity))
}

/// Visits every recognized archive entry below the signer and package folders, before
/// de-duplication, and returns their number; more than [`MAX_INSTALLED`] refuses.
/// Listing and publication share this count, so publication never installs a release
/// listing would refuse.
fn catalog(packages: &Path, mut visit: impl FnMut(&str, &str)) -> Result<usize, Fault> {
    let mut count = 0;
    for signer in listing(packages)? {
        let Some(_) = directory_name(&signer, packages)?.filter(|key| format::is_digest(key))
        else {
            continue;
        };
        for package in listing(&signer.path())? {
            let Some(package_id) = directory_name(&package, packages)?
                .filter(|package_id| PackageDraft::check_id(package_id).is_ok())
            else {
                continue;
            };
            for archive in listing(&package.path())? {
                let name = archive.file_name();
                let Some(artifact_id) = name.to_str().and_then(archive_digest) else {
                    continue;
                };
                count += 1;
                if count > MAX_INSTALLED {
                    return Err(limit_fault("more than 256 installed releases"));
                }
                visit(&package_id, artifact_id);
            }
        }
    }
    Ok(count)
}

/// Signer folders holding an entry, of any file type, named for this artifact.
fn signers(packages: &Path, identity: &InstalledIdentity) -> Result<Vec<String>, Fault> {
    match fs::symlink_metadata(packages) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(identity.fault(
                "InstalledIntegrity",
                "installed release location is not a real directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(identity.missing()),
        Err(error) => {
            return Err(identity.caused(
                "InstalledIntegrity",
                "installed releases are unavailable",
                io_fault("inspect installed releases", packages, &error),
            ));
        }
    }
    let entries = listing(packages).map_err(|cause| {
        identity.caused(
            "InstalledIntegrity",
            "installed releases cannot be listed",
            cause,
        )
    })?;
    let mut keys = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        let Some(key) = name.to_str().filter(|key| format::is_digest(key)) else {
            continue;
        };
        let path = packages
            .join(key)
            .join(&identity.package_id)
            .join(identity.file_name());
        match fs::symlink_metadata(&path) {
            Ok(_) => keys.push(key.to_owned()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => {
                return Err(identity.caused(
                    "InstalledIntegrity",
                    "installed release is unavailable",
                    io_fault("inspect the installed release", &path, &error),
                ));
            }
        }
    }
    keys.sort();
    Ok(keys)
}

/// Exactly one location verifies; none is missing and several are ambiguous.
fn verify_unique(
    packages: &Path,
    identity: &InstalledIdentity,
    keys: &[String],
) -> Result<VerifiedInstalled, Fault> {
    match keys {
        [] => Err(identity.missing()),
        [key] => verify_at(packages, key, identity),
        _ => {
            let mut fault = identity.fault(
                "InstalledIntegrity",
                "this installed artifact appears in more than one signer folder; none was used",
            );
            note(&mut fault, "key_ids", json!(keys));
            Err(fault)
        }
    }
}

/// Reads one archive once, then checks its exact digest, signed envelope and payload,
/// signer folder and package against the reference.
fn verify_at(
    packages: &Path,
    key: &str,
    identity: &InstalledIdentity,
) -> Result<VerifiedInstalled, Fault> {
    let integrity = |message: &str| {
        let mut fault = identity.fault("InstalledIntegrity", message);
        note(&mut fault, "key_id", json!(key));
        fault
    };
    let caused = |message: &str, cause: Fault| {
        let mut fault = integrity(message);
        note(&mut fault, "cause", json!(cause));
        fault
    };
    let signer = packages.join(key);
    let package = signer.join(&identity.package_id);
    for directory in [&signer, &package] {
        if !fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            return Err(integrity(
                "installed release location is not a real directory",
            ));
        }
    }
    let archive_path = package.join(identity.file_name());
    match fs::symlink_metadata(&archive_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(integrity("installed archive is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(identity.missing()),
        Err(error) => {
            return Err(caused(
                "installed archive is unavailable",
                io_fault("inspect the installed archive", &archive_path, &error),
            ));
        }
    }
    // Installed reads have no cancellation of their own.
    let uncancelled = AtomicBool::new(false);
    let archive = read_installed(&archive_path, &uncancelled)
        .map_err(|cause| caused("installed archive cannot be read", cause))?;
    if format::sha256_hex(&archive) != identity.artifact_id {
        return Err(integrity(
            "installed archive bytes differ from the artifact digest",
        ));
    }
    let (draft, facts) = verified_release(&archive, identity.artifact_id.clone(), &uncancelled)
        .map_err(|cause| caused("installed archive no longer verifies", cause))?;
    drop(archive);
    if facts.key_id != key {
        return Err(integrity(
            "installed archive is signed by another key than its signer folder",
        ));
    }
    if facts.package_id != identity.package_id {
        return Err(integrity(
            "installed archive names another package than its reference",
        ));
    }
    Ok(VerifiedInstalled {
        identity: identity.clone(),
        draft,
        archive_path,
        facts,
    })
}

/// A private regular file with one link, opened without following a final link.
fn read_installed(path: &Path, cancel: &AtomicBool) -> Result<super::archive::Bytes, Fault> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| io_fault("open the installed archive", path, &error))?;
    let (bytes, metadata) = read_archive(path, file, cancel)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
            return Err(Fault::new(
                "PackageStorage",
                "the installed archive must be private and have no hard links",
            )
            .with_context(json!({"path":path.to_string_lossy()})));
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(bytes)
}

/// Every installed artifact of `package_id`, by signer folder. Any entry the import
/// cannot classify refuses it rather than being adopted or ignored.
fn releases(packages: &Path, package_id: &str) -> Result<Vec<(String, String)>, Fault> {
    let mut found = Vec::new();
    for signer in listing(packages)? {
        let Some(key) = directory_name(&signer, packages)?.filter(|key| format::is_digest(key))
        else {
            continue;
        };
        let package = signer.path().join(package_id);
        match fs::symlink_metadata(&package) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(occupied(&package)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_fault("inspect installed releases", &package, &error)),
        }
        for artifact_id in archives(&package)? {
            if found.len() >= MAX_INSTALLED {
                return Err(limit_fault("more than 256 installed releases"));
            }
            found.push((key.clone(), artifact_id));
        }
    }
    Ok(found)
}

/// Archive digests in one package folder; any other entry is ordinary content the
/// import must not adopt. Another call's hidden staging file is tolerated.
fn archives(package: &Path) -> Result<Vec<String>, Fault> {
    let staging = format!(".{STAGING}-");
    let mut found = Vec::new();
    for entry in listing(package)? {
        let file = entry
            .file_type()
            .map_err(|error| io_fault("inspect installed releases", &entry.path(), &error))?
            .is_file();
        let name = entry.file_name();
        let name = name.to_str().filter(|_| file);
        if let Some(artifact_id) = name.and_then(archive_digest) {
            found.push(artifact_id.to_owned());
        } else if !name.is_some_and(|name| name.starts_with(&staging)) {
            return Err(occupied(package));
        }
    }
    Ok(found)
}

fn archive_digest(name: &str) -> Option<&str> {
    name.strip_suffix(ARCHIVE)
        .filter(|digest| format::is_digest(digest))
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

fn missing_or_integrity(error: &io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::NotFound {
        "InstalledMissing"
    } else {
        "InstalledIntegrity"
    }
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

/// Canonicalizes the longest existing ancestor and appends the missing remainder; a
/// remainder below a regular file, such as an installed archive, counts as missing.
fn resolve(path: &Path) -> Option<PathBuf> {
    let mut existing = std::path::absolute(path).ok()?;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut resolved) => {
                resolved.extend(missing.iter().rev());
                return Some(resolved);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                missing.push(existing.file_name()?.to_owned());
                if !existing.pop() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}
