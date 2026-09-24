//! Machine-local intent and metadata checks only; no process or native authority.
use crate::storage::{MAX_PATH_BYTES, validate_id, validate_internal_name, validate_package_id};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::{Fault, identity};
use plist::stream::{Event, Reader};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

pub(crate) const TARGET_VERSION: u32 = 1;
pub(crate) const MAX_TARGET_BYTES: usize = 64 * 1024;
pub const MAX_TARGET_REVISION: u64 = 9_007_199_254_740_991;
const MAX_METADATA_BYTES: usize = 64 * 1024;

// Like the snapshot entry visitor, require JSON objects rather than serde's
// alternate positional-struct representation. The derived map decoder also
// refuses duplicate/unknown fields, including in nested request objects.
macro_rules! object {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
        pub struct $name { $(pub $field: $ty),* }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Fields {
                    $(#[serde(deserialize_with = "required")] $field: $ty),*
                }
                struct Object;
                impl<'de> serde::de::Visitor<'de> for Object {
                    type Value = $name;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str(concat!(stringify!($name), " object"))
                    }
                    fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                        let fields = Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                        Ok($name { $($field: fields.$field),* })
                    }
                }
                deserializer.deserialize_map(Object)
            }
        }
    };
}

fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    T::deserialize(d)
}

object!(TargetLocation {
    kind: String,
    path: String
});
object!(TargetInputPolicy {
    route: String,
    focus: String,
    pointer_mode: Option<String>,
    click_hold_ms: u64,
});
object!(TargetConfiguration {
    platform: String,
    game: TargetLocation,
    launcher: Option<TargetLocation>,
    arguments: Vec<String>,
    working_directory: Option<String>,
    window_title: String,
    input: TargetInputPolicy,
});
object!(ResolvedLocation {
    path: String,
    executable: String
});
object!(TargetResolution {
    game: ResolvedLocation,
    launcher: Option<ResolvedLocation>,
    working_directory: Option<String>,
});
object!(TargetBinding {
    id: String,
    package_id: String,
    target_id: String,
    declaration_identity: String,
    configuration: TargetConfiguration,
    resolution: TargetResolution,
});
object!(TargetRecord {
    version: u32,
    internal_name: String,
    package_id: String,
    revision: u64,
    binding: Option<TargetBinding>,
});
object!(TargetExpectation { revision: u64, binding_id: Option<String> });
object!(TargetCheck {
    configuration_identity: String,
    resolution: TargetResolution,
    previous_resolution: Option<TargetResolution>,
    resolution_changed: bool,
});

impl TargetConfiguration {
    /// Portable shape validation; deliberately does not inspect the filesystem.
    pub fn validate(&self) -> Result<(), Fault> {
        if self.platform != "macos" {
            return Err(configuration_fault(
                "platform",
                "only macos target configuration is supported",
            ));
        }
        self.game.validate("game")?;
        if let Some(launcher) = &self.launcher {
            launcher.validate("launcher")?;
        }
        if let Some(directory) = &self.working_directory {
            absolute_path(directory, "working_directory")?;
        }
        text(&self.window_title, 512, false, "window_title")?;
        if self.arguments.len() > 32 {
            return Err(configuration_fault(
                "arguments",
                "at most 32 literal arguments are supported",
            ));
        }
        let mut total = 0;
        for argument in &self.arguments {
            text(argument, 1024, true, "arguments")?;
            total += argument.len();
        }
        if total > 8192 {
            return Err(configuration_fault(
                "arguments",
                "literal arguments exceed 8192 total bytes",
            ));
        }
        let policy = &self.input;
        let supported = match policy.route.as_str() {
            "system" => policy.focus == "require_focused" && policy.pointer_mode.is_none(),
            "process_directed" => match policy.pointer_mode.as_deref() {
                Some("core_graphics") => {
                    matches!(policy.focus.as_str(), "preserve" | "require_focused")
                }
                Some("appkit_background") => policy.focus == "preserve",
                _ => false,
            },
            _ => false,
        };
        if !supported || policy.click_hold_ms > 1000 {
            return Err(configuration_fault(
                "input",
                "unsupported input policy combination or click hold",
            ));
        }
        Ok(())
    }

    pub fn validate_declaration(&self, declaration: &TargetDeclaration) -> Result<(), Fault> {
        self.validate()?;
        declaration.validate()?;
        if declaration
            .window_title
            .as_ref()
            .is_some_and(|title| title != &self.window_title)
        {
            return Err(configuration_fault(
                "window_title",
                "exact title conflicts with the package declaration",
            ));
        }
        Ok(())
    }

    pub fn identity(&self) -> Result<String, Fault> {
        self.validate()?;
        identity(self)
    }

    pub fn resolve(&self, declaration: &TargetDeclaration) -> Result<TargetResolution, Fault> {
        self.validate_declaration(declaration)?;
        if !cfg!(unix) {
            return Err(Fault::new(
                "TargetPlatform",
                "executable metadata checks require a Unix host",
            ));
        }
        let game = resolve_location(&self.game, "game")?;
        let launcher = self
            .launcher
            .as_ref()
            .map(|location| resolve_location(location, "launcher"))
            .transpose()?;
        let working_directory = self
            .working_directory
            .as_ref()
            .map(|path| {
                let path = canonical(path, "working_directory")?;
                if !fs::metadata(&path)
                    .map_err(|_| metadata_fault("working_directory", "directory"))?
                    .is_dir()
                {
                    return Err(metadata_fault("working_directory", "directory"));
                }
                path_string(&path, "working_directory")
            })
            .transpose()?;
        Ok(TargetResolution {
            game,
            launcher,
            working_directory,
        })
    }
}

impl TargetLocation {
    fn validate(&self, field: &str) -> Result<(), Fault> {
        if !matches!(self.kind.as_str(), "executable" | "bundle") {
            return Err(configuration_fault(
                field,
                "location kind must be executable or bundle",
            ));
        }
        absolute_path(&self.path, field)
    }
}

impl TargetBinding {
    pub fn compatible(
        &self,
        package: &str,
        declaration: &TargetDeclaration,
    ) -> Result<bool, Fault> {
        Ok(self.package_id == package
            && self.target_id == declaration.id
            && self.declaration_identity == declaration.identity()?)
    }
}

impl TargetRecord {
    pub fn expectation(&self) -> TargetExpectation {
        TargetExpectation {
            revision: self.revision,
            binding_id: self.binding.as_ref().map(|binding| binding.id.clone()),
        }
    }

    pub(crate) fn compare(&self, expected: &TargetExpectation) -> Result<(), Fault> {
        if expected.revision > MAX_TARGET_REVISION
            || expected.revision != self.revision
            || expected.binding_id.as_deref()
                != self.binding.as_ref().map(|binding| binding.id.as_str())
        {
            return Err(Fault::new(
                "TargetConflict",
                "target configuration changed; reload and reconcile before retrying",
            ));
        }
        Ok(())
    }

    pub(crate) fn next_revision(&self) -> Result<u64, Fault> {
        if self.revision >= MAX_TARGET_REVISION {
            return Err(Fault::new(
                "TargetRevision",
                "target configuration revision is exhausted",
            ));
        }
        Ok(self.revision + 1)
    }

    /// Restore and load share this pure validation, even for removed installations.
    pub(crate) fn validate_owned(&self, tab: &str, package: &str) -> Result<(), Fault> {
        if self.version != TARGET_VERSION {
            return Err(Fault::new(
                "TargetVersion",
                "unsupported target configuration version; original data was preserved",
            ));
        }
        validate_internal_name(&self.internal_name)?;
        validate_package_id(&self.package_id)?;
        if self.internal_name != tab || self.package_id != package {
            return Err(Fault::new(
                "TargetOwner",
                "target configuration belongs to a different Tab/package",
            ));
        }
        if self.revision == 0 || self.revision > MAX_TARGET_REVISION {
            return Err(Fault::new(
                "TargetRevision",
                "invalid saved target configuration revision",
            ));
        }
        if let Some(binding) = &self.binding {
            validate_id(&binding.id)
                .map_err(|_| configuration_fault("id", "invalid target binding identity"))?;
            if binding.package_id != self.package_id {
                return Err(Fault::new(
                    "TargetOwner",
                    "target binding belongs to a different package",
                ));
            }
            TargetDeclaration {
                id: binding.target_id.clone(),
                window_title: None,
            }
            .validate()
            .map_err(|_| configuration_fault("target_id", "invalid portable target identity"))?;
            if !binding
                .declaration_identity
                .strip_prefix("sha256:")
                .is_some_and(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                return Err(configuration_fault(
                    "declaration_identity",
                    "invalid declaration identity",
                ));
            }
            binding.configuration.validate()?;
            binding.resolution.validate(&binding.configuration)?;
        }
        Ok(())
    }
}

impl TargetResolution {
    fn validate(&self, configuration: &TargetConfiguration) -> Result<(), Fault> {
        self.game.validate(&configuration.game, "game")?;
        match (&self.launcher, &configuration.launcher) {
            (Some(resolution), Some(location)) => resolution.validate(location, "launcher")?,
            (None, None) => {}
            _ => {
                return Err(configuration_fault(
                    "launcher",
                    "saved launcher resolution does not match configuration",
                ));
            }
        }
        match (&self.working_directory, &configuration.working_directory) {
            (Some(path), Some(_)) => canonical_path(path, "working_directory")?,
            (None, None) => {}
            _ => {
                return Err(configuration_fault(
                    "working_directory",
                    "saved directory resolution does not match configuration",
                ));
            }
        }
        Ok(())
    }
}

impl ResolvedLocation {
    fn validate(&self, location: &TargetLocation, field: &str) -> Result<(), Fault> {
        canonical_path(&self.path, field)?;
        canonical_path(&self.executable, field)?;
        let valid = if location.kind == "executable" {
            self.path == self.executable
        } else {
            self.path
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("app"))
                && self
                    .executable
                    .strip_prefix(&self.path)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        };
        if !valid {
            return Err(configuration_fault(
                field,
                "saved executable resolution is inconsistent with location kind",
            ));
        }
        Ok(())
    }
}

pub(crate) fn check(
    configuration: &TargetConfiguration,
    declaration: &TargetDeclaration,
    previous: Option<&TargetBinding>,
) -> Result<TargetCheck, Fault> {
    let resolution = configuration.resolve(declaration)?;
    let resolution_changed = previous.is_some_and(|binding| binding.resolution != resolution);
    Ok(TargetCheck {
        configuration_identity: configuration.identity()?,
        resolution,
        previous_resolution: previous.map(|binding| binding.resolution.clone()),
        resolution_changed,
    })
}

fn text(value: &str, maximum: usize, empty: bool, field: &str) -> Result<(), Fault> {
    if (!empty && value.is_empty()) || value.len() > maximum || value.chars().any(char::is_control)
    {
        return Err(configuration_fault(
            field,
            "value exceeds its text bound or contains unsupported control characters",
        ));
    }
    Ok(())
}

fn absolute_path(value: &str, field: &str) -> Result<(), Fault> {
    text(value, MAX_PATH_BYTES, false, field)?;
    // The persisted platform is macOS; restore must accept its paths on any host.
    if !value.starts_with('/') {
        return Err(configuration_fault(
            field,
            "an explicit absolute macOS path is required",
        ));
    }
    Ok(())
}

fn canonical_path(value: &str, field: &str) -> Result<(), Fault> {
    absolute_path(value, field)?;
    if value != "/"
        && value[1..]
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(configuration_fault(
            field,
            "saved resolution must be a canonical absolute path",
        ));
    }
    Ok(())
}

fn configuration_fault(field: &str, message: &str) -> Fault {
    Fault::new("TargetConfiguration", message)
        .with_context(json!({"field": field, "stage": "configuration"}))
}

fn metadata_fault(field: &str, stage: &str) -> Fault {
    Fault::new(
        "TargetMetadata",
        "selected target metadata is missing, invalid, or inaccessible",
    )
    .with_context(json!({"field": field, "stage": stage}))
}

fn canonical(path: impl AsRef<Path>, field: &str) -> Result<PathBuf, Fault> {
    fs::canonicalize(path).map_err(|_| metadata_fault(field, "canonicalize"))
}

fn path_string(path: &Path, field: &str) -> Result<String, Fault> {
    let value = path
        .to_str()
        .ok_or_else(|| metadata_fault(field, "canonicalize"))?;
    canonical_path(value, field)?;
    Ok(value.to_owned())
}

fn executable(path: &Path, field: &str) -> Result<(), Fault> {
    let metadata = fs::metadata(path).map_err(|_| metadata_fault(field, "executable"))?;
    if !metadata.is_file() {
        return Err(metadata_fault(field, "executable"));
    }
    #[cfg(unix)]
    if metadata.mode() & 0o111 == 0 {
        return Err(metadata_fault(field, "executable"));
    }
    Ok(())
}

fn resolve_location(location: &TargetLocation, field: &str) -> Result<ResolvedLocation, Fault> {
    let selected = canonical(&location.path, field)?;
    if location.kind == "executable" {
        executable(&selected, field)?;
        let path = path_string(&selected, field)?;
        return Ok(ResolvedLocation {
            executable: path.clone(),
            path,
        });
    }
    if !selected
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        || !fs::metadata(&selected)
            .map_err(|_| metadata_fault(field, "bundle"))?
            .is_dir()
    {
        return Err(metadata_fault(field, "bundle"));
    }
    let plist_path = canonical(selected.join("Contents/Info.plist"), field)?;
    if !plist_path.starts_with(&selected) {
        return Err(metadata_fault(field, "plist"));
    }
    let name = bundle_executable(&read_metadata(&plist_path, field)?, field)?;
    if name.is_empty()
        || name.len() > 255
        || matches!(name.as_str(), "." | "..")
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(metadata_fault(field, "plist"));
    }
    let path = canonical(selected.join("Contents/MacOS").join(name), field)?;
    if !path.starts_with(&selected) {
        return Err(metadata_fault(field, "executable"));
    }
    executable(&path, field)?;
    Ok(ResolvedLocation {
        path: path_string(&selected, field)?,
        executable: path_string(&path, field)?,
    })
}

fn read_metadata(path: &Path, field: &str) -> Result<Vec<u8>, Fault> {
    let failure = || metadata_fault(field, "plist");
    let before = fs::metadata(path).map_err(|_| failure())?;
    if !before.is_file() || before.len() > MAX_METADATA_BYTES as u64 {
        return Err(failure());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let mut file = options.open(path).map_err(|_| failure())?;
    let opened = file.metadata().map_err(|_| failure())?;
    if !opened.is_file() || opened.len() != before.len() {
        return Err(failure());
    }
    #[cfg(unix)]
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(failure());
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_METADATA_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure())?;
    let after = fs::metadata(path).map_err(|_| failure())?;
    if bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
    {
        return Err(failure());
    }
    #[cfg(unix)]
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(failure());
    }
    Ok(bytes)
}

// Stream instead of building a recursive Value tree: binary references can
// expand a small source many times. Bound depth, events and decoded payload too.
fn bundle_executable(bytes: &[u8], field: &str) -> Result<String, Fault> {
    struct Collection {
        dictionary: bool,
        expecting_value: bool,
        executable_key: bool,
        keys: BTreeSet<String>,
    }
    let failure = || metadata_fault(field, "plist");
    let mut stack: Vec<Collection> = Vec::new();
    let mut executable = None;
    let mut payload = 0usize;
    let mut root = false;
    for (index, event) in Reader::new(Cursor::new(bytes)).enumerate() {
        if index >= 8192 {
            return Err(failure());
        }
        let event = event.map_err(|_| failure())?;
        payload += match &event {
            Event::String(text) => text.len(),
            Event::Data(bytes) => bytes.len(),
            _ => 0,
        };
        if payload > MAX_METADATA_BYTES {
            return Err(failure());
        }
        if matches!(event, Event::EndCollection) {
            let collection = stack.pop().ok_or_else(failure)?;
            if collection.expecting_value {
                return Err(failure());
            }
            continue;
        }
        let top = stack.len() == 1;
        if let Some(collection) = stack.last_mut() {
            if collection.dictionary {
                if collection.expecting_value {
                    collection.expecting_value = false;
                    if top && collection.executable_key {
                        let Event::String(name) = &event else {
                            return Err(failure());
                        };
                        executable = Some(name.to_string());
                    }
                } else {
                    let Event::String(key) = event else {
                        return Err(failure());
                    };
                    collection.executable_key = key == "CFBundleExecutable";
                    if !collection.keys.insert(key.into_owned()) {
                        return Err(failure());
                    }
                    collection.expecting_value = true;
                    continue;
                }
            }
        } else if root || !matches!(event, Event::StartDictionary(_)) {
            return Err(failure());
        } else {
            root = true;
        }
        match event {
            Event::StartDictionary(_) | Event::StartArray(_) => {
                if stack.len() == 32 {
                    return Err(failure());
                }
                stack.push(Collection {
                    dictionary: matches!(event, Event::StartDictionary(_)),
                    expecting_value: false,
                    executable_key: false,
                    keys: BTreeSet::new(),
                });
            }
            Event::Boolean(_)
            | Event::Data(_)
            | Event::Date(_)
            | Event::Integer(_)
            | Event::Real(_)
            | Event::String(_)
            | Event::Uid(_) => {}
            _ => return Err(failure()),
        }
    }
    if !root || !stack.is_empty() {
        return Err(failure());
    }
    executable.ok_or_else(failure)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn declaration() -> TargetDeclaration {
        TargetDeclaration {
            id: "metadata-game".into(),
            window_title: Some("Exact title".into()),
        }
    }

    pub(crate) fn configuration(path: &str) -> TargetConfiguration {
        TargetConfiguration {
            platform: "macos".into(),
            game: TargetLocation {
                kind: "executable".into(),
                path: path.into(),
            },
            launcher: None,
            arguments: vec![
                "".into(),
                "two words".into(),
                "$(literal)".into(),
                "\"quoted\"".into(),
            ],
            working_directory: None,
            window_title: "Exact title".into(),
            input: TargetInputPolicy {
                route: "process_directed".into(),
                focus: "preserve".into(),
                pointer_mode: Some("appkit_background".into()),
                click_hold_ms: 0,
            },
        }
    }

    #[test]
    fn strict_requests_refuse_authority_shell_and_positional_shapes() {
        let valid = serde_json::to_value(configuration("/metadata/game")).unwrap();
        for (field, value) in [
            ("credentials", json!({"password": "not supported"})),
            ("pid", json!(42)),
            ("permission", json!(true)),
            ("command", json!("shell command")),
            ("environment", json!({"PATH": "/somewhere"})),
            ("arguments", json!("one command line")),
            ("game", json!(["executable", "/metadata/game"])),
            ("launcher", json!(["executable", "/metadata/launcher"])),
            (
                "input",
                json!(["process_directed", "preserve", "core_graphics", 0]),
            ),
        ] {
            let mut value_with_error = valid.clone();
            value_with_error[field] = value;
            assert!(
                serde_json::from_value::<TargetConfiguration>(value_with_error).is_err(),
                "{field}"
            );
        }
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("launcher");
        assert!(serde_json::from_value::<TargetConfiguration>(missing).is_err());
        assert!(
            serde_json::from_value::<TargetConfiguration>(json!([
                "macos",
                valid["game"],
                null,
                [],
                null,
                "Exact title",
                valid["input"]
            ]))
            .is_err()
        );
        assert!(
            serde_json::from_str::<TargetExpectation>(
                r#"{"revision":1,"revision":2,"binding_id":null}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<TargetExpectation>("[0,null]").is_err());
        assert!(
            serde_json::from_str::<TargetLocation>(
                r#"{"kind":"executable","path":"/first","path":"/second"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<TargetInputPolicy>(r#"{"route":"system","focus":"require_focused","pointer_mode":null,"pointer_mode":null,"click_hold_ms":0}"#).is_err());
        assert!(serde_json::from_str::<TargetInputPolicy>(r#"{"route":"system","focus":"require_focused","pointer_mode":null,"click_hold_ms":1.5}"#).is_err());
        assert!(
            serde_json::from_str::<TargetExpectation>(r#"{"revision":-1,"binding_id":null}"#)
                .is_err()
        );
    }

    #[test]
    fn configuration_bounds_and_policy_refuse_before_metadata() {
        let valid = configuration("/missing-metadata-fixture");
        valid.validate_declaration(&declaration()).unwrap();
        for (field, value) in [
            ("platform", json!("windows")),
            ("game", json!({"kind": "shell", "path": "/missing"})),
            ("game", json!({"kind": "executable", "path": "relative"})),
            ("working_directory", json!("~/not-expanded")),
            ("window_title", json!("")),
            ("window_title", json!("wrong title")),
            ("window_title", json!("a".repeat(513))),
            ("arguments", json!(["line\nbreak"])),
            ("arguments", json!(["x".repeat(1025)])),
            ("arguments", json!(vec![""; 33])),
            ("arguments", json!(vec!["x".repeat(1024); 9])),
            (
                "input",
                json!({"route":"system","focus":"preserve","pointer_mode":null,"click_hold_ms":0}),
            ),
            (
                "input",
                json!({"route":"system","focus":"require_focused","pointer_mode":"core_graphics","click_hold_ms":0}),
            ),
            (
                "input",
                json!({"route":"process_directed","focus":"require_focused","pointer_mode":"appkit_background","click_hold_ms":0}),
            ),
            (
                "input",
                json!({"route":"process_directed","focus":"preserve","pointer_mode":null,"click_hold_ms":0}),
            ),
            (
                "input",
                json!({"route":"process_directed","focus":"preserve","pointer_mode":"core_graphics","click_hold_ms":1001}),
            ),
        ] {
            let mut value_with_error = serde_json::to_value(&valid).unwrap();
            value_with_error[field] = value;
            let invalid: TargetConfiguration = serde_json::from_value(value_with_error).unwrap();
            let fault = invalid.resolve(&declaration()).unwrap_err();
            assert_eq!(fault.category, "TargetConfiguration", "{field}");
            assert_eq!(fault.context["field"], field);
            assert!(!fault.message.contains("/missing"));
        }
        for (route, focus, mode) in [
            ("system", "require_focused", None),
            ("process_directed", "require_focused", Some("core_graphics")),
            ("process_directed", "preserve", Some("core_graphics")),
            ("process_directed", "preserve", Some("appkit_background")),
        ] {
            let mut configuration = valid.clone();
            configuration.input = TargetInputPolicy {
                route: route.into(),
                focus: focus.into(),
                pointer_mode: mode.map(str::to_owned),
                click_hold_ms: 1000,
            };
            configuration.validate_declaration(&declaration()).unwrap();
        }
        let mut boundary = valid;
        boundary.arguments = vec!["x".repeat(1024); 8];
        boundary.game.path = format!("/{}", "x".repeat(4095));
        boundary.window_title = "x".repeat(512);
        boundary
            .validate_declaration(&TargetDeclaration {
                id: "game".into(),
                window_title: None,
            })
            .unwrap();
        boundary.game.path.push('x');
        assert!(boundary.validate().is_err());
    }

    #[test]
    fn literal_argument_boundaries_affect_identity_without_expansion() {
        let original = configuration("/metadata/game");
        let roundtrip: TargetConfiguration =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(
            roundtrip.arguments,
            ["", "two words", "$(literal)", "\"quoted\""]
        );
        let mut changed = original.clone();
        changed.arguments.remove(0);
        assert_ne!(changed.identity().unwrap(), original.identity().unwrap());
        changed = original.clone();
        changed.arguments.swap(1, 2);
        assert_ne!(changed.identity().unwrap(), original.identity().unwrap());
    }

    #[test]
    fn plist_metadata_refuses_duplicate_keys_wrong_types_and_excessive_depth() {
        for body in [
            "<key>CFBundleExecutable</key><string>game</string><key>CFBundleExecutable</key><string>other</string>".to_owned(),
            "<key>CFBundleExecutable</key><integer>1</integer>".to_owned(),
            "<key>MissingValue</key>".to_owned(),
            format!("<key>CFBundleExecutable</key><string>game</string><key>deep</key>{}<string>x</string>{}", "<array>".repeat(33), "</array>".repeat(33)),
        ] {
            let xml = format!("<plist version=\"1.0\"><dict>{body}</dict></plist>");
            assert_eq!(bundle_executable(xml.as_bytes(), "game").unwrap_err().category, "TargetMetadata");
        }
        assert!(bundle_executable(b"bplist00malformed", "game").is_err());
    }

    #[test]
    fn binary_plist_reference_expansion_respects_event_and_payload_bounds() {
        for values in [
            vec![plist::Value::Boolean(true); 8192],
            vec![plist::Value::String("x".repeat(1024)); 128],
        ] {
            let metadata = plist::Value::Dictionary(plist::Dictionary::from_iter([
                (
                    "CFBundleExecutable",
                    plist::Value::String("Game".into()),
                ),
                ("Extra", plist::Value::Array(values)),
            ]));
            let mut bytes = Vec::new();
            metadata.to_writer_binary(&mut bytes).unwrap();
            assert!(bytes.len() < MAX_METADATA_BYTES);
            assert_eq!(
                bundle_executable(&bytes, "game").unwrap_err().category,
                "TargetMetadata"
            );
        }
    }

    #[cfg(unix)]
    pub(crate) struct MetadataFixture(pub PathBuf);

    #[cfg(unix)]
    impl MetadataFixture {
        pub(crate) fn new() -> Self {
            let root = crate::configuration::temporary(&std::env::temp_dir(), "target-metadata");
            crate::storage::private_directory(&root).unwrap();
            Self(root)
        }

        pub(crate) fn executable(&self, relative: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"metadata fixture, never executed").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        }

        pub(crate) fn bundle(&self, binary: bool) -> PathBuf {
            let bundle = self.0.join(if binary { "Binary.app" } else { "Xml.app" });
            self.executable(if binary {
                "Binary.app/Contents/MacOS/Game"
            } else {
                "Xml.app/Contents/MacOS/Game"
            });
            let metadata = plist::Value::Dictionary(plist::Dictionary::from_iter([
                (
                    "CFBundleExecutable",
                    plist::Value::String("Game".into()),
                ),
                (
                    "CFBundleName",
                    plist::Value::String("Metadata fixture".into()),
                ),
            ]));
            let file = fs::File::create(bundle.join("Contents/Info.plist")).unwrap();
            if binary {
                metadata.to_writer_binary(file).unwrap();
            } else {
                metadata.to_writer_xml(file).unwrap();
            }
            bundle
        }
    }

    #[cfg(unix)]
    impl Drop for MetadataFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn metadata_checks_game_and_launcher_independently_and_require_executable_files() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = MetadataFixture::new();
        let launcher = fixture.executable("launcher");
        let mut configuration = configuration(fixture.0.join("missing-game").to_str().unwrap());
        configuration.launcher = Some(TargetLocation {
            kind: "executable".into(),
            path: launcher.to_str().unwrap().into(),
        });
        let fault = configuration.resolve(&declaration()).unwrap_err();
        assert_eq!(fault.context["field"], "game");
        assert!(!fault.message.contains(launcher.to_str().unwrap()));
        let game = fixture.executable("game");
        configuration.game.path = game.to_str().unwrap().into();
        configuration.working_directory = Some(fixture.0.to_str().unwrap().into());
        let resolved = configuration.resolve(&declaration()).unwrap();
        assert_ne!(resolved.game, resolved.launcher.unwrap());
        configuration.working_directory = Some(game.to_str().unwrap().into());
        assert_eq!(
            configuration.resolve(&declaration()).unwrap_err().context["field"],
            "working_directory"
        );
        configuration.working_directory = None;
        fs::set_permissions(&game, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            configuration.resolve(&declaration()).unwrap_err().context["stage"],
            "executable"
        );
        configuration.game.path = fixture.0.to_str().unwrap().into();
        assert_eq!(
            configuration.resolve(&declaration()).unwrap_err().context["field"],
            "game"
        );
        configuration.game.path = launcher.to_str().unwrap().into();
        configuration.launcher.as_mut().unwrap().path =
            fixture.0.join("missing-launcher").to_str().unwrap().into();
        assert_eq!(
            configuration.resolve(&declaration()).unwrap_err().context["field"],
            "launcher"
        );
    }

    #[cfg(unix)]
    #[test]
    fn xml_and_binary_bundles_resolve_only_contained_executables_and_bounded_metadata() {
        let fixture = MetadataFixture::new();
        for binary in [false, true] {
            let bundle = fixture.bundle(binary);
            let mut configuration = configuration(bundle.to_str().unwrap());
            configuration.game.kind = "bundle".into();
            let resolution = configuration.resolve(&declaration()).unwrap();
            assert_eq!(
                resolution.game.executable,
                fs::canonicalize(bundle.join("Contents/MacOS/Game"))
                    .unwrap()
                    .to_str()
                    .unwrap()
            );
            let info = bundle.join("Contents/Info.plist");
            let original = fs::read(&info).unwrap();
            for bad in [
                b"malformed".to_vec(),
                vec![b'x'; MAX_METADATA_BYTES + 1],
                b"<plist><dict><key>CFBundleExecutable</key><string>../Game</string></dict></plist>".to_vec(),
                b"<plist><dict><key>CFBundleExecutable</key><string>/outside</string></dict></plist>".to_vec(),
                b"<plist><dict><key>CFBundleExecutable</key><string>Missing</string></dict></plist>".to_vec(),
            ] {
                fs::write(&info, bad).unwrap();
                assert!(configuration.resolve(&declaration()).is_err());
            }
            fs::write(&info, original).unwrap();
            let executable = bundle.join("Contents/MacOS/Game");
            fs::remove_file(&executable).unwrap();
            let outside = fixture.executable(if binary {
                "outside-binary"
            } else {
                "outside-xml"
            });
            std::os::unix::fs::symlink(outside, &executable).unwrap();
            assert_eq!(
                configuration.resolve(&declaration()).unwrap_err().context["stage"],
                "executable"
            );
        }
    }
}
