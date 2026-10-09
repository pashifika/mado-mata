use super::{RuntimeArchive, RuntimeMember, check_cancel, download, fault, io_fault};
use crate::ocr_setup::catalog::{ArchiveFormat, safe_relative};
use flate2::read::GzDecoder;
use mado_runtime_comparison::model::Fault;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_ENTRIES: usize = 256;
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;
const MAX_DIRECTORY: usize = 1024 * 1024;

pub(super) fn extract(
    input: impl Read + Seek,
    destination: &Path,
    spec: &RuntimeArchive,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    check_cancel(cancel)?;
    let result = match spec.format {
        ArchiveFormat::Tgz => extract_tar(input, destination, spec, cancel),
        ArchiveFormat::Zip => extract_zip(input, destination, spec, cancel),
    };
    // A blocked read may finish with an I/O error after Stop. Preserve clean cancellation.
    check_cancel(cancel)?;
    result
}

struct Expansion<'a, R> {
    input: R,
    remaining: u64,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for Expansion<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::other("runtime extraction cancelled"));
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut extra = [0];
            return if self.input.read(&mut extra)? == 0 {
                Ok(0)
            } else {
                Err(std::io::Error::other(
                    "runtime archive exceeds the expansion bound",
                ))
            };
        }
        let limit = self.remaining.min(buffer.len() as u64) as usize;
        let length = self.input.read(&mut buffer[..limit])?;
        self.remaining -= length as u64;
        Ok(length)
    }
}

fn member_path(raw: &[u8], directory: bool) -> Result<&str, Fault> {
    let path =
        std::str::from_utf8(raw).map_err(|_| fault("archive", "archive paths must be UTF-8"))?;
    // The official tar uses one leading ./; ZIP directory names end with /.
    let path = path.strip_prefix("./").unwrap_or(path);
    let path = if directory {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    safe_relative(path)?;
    if path.len() > 1024
        || !path.is_ascii()
        || path.contains(['<', '>', '"', '|', '?', '*'])
        || path.split('/').any(|component| {
            let stem = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            component.ends_with(['.', ' '])
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
    {
        return Err(fault("archive", "unsafe archive destination"));
    }
    Ok(path)
}

fn record(
    names: &mut BTreeMap<String, bool>,
    path: &str,
    directory: bool,
    spec: &RuntimeArchive,
) -> Result<(), Fault> {
    let prefix = spec
        .members
        .first()
        .and_then(|member| member.archive_path.split('/').next())
        .ok_or_else(|| fault("catalog", "runtime has no selected members"))?;
    if path != prefix
        && !path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
    {
        return Err(fault(
            "archive",
            "archive member is outside its reviewed root",
        ));
    }
    let key = path.to_ascii_lowercase();
    if names.len() >= MAX_ENTRIES || names.contains_key(&key) {
        return Err(fault(
            "archive",
            "duplicate archive destination or entry count exceeded",
        ));
    }
    for (existing, is_directory) in names.iter() {
        if (!is_directory
            && key
                .strip_prefix(existing)
                .is_some_and(|suffix| suffix.starts_with('/')))
            || (!directory
                && existing
                    .strip_prefix(&key)
                    .is_some_and(|suffix| suffix.starts_with('/')))
        {
            return Err(fault(
                "archive",
                "archive file/directory destinations conflict",
            ));
        }
    }
    names.insert(key, directory);
    Ok(())
}

fn selected<'a>(spec: &'a RuntimeArchive, path: &str) -> Option<&'a RuntimeMember> {
    spec.members
        .iter()
        .find(|member| member.archive_path == path)
}

fn copy_member(
    input: &mut impl Read,
    member: &RuntimeMember,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    // Only the catalog's flat destinations are used; no archive path reaches the filesystem.
    safe_relative(&member.path)?;
    if member.path.contains('/') || matches!(member.path.as_str(), "receipt.json" | "archive.bin") {
        return Err(fault(
            "catalog",
            "runtime member destination must be a distinct filename",
        ));
    }
    let mut output = download::new_file(&destination.join(&member.path))?;
    download::transfer_verified(
        input,
        &mut output,
        member.bytes,
        &member.sha256,
        cancel,
        &mut |_| {},
    )?;
    output
        .sync_all()
        .map_err(|error| io_fault("staging", error))
}

fn extract_tar(
    input: impl Read,
    destination: &Path,
    spec: &RuntimeArchive,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    let expanded = Expansion {
        input: GzDecoder::new(input),
        remaining: MAX_EXPANDED,
        cancel,
    };
    let mut archive = tar::Archive::new(expanded);
    let mut names = BTreeMap::new();
    let mut found = BTreeSet::new();
    // No GNU/PAX rewriting, sparse files, hardlinks, unpacking or permission restoration.
    for entry in archive
        .entries()
        .map_err(|error| io_fault("archive", error))?
        .raw(true)
    {
        check_cancel(cancel)?;
        let mut entry = entry.map_err(|error| io_fault("archive", error))?;
        let kind = entry.header().entry_type().as_byte();
        let directory = kind == b'5';
        let name = {
            let raw = entry.path_bytes();
            member_path(&raw, directory)?.to_owned()
        };
        record(&mut names, &name, directory, spec)?;
        let size = entry.size();
        if size > MAX_EXPANDED || (directory && size != 0) {
            return Err(fault("archive", "archive member exceeds its size bound"));
        }
        if let Some(member) = selected(spec, &name) {
            if !matches!(kind, b'0' | 0) || size != member.bytes {
                return Err(fault(
                    "archive",
                    "selected runtime member must be a regular file of its exact size",
                ));
            }
            copy_member(&mut entry, member, destination, cancel)?;
            found.insert(member.path.as_str());
        } else if kind == b'2' {
            // This one unselected alias exists in the pinned official tar. Ignore it;
            // never create or follow any archive link, including this reviewed alias.
            if name != "onnxruntime-osx-arm64-1.29.0/lib/libonnxruntime.1.dylib"
                || entry.link_name_bytes().as_deref()
                    != Some(b"libonnxruntime.1.29.0.dylib".as_slice())
                || size != 0
            {
                return Err(fault("archive", "unreviewed archive link refused"));
            }
        } else if !matches!(kind, b'0' | 0 | b'5') {
            return Err(fault(
                "archive",
                "archive contains unsupported special entries",
            ));
        }
    }
    // Finish the bounded gzip stream (including CRC), allowing only tar end padding.
    let mut expanded = archive.into_inner();
    let mut buffer = [0; 65_536];
    loop {
        let length = expanded
            .read(&mut buffer)
            .map_err(|error| io_fault("archive", error))?;
        if length == 0 {
            break;
        }
        if buffer[..length].iter().any(|byte| *byte != 0) {
            return Err(fault("archive", "unexpected data after tar end marker"));
        }
    }
    complete(&found, spec)
}

fn extract_zip(
    mut input: impl Read + Seek,
    destination: &Path,
    spec: &RuntimeArchive,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    let count = zip_directory(&mut input, spec, cancel)?;
    input.rewind().map_err(|error| io_fault("archive", error))?;
    let mut archive = zip::ZipArchive::new(input).map_err(zip_fault)?;
    if archive.len() != count {
        return Err(fault("archive", "ZIP entries disagree with the directory"));
    }
    let mut found = BTreeSet::new();
    for index in 0..archive.len() {
        check_cancel(cancel)?;
        let mut entry = archive.by_index(index).map_err(zip_fault)?;
        let name = member_path(entry.name_raw(), entry.is_dir())?;
        if let Some(member) = selected(spec, name) {
            if entry.is_dir() || entry.is_symlink() || entry.size() != member.bytes {
                return Err(fault(
                    "archive",
                    "selected runtime ZIP member is not a regular file of its exact size",
                ));
            }
            copy_member(&mut entry, member, destination, cancel)?;
            found.insert(member.path.as_str());
        }
    }
    complete(&found, spec)
}

fn complete(found: &BTreeSet<&str>, spec: &RuntimeArchive) -> Result<(), Fault> {
    if found.len() != spec.members.len() {
        return Err(fault(
            "archive",
            "archive is missing a selected runtime library or license notice",
        ));
    }
    Ok(())
}

// Preflight central names before ZipArchive can collapse duplicate names. Only the
// pinned single-disk, non-ZIP64, comment-free layout is supported. Payloads stay on disk.
fn zip_directory(
    input: &mut (impl Read + Seek),
    spec: &RuntimeArchive,
    cancel: &AtomicBool,
) -> Result<usize, Fault> {
    let length = input
        .seek(SeekFrom::End(0))
        .map_err(|error| io_fault("archive", error))?;
    if length < 22 {
        return Err(fault("archive", "truncated ZIP footer"));
    }
    input
        .seek(SeekFrom::End(-22))
        .map_err(|error| io_fault("archive", error))?;
    let mut footer = [0; 22];
    input
        .read_exact(&mut footer)
        .map_err(|error| io_fault("archive", error))?;
    let count = usize::from(u16_at(&footer, 10)?);
    let bytes = u32_at(&footer, 12)? as usize;
    let start = u64::from(u32_at(&footer, 16)?);
    if u32_at(&footer, 0)? != 0x0605_4b50
        || u16_at(&footer, 4)? != 0
        || u16_at(&footer, 6)? != 0
        || u16_at(&footer, 20)? != 0
        || usize::from(u16_at(&footer, 8)?) != count
        || count == 0
        || count > MAX_ENTRIES
        || bytes > MAX_DIRECTORY
        || start + bytes as u64 != length - 22
    {
        return Err(fault("archive", "unsupported or oversized ZIP directory"));
    }
    input
        .seek(SeekFrom::Start(start))
        .map_err(|error| io_fault("archive", error))?;
    let mut central = vec![0; bytes];
    input
        .read_exact(&mut central)
        .map_err(|error| io_fault("archive", error))?;
    let mut cursor = 0;
    let mut expanded = 0_u64;
    let mut names = BTreeMap::new();
    for _ in 0..count {
        check_cancel(cancel)?;
        let header = central
            .get(cursor..cursor + 46)
            .ok_or_else(|| fault("archive", "truncated ZIP directory entry"))?;
        let flags = u16_at(header, 8)?;
        let size = u64::from(u32_at(header, 24)?);
        let name_length = usize::from(u16_at(header, 28)?);
        let extra = usize::from(u16_at(header, 30)?);
        let comment = usize::from(u16_at(header, 32)?);
        let attributes = u32_at(header, 38)?;
        let kind = (attributes >> 16) & 0o170000;
        if u32_at(header, 0)? != 0x0201_4b50
            || u16_at(header, 6)? > 20
            || flags & !0x080e != 0
            || !matches!(u16_at(header, 10)?, 0 | 8)
            || u16_at(header, 34)? != 0
            || !matches!(kind, 0 | 0o100000 | 0o040000)
            || name_length == 0
            || name_length > 1024
            || size > MAX_EXPANDED
            || u64::from(u32_at(header, 42)?) >= start
        {
            return Err(fault(
                "archive",
                "unsupported ZIP features or non-regular member",
            ));
        }
        let end = cursor + 46 + name_length;
        let raw = central
            .get(cursor + 46..end)
            .ok_or_else(|| fault("archive", "truncated ZIP name"))?;
        let directory = raw.ends_with(b"/");
        if (directory && (size != 0 || kind == 0o100000))
            || (!directory && (kind == 0o040000 || attributes & 0x10 != 0))
        {
            return Err(fault("archive", "ZIP directory attributes disagree"));
        }
        let name = member_path(raw, directory)?;
        record(&mut names, name, directory, spec)?;
        expanded += size;
        if expanded > MAX_EXPANDED {
            return Err(fault(
                "archive",
                "ZIP expansion exceeds the aggregate bound",
            ));
        }
        cursor = end + extra + comment;
        if cursor > central.len() {
            return Err(fault("archive", "truncated ZIP extra data"));
        }
    }
    if cursor != central.len() {
        return Err(fault("archive", "unlisted ZIP directory data"));
    }
    Ok(count)
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Fault> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| fault("archive", "truncated ZIP integer"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Fault> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| fault("archive", "truncated ZIP integer"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn zip_fault(error: zip::result::ZipError) -> Fault {
    fault(
        "archive",
        &format!("runtime ZIP could not be read: {error}"),
    )
}
