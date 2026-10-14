//! The fixed `.mmpkg` ZIP container: `index.json`, `signature.json` and regular files
//! under `package/`. The writer stores every entry; the reader also admits deflate.
//! A bounded central-directory preflight runs before the pinned ZIP library reads
//! anything, so duplicate names cannot collapse and unlisted bytes cannot hide.

use super::format::payload_path;
use super::{
    CHUNK, MAX_ARCHIVE_BYTES, MAX_PAYLOAD_FILES, MAX_SIGNATURE_BYTES, cancelled, format_fault,
    limit_fault,
};
use crate::author_signing::MAX_INDEX_BYTES;
use mado_runtime_comparison::images::{
    PACKAGE_BYTES, PayloadBytes, PayloadReservation, reserve_payload,
};
use mado_runtime_comparison::model::Fault;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};
use std::sync::atomic::AtomicBool;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

pub(super) const INDEX: &str = "index.json";
pub(super) const SIGNATURE: &str = "signature.json";
pub(super) const PAYLOAD_PREFIX: &str = "package/";
const MAX_MEMBERS: usize = MAX_PAYLOAD_FILES + 2;
const MAX_NAME_BYTES: usize = 1024;

/// Archive bytes charged to the shared payload budget while they are retained.
pub(super) struct Bytes {
    bytes: Vec<u8>,
    _reservation: PayloadReservation,
}

impl Bytes {
    pub(super) fn new(bytes: Vec<u8>, mut reservation: PayloadReservation) -> Result<Self, Fault> {
        if bytes.capacity() > reservation.bytes() {
            reservation.resize(bytes.capacity())?;
        }
        Ok(Self {
            bytes,
            _reservation: reservation,
        })
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Bytes")
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

/// Fixed writer policy: stored entries, the DOS epoch timestamp and `0644` modes, so one
/// capture always produces the same bytes. No directory entries or extra fields.
pub(super) fn write(
    index: &[u8],
    signature: &[u8],
    files: &BTreeMap<String, PayloadBytes>,
) -> Result<Bytes, Fault> {
    let members = [(INDEX.to_owned(), index), (SIGNATURE.to_owned(), signature)]
        .into_iter()
        .chain(
            files
                .iter()
                .map(|(path, bytes)| (format!("{PAYLOAD_PREFIX}{path}"), bytes.as_slice())),
        )
        .collect::<Vec<_>>();
    let estimate = members
        .iter()
        .map(|(name, bytes)| 76 + 2 * name.len() + bytes.len())
        .sum::<usize>()
        + 22;
    if estimate > MAX_ARCHIVE_BYTES {
        return Err(limit_fault("package archive would exceed 67 MiB"));
    }
    let reservation = reserve_payload(estimate)?;
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644);
    let mut writer = ZipWriter::new(Cursor::new(Vec::with_capacity(estimate)));
    for (name, bytes) in members {
        writer.start_file(name, options).map_err(zip_fault)?;
        writer.write_all(bytes).map_err(|error| {
            format_fault("package archive could not be written")
                .with_context(json!({"cause":error.to_string()}))
        })?;
    }
    let bytes = writer.finish().map_err(zip_fault)?.into_inner();
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(limit_fault("package archive exceeds 67 MiB"));
    }
    Bytes::new(bytes, reservation)
}

/// One preflighted member in central-directory order.
#[derive(Debug)]
pub(super) struct Member {
    pub(super) name: String,
    pub(super) size: usize,
    method: u16,
    compressed: u64,
    header: u64,
    data: u64,
}

pub(super) struct Reader<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    members: Vec<Member>,
}

impl<'a> Reader<'a> {
    pub(super) fn open(bytes: &'a [u8]) -> Result<Self, Fault> {
        let members = preflight(bytes)?;
        let zip = ZipArchive::new(Cursor::new(bytes)).map_err(zip_fault)?;
        if zip.len() != members.len() {
            return Err(format_fault(
                "ZIP library entries disagree with the archive directory",
            ));
        }
        Ok(Self { zip, members })
    }

    pub(super) fn members(&self) -> &[Member] {
        &self.members
    }

    pub(super) fn position(&self, name: &str) -> Result<usize, Fault> {
        self.members
            .iter()
            .position(|member| member.name == name)
            .ok_or_else(|| {
                format_fault("package archive member is missing").with_context(json!({"path":name}))
            })
    }

    /// Envelope members are plain bounded buffers.
    pub(super) fn read(&mut self, position: usize, cancel: &AtomicBool) -> Result<Vec<u8>, Fault> {
        self.read_into(position, cancel)
    }

    /// Payload members are charged to the shared payload budget before allocation.
    pub(super) fn read_payload(
        &mut self,
        position: usize,
        cancel: &AtomicBool,
    ) -> Result<PayloadBytes, Fault> {
        let mut reservation = reserve_payload(self.members[position].size)?;
        let bytes = self.read_into(position, cancel)?;
        if bytes.capacity() > reservation.bytes() {
            reservation.resize(bytes.capacity())?;
        }
        PayloadBytes::from_reserved(bytes, reservation)
    }

    /// Decompression stops at the declared size; one more byte refuses the member.
    fn read_into(&mut self, position: usize, cancel: &AtomicBool) -> Result<Vec<u8>, Fault> {
        let member = &self.members[position];
        let name = member.name.clone();
        let fault = |message: &str| format_fault(message).with_context(json!({"path":name}));
        let mut file = self.zip.by_index(position).map_err(zip_fault)?;
        if file.name_raw() != member.name.as_bytes()
            || file.header_start() != member.header
            || file.data_start() != Some(member.data)
            || file.compressed_size() != member.compressed
            || file.size() != member.size as u64
            || method_code(file.compression()) != Some(member.method)
            || file.encrypted()
            || !file.is_file()
        {
            return Err(fault(
                "ZIP library view of a member disagrees with the archive directory",
            ));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(member.size)
            .map_err(|_| limit_fault("package member allocation failed"))?;
        let mut remaining = (&mut file).take(member.size as u64);
        loop {
            cancelled(cancel)?;
            let read = (&mut remaining)
                .take(CHUNK as u64)
                .read_to_end(&mut bytes)
                .map_err(|error| {
                    format_fault("package archive member could not be decompressed")
                        .with_context(json!({"path":name,"cause":error.to_string()}))
                })?;
            if read == 0 {
                break;
            }
        }
        if bytes.len() != member.size {
            return Err(fault("package archive member is shorter than declared"));
        }
        // Reading past the declared end also completes the library's CRC check.
        let mut probe = [0_u8; 1];
        match file.read(&mut probe) {
            Ok(0) => Ok(bytes),
            Ok(_) => Err(fault(
                "package archive member expands beyond its declared size",
            )),
            Err(error) => Err(
                format_fault("package archive member failed its integrity check")
                    .with_context(json!({"path":name,"cause":error.to_string()})),
            ),
        }
    }
}

/// Supported layout: one disk, no ZIP64, archive or entry comments, encryption,
/// directory/link/special members or unicode-path overrides; stored or deflated
/// members with optional data descriptors and benign extra fields, contiguous from
/// offset zero to the central directory. Names are classified and bounded here.
fn preflight(bytes: &[u8]) -> Result<Vec<Member>, Fault> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(limit_fault("package archive exceeds 67 MiB"));
    }
    if bytes.len() < 22 {
        return Err(format_fault("package archive is truncated or not a ZIP"));
    }
    let end = bytes.len() - 22;
    if u32_at(bytes, end)? != 0x0605_4b50
        || u16_at(bytes, end + 4)? != 0
        || u16_at(bytes, end + 6)? != 0
        || u16_at(bytes, end + 20)? != 0
    {
        return Err(format_fault(
            "package archive is truncated, commented, multi-disk or not a ZIP",
        ));
    }
    let count = usize::from(u16_at(bytes, end + 10)?);
    let central_length = u32_at(bytes, end + 12)? as usize;
    let central_start = u32_at(bytes, end + 16)? as usize;
    if usize::from(u16_at(bytes, end + 8)?) != count
        || central_start.checked_add(central_length) != Some(end)
    {
        return Err(format_fault(
            "package archive directory is truncated or inconsistent",
        ));
    }
    if count > MAX_MEMBERS {
        return Err(limit_fault(
            "package archive has more than 128 payload files",
        ));
    }
    let mut members = Vec::with_capacity(count);
    let mut names = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    let mut payload_total = 0usize;
    let mut central = central_start;
    let mut local = 0usize;
    for _ in 0..count {
        if central.checked_add(46).is_none_or(|next| next > end)
            || u32_at(bytes, central)? != 0x0201_4b50
        {
            return Err(format_fault("package archive directory entry is truncated"));
        }
        let version = u16_at(bytes, central + 6)?;
        let flags = u16_at(bytes, central + 8)?;
        let method = u16_at(bytes, central + 10)?;
        let crc = u32_at(bytes, central + 16)?;
        let compressed = u32_at(bytes, central + 20)?;
        let size = u32_at(bytes, central + 24)?;
        let name_length = usize::from(u16_at(bytes, central + 28)?);
        let extra_length = usize::from(u16_at(bytes, central + 30)?);
        let comment_length = u16_at(bytes, central + 32)?;
        let attributes = u32_at(bytes, central + 38)?;
        let offset = u32_at(bytes, central + 42)? as usize;
        if flags & 0x0041 != 0 || flags & 0x2000 != 0 {
            return Err(format_fault(
                "encrypted package archive members are not supported",
            ));
        }
        let kind = (attributes >> 16) & 0o170_000;
        if attributes & 0x10 != 0 || !matches!(kind, 0 | 0o100_000) {
            return Err(format_fault(
                "package archive members must be regular files, never directories, links or special files",
            ));
        }
        if version > 20
            || flags & !0x080e != 0
            || !matches!(method, 0 | 8)
            || (method == 0 && compressed != size)
            || comment_length != 0
            || u16_at(bytes, central + 34)? != 0
            || name_length == 0
            || name_length > MAX_NAME_BYTES
        {
            return Err(format_fault(
                "package archive member uses an unsupported ZIP feature",
            ));
        }
        let name_start = central + 46;
        let name_end = name_start + name_length;
        let extra_end = name_end + extra_length;
        if extra_end > end {
            return Err(format_fault("package archive directory entry is truncated"));
        }
        let raw_name = &bytes[name_start..name_end];
        extra_fields(&bytes[name_end..extra_end])?;
        let name = std::str::from_utf8(raw_name)
            .ok()
            .filter(|name| name.is_ascii())
            .ok_or_else(|| format_fault("package archive member names must be portable ASCII"))?
            .to_owned();
        let size = size as usize;
        let maximum = if name == INDEX {
            MAX_INDEX_BYTES
        } else if name == SIGNATURE {
            MAX_SIGNATURE_BYTES
        } else if let Some(path) = name.strip_prefix(PAYLOAD_PREFIX) {
            payload_path(path)?;
            payload_total = payload_total
                .checked_add(size)
                .filter(|total| *total <= PACKAGE_BYTES)
                .ok_or_else(|| limit_fault("package payload exceeds the 65 MiB package bound"))?;
            PACKAGE_BYTES
        } else {
            return Err(format_fault("package archive has an unlisted member")
                .with_context(json!({"path":name})));
        };
        if !names.insert(name.clone()) {
            return Err(format_fault("package archive has a duplicate member")
                .with_context(json!({"path":name})));
        }
        if !aliases.insert(name.to_ascii_lowercase()) {
            return Err(format_fault("package archive members alias by case")
                .with_context(json!({"path":name})));
        }
        if size > maximum {
            return Err(limit_fault("package archive member exceeds its byte bound")
                .with_context(json!({"path":name})));
        }
        let data = local_entry(bytes, local, offset, central, central_start, raw_name)?;
        let data_end = data
            .checked_add(compressed as usize)
            .filter(|data_end| *data_end <= central_start)
            .ok_or_else(|| format_fault("package archive member data is truncated"))?;
        let descriptor = flags & 0x0008 != 0;
        if !descriptor
            && (u32_at(bytes, offset + 14)? != crc
                || u32_at(bytes, offset + 18)? != compressed
                || u32_at(bytes, offset + 22)? as usize != size)
        {
            return Err(format_fault(
                "package archive local entry disagrees with its directory",
            ));
        }
        local = if descriptor {
            data_descriptor(bytes, data_end, central_start, crc, compressed, size)?
        } else {
            data_end
        };
        members.push(Member {
            name,
            size,
            method,
            compressed: u64::from(compressed),
            header: offset as u64,
            data: data as u64,
        });
        central = extra_end;
    }
    if local != central_start || central != end {
        return Err(format_fault("package archive contains unlisted bytes"));
    }
    if !names.contains(INDEX) || !names.contains(SIGNATURE) {
        return Err(format_fault(
            "package archive lacks index.json or signature.json",
        ));
    }
    Ok(members)
}

/// Checks one local header at the expected contiguous offset against its central
/// header (version, flags and method) and returns the member's data start.
fn local_entry(
    bytes: &[u8],
    expected: usize,
    offset: usize,
    central: usize,
    central_start: usize,
    name: &[u8],
) -> Result<usize, Fault> {
    if offset != expected
        || offset
            .checked_add(30)
            .is_none_or(|next| next > central_start)
        || u32_at(bytes, offset)? != 0x0403_4b50
        || u16_at(bytes, offset + 4)? != u16_at(bytes, central + 6)?
        || u16_at(bytes, offset + 6)? != u16_at(bytes, central + 8)?
        || u16_at(bytes, offset + 8)? != u16_at(bytes, central + 10)?
        || usize::from(u16_at(bytes, offset + 26)?) != name.len()
    {
        return Err(format_fault(
            "package archive local entry disagrees with its directory",
        ));
    }
    let extra_length = usize::from(u16_at(bytes, offset + 28)?);
    let name_end = offset + 30 + name.len();
    let data = name_end + extra_length;
    if data > central_start || &bytes[offset + 30..name_end] != name {
        return Err(format_fault(
            "package archive local entry disagrees with its directory",
        ));
    }
    extra_fields(&bytes[name_end..data])?;
    Ok(data)
}

/// Returns the end of a data descriptor, with or without its optional signature.
fn data_descriptor(
    bytes: &[u8],
    start: usize,
    central_start: usize,
    crc: u32,
    compressed: u32,
    size: usize,
) -> Result<usize, Fault> {
    let fields = if u32_at(bytes, start).ok() == Some(0x0807_4b50) {
        start + 4
    } else {
        start
    };
    let end = fields + 12;
    if end > central_start
        || u32_at(bytes, fields)? != crc
        || u32_at(bytes, fields + 4)? != compressed
        || u32_at(bytes, fields + 8)? as usize != size
    {
        return Err(format_fault(
            "package archive data descriptor disagrees with its directory",
        ));
    }
    Ok(end)
}

/// Benign extra fields (timestamps, Unix/NTFS attributes) are admitted; ZIP64, AES and
/// Info-ZIP unicode overrides would change sizes, encryption or names the preflight saw.
fn extra_fields(mut extra: &[u8]) -> Result<(), Fault> {
    while !extra.is_empty() {
        if extra.len() < 4 {
            return Err(format_fault("package archive extra field is truncated"));
        }
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let length = usize::from(u16::from_le_bytes([extra[2], extra[3]]));
        if extra.len() < 4 + length {
            return Err(format_fault("package archive extra field is truncated"));
        }
        if matches!(id, 0x0001 | 0x9901 | 0x7075 | 0x6375) {
            return Err(format_fault(
                "package archive member uses an unsupported ZIP feature",
            ));
        }
        extra = &extra[4 + length..];
    }
    Ok(())
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Fault> {
    let value = offset
        .checked_add(2)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| format_fault("package archive header is truncated"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Fault> {
    let value = offset
        .checked_add(4)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| format_fault("package archive header is truncated"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn zip_fault(error: zip::result::ZipError) -> Fault {
    format_fault("package ZIP could not be read or written")
        .with_context(json!({"cause":error.to_string()}))
}

fn method_code(method: CompressionMethod) -> Option<u16> {
    match method {
        CompressionMethod::Stored => Some(0),
        CompressionMethod::Deflated => Some(8),
        _ => None,
    }
}
