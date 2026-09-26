use super::{Edit, MAX_BYTES, limits};
use mado_runtime_comparison::images::{PACKAGE_IMAGE_BYTES, PayloadBytes};
use mado_runtime_comparison::inventory::{DraftFileKind, PackageDraft};
use mado_runtime_comparison::model::Fault;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogFileKind {
    Source,
    Profile,
    Asset,
    SourceMap,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogEdit {
    Add {
        path: String,
        file_kind: CatalogFileKind,
        text: Option<String>,
        bytes: Option<Vec<u8>>,
        id: Option<String>,
        module: Option<String>,
        format: Option<String>,
        width: Option<usize>,
        height: Option<usize>,
    },
    Rename {
        path: String,
        destination: String,
    },
    Remove {
        path: String,
    },
}

fn refusal(message: &str) -> Fault {
    Fault::new("AuthoringCatalog", message)
}

pub(super) fn encode(value: &Value) -> Result<PayloadBytes, Fault> {
    PayloadBytes::new(crate::storage::encode(value, MAX_BYTES)?)
}

pub(super) fn apply(draft: &PackageDraft, edit: Edit) -> Result<PackageDraft, Fault> {
    let validate_recognition = matches!(&edit, Edit::Catalog(_));
    // Cloning this catalog shares immutable bytes; only edited files allocate new content.
    let mut files = draft.files().clone();
    match edit {
        Edit::Recognition(save) => return super::recognition::apply(draft, save),
        Edit::Text { path, text } => {
            PackageDraft::check_path(&path)?;
            let kind = draft
                .kinds()
                .get(&path)
                .ok_or_else(|| refusal("file is not declared by this package"))?;
            if *kind == DraftFileKind::Asset {
                return Err(refusal("binary assets are not text editor documents"));
            }
            if text.len() > MAX_BYTES {
                return Err(refusal("text exceeds the snapshot byte limit"));
            }
            files.insert(path, PayloadBytes::new(text.into_bytes())?);
        }
        Edit::Catalog(edit) => {
            let mut manifest = draft.manifest()?;
            match edit {
                CatalogEdit::Add {
                    path,
                    file_kind,
                    text,
                    bytes,
                    id,
                    module,
                    format,
                    width,
                    height,
                } => {
                    PackageDraft::check_path(&path)?;
                    if files.contains_key(&path) {
                        return Err(refusal("add destination is already declared"));
                    }
                    let content = match (text, bytes, file_kind) {
                        (Some(text), None, _) if text.len() <= MAX_BYTES => {
                            PayloadBytes::new(text.into_bytes())?
                        }
                        (None, Some(bytes), CatalogFileKind::Asset)
                            if bytes.len()
                                <= if matches!(format.as_deref(), Some("png" | "raw-rgba8")) {
                                    PACKAGE_IMAGE_BYTES
                                } else {
                                    MAX_BYTES
                                } =>
                        {
                            PayloadBytes::new(bytes)?
                        }
                        _ => {
                            return Err(refusal(
                                "provide exactly one bounded text document, or bytes for an asset",
                            ));
                        }
                    };
                    match file_kind {
                        CatalogFileKind::Source => {
                            manifest["sources"]
                                .as_array_mut()
                                .ok_or_else(|| refusal("sources are missing"))?
                                .push(json!(path));
                        }
                        CatalogFileKind::Profile => {
                            let id = id.ok_or_else(|| refusal("a profile ID is required"))?;
                            PackageDraft::check_id(&id)?;
                            if manifest["profiles"].get(&id).is_some() {
                                return Err(refusal("profile ID is already declared"));
                            }
                            manifest["profiles"][id] = json!(path);
                        }
                        CatalogFileKind::Asset => {
                            let id = id.ok_or_else(|| refusal("an asset ID is required"))?;
                            PackageDraft::check_id(&id)?;
                            if manifest["assets"].get(&id).is_some() {
                                return Err(refusal("asset ID is already declared"));
                            }
                            manifest["assets"][id] = json!({"path":path,"format":format.ok_or_else(|| refusal("asset format is required"))?,"width":width.ok_or_else(|| refusal("asset width is required"))?,"height":height.ok_or_else(|| refusal("asset height is required"))?});
                        }
                        CatalogFileKind::SourceMap => {
                            let module =
                                module.ok_or_else(|| refusal("source map module is required"))?;
                            if manifest["source_maps"].get(&module).is_some() {
                                return Err(refusal("module already has a source map"));
                            }
                            manifest["source_maps"][module] = json!(path);
                        }
                    }
                    files.insert(path, content);
                }
                CatalogEdit::Rename { path, destination } => {
                    PackageDraft::check_path(&destination)?;
                    if path == "package.json" || files.contains_key(&destination) {
                        return Err(refusal(
                            "manifest cannot be renamed and destination must be missing",
                        ));
                    }
                    let bytes = files
                        .remove(&path)
                        .ok_or_else(|| refusal("rename source is not declared"))?;
                    rename(&mut manifest, &path, &destination)?;
                    files.insert(destination, bytes);
                }
                CatalogEdit::Remove { path } => {
                    super::recognition::check_remove(draft, &path)?;
                    if path == "package.json"
                        || draft.kinds().get(&path) == Some(&DraftFileKind::Schema)
                    {
                        return Err(refusal("the manifest and schema are required"));
                    }
                    files
                        .remove(&path)
                        .ok_or_else(|| refusal("remove source is not declared"))?;
                    manifest["sources"]
                        .as_array_mut()
                        .ok_or_else(|| refusal("sources are missing"))?
                        .retain(|item| item.as_str() != Some(&path));
                    for key in ["profiles", "source_maps"] {
                        manifest[key]
                            .as_object_mut()
                            .ok_or_else(|| refusal("declarations are missing"))?
                            .retain(|_, value| value.as_str() != Some(&path));
                    }
                    manifest["assets"]
                        .as_object_mut()
                        .ok_or_else(|| refusal("assets are missing"))?
                        .retain(|_, asset| asset["path"].as_str() != Some(&path));
                }
            }
            files.insert("package.json".into(), encode(&manifest)?);
        }
    }
    let next = PackageDraft::from_files(files, &limits()?)?;
    if validate_recognition {
        super::recognition::load(&next)?;
    }
    if next.package_id() != draft.package_id() {
        return Err(refusal("package ID changes require Duplicate"));
    }
    Ok(next)
}

fn rename(manifest: &mut Value, path: &str, destination: &str) -> Result<(), Fault> {
    for source in manifest["sources"]
        .as_array_mut()
        .ok_or_else(|| refusal("sources are missing"))?
    {
        if source.as_str() == Some(path) {
            *source = json!(destination);
        }
    }
    if manifest["schema"].as_str() == Some(path) {
        manifest["schema"] = json!(destination);
    }
    for name in ["readiness", "workflow"] {
        if manifest["entries"][name]["module"].as_str() == Some(path) {
            manifest["entries"][name]["module"] = json!(destination);
        }
    }
    for key in ["profiles", "source_maps"] {
        for value in manifest[key]
            .as_object_mut()
            .ok_or_else(|| refusal("declarations are missing"))?
            .values_mut()
        {
            if value.as_str() == Some(path) {
                *value = json!(destination);
            }
        }
    }
    let maps = manifest["source_maps"]
        .as_object_mut()
        .ok_or_else(|| refusal("source maps are missing"))?;
    if let Some(map) = maps.remove(path) {
        maps.insert(destination.to_owned(), map);
    }
    for asset in manifest["assets"]
        .as_object_mut()
        .ok_or_else(|| refusal("assets are missing"))?
        .values_mut()
    {
        if asset["path"].as_str() == Some(path) {
            asset["path"] = json!(destination);
        }
    }
    Ok(())
}

pub(super) fn duplicate(draft: &PackageDraft, id: &str) -> Result<PackageDraft, Fault> {
    let mut files = draft.files().clone();
    let mut manifest = draft.manifest()?;
    manifest["package_id"] = json!(id);
    for (path, kind) in draft.kinds() {
        if *kind != DraftFileKind::Profile {
            continue;
        }
        let mut profile: Value = serde_json::from_slice(&files[path]).map_err(|error| {
            refusal("repair malformed presets before Duplicate")
                .with_context(json!({"path":path,"cause":error.to_string()}))
        })?;
        if profile["package_id"].as_str() != Some(draft.package_id()) || !profile.is_object() {
            return Err(
                refusal("preset belongs to another package; repair it before Duplicate")
                    .with_context(json!({"path":path})),
            );
        }
        profile["package_id"] = json!(id);
        files.insert(path.clone(), encode(&profile)?);
    }
    if let Some(document) = super::recognition::load(draft)? {
        super::recognition::synchronize(&document, &mut manifest, &mut files, true)?;
    }
    files.insert("package.json".into(), encode(&manifest)?);
    PackageDraft::from_files(files, &limits()?)
}

pub(super) fn starter(id: &str) -> Result<PackageDraft, Fault> {
    let manifest = json!({
        "version":1,"package_id":id,"runtime":"typescript","sdk":"mado-host-v1","entry_contract":"ready-string-v1",
        "entries":{"readiness":{"module":"main.ts","function":"readiness"},"workflow":{"module":"main.ts","function":"workflow"}},
        "sources":["main.ts"],"schema":"schema.json","profiles":{"default":"profiles/default.json"},"assets":{},"source_maps":{},"dependencies":{}
    });
    let schema = json!({"version":1,"type":"object","additionalProperties":false,"required":["message"],"properties":{"message":{"type":"string","minLength":1,"maxLength":256,"default":"Hello from MadoMata"}}});
    let profile = json!({"package_id":id,"schema_version":1,"options":{}});
    let source = "export function readiness(): MadoReady {\n  return \"Ready\";\n}\n\nexport function workflow(): void {\n  host.state.message = host.options.message;\n  host.call(\"log\", { message: host.options.message });\n}\n";
    let files = BTreeMap::from([
        ("package.json".to_owned(), encode(&manifest)?),
        ("schema.json".to_owned(), encode(&schema)?),
        ("profiles/default.json".to_owned(), encode(&profile)?),
        (
            "main.ts".to_owned(),
            PayloadBytes::new(source.as_bytes().to_vec())?,
        ),
    ]);
    let draft = PackageDraft::from_files(files, &limits()?)?;
    draft.validate()?;
    Ok(draft)
}
