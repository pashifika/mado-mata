use crate::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod capture;
mod validation;

use validation::portable_component;

const VERSION: &str = "1.0.0";
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 240;
const MAX_FILES: usize = 65_536;
const MAX_BYTES: usize = 256 * 1024 * 1024;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MacosTargetDeclaration {
    pub bundle_id: String,
}

impl<'de> Deserialize<'de> for MacosTargetDeclaration {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MacosVisitor;

        impl<'de> serde::de::Visitor<'de> for MacosVisitor {
            type Value = MacosTargetDeclaration;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a macOS target declaration object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Fields {
                    bundle_id: String,
                }

                let fields =
                    Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(MacosTargetDeclaration {
                    bundle_id: fields.bundle_id,
                })
            }
        }

        deserializer.deserialize_map(MacosVisitor)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TargetDeclaration {
    pub id: String,
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macos: Option<MacosTargetDeclaration>,
}

impl<'de> Deserialize<'de> for TargetDeclaration {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TargetVisitor;

        impl<'de> serde::de::Visitor<'de> for TargetVisitor {
            type Value = TargetDeclaration;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a portable target declaration object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Fields {
                    id: String,
                    window_title: Option<String>,
                    #[serde(default, deserialize_with = "present_macos")]
                    macos: Option<MacosTargetDeclaration>,
                }

                let fields =
                    Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(TargetDeclaration {
                    id: fields.id,
                    window_title: fields.window_title,
                    macos: fields.macos,
                })
            }
        }

        deserializer.deserialize_map(TargetVisitor)
    }
}

fn present_target<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<TargetDeclaration>, D::Error> {
    TargetDeclaration::deserialize(deserializer).map(Some)
}

fn present_macos<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<MacosTargetDeclaration>, D::Error> {
    MacosTargetDeclaration::deserialize(deserializer).map(Some)
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
    #[serde(
        default,
        deserialize_with = "present_target",
        skip_serializing_if = "Option::is_none"
    )]
    target: Option<TargetDeclaration>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    format: String,
    width: usize,
    height: usize,
}

impl Inventory {
    pub fn target(&self) -> Result<Option<TargetDeclaration>, Fault> {
        let manifest = self
            .metadata
            .get("manifest")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("inventory manifest is missing or invalid"))?;
        manifest
            .get("target")
            .map(|value| {
                let target = TargetDeclaration::deserialize(value).map_err(|error| {
                    invalid("invalid inventory target declaration").with_context(json!({
                        "path":"package.json","field":"target","cause":error.to_string()
                    }))
                })?;
                target.validate()?;
                Ok(target)
            })
            .transpose()
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
}

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new("Inventory", message)
}
