use super::catalog::{Asset, Catalog, Platform, RuntimeArchive, RuntimeMember};
use super::{SetupProgress, absolute_string, check_cancel, download, fault, files, io_fault};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

mod archive;
#[cfg(test)]
mod tests;

const STAGING: &str = "runtime-staging-v1";
const ARCHIVE: &str = "archive.bin";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    engine_revision: String,
    installation: String,
    os: String,
    arch: String,
    archive: Asset,
    members: Vec<RuntimeMember>,
}

pub(super) fn acquire(
    root: &Path,
    catalog: &Catalog,
    cancel: &AtomicBool,
    progress: impl FnMut(SetupProgress),
) -> Result<String, Fault> {
    let platform = supported(catalog)?;
    let agent = download::https_agent();
    acquire_with(
        root,
        catalog,
        platform,
        cancel,
        progress,
        |asset, file, cancel, report| download::fetch(&agent, asset, file, cancel, report),
    )
}

pub(super) fn installed(
    root: &Path,
    catalog: &Catalog,
    cancel: &AtomicBool,
) -> Result<String, Fault> {
    check_cancel(cancel)?;
    let platform = supported(catalog)?;
    let owned = download::owned(root, false)?
        .ok_or_else(|| fault("missing", "download the OCR runtime first"))?;
    verify_installation(
        &owned.join(platform.runtime_installation_name()),
        catalog,
        platform,
        cancel,
    )
}

fn supported(catalog: &Catalog) -> Result<&Platform, Fault> {
    catalog.platform().ok_or_else(|| {
        fault(
            "unsupported",
            "runtime download supports macOS arm64 and Windows x64 only",
        )
    })
}

fn specification(platform: &Platform) -> Result<&RuntimeArchive, Fault> {
    platform
        .runtime
        .archive
        .as_ref()
        .ok_or_else(|| fault("catalog", "runtime archive is unavailable"))
}

fn acquire_with(
    root: &Path,
    catalog: &Catalog,
    platform: &Platform,
    cancel: &AtomicBool,
    mut progress: impl FnMut(SetupProgress),
    mut fetch: impl FnMut(&Asset, &mut File, &AtomicBool, &mut dyn FnMut(u64)) -> Result<(), Fault>,
) -> Result<String, Fault> {
    check_cancel(cancel)?;
    let spec = specification(platform)?;
    let total = spec.asset.bytes;
    let report = |progress: &mut dyn FnMut(SetupProgress), stage: &str, bytes| {
        progress(SetupProgress {
            stage: stage.into(),
            resource_id: platform.runtime.id.clone(),
            bytes,
            total,
        });
    };
    report(&mut progress, "resolving", 0);
    check_cancel(cancel)?;
    let owned = download::owned(root, true)?
        .ok_or_else(|| fault("storage", "managed resource directory is unavailable"))?;
    let _lease = download::lease(&owned)?;
    download::cleanup(&owned, catalog)?;
    let owned = owned
        .canonicalize()
        .map_err(|error| io_fault("storage", error))?;
    let destination = owned.join(platform.runtime_installation_name());
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            report(&mut progress, "verifying", 0);
            let path = verify_installation(&destination, catalog, platform, cancel)?;
            check_cancel(cancel)?;
            report(&mut progress, "complete", total);
            return Ok(path);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_fault("storage", error)),
    }
    let published_path = destination
        .join(&platform.runtime.filename)
        .to_str()
        .ok_or_else(|| fault("path", "resource path is not UTF-8"))?
        .to_owned();
    let staging = owned.join(STAGING);
    fs::create_dir(&staging).map_err(|error| io_fault("staging", error))?;
    let result = (|| {
        let archive_path = staging.join(ARCHIVE);
        {
            let mut output = download::new_file(&archive_path)?;
            report(&mut progress, "downloading", 0);
            fetch(&spec.asset, &mut output, cancel, &mut |bytes| {
                report(&mut progress, "downloading", bytes)
            })?;
            output
                .sync_all()
                .map_err(|error| io_fault("staging", error))?;
        }
        report(&mut progress, "verifying", total);
        // Verify the staged file, not just bytes claimed by the transport.
        regular(&archive_path)?;
        files::verify(&archive_path, spec.asset.bytes, &spec.asset.sha256, cancel)?;
        let input = File::open(&archive_path).map_err(|error| io_fault("archive", error))?;
        archive::extract(input, &staging, spec, cancel)?;
        fs::remove_file(&archive_path).map_err(|error| io_fault("staging", error))?;
        let receipt = Receipt {
            schema: 1,
            engine_revision: catalog.engine_revision.clone(),
            installation: platform.runtime_installation_name(),
            os: platform.os.clone(),
            arch: platform.arch.clone(),
            archive: spec.asset.clone(),
            members: spec.members.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&receipt)
            .map_err(|error| fault("receipt", &error.to_string()))?;
        download::write_new(&staging.join("receipt.json"), &bytes)?;
        verify_installation(&staging, catalog, platform, cancel)?;
        report(&mut progress, "publishing", total);
        check_cancel(cancel)?;
        match fs::symlink_metadata(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_fault("publication", error)),
            Ok(_) => {
                return Err(fault(
                    "publication",
                    "published runtime already exists; refusing replacement",
                ));
            }
        }
        fs::rename(&staging, &destination).map_err(|error| io_fault("publication", error))?;
        // Rename publishes the library, both notices and receipt together. Late Stop cannot undo it.
        report(&mut progress, "complete", total);
        Ok(published_path)
    })();
    result.map_err(|mut error: Fault| {
        if let Err(cleanup_error) = cleanup(&owned, catalog) {
            error.context =
                json!({"cleanup_error": cleanup_error, "primary_context": error.context});
        }
        error
    })
}

fn regular(path: &Path) -> Result<(), Fault> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_fault("runtime", error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(fault(
            "runtime",
            "managed runtime members must be regular files, not links",
        ));
    }
    Ok(())
}

fn verify_installation(
    root: &Path,
    catalog: &Catalog,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<String, Fault> {
    check_cancel(cancel)?;
    let metadata = fs::symlink_metadata(root).map_err(|error| io_fault("receipt", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(fault(
            "receipt",
            "runtime installation must be a real directory",
        ));
    }
    let spec = specification(platform)?;
    let receipt: Receipt =
        serde_json::from_slice(&download::read_small(&root.join("receipt.json"), 65_536)?)
            .map_err(|error| fault("receipt", &error.to_string()))?;
    if receipt.schema != 1
        || receipt.engine_revision != catalog.engine_revision
        || receipt.installation != platform.runtime_installation_name()
        || receipt.os != platform.os
        || receipt.arch != platform.arch
        || receipt.archive.path != spec.asset.path
        || receipt.archive.bytes != spec.asset.bytes
        || receipt.archive.sha256 != spec.asset.sha256
        || receipt.members != spec.members
    {
        return Err(fault(
            "receipt",
            "runtime receipt differs from the reviewed installation; published files were not changed",
        ));
    }
    let allowed: BTreeSet<&str> = spec
        .members
        .iter()
        .map(|member| member.path.as_str())
        .chain(std::iter::once("receipt.json"))
        .collect();
    for entry in fs::read_dir(root).map_err(|error| io_fault("receipt", error))? {
        check_cancel(cancel)?;
        let entry = entry.map_err(|error| io_fault("receipt", error))?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| allowed.contains(name))
        {
            return Err(fault(
                "receipt",
                "unexpected runtime installation entry; published files were not changed",
            ));
        }
        regular(&entry.path())?;
    }
    for member in &spec.members {
        let path = root.join(&member.path);
        regular(&path)?;
        files::verify(&path, member.bytes, &member.sha256, cancel)?;
    }
    check_cancel(cancel)?;
    absolute_string(&root.join(&platform.runtime.filename))
}

pub(super) fn cleanup(owned: &Path, catalog: &Catalog) -> Result<(), Fault> {
    let staging = owned.join(STAGING);
    let allowed: BTreeSet<PathBuf> = catalog
        .platforms
        .iter()
        .filter_map(|platform| platform.runtime.archive.as_ref())
        .flat_map(|archive| {
            archive
                .members
                .iter()
                .map(|member| staging.join(&member.path))
        })
        .chain([staging.join(ARCHIVE), staging.join("receipt.json")])
        .collect();
    download::cleanup_tree(&staging, &allowed)
}
