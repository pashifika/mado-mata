//! Optional version-1 release and search metadata in `package.json`.
//! Search fields describe discovery only and never enter runtime target identity.

use super::invalid;
use super::validation::{BoundedWriter, portable_component};
use crate::model::Fault;
use icu_locale_core::LanguageIdentifier;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::marker::PhantomData;

const MAX_OBJECT_BYTES: usize = 64 * 1024;
const MAX_RELEASE_VERSION_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_ITEMS: usize = 64;
const MAX_LANGUAGE_KEY_BYTES: usize = 64;
const MAX_TAG_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Distribution {
    pub version: u32,
    pub release_version: String,
    pub name: String,
    /// Plain text; consumers never interpret it as markup.
    pub description: String,
    pub author_name: String,
    /// Stated terms or identifier, not an adjudicated or enforced entitlement.
    pub license: String,
    pub game: DistributionGame,
    /// Preserved internally; absence and an empty list remain distinct.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DistributionGame {
    /// Search-only ID, not a globally recognized or target identifier.
    pub id: String,
    /// Canonical language identifier to localized game name.
    pub names: BTreeMap<String, String>,
    pub aliases: Vec<String>,
}

impl<'de> Deserialize<'de> for Distribution {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            version: u32,
            release_version: String,
            name: String,
            description: String,
            author_name: String,
            license: String,
            game: DistributionGame,
            #[serde(default, deserialize_with = "present")]
            tags: Option<Vec<String>>,
        }

        let fields: Fields = object(deserializer)?;
        Ok(Self {
            version: fields.version,
            release_version: fields.release_version,
            name: fields.name,
            description: fields.description,
            author_name: fields.author_name,
            license: fields.license,
            game: fields.game,
            tags: fields.tags,
        })
    }
}

impl<'de> Deserialize<'de> for DistributionGame {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            id: String,
            #[serde(deserialize_with = "unique_names")]
            names: BTreeMap<String, String>,
            aliases: Vec<String>,
        }

        let fields: Fields = object(deserializer)?;
        Ok(Self {
            id: fields.id,
            names: fields.names,
            aliases: fields.aliases,
        })
    }
}

/// A present member must hold a value; `null` is not treated as absence.
pub(super) fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

/// Derived structs also admit positional arrays; require a JSON object.
fn object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<T, D::Error> {
    struct Object<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Object<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object")
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            T::deserialize(de::value::MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(Object(PhantomData))
}

/// Ordinary map decoding keeps the last duplicate; a duplicate key refuses instead.
fn unique_names<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct Names;

    impl<'de> Visitor<'de> for Names {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object of language keys to game names")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut names = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                match names.entry(key) {
                    Entry::Occupied(_) => {
                        return Err(de::Error::custom("duplicate game name language key"));
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(map.next_value()?);
                    }
                }
            }
            Ok(names)
        }
    }

    deserializer.deserialize_map(Names)
}

impl Distribution {
    pub(super) fn validate(&self) -> Result<(), Fault> {
        if self.version != 1 {
            return Err(refused(
                "distribution.version",
                "distribution metadata version must be 1",
            ));
        }
        if self.release_version.len() > MAX_RELEASE_VERSION_BYTES
            || !release_version(&self.release_version)
        {
            return Err(refused(
                "distribution.release_version",
                "release version must be SemVer 2.0.0 of at most 128 bytes",
            ));
        }
        for (field, value) in [
            ("distribution.name", &self.name),
            ("distribution.author_name", &self.author_name),
            ("distribution.license", &self.license),
        ] {
            if value.is_empty() || value.len() > MAX_TEXT_BYTES {
                return Err(refused(
                    field,
                    "distribution text must be nonempty and at most 256 bytes",
                ));
            }
        }
        if self.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(refused(
                "distribution.description",
                "description must be at most 8 KiB",
            ));
        }
        self.game.validate()?;
        if let Some(tags) = &self.tags {
            if tags.len() > MAX_ITEMS {
                return Err(refused(
                    "distribution.tags",
                    "tags must contain at most 64 entries",
                ));
            }
            if let Some(index) = tags.iter().position(|tag| tag.len() > MAX_TAG_BYTES) {
                return Err(invalid("tag must be at most 128 bytes").with_context(
                    json!({"path":"package.json","field":"distribution.tags","index":index}),
                ));
            }
        }
        // Escaping can expand individually bounded text past the aggregate bound.
        let mut serialized = BoundedWriter {
            count: 0,
            limit: MAX_OBJECT_BYTES,
        };
        serde_json::to_writer(&mut serialized, self).map_err(|_| {
            refused(
                "distribution",
                "distribution metadata must serialize to at most 64 KiB",
            )
        })
    }
}

fn release_version(version: &str) -> bool {
    let core = if let Some((core, build)) = version.split_once('+') {
        if build.is_empty() || semver::BuildMetadata::new(build).is_err() {
            return false;
        }
        core
    } else {
        version
    };
    let core = if let Some((core, prerelease)) = core.split_once('-') {
        if prerelease.is_empty() || semver::Prerelease::new(prerelease).is_err() {
            return false;
        }
        core
    } else {
        core
    };
    // SemVer core integers have no numeric upper bound; only syntax matters here.
    let mut parts = core.split('.');
    (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            !part.is_empty()
                && !(part.len() > 1 && part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
        })
    }) && parts.next().is_none()
}

impl DistributionGame {
    fn validate(&self) -> Result<(), Fault> {
        portable_component(&self.id).map_err(|_| {
            refused(
                "distribution.game.id",
                "search game ID must be a portable component of at most 128 bytes",
            )
        })?;
        if self.names.is_empty() || self.names.len() > MAX_ITEMS {
            return Err(refused(
                "distribution.game.names",
                "game names must contain 1 to 64 entries",
            ));
        }
        for (key, name) in &self.names {
            language_key(key)?;
            if name.is_empty() || name.len() > MAX_TEXT_BYTES {
                return Err(invalid("game name must be nonempty and at most 256 bytes")
                    .with_context(
                        json!({"path":"package.json","field":"distribution.game.names","key":key}),
                    ));
            }
        }
        if self.aliases.len() > MAX_ITEMS {
            return Err(refused(
                "distribution.game.aliases",
                "aliases must contain at most 64 entries",
            ));
        }
        if let Some(index) = self.aliases.iter().position(|alias| {
            alias.is_empty() || alias.len() > MAX_TEXT_BYTES || machine_path(alias)
        }) {
            return Err(invalid(
                "alias must be a nonempty search string of at most 256 bytes, not a machine path",
            )
            .with_context(
                json!({"path":"package.json","field":"distribution.game.aliases","index":index}),
            ));
        }
        Ok(())
    }
}

/// Saved keys must already use the shared canonical syntax (case and sorted
/// variants, no alias replacement); reading a package never rewrites them.
fn language_key(key: &str) -> Result<(), Fault> {
    let bounded = key.len() <= MAX_LANGUAGE_KEY_BYTES && key.is_ascii();
    let canonical = bounded
        .then(|| LanguageIdentifier::normalize(key).ok())
        .flatten();
    if canonical.as_deref() == Some(key) {
        return Ok(());
    }
    Err(
        invalid("game name key must be a canonical language identifier of at most 64 ASCII bytes")
            .with_context(json!({
                "path":"package.json",
                "field":"distribution.game.names",
                "key":bounded.then_some(key),
                "canonical":canonical
            })),
    )
}

/// Aliases may name an executable but never locate one on a machine.
fn machine_path(alias: &str) -> bool {
    match alias.as_bytes() {
        [b'/' | b'\\', ..] | [b'~', b'/' | b'\\', ..] => true,
        [drive, b':', b'/' | b'\\', ..] => drive.is_ascii_alphabetic(),
        _ => false,
    }
}

fn refused(field: &str, message: &str) -> Fault {
    invalid(message).with_context(json!({"path":"package.json","field":field}))
}
