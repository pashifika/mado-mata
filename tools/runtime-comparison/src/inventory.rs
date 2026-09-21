use crate::host::resolve_options;
use crate::model::{Fault, Limits};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

const HELPER: &str = "@mado/helper";
const ORDER: &str = "@mado/order";
const VERSION: &str = "1.0.0";
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 240;
const MAX_FILES: usize = 65_536;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const JS_HELPER: &str = "import { first } from '@mado/order';\nexport function choose(priorities, available) { return first(priorities, available); }\nexport function fail() { throw new Error('approved helper failure'); }\n";
const JS_ORDER: &str = "export function first(priorities, available) {\n  for (const priority of priorities) if (available[priority] === true) return priority;\n  return null;\n}\n";
const LUA_HELPER: &str = "local order = require('@mado/order')\nreturn {\n  choose = function(priorities, available) return order.first(priorities, available) end,\n  fail = function() error('approved helper failure') end\n}\n";
const LUA_ORDER: &str = "return { first = function(priorities, available)\n  for _, priority in ipairs(priorities) do\n    if available[priority] == true then return priority end\n  end\n  return nil\nend }\n";
const HELPER_TYPES: &str = "export declare function choose(priorities: readonly string[], available: Readonly<Record<string, boolean>>): string | null;\nexport declare function fail(): never;\n";
const ORDER_TYPES: &str = "export declare function first(priorities: readonly string[], available: Readonly<Record<string, boolean>>): string | null;\n";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub module: String,
    pub function: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entries {
    pub readiness: Entry,
    pub workflow: Entry,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub identity: String,
    pub package_id: String,
    pub sources: BTreeMap<String, String>,
    pub assets: BTreeMap<String, Vec<u8>>,
    pub schema: Value,
    pub profiles: BTreeMap<String, Value>,
    pub entries: Entries,
    pub metadata: Value,
    pub source_maps: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    package_id: String,
    runtime: String,
    sdk: String,
    entry_contract: String,
    entries: Entries,
    sources: Vec<String>,
    schema: String,
    profiles: BTreeMap<String, String>,
    assets: BTreeMap<String, Asset>,
    source_maps: BTreeMap<String, String>,
    dependencies: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    format: String,
    width: usize,
    height: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    directory: bool,
    length: u64,
    modified: SystemTime,
    identity: (u64, u64, u64, u64),
}

struct Capture<'a> {
    files: BTreeMap<String, Vec<u8>>,
    stamps: BTreeMap<String, Stamp>,
    bytes: usize,
    file_limit: usize,
    byte_limit: usize,
    deadline: Instant,
    stop: Option<&'a AtomicBool>,
}

impl Inventory {
    pub fn capture(root: &Path, limits: &Limits) -> Result<Self, Fault> {
        Self::capture_with_stop(root, limits, None)
    }

    pub(crate) fn capture_with_stop(
        root: &Path,
        limits: &Limits,
        stop: Option<&AtomicBool>,
    ) -> Result<Self, Fault> {
        if limits.snapshot_files == 0
            || limits.snapshot_files > MAX_FILES
            || limits.snapshot_bytes == 0
            || limits.snapshot_bytes > MAX_BYTES
            || limits.duration_ms == 0
        {
            return Err(invalid(
                "snapshot bounds must be positive and within application ceilings",
            ));
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(limits.duration_ms))
            .ok_or_else(|| invalid("snapshot deadline overflows"))?;
        let root = checked_root(root)?;
        let mut capture = Capture {
            files: BTreeMap::new(),
            stamps: BTreeMap::new(),
            bytes: 0,
            file_limit: limits.snapshot_files,
            byte_limit: limits.snapshot_bytes,
            deadline,
            stop,
        };
        capture.collect(&root, "", 0)?;
        // A second bounded pass compares file identities, contents and directory membership.
        // Execution never reopens these paths after the capture completes.
        capture.verify(&root)?;
        let raw_manifest = capture
            .files
            .get("package.json")
            .ok_or_else(|| invalid("package.json is required"))?;
        let manifest: Manifest = serde_json::from_slice(raw_manifest)
            .map_err(|error| invalid(format!("invalid package.json: {error}")))?;
        validate_manifest(&manifest)?;
        let files: BTreeMap<_, _> = capture
            .files
            .iter()
            .map(|(path, bytes)| {
                (
                    path.clone(),
                    json!({"sha256": sha256(bytes), "bytes": bytes.len()}),
                )
            })
            .collect();
        let mut declared = BTreeSet::from(["package.json".to_owned()]);
        let mut sources = BTreeMap::new();
        for path in &manifest.sources {
            declare(&mut declared, path)?;
            let source = text_file(&mut capture.files, path)?;
            validate_source(path, &source, &manifest.runtime)?;
            sources.insert(path.clone(), source);
        }
        declare(&mut declared, &manifest.schema)?;
        let schema = json_file(&mut capture.files, &manifest.schema)?;
        let mut profiles = BTreeMap::new();
        for (id, path) in &manifest.profiles {
            portable_component(id)?;
            declare(&mut declared, path)?;
            let profile = json_file(&mut capture.files, path)?;
            resolve_options(&schema, &profile, &manifest.package_id)?;
            profiles.insert(id.clone(), profile);
        }
        let mut assets = BTreeMap::new();
        for (id, asset) in &manifest.assets {
            portable_component(id)?;
            declare(&mut declared, &asset.path)?;
            let bytes = capture
                .files
                .remove(&asset.path)
                .ok_or_else(|| invalid(format!("asset {id} is missing: {}", asset.path)))?;
            validate_asset(id, asset, &bytes)?;
            assets.insert(id.clone(), bytes);
        }
        let mut source_maps = BTreeMap::new();
        for (module, path) in &manifest.source_maps {
            if !sources.contains_key(module) {
                return Err(invalid(format!(
                    "source map refers to undeclared source: {module}"
                )));
            }
            declare(&mut declared, path)?;
            let map = text_file(&mut capture.files, path)?;
            validate_map(&map)?;
            source_maps.insert(module.clone(), map);
        }
        for path in capture.files.keys() {
            if !declared.contains(path) {
                return Err(invalid(format!("undeclared package file: {path}")));
            }
        }
        let (approved_sources, catalog) = catalog(&manifest)?;
        for (id, source) in approved_sources {
            capture.reserve(source.len())?;
            sources.insert(id, source);
        }
        if sources.len() + declared.len() - manifest.sources.len() > limits.snapshot_files {
            return Err(invalid(
                "approved dependency closure exceeds snapshot file bound",
            ));
        }
        let metadata = json!({
            "manifest": manifest,
            "runtime": manifest.runtime,
            "files": files,
            "catalog": catalog,
            "capture_limits": {"files": limits.snapshot_files, "bytes": limits.snapshot_bytes}
        });
        let mut inventory = Self {
            identity: String::new(),
            package_id: manifest.package_id,
            sources,
            assets,
            schema,
            profiles,
            entries: manifest.entries,
            metadata,
            source_maps,
        };
        inventory.refresh_identity()?;
        capture.check()?;
        Ok(inventory)
    }

    pub fn resolve(&self, from: &str, specifier: &str) -> Result<String, Fault> {
        let refused = |reason: &str, destination: Option<&str>| {
            Fault::new("ImportRefused", reason).with_context(json!({
                "inventory": self.identity,
                "requester": from,
                "specifier": specifier,
                "destination": destination,
                "chain": [from, specifier]
            }))
        };
        if !from.is_empty() && !self.sources.contains_key(from) {
            return Err(refused("requester is not a captured module", None));
        }
        if specifier.is_empty()
            || specifier.len() > MAX_PATH_BYTES
            || !specifier.is_ascii()
            || specifier.contains(['\\', ':', '?', '#', '%', '\0'])
            || specifier.starts_with('/')
        {
            return Err(refused("non-portable or external module specifier", None));
        }
        let catalog = self
            .metadata
            .get("catalog")
            .and_then(Value::as_object)
            .ok_or_else(|| refused("catalog metadata is absent", None))?;
        let owner = catalog.iter().find(|(_, item)| {
            item.get("entry").and_then(Value::as_str) == Some(from)
                || item.get("types").and_then(Value::as_str) == Some(from)
        });
        if specifier.starts_with('@') {
            let target = catalog
                .iter()
                .find(|(name, item)| {
                    name.as_str() == specifier
                        || item.get("entry").and_then(Value::as_str) == Some(specifier)
                        || item.get("types").and_then(Value::as_str) == Some(specifier)
                })
                .ok_or_else(|| refused("module is absent from the application catalog", None))?;
            let (name, item) = target;
            let permitted = if let Some((owner_name, owner_item)) = owner {
                owner_name == name
                    || owner_item
                        .get("dependencies")
                        .and_then(|deps| deps.get(name))
                        .is_some()
            } else {
                self.metadata
                    .pointer("/manifest/dependencies")
                    .and_then(|deps| deps.get(name))
                    .and_then(Value::as_str)
                    == Some(VERSION)
            };
            if !permitted {
                return Err(refused(
                    "requester has no approved dependency edge",
                    Some(specifier),
                ));
            }
            let target = if item.get("types").and_then(Value::as_str) == Some(specifier) {
                specifier
            } else {
                item.get("entry")
                    .and_then(Value::as_str)
                    .ok_or_else(|| refused("approved entry is unavailable", None))?
            };
            if !self.sources.contains_key(target) {
                return Err(refused("approved content is unavailable", Some(target)));
            }
            return Ok(target.to_owned());
        }
        if let Some((_, item)) = owner {
            // Catalog modules cannot import package code or escape their own immutable directory.
            let entry = item
                .get("entry")
                .and_then(Value::as_str)
                .ok_or_else(|| refused("approved entry is unavailable", None))?;
            let directory = entry
                .rsplit_once('/')
                .map(|(directory, _)| directory)
                .unwrap_or("");
            let leaf = specifier.strip_prefix("./").unwrap_or(specifier);
            if leaf.contains('/') || portable_component(leaf).is_err() {
                return Err(refused(
                    "approved module relative path escapes its catalog scope",
                    None,
                ));
            }
            let destination = format!("{directory}/{leaf}");
            if !self.sources.contains_key(&destination) {
                return Err(refused(
                    "approved relative module is unavailable",
                    Some(&destination),
                ));
            }
            return Ok(destination);
        }
        let directory = if specifier.starts_with("./") || specifier.starts_with("../") {
            from.rsplit_once('/')
                .map(|(directory, _)| directory)
                .unwrap_or("")
        } else {
            ""
        };
        let mut segments: Vec<&str> = if directory.is_empty() {
            Vec::new()
        } else {
            directory.split('/').collect()
        };
        for segment in specifier.split('/') {
            match segment {
                "." => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err(refused("module path escapes package root", None));
                    }
                }
                _ => {
                    portable_component(segment)
                        .map_err(|_| refused("invalid module path component", None))?;
                    segments.push(segment);
                }
            }
        }
        let mut destination = segments.join("/");
        if destination.is_empty() || destination.starts_with('@') {
            return Err(refused(
                "invalid normalized module destination",
                Some(&destination),
            ));
        }
        if !self.sources.contains_key(&destination)
            && self.metadata.get("runtime").and_then(Value::as_str) == Some("typescript")
            && destination.ends_with(".js")
        {
            let typed = format!("{}.ts", &destination[..destination.len() - 3]);
            if self.sources.contains_key(&typed) {
                destination = typed;
            }
        }
        if !self.sources.contains_key(&destination) {
            return Err(refused(
                "module is absent from captured package sources",
                Some(&destination),
            ));
        }
        Ok(destination)
    }

    pub fn source(&self, id: &str) -> Result<&str, Fault> {
        self.sources.get(id).map(String::as_str).ok_or_else(|| {
            Fault::new("ImportRefused", "source is absent from captured inventory")
                .with_context(json!({"inventory": self.identity, "module": id}))
        })
    }

    pub fn validate(&self) -> Result<(), Fault> {
        self.validate_contents()?;
        if self.identity != self.content_identity()? {
            return Err(invalid(
                "inventory content identity does not match captured inputs",
            ));
        }
        Ok(())
    }

    /// For trusted compiler output only; never exposed to package code.
    pub fn refresh_identity(&mut self) -> Result<(), Fault> {
        self.validate_contents()?;
        self.identity = self.content_identity()?;
        Ok(())
    }

    fn validate_contents(&self) -> Result<(), Fault> {
        let manifest: Manifest = serde_json::from_value(
            self.metadata
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid("inventory manifest is missing"))?,
        )
        .map_err(|error| invalid(format!("invalid inventory manifest: {error}")))?;
        validate_manifest(&manifest)?;
        if manifest.package_id != self.package_id {
            return Err(invalid(
                "inventory package identity disagrees with manifest",
            ));
        }
        let runtime = self
            .metadata
            .get("runtime")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("inventory runtime is missing"))?;
        if runtime != manifest.runtime
            && !(manifest.runtime == "typescript" && runtime == "javascript")
        {
            return Err(invalid(
                "inventory runtime is incompatible with declared runtime",
            ));
        }
        let (approved, expected_catalog) = catalog(&manifest)?;
        if self.metadata.get("catalog") != Some(&expected_catalog) {
            return Err(invalid("catalog identity is not application-approved"));
        }
        let mut names = BTreeSet::new();
        let mut total = 0usize;
        for (id, source) in &self.sources {
            if id.starts_with('@') {
                if approved.get(id) != Some(source) {
                    return Err(invalid(format!(
                        "catalog content is not application-approved: {id}"
                    )));
                }
            } else {
                portable_path(id)?;
                validate_source(id, source, runtime)?;
            }
            if !names.insert(id.to_ascii_lowercase()) {
                return Err(invalid(format!("case-colliding source: {id}")));
            }
            total = total
                .checked_add(source.len())
                .ok_or_else(|| invalid("inventory size overflows"))?;
        }
        for (id, source) in approved {
            if self.sources.get(&id) != Some(&source) {
                return Err(invalid(format!("approved closure member is missing: {id}")));
            }
        }
        for (name, entry) in [
            ("readiness", &self.entries.readiness),
            ("workflow", &self.entries.workflow),
        ] {
            validate_entry(entry)?;
            if entry.module.starts_with('@') || !self.sources.contains_key(&entry.module) {
                return Err(invalid(format!(
                    "{name} entry module is missing: {}",
                    entry.module
                )));
            }
        }
        if self.profiles.len() != manifest.profiles.len()
            || self.assets.len() != manifest.assets.len()
        {
            return Err(invalid(
                "inventory profile or asset declarations disagree with manifest",
            ));
        }
        for id in manifest.profiles.keys() {
            let profile = self
                .profiles
                .get(id)
                .ok_or_else(|| invalid(format!("profile is missing: {id}")))?;
            resolve_options(&self.schema, profile, &self.package_id)?;
        }
        for (id, asset) in &manifest.assets {
            let bytes = self
                .assets
                .get(id)
                .ok_or_else(|| invalid(format!("asset is missing: {id}")))?;
            validate_asset(id, asset, bytes)?;
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| invalid("inventory size overflows"))?;
        }
        for (module, map) in &self.source_maps {
            if !self.sources.contains_key(module) {
                return Err(invalid(format!("source map module is missing: {module}")));
            }
            validate_map(map)?;
            total = total
                .checked_add(map.len())
                .ok_or_else(|| invalid("inventory size overflows"))?;
        }
        let file_limit =
            self.metadata
                .pointer("/capture_limits/files")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0 && *value <= MAX_FILES as u64)
                .ok_or_else(|| invalid("inventory file bound is invalid"))? as usize;
        let byte_limit =
            self.metadata
                .pointer("/capture_limits/bytes")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0 && *value <= MAX_BYTES as u64)
                .ok_or_else(|| invalid("inventory byte bound is invalid"))? as usize;
        let mut metadata_bytes = BoundedWriter {
            count: 0,
            limit: byte_limit.saturating_sub(total),
        };
        for value in [&self.schema, &self.metadata] {
            serde_json::to_writer(&mut metadata_bytes, value)
                .map_err(|_| invalid("inventory metadata exceeds captured byte bound"))?;
        }
        serde_json::to_writer(&mut metadata_bytes, &self.profiles)
            .map_err(|_| invalid("inventory profiles exceed captured byte bound"))?;
        if self.sources.len() + self.source_maps.len() + self.assets.len() + self.profiles.len() + 2
            > file_limit
            || total > byte_limit
        {
            return Err(invalid("inventory exceeds captured bounds"));
        }
        Ok(())
    }

    fn content_identity(&self) -> Result<String, Fault> {
        #[derive(Serialize)]
        struct Content<'a> {
            contract: &'static str,
            package_id: &'a str,
            sources: &'a BTreeMap<String, String>,
            assets: &'a BTreeMap<String, Vec<u8>>,
            schema: &'a Value,
            profiles: &'a BTreeMap<String, Value>,
            entries: &'a Entries,
            metadata: &'a Value,
            source_maps: &'a BTreeMap<String, String>,
        }
        let content = Content {
            contract: "mado-inventory-v1",
            package_id: &self.package_id,
            sources: &self.sources,
            assets: &self.assets,
            schema: &self.schema,
            profiles: &self.profiles,
            entries: &self.entries,
            metadata: &self.metadata,
            source_maps: &self.source_maps,
        };
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, &content)
            .map_err(|error| invalid(format!("cannot identify inventory: {error}")))?;
        Ok(format!("sha256:{:x}", writer.0.finalize()))
    }
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct BoundedWriter {
    count: usize,
    limit: usize,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.count) {
            return Err(std::io::Error::other("inventory byte bound exceeded"));
        }
        self.count += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture<'_> {
    fn check(&self) -> Result<(), Fault> {
        if self.stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err(Fault::new("Cancelled", "Stop requested during inventory capture"));
        }
        if Instant::now() >= self.deadline {
            return Err(Fault::new("Timeout", "inventory capture deadline exceeded"));
        }
        Ok(())
    }

    fn reserve(&mut self, bytes: usize) -> Result<(), Fault> {
        self.check()?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| invalid("snapshot size overflows"))?;
        if self.bytes > self.byte_limit {
            return Err(invalid("snapshot byte limit exceeded"));
        }
        Ok(())
    }

    fn collect(&mut self, root: &Path, id: &str, depth: usize) -> Result<(), Fault> {
        self.check()?;
        if depth > MAX_DEPTH
            || self.stamps.len() >= self.file_limit.saturating_mul(2).saturating_add(1)
        {
            return Err(invalid("snapshot directory/depth bound exceeded"));
        }
        let path = root.join(id);
        let before = checked_metadata(&path)?;
        let original = stamp(&before)?;
        if before.is_dir() {
            self.stamps.insert(id.to_owned(), original);
            let mut names = BTreeSet::new();
            for entry in fs::read_dir(&path).map_err(|error| io_fault(id, error))? {
                self.check()?;
                let entry = entry.map_err(|error| io_fault(id, error))?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid("non-UTF-8 package path"))?;
                portable_component(&name)?;
                if !names.insert(name.to_ascii_lowercase()) {
                    return Err(invalid(format!(
                        "case-colliding package entry: {id}/{name}"
                    )));
                }
                let child = if id.is_empty() {
                    name
                } else {
                    format!("{id}/{name}")
                };
                portable_path(&child)?;
                self.collect(root, &child, depth + 1)?;
            }
            if self.stamps.get(id) != Some(&stamp(&checked_metadata(&path)?)?) {
                return Err(changed(id));
            }
        } else {
            if self.files.len() >= self.file_limit {
                return Err(invalid("snapshot file limit exceeded"));
            }
            let size = usize::try_from(before.len())
                .map_err(|_| invalid("snapshot file size overflows"))?;
            self.reserve(size)?;
            let bytes = read_stable(&path, id, &original, size, self.deadline)?;
            self.stamps.insert(id.to_owned(), original);
            self.files.insert(id.to_owned(), bytes);
        }
        Ok(())
    }

    fn verify(&self, root: &Path) -> Result<(), Fault> {
        self.check()?;
        checked_root(root)?;
        for (id, original) in &self.stamps {
            self.check()?;
            let path = root.join(id);
            if stamp(&checked_metadata(&path)?)? != *original {
                return Err(changed(id));
            }
            if original.directory {
                let mut count = 0usize;
                for entry in fs::read_dir(&path).map_err(|error| io_fault(id, error))? {
                    self.check()?;
                    let entry = entry.map_err(|error| io_fault(id, error))?;
                    let name = entry.file_name().into_string().map_err(|_| changed(id))?;
                    let child = if id.is_empty() {
                        name
                    } else {
                        format!("{id}/{name}")
                    };
                    if !self.stamps.contains_key(&child) {
                        return Err(changed(&child));
                    }
                    count += 1;
                    if count > self.file_limit.saturating_mul(2) {
                        return Err(invalid("snapshot directory membership exceeds bound"));
                    }
                }
            } else {
                let expected = self.files.get(id).ok_or_else(|| changed(id))?;
                let bytes = read_stable(&path, id, original, expected.len(), self.deadline)?;
                if bytes != *expected {
                    return Err(changed(id));
                }
            }
        }
        for (id, original) in &self.stamps {
            self.check()?;
            if stamp(&checked_metadata(&root.join(id))?)? != *original {
                return Err(changed(id));
            }
        }
        checked_root(root)?;
        Ok(())
    }
}

fn checked_root(root: &Path) -> Result<PathBuf, Fault> {
    let absolute = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| io_fault("package root", error))?
            .join(root)
    };
    if absolute
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(invalid("package root may not contain parent traversal"));
    }
    // The operator's root may live below an OS path alias. Resolve that trusted
    // location once; links inside the captured package remain forbidden.
    if !checked_metadata(&absolute)?.is_dir() {
        return Err(invalid("package root is not a directory"));
    }
    let canonical = absolute
        .canonicalize()
        .map_err(|error| io_fault("package root", error))?;
    if !checked_metadata(&canonical)?.is_dir() {
        return Err(invalid("canonical package root is not a directory"));
    }
    Ok(canonical)
}

fn checked_metadata(path: &Path) -> Result<Metadata, Fault> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package root");
    let metadata = fs::symlink_metadata(path).map_err(|error| io_fault(name, error))?;
    if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
        return Err(
            invalid("package paths must be regular files or directories, never links")
                .with_context(json!({"path": name})),
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(
                invalid("package reparse paths are forbidden").with_context(json!({"path": name}))
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.is_file() && metadata.nlink() != 1 {
            return Err(invalid("package hard-linked files are forbidden")
                .with_context(json!({"path": name})));
        }
    }
    Ok(metadata)
}

fn stamp(metadata: &Metadata) -> Result<Stamp, Fault> {
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        )
    };
    #[cfg(windows)]
    let identity = {
        use std::os::windows::fs::MetadataExt;
        (
            metadata.creation_time(),
            metadata.last_write_time(),
            metadata.file_attributes() as u64,
            0,
        )
    };
    #[cfg(not(any(unix, windows)))]
    return Err(invalid(
        "snapshot file identity is unsupported on this platform",
    ));
    #[cfg(any(unix, windows))]
    Ok(Stamp {
        directory: metadata.is_dir(),
        length: metadata.len(),
        modified: metadata
            .modified()
            .map_err(|error| io_fault("file timestamp", error))?,
        identity,
    })
}

fn read_stable(
    path: &Path,
    id: &str,
    original: &Stamp,
    size: usize,
    deadline: Instant,
) -> Result<Vec<u8>, Fault> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x100 | 0x4); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000 | 0x800); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000); // FILE_SHARE_READ, OPEN_REPARSE_POINT
    }
    let mut file: File = options.open(path).map_err(|error| io_fault(id, error))?;
    let opened = file.metadata().map_err(|error| io_fault(id, error))?;
    if !opened.is_file() || stamp(&opened)? != *original {
        return Err(changed(id));
    }
    #[cfg(windows)]
    let windows_identity = windows_file_identity(&file)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| invalid("snapshot allocation refused"))?;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err(Fault::new(
                "Timeout",
                "inventory file read deadline exceeded",
            ));
        }
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_fault(id, error))?;
        if count == 0 {
            break;
        }
        if count > size.saturating_sub(bytes.len()) {
            return Err(changed(id));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if bytes.len() != size
        || stamp(&file.metadata().map_err(|error| io_fault(id, error))?)? != *original
        || stamp(&checked_metadata(path)?)? != *original
    {
        return Err(changed(id));
    }
    #[cfg(windows)]
    if windows_file_identity(&file)? != windows_identity {
        return Err(changed(id));
    }
    Ok(bytes)
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<(u32, u32, u32), Fault> {
    use std::os::windows::io::AsRawHandle;
    // BY_HANDLE_FILE_INFORMATION: DWORD fields and three FILETIME pairs.
    #[repr(C)]
    #[derive(Default)]
    struct FileInformation {
        attributes: u32,
        creation: [u32; 2],
        access: [u32; 2],
        write: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            info: *mut FileInformation,
        ) -> i32;
    }
    let mut info = FileInformation::default();
    // SAFETY: file owns a live non-pipe handle; info is a writable, correctly laid-out buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io_fault("file identity", std::io::Error::last_os_error()));
    }
    if info.links != 1 || info.attributes & 0x400 != 0 {
        return Err(invalid(
            "package hard links and reparse points are forbidden",
        ));
    }
    Ok((info.volume, info.index_high, info.index_low))
}

fn validate_manifest(manifest: &Manifest) -> Result<(), Fault> {
    portable_component(&manifest.package_id)?;
    if manifest.version != 1
        || !matches!(
            manifest.runtime.as_str(),
            "javascript" | "lua" | "typescript"
        )
        || manifest.sdk != "mado-host-v1"
        || manifest.entry_contract != "ready-string-v1"
    {
        return Err(invalid("unsupported package/runtime/SDK/entry contract"));
    }
    if manifest.sources.is_empty() || manifest.profiles.is_empty() {
        return Err(invalid("package sources and profiles must be nonempty"));
    }
    validate_entry(&manifest.entries.readiness)?;
    validate_entry(&manifest.entries.workflow)?;
    for entry in [&manifest.entries.readiness, &manifest.entries.workflow] {
        if entry.module.ends_with(".d.ts") {
            return Err(invalid(
                "entry modules must be executable source, not declarations",
            ));
        }
        if !manifest.sources.contains(&entry.module) {
            return Err(invalid(format!(
                "entry module is not declared: {}",
                entry.module
            )));
        }
    }
    for (name, version) in &manifest.dependencies {
        if name != HELPER || version != VERSION {
            return Err(Fault::new(
                "ImportRefused",
                "dependency is not in the application-approved catalog",
            )
            .with_context(
                json!({"requester": "package.json", "specifier": name, "version": version}),
            ));
        }
    }
    Ok(())
}

fn validate_entry(entry: &Entry) -> Result<(), Fault> {
    portable_path(&entry.module)?;
    let mut chars = entry.function.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        || entry.function.len() > 128
    {
        return Err(invalid("entry function must be an ASCII identifier"));
    }
    Ok(())
}

fn validate_source(path: &str, source: &str, runtime: &str) -> Result<(), Fault> {
    let extension_allowed = match runtime {
        "javascript" => path.ends_with(".js") || path.ends_with(".d.ts"),
        "lua" => path.ends_with(".lua"),
        "typescript" => path.ends_with(".ts") || path.ends_with(".js"),
        _ => false,
    };
    if !extension_allowed || source.contains('\0') || source.starts_with('\u{1b}') {
        return Err(invalid(format!("incompatible or bytecode source: {path}")));
    }
    Ok(())
}

fn validate_asset(id: &str, asset: &Asset, bytes: &[u8]) -> Result<(), Fault> {
    if asset.format == "json" {
        if asset.width != 0 || asset.height != 0 {
            return Err(invalid(format!(
                "JSON asset {id} must declare zero pixel dimensions"
            )));
        }
        serde_json::from_slice::<Value>(bytes)
            .map_err(|error| invalid(format!("JSON asset {id} is invalid: {error}")))?;
        return Ok(());
    }
    if asset.width == 0 || asset.height == 0 || asset.width > 16_384 || asset.height > 16_384 {
        return Err(invalid(format!("asset {id} has invalid pixel dimensions")));
    }
    match asset.format.as_str() {
        "raw-rgba8" => {
            let length = asset
                .width
                .checked_mul(asset.height)
                .and_then(|pixels| pixels.checked_mul(4));
            if length != Some(bytes.len()) {
                return Err(invalid(format!(
                    "asset {id} has invalid raw-rgba8 content length"
                )));
            }
        }
        "png" => {
            // The facade performs full decoding; capture validates the declared pixel contract.
            if bytes.len() < 45
                || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
                || &bytes[8..16] != b"\0\0\0\rIHDR"
            {
                return Err(invalid(format!("asset {id} has no valid PNG header")));
            }
            let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;
            let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as usize;
            let supported = matches!(
                (bytes[25], bytes[24]),
                (0, 1 | 2 | 4 | 8 | 16) | (2, 8 | 16) | (3, 1 | 2 | 4 | 8) | (4 | 6, 8 | 16)
            );
            if width != asset.width
                || height != asset.height
                || !supported
                || bytes[26] != 0
                || bytes[27] != 0
                || bytes[28] > 1
            {
                return Err(invalid(format!("asset {id} has incompatible PNG metadata")));
            }
        }
        _ => return Err(invalid(format!("asset {id} has an unsupported format"))),
    }
    Ok(())
}

fn validate_map(map: &str) -> Result<(), Fault> {
    let value: Value = serde_json::from_str(map)
        .map_err(|error| invalid(format!("invalid source map: {error}")))?;
    if value.get("version").and_then(Value::as_u64) != Some(3)
        || !value.get("sources").is_some_and(Value::is_array)
        || !value.get("mappings").is_some_and(Value::is_string)
    {
        return Err(invalid(
            "source map must use version 3 sources and mappings",
        ));
    }
    if value
        .get("sourceRoot")
        .is_some_and(|root| root.as_str() != Some(""))
    {
        return Err(invalid(
            "source maps may not select an external source root",
        ));
    }
    for source in value["sources"]
        .as_array()
        .ok_or_else(|| invalid("source map sources are missing"))?
    {
        portable_path(
            source
                .as_str()
                .ok_or_else(|| invalid("source map source must be a string"))?,
        )?;
    }
    Ok(())
}

fn catalog(manifest: &Manifest) -> Result<(BTreeMap<String, String>, Value), Fault> {
    let mut sources = BTreeMap::new();
    let mut records = serde_json::Map::new();
    if !manifest.dependencies.contains_key(HELPER) {
        return Ok((sources, Value::Object(records)));
    }
    let lua = manifest.runtime == "lua";
    let extension = if lua { "lua" } else { "js" };
    for (name, source, types) in [
        (
            HELPER,
            if lua { LUA_HELPER } else { JS_HELPER },
            HELPER_TYPES,
        ),
        (ORDER, if lua { LUA_ORDER } else { JS_ORDER }, ORDER_TYPES),
    ] {
        let entry = format!("{name}@{VERSION}/index.{extension}");
        let type_id = format!("{name}@{VERSION}/index.d.ts");
        sources.insert(entry.clone(), source.to_owned());
        let mut dependencies = BTreeMap::new();
        if name == HELPER {
            dependencies.insert(ORDER, format!("{ORDER}@{VERSION}/index.{extension}"));
        }
        let mut record = json!({
            "version": VERSION,
            "language": if lua { "lua" } else { "javascript" },
            "entry": entry,
            "sha256": sha256(source.as_bytes()),
            "dependencies": dependencies
        });
        if !lua {
            sources.insert(type_id.clone(), types.to_owned());
            record["types"] = json!(type_id);
            record["types_sha256"] = json!(sha256(types.as_bytes()));
        }
        records.insert(name.to_owned(), record);
    }
    Ok((sources, Value::Object(records)))
}

fn portable_component(component: &str) -> Result<(), Fault> {
    let base = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && base.as_bytes()[3].is_ascii_digit()
            && base.as_bytes()[3] != b'0');
    if component.is_empty()
        || component.len() > 128
        || component.starts_with('.')
        || component.ends_with('.')
        || !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        || reserved
        || component.eq_ignore_ascii_case("node_modules")
    {
        return Err(invalid(format!("non-portable package name: {component}")));
    }
    Ok(())
}

fn portable_path(path: &str) -> Result<(), Fault> {
    if path.len() > MAX_PATH_BYTES || path.split('/').count() > MAX_DEPTH {
        return Err(invalid("package path exceeds portable bounds"));
    }
    for component in path.split('/') {
        portable_component(component)?;
    }
    Ok(())
}

fn declare(declared: &mut BTreeSet<String>, path: &str) -> Result<(), Fault> {
    portable_path(path)?;
    if !declared.insert(path.to_owned()) {
        return Err(invalid(format!(
            "package file is declared more than once: {path}"
        )));
    }
    Ok(())
}

fn text_file(files: &mut BTreeMap<String, Vec<u8>>, path: &str) -> Result<String, Fault> {
    let bytes = files
        .remove(path)
        .ok_or_else(|| invalid(format!("declared file is missing: {path}")))?;
    String::from_utf8(bytes).map_err(|error| invalid(format!("file is not UTF-8: {path}: {error}")))
}

fn json_file(files: &mut BTreeMap<String, Vec<u8>>, path: &str) -> Result<Value, Fault> {
    serde_json::from_str(&text_file(files, path)?)
        .map_err(|error| invalid(format!("invalid JSON: {path}: {error}")))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn invalid(message: impl Into<String>) -> Fault {
    Fault::new("Inventory", message)
}
fn changed(id: &str) -> Fault {
    invalid("package changed during bounded capture").with_context(json!({"path": id}))
}
fn io_fault(id: &str, error: std::io::Error) -> Fault {
    invalid("package file operation failed")
        .with_context(json!({"path": id, "cause": error.to_string()}))
}
