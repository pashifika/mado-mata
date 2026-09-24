//! Versioned configuration snapshots: standard stored ZIP, not package archives.
use crate::configuration::{
    self, Capture, Kind, MAX_BYTES, MAX_FILES, digest, io_fault, path_kind,
};
use crate::storage::{
    MAX_PATH_BYTES, decode, encode, filesystem_key, private_directory, read_bytes,
};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MANIFEST: &str = "manifest.json";
pub(crate) const MAX_MANIFEST: usize = 4 * 1024 * 1024;
const MAX_ARCHIVE: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotReceipt {
    pub path: String,
    pub generation: String,
    pub files: usize,
    pub bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub version: u32,
    pub root_present: bool,
    pub settings_present: bool,
    pub generation: String,
    #[serde(deserialize_with = "bounded_manifest_entries")]
    pub files: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub path: String,
    pub kind: Kind,
    pub length: usize,
    pub sha256: String,
}

fn bounded_manifest_entries<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Entry>, D::Error> {
    struct ObjectEntry(Entry);
    impl<'de> Deserialize<'de> for ObjectEntry {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Object;
            impl<'de> serde::de::Visitor<'de> for Object {
                type Value = ObjectEntry;
                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("a configuration entry object")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    map: A,
                ) -> Result<Self::Value, A::Error> {
                    Entry::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                        .map(ObjectEntry)
                }
            }
            deserializer.deserialize_map(Object)
        }
    }
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = Vec<Entry>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "at most {MAX_FILES} configuration entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut entries = Vec::new();
            while let Some(entry) = sequence.next_element::<ObjectEntry>()? {
                if entries.len() == MAX_FILES {
                    return Err(serde::de::Error::custom(
                        "snapshot manifest exceeds its entry bound",
                    ));
                }
                entries.push(entry.0);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_seq(Entries)
}

impl Manifest {
    pub(crate) fn new(capture: &Capture) -> Self {
        Self {
            version: 1,
            root_present: capture.root_present,
            settings_present: capture.settings_present,
            generation: capture.generation.clone(),
            files: capture
                .files
                .iter()
                .map(|(path, bytes)| Entry {
                    path: path.clone(),
                    kind: path_kind(path).expect("validated capture path"),
                    length: bytes.len(),
                    sha256: digest(bytes),
                })
                .collect(),
        }
    }

    pub(crate) fn check(&self) -> Result<(), Fault> {
        if self.version != 1
            || self.files.len() > MAX_FILES
            || (!self.root_present && !self.files.is_empty())
        {
            return Err(invalid(
                "unsupported snapshot manifest version, count or root state",
            ));
        }
        let mut names = BTreeSet::new();
        let mut aliases = BTreeMap::new();
        let mut total = 0usize;
        for entry in &self.files {
            if path_kind(&entry.path)? != entry.kind
                || entry.length > entry.kind.maximum()
                || !names.insert(entry.path.as_str())
                || !is_digest(&entry.sha256)
            {
                return Err(invalid("invalid manifest file identity, length or digest"));
            }
            configuration::check_aliases(&entry.path, &mut aliases)?;
            total = total
                .checked_add(entry.length)
                .ok_or_else(|| invalid("snapshot size overflow"))?;
            if total > MAX_BYTES {
                return Err(invalid("snapshot payload exceeds its byte budget"));
            }
        }
        if !is_digest(&self.generation) || self.settings_present != names.contains("settings.json")
        {
            return Err(invalid(
                "manifest settings presence or generation is invalid",
            ));
        }
        Ok(())
    }

    pub(crate) fn capture(&self, files: BTreeMap<String, Vec<u8>>) -> Result<Capture, Fault> {
        self.check()?;
        if files.len() != self.files.len() {
            return Err(invalid("snapshot file set differs from its manifest"));
        }
        for entry in &self.files {
            let bytes = files
                .get(&entry.path)
                .ok_or_else(|| invalid("snapshot is missing a declared file"))?;
            if bytes.len() != entry.length || digest(bytes) != entry.sha256 {
                return Err(invalid("snapshot file length or digest mismatch"));
            }
        }
        let capture = Capture::from_files(files, self.root_present)?;
        if capture.generation != self.generation
            || capture.settings_present != self.settings_present
        {
            return Err(invalid("snapshot generation mismatch"));
        }
        Ok(capture)
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn resolved_destination(path: &Path) -> Result<PathBuf, Fault> {
    let absolute =
        std::path::absolute(path).map_err(|error| io_fault("resolve backup destination", error))?;
    for ancestor in absolute.ancestors() {
        match ancestor.canonicalize() {
            Ok(mut resolved) => {
                for component in absolute
                    .strip_prefix(ancestor)
                    .expect("path ancestor")
                    .components()
                {
                    match component {
                        Component::Normal(name) => resolved.push(name),
                        Component::ParentDir => {
                            resolved.pop();
                        }
                        Component::CurDir => {}
                        _ => {
                            return Err(invalid(
                                "backup destination has an invalid path component",
                            ));
                        }
                    }
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_fault("resolve backup destination", error)),
        }
    }
    Err(invalid("backup destination has no existing ancestor"))
}

fn check_destination(root: &Path, directory: &Path) -> Result<(), Fault> {
    let root = filesystem_key(&resolved_destination(root)?.to_string_lossy());
    let directory = filesystem_key(&resolved_destination(directory)?.to_string_lossy());
    if let Ok(relative) = Path::new(&directory).strip_prefix(&root) {
        if let Some(Component::Normal(component)) = relative.components().next() {
            let component = component.to_string_lossy();
            if component == "tabs" || component == "profiles" || component.starts_with(".restore") {
                return Err(invalid(
                    "backup destination overlaps managed configuration or restore storage",
                ));
            }
        }
    }
    Ok(())
}

pub fn write(
    root: &Path,
    capture: Capture,
    destination: Option<&Path>,
) -> Result<SnapshotReceipt, Fault> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("system time precedes the Unix epoch"))?
        .as_secs();
    write_at(root, capture, destination, seconds)
}

fn write_at(
    root: &Path,
    capture: Capture,
    destination: Option<&Path>,
    seconds: u64,
) -> Result<SnapshotReceipt, Fault> {
    write_at_with(root, capture, destination, seconds, |_| Ok(()))
}

fn write_at_with(
    root: &Path,
    capture: Capture,
    destination: Option<&Path>,
    seconds: u64,
    before_publish: impl FnOnce(&Path) -> Result<(), Fault>,
) -> Result<SnapshotReceipt, Fault> {
    capture.check()?;
    if capture.files.is_empty() {
        return Err(Fault::new("NothingToBackUp", "Nothing to back up"));
    }
    let default = root.join("backups");
    let directory = destination.unwrap_or(&default);
    if destination.is_some() {
        if !directory.is_absolute() || directory.as_os_str().len() > MAX_PATH_BYTES {
            return Err(invalid(
                "backup destination must be an absolute path of at most 4096 bytes",
            ));
        }
        check_destination(root, directory)?;
    }
    private_directory(directory)?;
    let final_path = directory.join(format!("app.config.{seconds}"));
    let temporary = configuration::temporary(directory, "snapshot");
    let file = configuration::create_private_file(&temporary)?;
    let mut published = false;
    let result = (|| {
        let manifest = encode(&Manifest::new(&capture), MAX_MANIFEST)?;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .unix_permissions(0o600);
        let mut archive = ZipWriter::new(file);
        archive.start_file(MANIFEST, options).map_err(zip_fault)?;
        archive
            .write_all(&manifest)
            .map_err(|e| io_fault("write snapshot manifest", e))?;
        for (path, bytes) in &capture.files {
            archive.start_file(path, options).map_err(zip_fault)?;
            archive
                .write_all(bytes)
                .map_err(|e| io_fault("write snapshot payload", e))?;
        }
        let file = archive.finish().map_err(zip_fault)?;
        file.sync_all()
            .map_err(|e| io_fault("sync snapshot staging", e))?;
        drop(file);
        let staged = read(&temporary)?;
        if staged != capture {
            return Err(invalid("staged snapshot verification failed"));
        }
        before_publish(&temporary)?;
        configuration::publish_no_replace(&temporary, &final_path)
            .map_err(|e| io_fault("publish snapshot without replacement", e))?;
        published = true;
        configuration::sync_directory(directory)?;
        if read(&final_path)? != capture {
            return Err(invalid("published snapshot verification failed"));
        }
        Ok(SnapshotReceipt {
            path: final_path.to_string_lossy().into_owned(),
            generation: capture.generation.clone(),
            files: capture.files.len(),
            bytes: capture.files.values().map(Vec::len).sum(),
        })
    })();
    result.map_err(|mut fault| {
        if published {
            fault.context["published_path"] = json!(final_path);
            fault.context["publication_completed"] = json!(true);
        } else if let Err(error) = fs::remove_file(&temporary) {
            fault.context["temporary_cleanup"] =
                json!({"path": temporary, "kind": format!("{:?}", error.kind())});
        }
        fault
    })
}

/// Validate container structure before asking the ZIP crate to allocate its
/// directory table. Payload decoding is bounded and never uses extraction APIs.
pub fn read(path: &Path) -> Result<Capture, Fault> {
    let bytes = read_bytes(path, MAX_ARCHIVE)?;
    read_container(&bytes)
}

fn read_container(bytes: &[u8]) -> Result<Capture, Fault> {
    preflight(bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(zip_fault)?;
    let manifest_bytes = read_entry(&mut archive, MANIFEST, MAX_MANIFEST)?;
    let manifest: Manifest =
        decode(&manifest_bytes).map_err(|_| invalid("malformed snapshot manifest"))?;
    manifest.check()?;
    if archive.len() != manifest.files.len() + 1 {
        return Err(invalid("snapshot contains undeclared entries"));
    }
    let mut files = BTreeMap::new();
    for entry in &manifest.files {
        let bytes = read_entry(&mut archive, &entry.path, entry.length)?;
        if bytes.len() != entry.length || digest(&bytes) != entry.sha256 {
            return Err(invalid(
                "snapshot payload length or hash differs from manifest",
            ));
        }
        files.insert(entry.path.clone(), bytes);
    }
    manifest.capture(files)
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    maximum: usize,
) -> Result<Vec<u8>, Fault> {
    let mut file = archive.by_name(path).map_err(zip_fault)?;
    if !file.is_file()
        || file.is_symlink()
        || file.encrypted()
        || file.compression() != CompressionMethod::Stored
        || file.size() > maximum as u64
        || file.compressed_size() != file.size()
    {
        return Err(invalid(
            "snapshot entry uses unsupported features or exceeds its bound",
        ));
    }
    let mut bytes = Vec::with_capacity(file.size() as usize);
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_fault("read snapshot entry", e))?;
    if bytes.len() > maximum {
        return Err(invalid("snapshot entry exceeds its byte bound"));
    }
    Ok(bytes)
}

/// Supported v1 is deliberately narrow: no ZIP64, descriptors, extra fields,
/// comments, encryption, compression, directory/link entries, prefixes or gaps.
/// ZIP remains responsible for CRC and entry reads; this checks bounded framing
/// and rejects duplicates before ZipArchive's name-index can collapse them.
fn preflight(bytes: &[u8]) -> Result<(), Fault> {
    if bytes.len() < 22 || bytes.len() > MAX_ARCHIVE {
        return Err(invalid("snapshot archive size is invalid"));
    }
    let end = bytes.len() - 22;
    if u32_at(bytes, end)? != 0x0605_4b50
        || u16_at(bytes, end + 4)? != 0
        || u16_at(bytes, end + 6)? != 0
        || u16_at(bytes, end + 20)? != 0
    {
        return Err(invalid("unsupported ZIP footer or multidisk archive"));
    }
    let count = usize::from(u16_at(bytes, end + 10)?);
    let central_length = u32_at(bytes, end + 12)? as usize;
    let central_start = u32_at(bytes, end + 16)? as usize;
    if count == 0
        || count > MAX_FILES + 1
        || usize::from(u16_at(bytes, end + 8)?) != count
        || central_start.checked_add(central_length) != Some(end)
    {
        return Err(invalid("ZIP directory exceeds its bounds"));
    }
    let mut central = central_start;
    let mut local = 0usize;
    let mut names = BTreeSet::new();
    let mut aliases = BTreeMap::new();
    let mut total = 0usize;
    for _ in 0..count {
        if central.checked_add(46).is_none_or(|n| n > end) || u32_at(bytes, central)? != 0x0201_4b50
        {
            return Err(invalid("invalid ZIP central directory"));
        }
        let flags = u16_at(bytes, central + 8)?;
        let size = u32_at(bytes, central + 24)? as usize;
        let name_length = usize::from(u16_at(bytes, central + 28)?);
        let mode = u32_at(bytes, central + 38)?;
        if u16_at(bytes, central + 6)? > 20
            || flags & !0x0800 != 0
            || u16_at(bytes, central + 10)? != 0
            || u32_at(bytes, central + 20)? as usize != size
            || name_length == 0
            || name_length > 1024
            || u16_at(bytes, central + 30)? != 0
            || u16_at(bytes, central + 32)? != 0
            || u16_at(bytes, central + 34)? != 0
            || mode & 0x10 != 0
            || !matches!((mode >> 16) & 0o170000, 0 | 0o100000)
            || u32_at(bytes, central + 42)? as usize != local
        {
            return Err(invalid(
                "ZIP entry uses unsupported features, links or layout",
            ));
        }
        let name_end = central + 46 + name_length;
        let name_bytes = bytes
            .get(central + 46..name_end)
            .filter(|_| name_end <= end)
            .ok_or_else(|| invalid("truncated ZIP filename"))?;
        let name =
            std::str::from_utf8(name_bytes).map_err(|_| invalid("ZIP filenames must be UTF-8"))?;
        if !names.insert(name) {
            return Err(invalid("duplicate ZIP entry"));
        }
        let maximum = if name == MANIFEST {
            MAX_MANIFEST
        } else {
            configuration::check_aliases(name, &mut aliases)?;
            let maximum = path_kind(name)?.maximum();
            total = total
                .checked_add(size)
                .ok_or_else(|| invalid("ZIP payload size overflow"))?;
            if total > MAX_BYTES {
                return Err(invalid("ZIP payload exceeds its aggregate bound"));
            }
            maximum
        };
        if size > maximum
            || local
                .checked_add(30 + name_length)
                .is_none_or(|n| n > central_start)
            || u32_at(bytes, local)? != 0x0403_4b50
            || u16_at(bytes, local + 4)? != u16_at(bytes, central + 6)?
            || u16_at(bytes, local + 6)? != flags
            || u16_at(bytes, local + 8)? != 0
            || u32_at(bytes, local + 14)? != u32_at(bytes, central + 16)?
            || u32_at(bytes, local + 18)? as usize != size
            || u32_at(bytes, local + 22)? as usize != size
            || usize::from(u16_at(bytes, local + 26)?) != name_length
            || u16_at(bytes, local + 28)? != 0
            || bytes.get(local + 30..local + 30 + name_length) != Some(name_bytes)
        {
            return Err(invalid("ZIP local entry disagrees with its directory"));
        }
        local = local
            .checked_add(30 + name_length + size)
            .ok_or_else(|| invalid("ZIP offset overflow"))?;
        if local > central_start {
            return Err(invalid("overlapping ZIP entries"));
        }
        central = name_end;
    }
    if local != central_start || central != end || !names.contains(MANIFEST) {
        return Err(invalid("ZIP contains missing or unlisted data"));
    }
    Ok(())
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, Fault> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid("truncated ZIP header"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Fault> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated ZIP header"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}
fn invalid(message: &str) -> Fault {
    Fault::new("Snapshot", message)
}
fn zip_fault(error: zip::result::ZipError) -> Fault {
    // ZIP diagnostics can contain entry names; disclose only through the explicit action result.
    invalid("snapshot ZIP could not be read or written")
        .with_context(json!({"cause": error.to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::{capture, tests::Root};

    #[test]
    fn raw_roundtrip_collision_and_explicit_destination() {
        let root = Root::new();
        let destination = Root::new();
        root.put("settings.json", b"malformed\0raw");
        root.put("tabs/Closed/tab.config", br#"{"open":false}"#);
        let original = capture(&root.0).unwrap();
        let receipt = write_at(&root.0, original.clone(), Some(&destination.0), 42).unwrap();
        assert_eq!(
            Path::new(&receipt.path).file_name().unwrap(),
            "app.config.42"
        );
        assert_eq!(read(Path::new(&receipt.path)).unwrap(), original);
        let preserved = fs::read(&receipt.path).unwrap();
        assert!(write_at(&root.0, original, Some(&destination.0), 42).is_err());
        assert_eq!(fs::read(&receipt.path).unwrap(), preserved);
        assert!(!root.0.join("backups").exists());
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            archive
                .start_file(
                    *name,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Stored)
                        .unix_permissions(0o600),
                )
                .unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    #[test]
    fn refuses_malicious_features_and_manifest_disagreement_at_reader() {
        for name in [
            "../settings.json",
            "/settings.json",
            "tabs/A/../tab.config",
            "tabs/A/pkg/target.config/extra",
            "C:\\settings.json",
        ] {
            assert!(
                read_container(&archive(&[(MANIFEST, b"{}"), (name, b"x")])).is_err(),
                "{name}"
            );
        }
        let capture = Capture::from_files(
            BTreeMap::from([("settings.json".into(), b"{}".to_vec())]),
            true,
        )
        .unwrap();
        let manifest = encode(&Manifest::new(&capture), MAX_MANIFEST).unwrap();
        let valid = archive(&[(MANIFEST, &manifest), ("settings.json", b"{}")]);
        assert_eq!(read_container(&valid).unwrap(), capture);
        assert!(
            read_container(&archive(&[(MANIFEST, &manifest), ("settings.json", b"[]")])).is_err()
        );
        assert!(read_container(&archive(&[(MANIFEST, &manifest)])).is_err());
        assert!(
            read_container(&archive(&[
                (MANIFEST, &manifest),
                ("settings.json", b"{}"),
                ("profiles/extra.json", b"{}")
            ]))
            .is_err()
        );
        let central = u32_at(&valid, valid.len() - 6).unwrap() as usize;
        for (offset, value) in [
            (central + 8, 1u16),
            (central + 10, 8),
            (central + 30, 1),
            (valid.len() - 12, 5000),
        ] {
            let mut malicious = valid.clone();
            malicious[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            assert!(read_container(&malicious).is_err());
        }
        let mut linked = valid.clone();
        linked[central + 38..central + 42].copy_from_slice(&(0o120600u32 << 16).to_le_bytes());
        assert!(read_container(&linked).is_err());
        let mut oversized = valid;
        oversized[central + 24..central + 28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_container(&oversized).is_err());
    }

    #[test]
    fn refuses_container_aliases_and_duplicate_central_names() {
        let aliases = archive(&[
            (MANIFEST, b"{}"),
            ("tabs/A/tab.config", b"{}"),
            ("tabs/a/pkg/p.config", b"{}"),
        ]);
        assert!(read_container(&aliases).is_err());
        let mut duplicate = archive(&[
            (MANIFEST, b"{}"),
            ("profiles/a.json", b"a"),
            ("profiles/b.json", b"b"),
        ]);
        // Both the local and central names become the same, leaving CRCs valid.
        for index in 0..duplicate.len() - 15 {
            if duplicate[index..].starts_with(b"profiles/b.json") {
                duplicate[index + 9] = b'a';
            }
        }
        assert!(read_container(&duplicate).is_err());
    }

    #[test]
    fn staging_failure_reports_retained_owned_artifact() {
        let root = Root::new();
        let destination = Root::new();
        root.put("settings.json", b"preserve");
        let original = capture(&root.0).unwrap();
        let fault = write_at_with(
            &root.0,
            original.clone(),
            Some(&destination.0),
            51,
            |temporary| {
                // Simulate a failed operation whose owned staging cannot be unlinked
                // as a regular file. Cleanup must report, not recursively erase it.
                fs::remove_file(temporary).unwrap();
                configuration::create_private_directory(temporary).unwrap();
                Err(invalid("injected publication failure"))
            },
        )
        .unwrap_err();
        assert!(fault.context["temporary_cleanup"]["path"].is_string());
        assert!(!destination.0.join("app.config.51").exists());
        assert_eq!(capture(&root.0).unwrap(), original);
    }

    #[test]
    fn rejects_unknown_manifest_version_before_payload_use() {
        let capture = Capture::from_files(
            BTreeMap::from([("settings.json".into(), b"{}".to_vec())]),
            true,
        )
        .unwrap();
        let mut manifest = Manifest::new(&capture);
        manifest.version = 2;
        let manifest = encode(&manifest, MAX_MANIFEST).unwrap();
        assert!(
            read_container(&archive(&[(MANIFEST, &manifest), ("settings.json", b"{}")])).is_err()
        );
    }

    #[test]
    fn manifest_and_entries_require_objects_not_positional_arrays() {
        let capture = Capture::from_files(
            BTreeMap::from([("settings.json".into(), b"{}".to_vec())]),
            true,
        )
        .unwrap();
        let manifest = Manifest::new(&capture);
        let entry = &manifest.files[0];
        let positional_entry = json!([entry.path, entry.kind, entry.length, entry.sha256]);
        let positional_manifest = json!([1, true, true, manifest.generation, manifest.files]);
        let mut object = serde_json::to_value(&manifest).unwrap();
        object["files"] = json!([positional_entry]);
        for value in [positional_manifest, object] {
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(
                read_container(&archive(&[(MANIFEST, &bytes), ("settings.json", b"{}")])).is_err()
            );
        }
    }

    #[test]
    fn destinations_cannot_create_configuration_or_transaction_containers() {
        let root = Root::new();
        root.put("settings.json", b"preserve malformed bytes");
        let original = capture(&root.0).unwrap();
        for relative in [
            "tabs/Backup",
            "TABS/Backup",
            "profiles/Backup",
            ".restore-journal/Backup",
            "absent/../tabs/Backup",
        ] {
            let destination = root.0.join(relative);
            assert!(write_at(&root.0, original.clone(), Some(&destination), 42).is_err());
            assert!(!destination.exists());
        }
        assert!(!root.0.join("absent").exists());
        assert_eq!(capture(&root.0).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn destination_ancestor_alias_cannot_enter_configuration_storage() {
        let root = Root::new();
        let outside = Root::new();
        root.put("settings.json", b"preserve");
        std::os::unix::fs::symlink(&root.0, outside.0.join("alias")).unwrap();
        let original = capture(&root.0).unwrap();
        assert!(
            write_at(
                &root.0,
                original.clone(),
                Some(&outside.0.join("alias/tabs/Backup")),
                42
            )
            .is_err()
        );
        assert!(!root.0.join("tabs").exists());
        assert_eq!(capture(&root.0).unwrap(), original);
    }
}
