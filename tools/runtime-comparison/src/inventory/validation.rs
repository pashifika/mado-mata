use super::{
    Asset, Entries, Entry, Inventory, MAX_BYTES, MAX_DEPTH, MAX_FILES, MAX_PATH_BYTES, Manifest,
    TargetDeclaration, VERSION, invalid,
};
use crate::host::resolve_options;
use crate::model::Fault;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

const HELPER: &str = "@mado/helper";
const ORDER: &str = "@mado/order";
const JS_HELPER: &str = "import { first } from '@mado/order';\nexport function choose(priorities, available) { return first(priorities, available); }\nexport function fail() { throw new Error('approved helper failure'); }\n";
const JS_ORDER: &str = "export function first(priorities, available) {\n  for (const priority of priorities) if (available[priority] === true) return priority;\n  return null;\n}\n";
const LUA_HELPER: &str = "local order = require('@mado/order')\nreturn {\n  choose = function(priorities, available) return order.first(priorities, available) end,\n  fail = function() error('approved helper failure') end\n}\n";
const LUA_ORDER: &str = "return { first = function(priorities, available)\n  for _, priority in ipairs(priorities) do\n    if available[priority] == true then return priority end\n  end\n  return nil\nend }\n";
const HELPER_TYPES: &str = "export declare function choose(priorities: readonly string[], available: Readonly<Record<string, boolean>>): string | null;\nexport declare function fail(): never;\n";
const ORDER_TYPES: &str = "export declare function first(priorities: readonly string[], available: Readonly<Record<string, boolean>>): string | null;\n";

impl Inventory {
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

impl TargetDeclaration {
    pub fn validate(&self) -> Result<(), Fault> {
        portable_component(&self.id).map_err(|_| {
            invalid("target ID must be a portable component of at most 128 bytes")
                .with_context(json!({"path":"package.json","field":"target.id"}))
        })?;
        if self.window_title.as_ref().is_some_and(|title| {
            title.is_empty() || title.len() > 512 || title.chars().any(char::is_control)
        }) {
            return Err(invalid(
                "target window title must be nonempty, at most 512 bytes and free of controls",
            )
            .with_context(json!({"path":"package.json","field":"target.window_title"})));
        }
        if self.macos.as_ref().is_some_and(|macos| {
            macos.bundle_id.is_empty()
                || macos.bundle_id.len() > 255
                || !macos
                    .bundle_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
        }) {
            return Err(invalid(
                "target macOS bundle ID must be 1 to 255 ASCII letters, digits, periods or hyphens",
            )
            .with_context(json!({"path":"package.json","field":"target.macos.bundle_id"})));
        }
        Ok(())
    }

    pub fn identity(&self) -> Result<String, Fault> {
        self.validate()?;
        #[derive(Serialize)]
        struct Content<'a> {
            contract: &'static str,
            declaration: &'a TargetDeclaration,
        }
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(
            &mut writer,
            &Content {
                contract: "mado-target-declaration-v1",
                declaration: self,
            },
        )
        .map_err(|error| invalid(format!("cannot identify target declaration: {error}")))?;
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

pub(super) fn validate_manifest(manifest: &Manifest) -> Result<(), Fault> {
    portable_component(&manifest.package_id)?;
    if let Some(target) = &manifest.target {
        target.validate()?;
    }
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

pub(super) fn validate_source(path: &str, source: &str, runtime: &str) -> Result<(), Fault> {
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

pub(super) fn validate_asset(id: &str, asset: &Asset, bytes: &[u8]) -> Result<(), Fault> {
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

pub(super) fn validate_map(map: &str) -> Result<(), Fault> {
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

pub(super) fn catalog(manifest: &Manifest) -> Result<(BTreeMap<String, String>, Value), Fault> {
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

pub(super) fn portable_component(component: &str) -> Result<(), Fault> {
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

pub(super) fn portable_path(path: &str) -> Result<(), Fault> {
    if path.len() > MAX_PATH_BYTES || path.split('/').count() > MAX_DEPTH {
        return Err(invalid("package path exceeds portable bounds"));
    }
    for component in path.split('/') {
        portable_component(component)?;
    }
    Ok(())
}

pub(super) fn declare(declared: &mut BTreeSet<String>, path: &str) -> Result<(), Fault> {
    portable_path(path)?;
    if !declared.insert(path.to_owned()) {
        return Err(invalid(format!(
            "package file is declared more than once: {path}"
        )));
    }
    Ok(())
}

pub(super) fn text_file(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<String, Fault> {
    let bytes = files
        .remove(path)
        .ok_or_else(|| invalid(format!("declared file is missing: {path}")))?;
    String::from_utf8(bytes).map_err(|error| invalid(format!("file is not UTF-8: {path}: {error}")))
}

pub(super) fn json_file(files: &mut BTreeMap<String, Vec<u8>>, path: &str) -> Result<Value, Fault> {
    serde_json::from_str(&text_file(files, path)?)
        .map_err(|error| invalid(format!("invalid JSON: {path}: {error}")))
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
