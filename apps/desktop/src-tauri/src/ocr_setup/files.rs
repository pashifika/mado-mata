use super::catalog::{Catalog, Dependency, Platform};
use super::{absolute_string, check_cancel, fault, io_fault};
use mado_runtime_comparison::model::Fault;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub(super) fn models(root: &Path, catalog: &Catalog, cancel: &AtomicBool) -> Result<(), Fault> {
    if !root.is_absolute() {
        return Err(fault("models", "model root must be absolute"));
    }
    for asset in &catalog.models.assets {
        verify(&root.join(&asset.path), asset.bytes, &asset.sha256, cancel)?;
    }
    Ok(())
}

pub(super) fn verify(
    path: &Path,
    bytes: u64,
    digest: &str,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    check_cancel(cancel)?;
    let resolved = path
        .canonicalize()
        .map_err(|error| io_fault("file", error))?;
    let mut file = regular(&resolved)?;
    let before = file.metadata().map_err(|error| io_fault("file", error))?;
    if before.len() != bytes {
        return Err(fault(
            "integrity",
            "resource size differs from the reviewed catalog",
        ));
    }
    let mut hasher = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 65_536];
    loop {
        check_cancel(cancel)?;
        let length = file
            .read(&mut buffer)
            .map_err(|error| io_fault("file", error))?;
        if length == 0 {
            break;
        }
        count += length as u64;
        if count > bytes {
            return Err(fault("integrity", "resource exceeded its reviewed size"));
        }
        hasher.update(&buffer[..length]);
    }
    if count != bytes || format!("{:x}", hasher.finalize()) != digest {
        return Err(fault(
            "integrity",
            "resource SHA-256 differs from the reviewed catalog",
        ));
    }
    let after = fs::metadata(&resolved).map_err(|error| io_fault("file", error))?;
    if after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
        || path
            .canonicalize()
            .map_err(|error| io_fault("file", error))?
            != resolved
    {
        return Err(fault("integrity", "resource changed during verification"));
    }
    check_cancel(cancel)
}

pub(super) fn runtime(
    path: &Path,
    dependency: &Dependency,
    cancel: &AtomicBool,
) -> Result<String, Fault> {
    validate_path(path)?;
    let canonical = path
        .canonicalize()
        .map_err(|error| io_fault("runtime", error))?;
    if canonical.file_name().and_then(|name| name.to_str()) != Some(dependency.filename.as_str()) {
        return Err(fault(
            "runtime",
            "select the exact versioned runtime file named in the catalog",
        ));
    }
    verify(&canonical, dependency.bytes, &dependency.sha256, cancel)?;
    absolute_string(&canonical)
}

pub(super) fn native(
    paths: &[String],
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<Vec<String>, Fault> {
    if paths.is_empty() {
        return Err(fault(
            "missing",
            "select the complete reviewed native library list in manual settings",
        ));
    }
    if paths.len() > 64 {
        return Err(fault(
            "native",
            "at most 64 reviewed native libraries may be selected",
        ));
    }
    let mut canonical = Vec::with_capacity(paths.len());
    let mut seen = BTreeSet::new();
    for path in paths {
        check_cancel(cancel)?;
        validate_path(Path::new(path))?;
        let resolved = absolute_string(Path::new(path))?;
        if !seen.insert(resolved.clone()) {
            return Err(fault(
                "native",
                "native library locations resolve to the same file",
            ));
        }
        let mut file = regular(Path::new(&resolved))?;
        target_header(&mut file, &platform.os)?;
        canonical.push(resolved);
    }
    for module in &platform.native.required_modules {
        if !canonical.iter().any(|path| {
            let name = Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if platform.os == "windows" {
                name.eq_ignore_ascii_case(module)
            } else {
                name.strip_prefix(module)
                    .is_some_and(|suffix| suffix.starts_with('.') && suffix.ends_with(".dylib"))
            }
        }) {
            return Err(fault(
                "native",
                &format!("required native module is missing: {module}"),
            ));
        }
    }
    check_cancel(cancel)?;
    Ok(canonical)
}

fn validate_path(path: &Path) -> Result<(), Fault> {
    let text = path
        .to_str()
        .ok_or_else(|| fault("path", "resource path is not UTF-8"))?;
    if !path.is_absolute() || text.len() > 4096 || text.chars().any(char::is_control) {
        return Err(fault("path", "select a bounded absolute resource path"));
    }
    Ok(())
}

fn regular(path: &Path) -> Result<File, Fault> {
    let metadata = fs::metadata(path).map_err(|error| io_fault("file", error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 1_073_741_824 {
        return Err(fault(
            "file",
            "resource must be a nonempty regular file no larger than 1 GiB",
        ));
    }
    File::open(path).map_err(|error| io_fault("file", error))
}

// Header inspection only, deliberately not an executable/dependency loader.
fn target_header(file: &mut File, os: &str) -> Result<(), Fault> {
    let mut header = [0_u8; 64];
    file.read_exact(&mut header)
        .map_err(|error| io_fault("native", error))?;
    if os == "macos" {
        if header[..4] == [0xca, 0xfe, 0xba, 0xbe] {
            let count = u32::from_be_bytes(header[4..8].try_into().unwrap());
            if count > 32 {
                return Err(fault("native", "unrecognized universal library header"));
            }
            for index in 0..count {
                file.seek(SeekFrom::Start(8 + u64::from(index) * 20))
                    .map_err(|error| io_fault("native", error))?;
                let mut arch = [0_u8; 20];
                file.read_exact(&mut arch)
                    .map_err(|error| io_fault("native", error))?;
                if u32::from_be_bytes(arch[..4].try_into().unwrap()) == 0x0100_000c {
                    let offset = u32::from_be_bytes(arch[8..12].try_into().unwrap());
                    file.seek(SeekFrom::Start(u64::from(offset)))
                        .map_err(|error| io_fault("native", error))?;
                    file.read_exact(&mut header)
                        .map_err(|error| io_fault("native", error))?;
                    break;
                }
            }
        }
        if header[..4] == [0xcf, 0xfa, 0xed, 0xfe]
            && u32::from_le_bytes(header[4..8].try_into().unwrap()) == 0x0100_000c
            && u32::from_le_bytes(header[12..16].try_into().unwrap()) == 6
        {
            return Ok(());
        }
    } else if os == "windows" && &header[..2] == b"MZ" {
        let offset = u32::from_le_bytes(header[60..64].try_into().unwrap());
        if offset <= 1_048_576 {
            let mut pe = [0_u8; 24];
            file.seek(SeekFrom::Start(u64::from(offset)))
                .map_err(|error| io_fault("native", error))?;
            file.read_exact(&mut pe)
                .map_err(|error| io_fault("native", error))?;
            if &pe[..4] == b"PE\0\0"
                && pe[4..6] == [0x64, 0x86]
                && u16::from_le_bytes(pe[22..24].try_into().unwrap()) & 0x2000 != 0
            {
                return Ok(());
            }
        }
    }
    Err(fault(
        "native",
        "library header does not identify a supported target dynamic library",
    ))
}
