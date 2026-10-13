//! Signed package distribution: version-1 `.mmpkg` export and import, and verified
//! installed releases.
//!
//! Export signs exactly one captured [`PackageDraft`] and builds its archive once;
//! import captures one selected archive, verifies its structure, exact signed envelope
//! and payload digests, and keeps those exact archive bytes with the decoded payload.
//! Installation publishes those bytes as one file and every installed read decodes
//! the file again through the same verified boundary; nothing is extracted.
//! Signature validity is relative to the included public key; it never verifies an
//! account, publisher or entitlement. Callers own static inventory/compiler
//! validation, review receipts and operation ownership.

mod archive;
mod format;
mod installed;

#[cfg(test)]
mod tests;

pub use installed::{
    InstalledIdentity, InstalledRelease, VerifiedInstalled, installed_destination,
    installed_releases, managed_identity, verify_installed,
};
pub(crate) use installed::observe_installed;

use crate::author_signing::Signer;
use crate::configuration::{create_private_file, publish_no_replace, sync_directory, temporary};
use format::{Envelope, FileFact};
use mado_runtime_comparison::images::{PayloadBytes, reserve_payload};
use mado_runtime_comparison::inventory::PackageDraft;
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whole `.mmpkg` input or output.
const MAX_ARCHIVE_BYTES: usize = 67 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
/// Payload membership, excluding the two envelope members.
const MAX_PAYLOAD_FILES: usize = crate::authoring::MAX_FILES;
const CHUNK: usize = 1024 * 1024;

/// Public facts of one prepared or installed release. `revision` is the captured
/// draft revision and `artifact_id` the SHA-256 of the exact archive bytes; neither
/// is the runtime inventory identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReleaseFacts {
    pub package_id: String,
    pub release_version: String,
    pub name: String,
    pub description: String,
    pub game_names: BTreeMap<String, String>,
    pub key_id: String,
    pub revision: String,
    pub artifact_id: String,
    pub files: usize,
    pub bytes: usize,
}

/// Outcome of a publication call. `committed` means this call made the artifact
/// durable-visible; `unchanged` means an identical verified installation was reused.
/// A committed publication with `cleanup_error` is not a clean success.
#[derive(Clone, Debug, Serialize)]
pub struct Publication {
    pub path: String,
    pub package_id: String,
    pub release_version: String,
    pub artifact_id: String,
    pub unchanged: bool,
    pub committed: bool,
    pub cleanup_error: Option<Fault>,
}

/// One verified capture: the payload draft and the exact archive bytes it came from,
/// built once for an export or captured once for an import.
#[derive(Debug)]
pub struct PreparedPackage {
    draft: PackageDraft,
    facts: ReleaseFacts,
    archive: archive::Bytes,
    /// Only an export is written to an operator-chosen file.
    exported: bool,
}

impl PreparedPackage {
    pub fn draft(&self) -> &PackageDraft {
        &self.draft
    }
    pub fn facts(&self) -> &ReleaseFacts {
        &self.facts
    }
}

impl ReleaseFacts {
    fn new(draft: &PackageDraft, key_id: &str, artifact_id: String) -> Result<Self, Fault> {
        let distribution = draft.distribution().ok_or_else(|| {
            Fault::new(
                "PackageMetadata",
                "signed packages require complete distribution metadata in package.json",
            )
        })?;
        Ok(Self {
            package_id: draft.package_id().to_owned(),
            release_version: distribution.release_version.clone(),
            name: distribution.name.clone(),
            description: distribution.description.clone(),
            game_names: distribution.game.names.clone(),
            key_id: key_id.to_owned(),
            revision: draft.revision().to_owned(),
            artifact_id,
            files: draft.files().len(),
            bytes: draft.files().values().map(|bytes| bytes.len()).sum(),
        })
    }
}

/// Publication points where tests inject real filesystem failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// The complete staging copy exists and verified; cancellation is checked next.
    Staged,
    /// Immediately before the no-replace rename.
    Publish,
    /// After the rename; only finishing work remains.
    Committed,
}

type Hook<'a> = &'a mut dyn FnMut(Stage, &Path) -> Result<(), Fault>;

/// Signs the captured bytes and builds the stored archive once.
pub fn prepare_export(
    draft: PackageDraft,
    signer: &Signer,
    cancel: &AtomicBool,
) -> Result<PreparedPackage, Fault> {
    cancelled(cancel)?;
    if draft.distribution().is_none() {
        return Err(Fault::new(
            "PackageMetadata",
            "export requires complete distribution metadata in package.json",
        ));
    }
    let index = format::encode_index(draft.files())?;
    let signature = signer.sign_index(&index)?;
    let signature = format::encode_signature(&signer.public_key(), signer.key_id(), &signature)?;
    cancelled(cancel)?;
    let archive = archive::write(&index, &signature, draft.files())?;
    let facts = ReleaseFacts::new(&draft, signer.key_id(), format::sha256_hex(&archive))?;
    cancelled(cancel)?;
    Ok(PreparedPackage {
        draft,
        facts,
        archive,
        exported: true,
    })
}

/// Captures the selected archive once and verifies it completely; the exact captured
/// bytes are retained so installation publishes what was reviewed.
pub fn prepare_import(path: &Path, cancel: &AtomicBool) -> Result<PreparedPackage, Fault> {
    cancelled(cancel)?;
    let archive = read_selected(path, cancel)?;
    let artifact_id = format::sha256_hex(&archive);
    let (draft, facts) = verified_release(&archive, artifact_id, cancel)?;
    Ok(PreparedPackage {
        draft,
        facts,
        archive,
        exported: false,
    })
}

/// Writes the prepared archive to private staging beside `destination`, verifies the
/// staged bytes and publishes it without replacing any existing file.
pub fn publish_export(
    prepared: &PreparedPackage,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<Publication, Fault> {
    publish_export_with(prepared, destination, cancel, &mut |_, _| Ok(()))
}

/// Installs the captured archive bytes as the one file named by
/// [`installed_destination`] without replacing anything; identical verified reimport
/// is reported unchanged.
pub fn publish_import(
    root: &Path,
    prepared: &PreparedPackage,
    cancel: &AtomicBool,
) -> Result<Publication, Fault> {
    installed::publish(root, prepared, cancel, &mut |_, _| Ok(()))
}

fn publish_export_with(
    prepared: &PreparedPackage,
    destination: &Path,
    cancel: &AtomicBool,
    hook: Hook<'_>,
) -> Result<Publication, Fault> {
    cancelled(cancel)?;
    if !prepared.exported {
        return Err(Fault::new(
            "PackagePrepared",
            "only a prepared export can be published as an archive",
        ));
    }
    let archive = &*prepared.archive;
    let directory = destination
        .parent()
        .filter(|directory| destination.is_absolute() && !directory.as_os_str().is_empty())
        .filter(|_| destination.file_name().is_some())
        .ok_or_else(|| {
            Fault::new(
                "PackageDestination",
                "export destination must be an absolute file path",
            )
        })?;
    if crate::storage::exists(destination)? {
        return Err(destination_exists(destination));
    }
    let staging = temporary(directory, "mmpkg-export");
    let file = create_private_file(&staging)?;
    let staged = (|| -> Result<(), Fault> {
        write_staged(file, archive, &staging, cancel)?;
        verify_staged(&staging, archive)?;
        hook(Stage::Staged, &staging)?;
        cancelled(cancel)?;
        hook(Stage::Publish, destination)?;
        publish_no_replace(&staging, destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                destination_exists(destination)
            } else {
                io_fault(
                    "publish the package archive without replacement",
                    destination,
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
                json!(io_fault("remove export staging", &staging, &error)),
            );
        }
        return Err(fault);
    }
    let cleanup_error = hook(Stage::Committed, destination)
        .and_then(|()| sync_directory(directory))
        .err();
    let facts = prepared.facts();
    Ok(Publication {
        path: destination.to_string_lossy().into_owned(),
        package_id: facts.package_id.clone(),
        release_version: facts.release_version.clone(),
        artifact_id: facts.artifact_id.clone(),
        unchanged: false,
        committed: true,
        cleanup_error,
    })
}

fn write_staged(
    mut file: File,
    archive: &[u8],
    staging: &Path,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    for chunk in archive.chunks(CHUNK) {
        cancelled(cancel)?;
        file.write_all(chunk)
            .map_err(|error| io_fault("write package staging", staging, &error))?;
    }
    file.sync_all()
        .map_err(|error| io_fault("sync package staging", staging, &error))
}

/// Compares the staged file with the prepared bytes without a second full buffer.
fn verify_staged(staging: &Path, archive: &[u8]) -> Result<(), Fault> {
    let mut file =
        File::open(staging).map_err(|error| io_fault("reopen package staging", staging, &error))?;
    let mut buffer = vec![0; CHUNK];
    let mut offset = 0;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_fault("read package staging", staging, &error))?;
        if read == 0 {
            break;
        }
        if archive.get(offset..offset + read) != Some(&buffer[..read]) {
            return Err(integrity_fault(
                "staged package archive differs from the prepared bytes",
            ));
        }
        offset += read;
    }
    if offset != archive.len() {
        return Err(integrity_fault(
            "staged package archive differs from the prepared bytes",
        ));
    }
    Ok(())
}

/// Bounded single capture of an operator-selected file; a change while reading refuses.
fn read_selected(path: &Path, cancel: &AtomicBool) -> Result<archive::Bytes, Fault> {
    let file =
        File::open(path).map_err(|error| io_fault("open the selected package", path, &error))?;
    read_archive(path, file, cancel).map(|(bytes, _)| bytes)
}

/// Bounded single capture of one opened archive file, charged to the payload budget;
/// a change while reading refuses. Returns the bytes and the opened file's metadata.
fn read_archive(
    path: &Path,
    mut file: File,
    cancel: &AtomicBool,
) -> Result<(archive::Bytes, fs::Metadata), Fault> {
    let before = file
        .metadata()
        .map_err(|error| io_fault("inspect the package archive", path, &error))?;
    if !before.is_file() {
        return Err(format_fault("the package archive is not a regular file"));
    }
    let length = usize::try_from(before.len())
        .ok()
        .filter(|length| *length <= MAX_ARCHIVE_BYTES)
        .ok_or_else(|| limit_fault("package archive exceeds 67 MiB"))?;
    let reservation = reserve_payload(length)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| limit_fault("package archive allocation failed"))?;
    let mut limited = (&mut file).take(length as u64 + 1);
    loop {
        cancelled(cancel)?;
        let read = (&mut limited)
            .take(CHUNK as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| io_fault("read the package archive", path, &error))?;
        if read == 0 {
            break;
        }
    }
    let after = fs::metadata(path)
        .map_err(|error| io_fault("inspect the package archive", path, &error))?;
    let changed = bytes.len() != length
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok();
    #[cfg(unix)]
    let changed = changed || {
        use std::os::unix::fs::MetadataExt;
        after.dev() != before.dev() || after.ino() != before.ino()
    };
    if changed {
        return Err(Fault::new(
            "PackageChanged",
            "the package archive changed while being read; select it again",
        ));
    }
    Ok((archive::Bytes::new(bytes, reservation)?, before))
}

/// Verifies captured archive bytes into a PackageDraft for import and installed reads.
/// Callers validate that in-memory draft into the execution Inventory.
fn verified_release(
    archive: &[u8],
    artifact_id: String,
    cancel: &AtomicBool,
) -> Result<(PackageDraft, ReleaseFacts), Fault> {
    let (envelope, files) = verified_payload(archive, cancel)?;
    cancelled(cancel)?;
    let draft = PackageDraft::from_files(files, &crate::authoring::limits()?)?;
    let facts = ReleaseFacts::new(&draft, &envelope.key_id, artifact_id)?;
    Ok((draft, facts))
}

/// Structure, exact envelope, membership, lengths, the payload budget and digests, in
/// that order. `package.json` is bounded by its signed length before it is read, and
/// its declarations admit every signed length before any other payload member is
/// reserved or decompressed; the draft still validates the complete payload.
fn verified_payload(
    bytes: &[u8],
    cancel: &AtomicBool,
) -> Result<(Envelope, BTreeMap<String, PayloadBytes>), Fault> {
    const MANIFEST: &str = "package.json";
    let mut reader = archive::Reader::open(bytes)?;
    let index = reader.position(archive::INDEX)?;
    let index = reader.read(index, cancel)?;
    let signature = reader.position(archive::SIGNATURE)?;
    let signature = reader.read(signature, cancel)?;
    let envelope = format::verify(&index, &signature)?;
    let mut members = BTreeMap::new();
    for (position, member) in reader.members().iter().enumerate() {
        if let Some(path) = member.name.strip_prefix(archive::PAYLOAD_PREFIX) {
            members.insert(path.to_owned(), (position, member.size));
        }
    }
    if let Some(path) = differing_member(members.keys(), &envelope.files) {
        return Err(
            integrity_fault("archive payload members differ from the signed index")
                .with_context(json!({"path":path})),
        );
    }
    for fact in &envelope.files {
        if members[&fact.path].1 != fact.bytes {
            return Err(
                integrity_fault("payload length differs from the signed index")
                    .with_context(json!({"path":fact.path})),
            );
        }
    }
    let mut read = |fact: &FileFact| -> Result<PayloadBytes, Fault> {
        let bytes = reader.read_payload(members[&fact.path].0, cancel)?;
        if format::hex(bytes.digest()) != fact.sha256 {
            return Err(
                integrity_fault("payload digest differs from the signed index")
                    .with_context(json!({"path":fact.path})),
            );
        }
        Ok(bytes)
    };
    let manifest = envelope.files.iter().find(|fact| fact.path == MANIFEST);
    PackageDraft::check_manifest_length(manifest.map(|fact| fact.bytes))?;
    let mut files = BTreeMap::new();
    // A missing manifest was refused above.
    if let Some(fact) = manifest {
        let bytes = read(fact)?;
        let total = envelope.files.iter().map(|file| file.bytes).sum();
        let length = |path: &str| members.get(path).map(|&(_, size)| size);
        PackageDraft::check_payload_budget(&bytes, length, total)?;
        files.insert(fact.path.clone(), bytes);
    }
    for fact in &envelope.files {
        if fact.path != MANIFEST {
            files.insert(fact.path.clone(), read(fact)?);
        }
    }
    Ok((envelope, files))
}

/// The first path present on only one side; both sides are sorted and unique.
fn differing_member<'a>(
    paths: impl Iterator<Item = &'a String>,
    facts: &'a [FileFact],
) -> Option<&'a str> {
    let mut indexed = facts.iter().map(|fact| fact.path.as_str());
    let mut present = paths.map(String::as_str);
    loop {
        match (present.next(), indexed.next()) {
            (None, None) => return None,
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) => return Some(left.min(right)),
            (Some(path), None) | (None, Some(path)) => return Some(path),
        }
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), Fault> {
    if cancel.load(Ordering::Acquire) {
        Err(Fault::new(
            "PackageCancelled",
            "Package operation was cancelled",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn destination_exists(destination: &Path) -> Fault {
    Fault::new(
        "PackageDestinationExists",
        "the export destination already exists; choose another name",
    )
    .with_context(json!({"path":destination.to_string_lossy()}))
}

fn format_fault(message: &str) -> Fault {
    Fault::new("PackageFormat", message)
}

fn signature_fault(message: &str) -> Fault {
    Fault::new("PackageSignature", message)
}

fn integrity_fault(message: &str) -> Fault {
    Fault::new("PackageIntegrity", message)
}

fn limit_fault(message: &str) -> Fault {
    Fault::new("PackageLimit", message)
}

fn io_fault(operation: &str, path: &Path, error: &io::Error) -> Fault {
    Fault::new("PackageStorage", format!("could not {operation}")).with_context(json!({
        "operation":operation,
        "path":path.to_string_lossy(),
        "kind":format!("{:?}", error.kind()),
        "os_code":error.raw_os_error()
    }))
}

/// Adds attributable detail without discarding a non-object context.
fn note(fault: &mut Fault, key: &str, value: Value) {
    if !fault.context.is_object() {
        let cause = fault.context.take();
        fault.context = if cause.is_null() {
            json!({})
        } else {
            json!({"cause":cause})
        };
    }
    fault.context[key] = value;
}

/// Installs `source` through export, import and publication using the key derived from `seed`.
#[cfg(test)]
pub(crate) fn install_for_test_with(
    root: &Path,
    source: &Path,
    seed: [u8; 32],
) -> InstalledIdentity {
    let cancel = AtomicBool::new(false);
    let draft = PackageDraft::capture(source, &crate::authoring::limits().unwrap()).unwrap();
    let prepared = prepare_export(draft, &Signer::from_seed(seed), &cancel).unwrap();
    let file = temporary(&std::env::temp_dir(), "mmpkg-fixture");
    fs::write(&file, &*prepared.archive).unwrap();
    let imported = prepare_import(&file, &cancel);
    fs::remove_file(&file).unwrap();
    let imported = imported.unwrap();
    let publication = publish_import(root, &imported, &cancel).unwrap();
    assert!(publication.cleanup_error.is_none());
    InstalledIdentity {
        package_id: imported.facts.package_id.clone(),
        artifact_id: imported.facts.artifact_id.clone(),
    }
}
