use super::{Candidate, Commit, Edit, Publisher, limits};
use mado_runtime_comparison::images::PayloadBytes;
use mado_runtime_comparison::inventory::PackageDraft;
use mado_runtime_comparison::model::Fault;
use mado_runtime_comparison::recognition::{
    AUTHORING_ASSET, ENGINE_MANIFEST_ASSET, RecognitionMetadata, RecognitionPackage, SavedCrop,
    TEMPLATE_MAPS_ASSET, build_template_assets, validate_package_assets,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct SelectedCrop {
    pub definition_id: String,
    pub png: PayloadBytes,
}

#[derive(Debug)]
pub struct RecognitionSave {
    pub package: RecognitionPackage,
    pub capture_id: String,
    /// Host-encoded selected regions only; never supplied by a WebView byte array.
    pub crops: Vec<SelectedCrop>,
}

impl Candidate {
    pub fn recognition(&self) -> Result<Option<RecognitionMetadata>, Fault> {
        load(&self.draft)
    }

    pub fn recognition_crop(
        &self,
        capture_id: &str,
        definition_id: &str,
    ) -> Result<Option<PayloadBytes>, Fault> {
        let metadata = self
            .recognition()?
            .ok_or_else(|| refusal("recognition metadata is absent"))?;
        let Some(saved) = &metadata
            .document(capture_id)?
            .definition(definition_id)?
            .saved
        else {
            return Ok(None);
        };
        let manifest = self.draft.manifest()?;
        let path = manifest["assets"][&saved.asset]["path"]
            .as_str()
            .ok_or_else(|| refusal("saved crop declaration is missing"))?;
        self.draft
            .files()
            .get(path)
            .cloned()
            .map(Some)
            .ok_or_else(|| refusal("saved crop bytes are missing"))
    }
}

impl Publisher {
    pub fn publish_recognition(
        &self,
        candidate: &Candidate,
        expected_revision: &str,
        save: RecognitionSave,
    ) -> Result<Commit, Fault> {
        self.publish(candidate, expected_revision, Edit::Recognition(save))
    }
}

pub(super) fn load(draft: &PackageDraft) -> Result<Option<RecognitionMetadata>, Fault> {
    let manifest = draft.manifest()?;
    validate_package_assets(&manifest, |id| {
        manifest["assets"][id]["path"]
            .as_str()
            .and_then(|path| draft.files().get(path))
            .map(AsRef::as_ref)
    })
}

pub(super) fn apply(
    draft: &PackageDraft,
    mut save: RecognitionSave,
) -> Result<PackageDraft, Fault> {
    save.package.validate()?;
    save.package.document(&save.capture_id)?;
    let previous = load(draft)?;
    if let Some(RecognitionMetadata::Captures(previous)) = &previous {
        if previous
            .captures
            .iter()
            .any(|capture| save.package.document(&capture.capture_id).is_err())
        {
            return Err(refusal(
                "Recognition Save must retain every existing capture",
            ));
        }
    }
    let mut manifest = draft.manifest()?;
    let mut files = draft.files().clone();
    let mut selected = BTreeSet::new();
    for crop in &save.crops {
        if !selected.insert(crop.definition_id.as_str()) {
            return Err(refusal("a crop may be selected only once per save"));
        }
        save.package
            .document(&save.capture_id)?
            .definition(&crop.definition_id)?;
    }
    let legacy_id = save
        .package
        .captures
        .first()
        .map(|capture| capture.capture_id.clone());
    for capture in &mut save.package.captures {
        let prior = previous.as_ref().and_then(|metadata| match metadata {
            RecognitionMetadata::Legacy(document)
                if legacy_id.as_deref() == Some(capture.capture_id.as_str()) =>
            {
                Some(document)
            }
            RecognitionMetadata::Captures(package) => package.document(&capture.capture_id).ok(),
            _ => None,
        });
        for definition in &mut capture.document.definitions {
            let retained = prior
                .and_then(|document| document.definition(&definition.id).ok())
                .and_then(|item| item.saved.as_ref());
            if let Some(submitted) = &definition.saved {
                let known = previous.as_ref().is_some_and(|metadata| {
                    metadata
                        .definitions()
                        .any(|item| item.saved.as_ref() == Some(submitted))
                });
                if !known || retained.is_some_and(|saved| saved != submitted) {
                    return Err(refusal(
                        "saved crop identities are host-owned; select an explicit replacement",
                    ));
                }
            } else if let Some(retained) = retained {
                definition.saved = Some(retained.clone());
            }
        }
    }
    save.package.validate()?;
    let mut crop_number = None;
    for crop in save.crops {
        let shared = {
            let definition = save
                .package
                .document(&save.capture_id)?
                .definition(&crop.definition_id)?;
            if let Some(saved) = &definition.saved {
                let path = manifest["assets"][&saved.asset]["path"]
                    .as_str()
                    .ok_or_else(|| refusal("retained crop declaration is missing"))?;
                save.package
                    .definitions()
                    .filter(|item| {
                        item.saved
                            .as_ref()
                            .is_some_and(|other| other.asset == saved.asset)
                    })
                    .count()
                    > 1
                    || unrelated_reference(&manifest, &files, &saved.asset, path)?
            } else {
                false
            }
        };
        let document = save.package.document_mut(&save.capture_id)?;
        let definition = document
            .definitions
            .iter_mut()
            .find(|item| item.id == crop.definition_id)
            .ok_or_else(|| refusal("selected crop definition is missing"))?;
        let rect = definition.region.map_to_pixels(&document.basis)?;
        let asset = match definition.saved.as_ref().filter(|_| !shared) {
            Some(saved) => saved.asset.clone(),
            None => crate::storage::new_id()?,
        };
        let saved = SavedCrop::from_png(asset.clone(), &crop.png)?;
        if saved.width != rect.width || saved.height != rect.height {
            return Err(refusal(
                "selected PNG dimensions disagree with the mapped original-resolution crop",
            ));
        }
        let replace = !shared
            && definition
                .saved
                .as_ref()
                .is_some_and(|old| old.asset == asset);
        let path = if replace {
            manifest["assets"][&asset]["path"]
                .as_str()
                .ok_or_else(|| refusal("retained crop declaration is missing"))?
                .to_owned()
        } else {
            let number = match crop_number {
                Some(number) => number,
                None => maximum_crop_number(&files)?,
            }
            .checked_add(1)
            .ok_or_else(|| refusal("numeric crop filename exceeds its integer bound"))?;
            crop_number = Some(number);
            let path = format!("recognition/crops/{number:04}.png");
            vacant(&manifest, &files, &asset, &path)?;
            path
        };
        files.insert(path.clone(), crop.png);
        manifest["assets"][&asset] =
            json!({"path":path,"format":"png","width":saved.width,"height":saved.height});
        definition.saved = Some(saved);
    }
    save.package.validate()?;
    if let Some(previous) = &previous {
        let retained: BTreeSet<_> = save
            .package
            .definitions()
            .filter_map(|item| item.saved.as_ref().map(|saved| saved.asset.as_str()))
            .collect();
        let old: BTreeSet<_> = previous
            .definitions()
            .filter_map(|item| item.saved.as_ref().map(|saved| saved.asset.as_str()))
            .collect();
        for asset in old.difference(&retained) {
            let path = manifest["assets"][*asset]["path"]
                .as_str()
                .ok_or_else(|| refusal("old crop declaration is missing"))?
                .to_owned();
            // Unknown JSON consumers are not ours to rewrite or destroy.
            if unrelated_reference(&manifest, &files, asset, &path)? {
                continue;
            }
            manifest["assets"]
                .as_object_mut()
                .ok_or_else(|| refusal("asset declarations are missing"))?
                .remove(*asset);
            files.remove(&path);
        }
    }
    put_json(
        &mut manifest,
        &mut files,
        AUTHORING_ASSET,
        "recognition/authoring.json",
        save.package.to_bytes()?,
        previous.is_some(),
    )?;
    synchronize(
        save.package.documents(),
        &mut manifest,
        &mut files,
        previous.is_some(),
    )?;
    files.insert("package.json".into(), super::catalog::encode(&manifest)?);
    let next = PackageDraft::from_files(files, &limits()?)?;
    load(&next)?;
    Ok(next)
}

fn maximum_crop_number(files: &BTreeMap<String, PayloadBytes>) -> Result<u64, Fault> {
    let mut maximum = 0;
    for path in files.keys() {
        let Some(name) = path.strip_prefix("recognition/crops/") else {
            continue;
        };
        let Some((stem, extension)) = name.rsplit_once('.') else {
            continue;
        };
        if !extension.eq_ignore_ascii_case("png")
            || stem.is_empty()
            || !stem.bytes().all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let number = stem
            .parse::<u64>()
            .map_err(|_| refusal("numeric crop filename exceeds its integer bound"))?;
        maximum = maximum.max(number);
    }
    Ok(maximum)
}

pub(super) fn check_remove(draft: &PackageDraft, path: &str) -> Result<(), Fault> {
    let Some(document) = load(draft)? else {
        return Ok(());
    };
    let manifest = draft.manifest()?;
    for id in [AUTHORING_ASSET, TEMPLATE_MAPS_ASSET, ENGINE_MANIFEST_ASSET]
        .into_iter()
        .chain(
            document
                .definitions()
                .filter_map(|item| item.saved.as_ref().map(|saved| saved.asset.as_str())),
        )
    {
        if manifest["assets"][id]["path"].as_str() == Some(path) {
            return Err(refusal(
                "remove retained recognition definitions through Recognition Save before removing referenced assets; pasted source is not refactored",
            ));
        }
    }
    if let Some(declarations) = manifest["assets"].as_object() {
        for (id, declaration) in declarations {
            if declaration["path"].as_str() == Some(path)
                && unrelated_reference(&manifest, draft.files(), id, path)?
            {
                return Err(refusal(
                    "asset remains referenced by an unrelated JSON consumer",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn synchronize<'a>(
    documents: impl IntoIterator<Item = &'a mado_runtime_comparison::recognition::RecognitionDocument>,
    manifest: &mut Value,
    files: &mut BTreeMap<String, PayloadBytes>,
    owned: bool,
) -> Result<(), Fault> {
    let id = manifest["package_id"]
        .as_str()
        .ok_or_else(|| refusal("package identity is missing"))?;
    match build_template_assets(documents, id)? {
        Some((maps, engine)) => {
            let maps = serde_json::to_vec(&maps)
                .map_err(|_| refusal("template maps cannot be encoded"))?;
            put_json(
                manifest,
                files,
                TEMPLATE_MAPS_ASSET,
                "recognition/template-maps.json",
                maps,
                owned,
            )?;
            put_json(
                manifest,
                files,
                ENGINE_MANIFEST_ASSET,
                "recognition/engine-manifest.json",
                engine,
                owned,
            )?;
        }
        None => {
            for id in [TEMPLATE_MAPS_ASSET, ENGINE_MANIFEST_ASSET] {
                if let Some(path) = manifest["assets"][id]["path"].as_str().map(str::to_owned) {
                    if !owned {
                        return Err(refusal("recognition asset ID is occupied"));
                    }
                    files.remove(&path);
                    manifest["assets"]
                        .as_object_mut()
                        .ok_or_else(|| refusal("asset declarations are missing"))?
                        .remove(id);
                }
            }
        }
    }
    Ok(())
}

fn put_json(
    manifest: &mut Value,
    files: &mut BTreeMap<String, PayloadBytes>,
    id: &str,
    default_path: &str,
    bytes: Vec<u8>,
    owned: bool,
) -> Result<(), Fault> {
    let path = if let Some(path) = manifest["assets"][id]["path"].as_str() {
        if !owned {
            return Err(refusal("recognition asset ID is occupied"));
        }
        path.to_owned()
    } else {
        vacant(manifest, files, id, default_path)?;
        default_path.to_owned()
    };
    manifest["assets"][id] = json!({"path":path,"format":"json","width":0,"height":0});
    files.insert(path, PayloadBytes::new(bytes)?);
    Ok(())
}

fn vacant(
    manifest: &Value,
    files: &BTreeMap<String, PayloadBytes>,
    id: &str,
    path: &str,
) -> Result<(), Fault> {
    PackageDraft::check_id(id)?;
    PackageDraft::check_path(path)?;
    if manifest["assets"]
        .as_object()
        .is_some_and(|assets| assets.keys().any(|key| key.eq_ignore_ascii_case(id)))
        || files.keys().any(|key| key.eq_ignore_ascii_case(path))
    {
        return Err(refusal(
            "recognition asset ID or path collides with existing package content",
        ));
    }
    Ok(())
}

fn unrelated_reference(
    manifest: &Value,
    files: &BTreeMap<String, PayloadBytes>,
    id: &str,
    path: &str,
) -> Result<bool, Fault> {
    let declarations = manifest["assets"]
        .as_object()
        .ok_or_else(|| refusal("asset declarations are missing"))?;
    for (asset, declaration) in declarations {
        if [AUTHORING_ASSET, TEMPLATE_MAPS_ASSET, ENGINE_MANIFEST_ASSET].contains(&asset.as_str())
            || declaration["format"] != "json"
        {
            continue;
        }
        let bytes = declaration["path"]
            .as_str()
            .and_then(|path| files.get(path))
            .ok_or_else(|| refusal("JSON asset is missing"))?;
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| refusal("JSON asset is malformed"))?;
        if references(&value, id, path) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn references(value: &Value, id: &str, path: &str) -> bool {
    match value {
        Value::String(text) => text == id || text == path,
        Value::Array(values) => values.iter().any(|value| references(value, id, path)),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| key == id || key == path || references(value, id, path)),
        _ => false,
    }
}

fn refusal(message: &str) -> Fault {
    Fault::new("RecognitionSave", message)
}
