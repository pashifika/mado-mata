use super::{
    AUTHORING_ASSET, ENGINE_MANIFEST_ASSET, ENGINE_MANIFEST_PATH, MAX_METADATA_BYTES,
    RecognitionDocument, RecognitionKind, TEMPLATE_MAPS_ASSET, bounded_json, invalid,
};
use crate::images::{ImageKind, validate_png};
use crate::inventory::{Inventory, PackageDraft};
use crate::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateMaps {
    pub version: u32,
    pub package_entries: BTreeMap<String, String>,
    pub templates: BTreeMap<String, String>,
}

/// Builds only the authored manifest. Paths here are engine-relative entry names,
/// not filesystem paths. Shared crops must agree on their matching defaults.
pub fn build_template_assets(
    document: &RecognitionDocument,
    package_id: &str,
) -> Result<Option<(TemplateMaps, Vec<u8>)>, Fault> {
    document.validate()?;
    PackageDraft::check_id(package_id)?;
    let mut entries = BTreeMap::from([(
        ENGINE_MANIFEST_PATH.to_owned(),
        ENGINE_MANIFEST_ASSET.to_owned(),
    )]);
    let mut aliases = BTreeMap::new();
    let mut templates = BTreeMap::<String, Value>::new();
    for definition in &document.definitions {
        if definition.kind != RecognitionKind::Template {
            continue;
        }
        let Some(saved) = &definition.saved else {
            continue;
        };
        let settings = definition
            .template
            .as_ref()
            .ok_or_else(|| invalid("template settings are missing"))?;
        // A crop can be retained by multiple definitions. Its engine identity must
        // therefore depend on the crop rather than on whichever definition is first.
        let id = format!("recognition.{}", saved.asset);
        let path = format!("templates/{}.png", saved.asset);
        let template = json!({
            "id":id,"path":path,"width":saved.width,"height":saved.height,
            "coordinate_space":"capture_pixels",
            "content":{"algorithm":"sha256","value":saved.sha256},
            "match_defaults":{"min_score":settings.threshold,"max_results":settings.max_results}
        });
        if templates
            .get(&saved.asset)
            .is_some_and(|previous| previous != &template)
        {
            return Err(invalid(
                "shared template crop has conflicting match defaults",
            ));
        }
        templates.insert(saved.asset.clone(), template);
        entries.insert(path, saved.asset.clone());
        aliases.insert(saved.asset.clone(), id);
    }
    if templates.is_empty() {
        return Ok(None);
    }
    let rights = document
        .template_rights
        .as_ref()
        .ok_or_else(|| invalid("template rights are missing"))?;
    let mut provenance = json!({"created_by":rights.created_by});
    if let Some(created_for) = &rights.created_for {
        provenance["created_for"] = json!(created_for);
    }
    let manifest = json!({
        "schema_version":1,"package":{"id":package_id,"version":"1.0.0"},
        "license":rights.license,"provenance":provenance,
        "templates":templates.into_values().collect::<Vec<_>>()
    });
    let maps = TemplateMaps {
        version: 1,
        package_entries: entries,
        templates: aliases,
    };
    bounded_json(&maps)?;
    Ok(Some((maps, bounded_json(&manifest)?)))
}

/// Validates a declared package asset projection without validating/evaluating
/// source, presets or schema. `asset` resolves only already-captured asset IDs.
pub fn validate_package_assets<'a>(
    manifest: &Value,
    asset: impl Fn(&str) -> Option<&'a [u8]>,
) -> Result<Option<RecognitionDocument>, Fault> {
    let declarations = manifest["assets"]
        .as_object()
        .ok_or_else(|| invalid("package asset declarations are missing"))?;
    if !declarations.contains_key(AUTHORING_ASSET) {
        if declarations.contains_key(TEMPLATE_MAPS_ASSET)
            || declarations.contains_key(ENGINE_MANIFEST_ASSET)
        {
            return Err(invalid("recognition maps require their authoring document"));
        }
        return Ok(None);
    }
    let metadata = declared_json(manifest, AUTHORING_ASSET, &asset)?;
    let document = RecognitionDocument::from_bytes(metadata)?;
    let mut checked = BTreeMap::new();
    for definition in &document.definitions {
        let Some(saved) = &definition.saved else {
            continue;
        };
        if let Some(previous) = checked.insert(&saved.asset, saved) {
            if previous != saved {
                return Err(invalid("retained crop references disagree"));
            }
            continue;
        }
        let declaration = &manifest["assets"][&saved.asset];
        let path = declaration["path"]
            .as_str()
            .ok_or_else(|| invalid("saved crop declaration is missing"))?;
        PackageDraft::check_path(path)?;
        if declaration["format"] != "png"
            || declaration["width"].as_u64() != Some(u64::from(saved.width))
            || declaration["height"].as_u64() != Some(u64::from(saved.height))
        {
            return Err(invalid(
                "saved crop declaration disagrees with recognition metadata",
            ));
        }
        let png = asset(&saved.asset).ok_or_else(|| invalid("saved crop asset is missing"))?;
        let info = validate_png(png, ImageKind::Crop)?;
        if info.width != saved.width
            || info.height != saved.height
            || super::png_digest(png) != saved.sha256
        {
            return Err(invalid("saved crop dimensions or hash are stale"));
        }
    }
    let package_id = manifest["package_id"]
        .as_str()
        .ok_or_else(|| invalid("package identity is missing"))?;
    match build_template_assets(&document, package_id)? {
        Some((expected_maps, expected_manifest)) => {
            let maps: TemplateMaps =
                serde_json::from_slice(declared_json(manifest, TEMPLATE_MAPS_ASSET, &asset)?)
                    .map_err(|_| invalid("malformed recognition template maps"))?;
            if maps != expected_maps {
                return Err(invalid(
                    "recognition template maps disagree with saved definitions",
                ));
            }
            let actual: Value =
                serde_json::from_slice(declared_json(manifest, ENGINE_MANIFEST_ASSET, &asset)?)
                    .map_err(|_| invalid("malformed recognition engine manifest"))?;
            let expected: Value = serde_json::from_slice(&expected_manifest)
                .map_err(|_| invalid("engine manifest encoding failed"))?;
            if actual != expected {
                return Err(invalid(
                    "recognition engine manifest disagrees with saved hashes, dimensions, rights or defaults",
                ));
            }
        }
        None => {
            if declarations.contains_key(TEMPLATE_MAPS_ASSET)
                || declarations.contains_key(ENGINE_MANIFEST_ASSET)
            {
                return Err(invalid(
                    "recognition maps have no saved template definitions",
                ));
            }
        }
    }
    Ok(Some(document))
}

pub fn validate_inventory(inventory: &Inventory) -> Result<Option<RecognitionDocument>, Fault> {
    validate_package_assets(&inventory.metadata["manifest"], |id| {
        inventory.assets.get(id).map(AsRef::as_ref)
    })
}

/// Validates first and mutates neither destination map on any disagreement.
/// Explicit frame/corpus selection remains the caller's responsibility.
pub fn merge_effective_template_maps(
    inventory: &Inventory,
    package_entries: &mut BTreeMap<String, String>,
    templates: &mut BTreeMap<String, String>,
) -> Result<(), Fault> {
    let Some(document) = validate_inventory(inventory)? else {
        return Ok(());
    };
    let Some((maps, _)) = build_template_assets(&document, &inventory.package_id)? else {
        return Ok(());
    };
    for (destination, incoming) in [
        (&*package_entries, &maps.package_entries),
        (&*templates, &maps.templates),
    ] {
        let mut names: BTreeSet<_> = destination
            .keys()
            .map(|key| key.to_ascii_lowercase())
            .collect();
        for (key, value) in incoming {
            if let Some(existing) = destination.get(key) {
                if existing != value {
                    return Err(invalid("authored and explicit template mappings disagree"));
                }
            } else if !names.insert(key.to_ascii_lowercase()) {
                return Err(invalid(
                    "authored and explicit template mappings have case-colliding keys",
                ));
            }
        }
    }
    package_entries.extend(maps.package_entries);
    templates.extend(maps.templates);
    Ok(())
}

fn declared_json<'a>(
    manifest: &Value,
    id: &str,
    asset: &impl Fn(&str) -> Option<&'a [u8]>,
) -> Result<&'a [u8], Fault> {
    let declaration = &manifest["assets"][id];
    if declaration["format"] != "json" || declaration["width"] != 0 || declaration["height"] != 0 {
        return Err(invalid(
            "recognition metadata requires declared zero-dimension JSON assets",
        ));
    }
    PackageDraft::check_path(
        declaration["path"]
            .as_str()
            .ok_or_else(|| invalid("recognition asset path is missing"))?,
    )?;
    let bytes = asset(id).ok_or_else(|| invalid("declared recognition JSON asset is missing"))?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(invalid("recognition metadata byte limit exceeded"));
    }
    Ok(bytes)
}
