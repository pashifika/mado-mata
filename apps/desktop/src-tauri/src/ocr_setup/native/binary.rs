use super::{MAX_PATH, MAX_RPATHS, check_cancel, fault, path_fault};
use mado_runtime_comparison::model::Fault;
use std::collections::BTreeSet;
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;

const MAX_FILE_BYTES: u64 = 1_073_741_824;
const MAX_READ_BYTES: usize = 32 * 1_048_576;
const MAX_COMMAND_BYTES: usize = 1_048_576;
const MAX_DEPENDENCIES: usize = 256;

#[derive(Default)]
pub(super) struct Budget {
    bytes: usize,
}

pub(super) struct Image {
    pub dependencies: Vec<String>,
    pub rpaths: Vec<String>,
    metadata: Metadata,
}

impl Image {
    pub fn unchanged(&self, path: &Path) -> Result<(), Fault> {
        let after = fs::metadata(path).map_err(|error| path_fault(path, error))?;
        if !same_file(&self.metadata, &after) {
            return Err(fault(
                "native",
                &format!("library changed during discovery: {}", path.display()),
            ));
        }
        Ok(())
    }
}

fn same_file(before: &Metadata, after: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return false;
        }
    }
    before.is_file()
        && after.is_file()
        && before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
}

pub(super) fn inspect(
    path: &Path,
    os: &str,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Result<Image, Fault> {
    check_cancel(cancel)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| path_fault(path, error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
        return Err(fault(
            "native",
            &format!(
                "library must be a nonempty regular file no larger than 1 GiB: {}",
                path.display()
            ),
        ));
    }
    let file = File::open(path).map_err(|error| path_fault(path, error))?;
    if !same_file(
        &metadata,
        &file.metadata().map_err(|error| path_fault(path, error))?,
    ) {
        return Err(fault(
            "native",
            "library changed while opening its metadata",
        ));
    }
    let mut reader = Reader {
        file,
        length: metadata.len(),
        budget,
        cancel,
    };
    let result = if os == "macos" {
        mach(&mut reader)
    } else {
        pe(&mut reader)
    };
    let (dependencies, rpaths) = result.map_err(|error| {
        if error.category == "OcrSetupCancelled" {
            error
        } else {
            fault("native", &format!("{}: {}", path.display(), error.message))
        }
    })?;
    let image = Image {
        dependencies,
        rpaths,
        metadata,
    };
    image.unchanged(path)?;
    check_cancel(cancel)?;
    Ok(image)
}

struct Reader<'a> {
    file: File,
    length: u64,
    budget: &'a mut Budget,
    cancel: &'a AtomicBool,
}

impl Reader<'_> {
    fn read(&mut self, offset: u64, bytes: usize) -> Result<Vec<u8>, Fault> {
        check_cancel(self.cancel)?;
        if offset
            .checked_add(bytes as u64)
            .is_none_or(|end| end > self.length)
            || self
                .budget
                .bytes
                .checked_add(bytes)
                .is_none_or(|end| end > MAX_READ_BYTES)
        {
            return Err(fault(
                "native",
                "binary metadata is truncated or exceeds the 32 MiB discovery read budget",
            ));
        }
        self.budget.bytes += bytes;
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| fault("native", &error.to_string()))?;
        let mut data = vec![0; bytes];
        // At most 64 KiB between cancellation observations, including large command tables.
        for chunk in data.chunks_mut(65_536) {
            check_cancel(self.cancel)?;
            self.file
                .read_exact(chunk)
                .map_err(|error| fault("native", &error.to_string()))?;
        }
        Ok(data)
    }
}

fn invalid() -> Fault {
    fault(
        "native",
        "malformed or unsupported dynamic-library metadata",
    )
}

fn u16le(data: &[u8], offset: usize) -> Result<u16, Fault> {
    Ok(u16::from_le_bytes(
        data.get(offset..offset + 2)
            .ok_or_else(invalid)?
            .try_into()
            .unwrap(),
    ))
}

fn u32le(data: &[u8], offset: usize) -> Result<u32, Fault> {
    Ok(u32::from_le_bytes(
        data.get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .unwrap(),
    ))
}

fn u32be(data: &[u8], offset: usize) -> Result<u32, Fault> {
    Ok(u32::from_be_bytes(
        data.get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .unwrap(),
    ))
}

fn string(data: &[u8]) -> Result<String, Fault> {
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(invalid)?;
    if end == 0 || end > MAX_PATH {
        return Err(invalid());
    }
    let text = std::str::from_utf8(&data[..end]).map_err(|_| invalid())?;
    if text.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(text.into())
}

fn add_dependency(dependencies: &mut BTreeSet<String>, name: String) -> Result<(), Fault> {
    dependencies.insert(name);
    if dependencies.len() > MAX_DEPENDENCIES {
        return Err(fault("native", "binary exceeds 256 dependency names"));
    }
    Ok(())
}

fn mach(reader: &mut Reader<'_>) -> Result<(Vec<String>, Vec<String>), Fault> {
    let header = reader.read(0, 32)?;
    let (offset, length) = if header[..4] == [0xca, 0xfe, 0xba, 0xbe] {
        let count = u32be(&header, 4)?;
        if count == 0 || count > 32 {
            return Err(invalid());
        }
        let arches = reader.read(8, count as usize * 20)?;
        let mut selected = None;
        for arch in arches.chunks_exact(20) {
            check_cancel(reader.cancel)?;
            let start = u64::from(u32be(arch, 8)?);
            let size = u64::from(u32be(arch, 12)?);
            if start < 8 + u64::from(count) * 20
                || size < 32
                || start
                    .checked_add(size)
                    .is_none_or(|end| end > reader.length)
            {
                return Err(invalid());
            }
            if u32be(arch, 0)? == 0x0100_000c {
                if selected.replace((start, size)).is_some() {
                    return Err(fault(
                        "native",
                        "ambiguous arm64 slices in universal library",
                    ));
                }
            }
        }
        selected.ok_or_else(|| fault("native", "universal library has no supported arm64 slice"))?
    } else {
        (0, reader.length)
    };
    let header = if offset == 0 {
        header
    } else {
        reader.read(offset, 32)?
    };
    // arm64e is not the supported generic arm64 target. Mask capability bits.
    if header[..4] != [0xcf, 0xfa, 0xed, 0xfe]
        || u32le(&header, 4)? != 0x0100_000c
        || u32le(&header, 8)? & 0x00ff_ffff != 0
        || u32le(&header, 12)? != 6
    {
        return Err(fault(
            "native",
            "library is not a supported arm64 Mach-O dylib",
        ));
    }
    let count = u32le(&header, 16)? as usize;
    let size = u32le(&header, 20)? as usize;
    if count > 8_192 || size > MAX_COMMAND_BYTES || size as u64 + 32 > length || count > size / 8 {
        return Err(fault(
            "native",
            "Mach-O load commands exceed bounds or the selected slice",
        ));
    }
    let commands = reader.read(offset + 32, size)?;
    let mut cursor = 0;
    let mut dependencies = BTreeSet::new();
    let mut rpaths = Vec::new();
    for _ in 0..count {
        check_cancel(reader.cancel)?;
        let command = u32le(&commands, cursor)?;
        let size = u32le(&commands, cursor + 4)? as usize;
        if size < 8 || size % 8 != 0 {
            return Err(invalid());
        }
        let data = commands.get(cursor..cursor + size).ok_or_else(invalid)?;
        match command {
            // Load, weak-load, reexport, upward-load and lazy-load are all explicit dependencies.
            0xc | 0x8000_0018 | 0x8000_001f | 0x8000_0023 | 0x20 => {
                let start = u32le(data, 8)? as usize;
                if start < 24 {
                    return Err(invalid());
                }
                add_dependency(
                    &mut dependencies,
                    string(data.get(start..).ok_or_else(invalid)?)?,
                )?;
            }
            0x8000_001c => {
                let start = u32le(data, 8)? as usize;
                if start < 12 {
                    return Err(invalid());
                }
                let value = string(data.get(start..).ok_or_else(invalid)?)?;
                if !rpaths.contains(&value) {
                    rpaths.push(value);
                }
                if rpaths.len() > MAX_RPATHS {
                    return Err(fault("native", "Mach-O exceeds 64 runpaths"));
                }
            }
            _ => {}
        }
        cursor += size;
    }
    if cursor != commands.len() {
        return Err(invalid());
    }
    Ok((dependencies.into_iter().collect(), rpaths))
}

struct Section {
    rva: u32,
    size: u32,
    offset: u32,
    raw_size: u32,
}

struct Pe {
    sections: Vec<Section>,
}

impl Pe {
    fn location(&self, rva: u32, size: usize) -> Result<u64, Fault> {
        let mut location = None;
        for section in &self.sections {
            if let Some(delta) = rva.checked_sub(section.rva)
                && delta < section.size
                && u64::from(delta) + size as u64 <= u64::from(section.raw_size)
                && u64::from(delta) + size as u64 <= u64::from(section.size)
            {
                if location
                    .replace(u64::from(section.offset) + u64::from(delta))
                    .is_some()
                {
                    return Err(fault("native", "ambiguous overlapping PE sections"));
                }
            }
        }
        location.ok_or_else(invalid)
    }

    fn read(&self, reader: &mut Reader<'_>, rva: u32, size: usize) -> Result<Vec<u8>, Fault> {
        reader.read(self.location(rva, size)?, size)
    }

    fn string(&self, reader: &mut Reader<'_>, rva: u32) -> Result<String, Fault> {
        let offset = self.location(rva, 1)?;
        let remaining = self
            .sections
            .iter()
            .filter_map(|section| {
                let delta = rva.checked_sub(section.rva)?;
                if delta < section.size && delta < section.raw_size {
                    Some(u64::from(section.size.min(section.raw_size) - delta))
                } else {
                    None
                }
            })
            .min()
            .ok_or_else(invalid)?;
        // Read only the containing raw section, never a virtual/uninitialized tail.
        let bytes = reader.read(offset, remaining.min((MAX_PATH + 1) as u64) as usize)?;
        string(&bytes)
    }
}

fn dll_name(name: &str) -> Result<String, Fault> {
    if name.is_empty()
        || name.len() > 255
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || !name.to_ascii_lowercase().ends_with(".dll")
            && !name.eq_ignore_ascii_case("winspool.drv")
    {
        return Err(fault(
            "native",
            &format!("unsupported PE dependency name: {name}"),
        ));
    }
    Ok(name.to_ascii_lowercase())
}

fn pe(reader: &mut Reader<'_>) -> Result<(Vec<String>, Vec<String>), Fault> {
    let dos = reader.read(0, 64)?;
    if &dos[..2] != b"MZ" {
        return Err(invalid());
    }
    let offset = u64::from(u32le(&dos, 60)?);
    if offset < 64 || offset > 1_048_576 {
        return Err(invalid());
    }
    let coff = reader.read(offset, 24)?;
    if &coff[..4] != b"PE\0\0" || u16le(&coff, 4)? != 0x8664 || u16le(&coff, 22)? & 0x2000 == 0 {
        return Err(fault("native", "library is not a supported x64 PE DLL"));
    }
    let count = u16le(&coff, 6)? as usize;
    let optional_size = u16le(&coff, 20)? as usize;
    if count == 0 || count > 96 || !(112..=4096).contains(&optional_size) {
        return Err(invalid());
    }
    let optional = reader.read(offset + 24, optional_size)?;
    if u16le(&optional, 0)? != 0x20b {
        return Err(invalid());
    }
    let directory_count = u32le(&optional, 108)? as usize;
    if directory_count > 16 || 112 + directory_count * 8 > optional.len() {
        return Err(invalid());
    }
    let data = reader.read(offset + 24 + optional_size as u64, count * 40)?;
    let mut sections = Vec::with_capacity(count);
    for data in data.chunks_exact(40) {
        let section = Section {
            rva: u32le(data, 12)?,
            size: u32le(data, 8)?.max(u32le(data, 16)?),
            offset: u32le(data, 20)?,
            raw_size: u32le(data, 16)?,
        };
        if u64::from(section.offset) + u64::from(section.raw_size) > reader.length
            || section.rva.checked_add(section.size).is_none()
        {
            return Err(invalid());
        }
        sections.push(section);
    }
    let pe = Pe { sections };
    let directory = |index: usize| -> Result<(u32, u32), Fault> {
        if index >= directory_count {
            return Ok((0, 0));
        }
        let start = u32le(&optional, 112 + index * 8)?;
        let size = u32le(&optional, 116 + index * 8)?;
        if (start == 0) != (size == 0) {
            return Err(invalid());
        }
        Ok((start, size))
    };
    let mut dependencies = BTreeSet::new();
    // IMAGE_IMPORT_DESCRIPTOR and IMAGE_DELAYLOAD_DESCRIPTOR. Delayed imports
    // remain dependencies: setup cannot know which OCR operation will trigger them.
    for (index, descriptor_size, name_offset) in [(1, 20, 12), (13, 32, 4)] {
        let (start, size) = directory(index)?;
        if start == 0 {
            continue;
        }
        if size < descriptor_size || size > (MAX_DEPENDENCIES as u32 + 1) * descriptor_size {
            return Err(fault(
                "native",
                "PE import directory exceeds 256 descriptors",
            ));
        }
        let data = pe.read(reader, start, size as usize)?;
        let mut terminated = false;
        for descriptor in data.chunks_exact(descriptor_size as usize) {
            check_cancel(reader.cancel)?;
            if descriptor.iter().all(|byte| *byte == 0) {
                terminated = true;
                break;
            }
            if index == 13 && u32le(descriptor, 0)? != 1 {
                return Err(fault("native", "PE delay imports must use RVA addressing"));
            }
            let name = pe.string(reader, u32le(descriptor, name_offset)?)?;
            add_dependency(&mut dependencies, dll_name(&name)?)?;
        }
        if !terminated {
            return Err(invalid());
        }
    }
    // Forwarded exports also name libraries, even without an import descriptor.
    let (start, size) = directory(0)?;
    if start != 0 {
        if size < 40 {
            return Err(invalid());
        }
        let end = start.checked_add(size).ok_or_else(invalid)?;
        let exports = pe.read(reader, start, 40)?;
        let count = u32le(&exports, 20)? as usize;
        if count > 65_536 {
            return Err(fault("native", "PE export table exceeds 65536 entries"));
        }
        if count != 0 {
            let functions = pe.read(reader, u32le(&exports, 28)?, count * 4)?;
            for entry in functions.chunks_exact(4) {
                check_cancel(reader.cancel)?;
                let rva = u32le(entry, 0)?;
                if rva >= start && rva < end {
                    let forward = pe.string(reader, rva)?;
                    let (module, symbol) = forward.rsplit_once('.').ok_or_else(invalid)?;
                    if symbol.is_empty() {
                        return Err(invalid());
                    }
                    let name = if module.to_ascii_lowercase().ends_with(".dll") {
                        dll_name(module)?
                    } else {
                        dll_name(&format!("{module}.dll"))?
                    };
                    add_dependency(&mut dependencies, name)?;
                }
            }
        }
    }
    Ok((dependencies.into_iter().collect(), Vec::new()))
}
