use super::catalog::{Asset, Catalog, LICENSE, NOTICE};
use super::{SetupProgress, check_cancel, fault, files, io_fault};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const OWNER: &str = "MadoMata OCR resources v1\n";
const MARKER: &str = ".mado-ocr-resources-v1";
const STAGING: &str = "staging-v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    engine_revision: String,
    model_set: String,
    assets: Vec<Asset>,
}

pub(super) fn acquire(
    root: &Path,
    catalog: &Catalog,
    cancel: &AtomicBool,
    progress: impl FnMut(SetupProgress),
) -> Result<(), Fault> {
    let agent = https_agent();
    acquire_with(
        root,
        catalog,
        cancel,
        progress,
        |asset, output, cancel, report| fetch(&agent, asset, output, cancel, report),
    )
}

pub(super) fn https_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(5)
        .max_response_header_size(65_536)
        .timeout_global(Some(Duration::from_secs(120)))
        .timeout_resolve(Some(Duration::from_secs(10)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .user_agent("MadoMata-OCR-Setup/1")
        .build()
        .new_agent()
}

pub(super) fn fetch(
    agent: &ureq::Agent,
    asset: &Asset,
    output: &mut File,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(u64),
) -> Result<(), Fault> {
    check_cancel(cancel)?;
    let response = agent
        .get(&asset.url)
        .header("Accept-Encoding", "identity")
        .call();
    check_cancel(cancel)?;
    let mut response = response.map_err(|error| {
        fault(
            "download",
            &format!("HTTPS resource transfer failed: {error}"),
        )
    })?;
    if response.status() != ureq::http::StatusCode::OK {
        return Err(fault("download", "resource server did not return HTTP 200"));
    }
    if response
        .body()
        .content_length()
        .is_some_and(|length| length != asset.bytes)
    {
        return Err(fault(
            "integrity",
            "HTTP content length differs from the accepted resource",
        ));
    }
    transfer(
        &mut response.body_mut().as_reader(),
        output,
        asset,
        cancel,
        report,
    )
}

pub(super) fn acquire_with(
    root: &Path,
    catalog: &Catalog,
    cancel: &AtomicBool,
    mut progress: impl FnMut(SetupProgress),
    mut fetch: impl FnMut(&Asset, &mut File, &AtomicBool, &mut dyn FnMut(u64)) -> Result<(), Fault>,
) -> Result<(), Fault> {
    check_cancel(cancel)?;
    let total = catalog.models.assets.iter().map(|asset| asset.bytes).sum();
    let report = |progress: &mut dyn FnMut(SetupProgress), stage: &str, bytes| {
        progress(SetupProgress {
            stage: stage.into(),
            resource_id: catalog.models.id.clone(),
            bytes,
            total,
        });
    };
    report(&mut progress, "resolving", 0);
    let owned = owned(root, true)?
        .ok_or_else(|| fault("storage", "managed resource directory is unavailable"))?;
    let _lease = lease(&owned)?;
    cleanup(&owned, catalog)?;
    let mut destination = owned.join(catalog.installation_name());
    if !destination.is_absolute() {
        destination =
            std::path::absolute(destination).map_err(|error| io_fault("storage", error))?;
    }
    if fs::symlink_metadata(&destination).is_ok() {
        report(&mut progress, "verifying", 0);
        verify_receipt(&destination, catalog)?;
        files::models(&destination, catalog, cancel)?;
        check_cancel(cancel)?;
        report(&mut progress, "complete", total);
        return Ok(());
    }
    let staging = owned.join(STAGING);
    fs::create_dir(&staging).map_err(|error| io_fault("staging", error))?;
    let result = (|| {
        let mut received = 0;
        for asset in &catalog.models.assets {
            check_cancel(cancel)?;
            let path = staging.join(&asset.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| io_fault("staging", error))?;
            }
            let mut output = new_file(&path)?;
            report(&mut progress, "downloading", received);
            fetch(asset, &mut output, cancel, &mut |bytes| {
                report(&mut progress, "downloading", received + bytes)
            })?;
            output
                .sync_all()
                .map_err(|error| io_fault("staging", error))?;
            received += asset.bytes;
        }
        report(&mut progress, "verifying", total);
        check_cancel(cancel)?;
        write_new(&staging.join("LICENSE.txt"), LICENSE.as_bytes())?;
        write_new(&staging.join("NOTICE.txt"), NOTICE.as_bytes())?;
        let receipt = Receipt {
            schema: 1,
            engine_revision: catalog.engine_revision.clone(),
            model_set: catalog.installation_name(),
            assets: catalog.models.assets.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&receipt)
            .map_err(|error| fault("receipt", &error.to_string()))?;
        write_new(&staging.join("receipt.json"), &bytes)?;
        report(&mut progress, "publishing", total);
        check_cancel(cancel)?;
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(fault(
                "publication",
                "published destination already exists; refusing replacement",
            ));
        }
        fs::rename(&staging, &destination).map_err(|error| io_fault("publication", error))?;
        // Rename is the commit point: late cancellation cannot undo this result.
        report(&mut progress, "complete", total);
        Ok(())
    })();
    if let Err(mut error) = result {
        if let Err(cleanup_error) = cleanup(&owned, catalog) {
            error.context =
                json!({"cleanup_error": cleanup_error, "primary_context": error.context});
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn transfer(
    input: &mut impl Read,
    output: &mut impl Write,
    asset: &Asset,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(u64),
) -> Result<(), Fault> {
    transfer_verified(input, output, asset.bytes, &asset.sha256, cancel, report)
}

pub(super) fn transfer_verified(
    input: &mut impl Read,
    output: &mut impl Write,
    expected_bytes: u64,
    expected_sha256: &str,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(u64),
) -> Result<(), Fault> {
    let mut count = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        check_cancel(cancel)?;
        let read = input.read(&mut buffer);
        check_cancel(cancel)?;
        let length = read.map_err(|error| io_fault("download", error))?;
        if length == 0 {
            break;
        }
        count += length as u64;
        if count > expected_bytes {
            return Err(fault(
                "integrity",
                "download exceeds the exact accepted size",
            ));
        }
        output
            .write_all(&buffer[..length])
            .map_err(|error| io_fault("staging", error))?;
        hasher.update(&buffer[..length]);
        report(count);
    }
    if count != expected_bytes || format!("{:x}", hasher.finalize()) != expected_sha256 {
        return Err(fault(
            "integrity",
            "download length or SHA-256 does not match the accepted resource",
        ));
    }
    check_cancel(cancel)
}

pub(super) fn recover(root: &Path, catalog: &Catalog) -> Result<Option<File>, Fault> {
    if let Some(owned) = owned(root, false)? {
        let lease = lease(&owned)?;
        cleanup(&owned, catalog)?;
        return Ok(Some(lease));
    }
    Ok(None)
}

pub(super) fn lease(owned: &Path) -> Result<File, Fault> {
    let path = owned.join(".setup.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(fault(
                "storage",
                "setup lock must be a regular file, not a link",
            ));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(io_fault("storage", error));
        }
        _ => {}
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| io_fault("storage", error))?;
    file.try_lock().map_err(|error| {
        Fault::new(
            "OcrSetupBusy",
            format!("cannot acquire exclusive resource ownership: {error}"),
        )
    })?;
    Ok(file)
}

pub(super) fn owned(root: &Path, create: bool) -> Result<Option<PathBuf>, Fault> {
    let path = root.join("ocr-resources");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if read_small(&path.join(MARKER), 64)? != OWNER.as_bytes() {
                return Err(fault(
                    "storage",
                    "resource directory has no valid ownership marker; refusing mutation",
                ));
            }
        }
        Ok(_) => {
            return Err(fault(
                "storage",
                "managed resource location must be a real directory, not a link",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            fs::create_dir(&path).map_err(|error| io_fault("storage", error))?;
            write_new(&path.join(MARKER), OWNER.as_bytes())?;
        }
        Err(error) => return Err(io_fault("storage", error)),
    }
    Ok(Some(path))
}

pub(super) fn verify_receipt(root: &Path, catalog: &Catalog) -> Result<(), Fault> {
    let metadata = fs::symlink_metadata(root).map_err(|error| io_fault("receipt", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(fault("receipt", "installation must be a real directory"));
    }
    let receipt: Receipt = serde_json::from_slice(&read_small(&root.join("receipt.json"), 65_536)?)
        .map_err(|error| fault("receipt", &error.to_string()))?;
    if receipt.schema != 1
        || receipt.engine_revision != catalog.engine_revision
        || receipt.model_set != catalog.installation_name()
        || receipt.assets.len() != catalog.models.assets.len()
        || !catalog.models.assets.iter().all(|asset| {
            receipt.assets.iter().any(|record| {
                record.path == asset.path
                    && record.bytes == asset.bytes
                    && record.sha256 == asset.sha256
            })
        })
        || read_small(&root.join("LICENSE.txt"), 32_768)? != LICENSE.as_bytes()
        || read_small(&root.join("NOTICE.txt"), 32_768)?.is_empty()
    {
        return Err(fault(
            "receipt",
            "incomplete installation receipt or license notices; published files were not changed",
        ));
    }
    Ok(())
}

pub(super) fn new_file(path: &Path) -> Result<File, Fault> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_fault("staging", error))
}

pub(super) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Fault> {
    let mut file = new_file(path)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| io_fault("staging", error))
}

pub(super) fn read_small(path: &Path, limit: u64) -> Result<Vec<u8>, Fault> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_fault("storage", error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(fault(
            "storage",
            "owned metadata must be a bounded regular file, not a link",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|error| io_fault("storage", error))?;
    if bytes.len() as u64 > limit {
        return Err(fault("storage", "owned metadata exceeded its bound"));
    }
    Ok(bytes)
}

pub(super) fn cleanup(owned: &Path, catalog: &Catalog) -> Result<(), Fault> {
    let staging = owned.join(STAGING);
    let mut allowed_files: BTreeSet<PathBuf> = catalog
        .models
        .assets
        .iter()
        .map(|asset| staging.join(&asset.path))
        .collect();
    allowed_files
        .extend(["LICENSE.txt", "NOTICE.txt", "receipt.json"].map(|name| staging.join(name)));
    cleanup_tree(&staging, &allowed_files)?;
    super::runtime::cleanup(owned, catalog)
}

// Validate the entire small known tree before deleting any member. Never recurse
// through links or remove unknown files, even inside the app-owned directory.
pub(super) fn cleanup_tree(staging: &Path, allowed_files: &BTreeSet<PathBuf>) -> Result<(), Fault> {
    match fs::symlink_metadata(staging) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(cleanup_fault(io_fault("cleanup", error))),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(cleanup_fault(fault(
                "cleanup",
                "staging is not an owned regular directory",
            )));
        }
        Ok(_) => {}
    }
    let mut allowed_dirs = BTreeSet::new();
    for file in allowed_files {
        let mut parent = file.parent();
        while let Some(directory) = parent {
            if directory == staging {
                break;
            }
            allowed_dirs.insert(directory.to_path_buf());
            parent = directory.parent();
        }
    }
    let result = (|| {
        let mut directories = vec![staging.to_path_buf()];
        let mut files = Vec::new();
        let mut cursor = 0;
        while cursor < directories.len() {
            for entry in
                fs::read_dir(&directories[cursor]).map_err(|error| io_fault("cleanup", error))?
            {
                let entry = entry.map_err(|error| io_fault("cleanup", error))?;
                let kind = entry
                    .file_type()
                    .map_err(|error| io_fault("cleanup", error))?;
                let path = entry.path();
                if kind.is_file() && allowed_files.contains(&path) {
                    files.push(path);
                } else if kind.is_dir() && allowed_dirs.contains(&path) {
                    directories.push(path);
                } else {
                    return Err(fault(
                        "cleanup",
                        "unexpected staging entry retained; inspect it manually before retrying setup",
                    ));
                }
            }
            cursor += 1;
        }
        for file in files {
            fs::remove_file(file).map_err(|error| io_fault("cleanup", error))?;
        }
        for directory in directories.into_iter().rev() {
            fs::remove_dir(directory).map_err(|error| io_fault("cleanup", error))?;
        }
        Ok(())
    })();
    result.map_err(cleanup_fault)
}

fn cleanup_fault(error: Fault) -> Fault {
    fault(
        "cleanup",
        "incomplete OCR staging could not be safely removed; setup admission must remain blocked",
    )
    .with_context(json!({"stage": "cleanup", "cleanup_error": error}))
}
