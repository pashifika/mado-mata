use super::capture::{capture_files, capture_recovery_files, checked_root, inventory_from_files};
use super::validation::{
    ImageBudget, catalog, declare, portable_component, portable_path, sha256, validate_asset,
    validate_manifest, validate_source,
};
use super::{Inventory, Manifest, invalid};
use crate::images::PayloadBytes;
use crate::model::{Fault, Limits};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftFileKind {
    Manifest,
    Source,
    Schema,
    Profile,
    Asset,
    SourceMap,
}

/// Safe, bounded source bytes, not an executable inventory. Invalid JSON definitions
/// and incomplete programs remain available for repair without relaxing Inventory.
#[derive(Clone, Debug)]
pub struct PackageDraft {
    files: BTreeMap<String, PayloadBytes>,
    kinds: BTreeMap<String, DraftFileKind>,
    directories: BTreeSet<String>,
    manifest: Manifest,
    revision: String,
    limits: Limits,
}

impl PackageDraft {
    /// Safe filesystem capture for journal recovery while declarations are between revisions.
    /// This never creates an executable inventory or relaxes capture's filesystem checks.
    pub fn capture_recovery_bytes(
        root: &Path,
        limits: &Limits,
    ) -> Result<BTreeMap<String, PayloadBytes>, Fault> {
        Ok(capture_recovery_files(root, limits)?.files)
    }
    pub fn capture(root: &Path, limits: &Limits) -> Result<Self, Fault> {
        let capture = capture_files(root, limits, None)?;
        let directories = capture.directories().map(str::to_owned).collect();
        let mut draft = Self::from_files(capture.files, limits)?;
        draft.directories = directories;
        Ok(draft)
    }

    pub fn canonical_root(root: &Path) -> Result<PathBuf, Fault> {
        checked_root(root)
    }

    pub fn check_path(path: &str) -> Result<(), Fault> {
        portable_path(path)
    }

    pub fn check_id(id: &str) -> Result<(), Fault> {
        portable_component(id)
    }

    pub fn from_files(
        files: BTreeMap<String, PayloadBytes>,
        limits: &Limits,
    ) -> Result<Self, Fault> {
        limits.validate()?;
        if files.len() > limits.snapshot_files {
            return Err(invalid("snapshot file limit exceeded"));
        }
        let mut total = 0usize;
        let mut paths = BTreeMap::new();
        for (path, bytes) in &files {
            portable_path(path)?;
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| invalid("snapshot size overflows"))?;
            if total > limits.snapshot_bytes {
                return Err(invalid("snapshot byte limit exceeded"));
            }
            let mut prefix = String::new();
            for component in path.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                if let Some(existing) = paths.insert(prefix.to_ascii_lowercase(), prefix.clone()) {
                    if existing != prefix {
                        return Err(invalid("case-colliding package paths")
                            .with_context(json!({"path":path})));
                    }
                }
            }
            if path.split('/').count() > 1 {
                let mut parent = path.as_str();
                while let Some((next, _)) = parent.rsplit_once('/') {
                    if files.contains_key(next) {
                        return Err(invalid("package file is also a parent directory"));
                    }
                    parent = next;
                }
            }
        }
        let raw = files
            .get("package.json")
            .ok_or_else(|| invalid("package.json is required"))?;
        if raw.len() > crate::images::PACKAGE_NON_IMAGE_BYTES {
            return Err(invalid("package non-image byte limit exceeded"));
        }
        let manifest: Manifest = serde_json::from_slice(raw).map_err(|error| {
            invalid("invalid package.json").with_context(json!({"path":"package.json","line":error.line(),"column":error.column(),"cause":error.to_string()}))
        })?;
        validate_manifest(&manifest)?;
        let mut image_budget = ImageBudget::default();
        for (id, asset) in &manifest.assets {
            let bytes = files.get(&asset.path).ok_or_else(|| {
                invalid("declared file is missing").with_context(json!({"path":asset.path}))
            })?;
            image_budget.add(id, asset, bytes.len())?;
        }
        image_budget.check_total(total)?;
        let mut declared = BTreeSet::from(["package.json".to_owned()]);
        let mut kinds = BTreeMap::from([("package.json".to_owned(), DraftFileKind::Manifest)]);
        let mut add = |path: &str, kind| -> Result<(), Fault> {
            declare(&mut declared, path)?;
            let bytes = files.get(path).ok_or_else(|| {
                invalid("declared file is missing").with_context(json!({"path":path}))
            })?;
            if kind != DraftFileKind::Asset {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| invalid("file is not UTF-8").with_context(json!({"path":path})))?;
                if text.contains('\0') {
                    return Err(
                        invalid("text contains a NUL byte").with_context(json!({"path":path}))
                    );
                }
                if kind == DraftFileKind::Source {
                    validate_source(path, text, &manifest.runtime)?;
                }
            }
            kinds.insert(path.to_owned(), kind);
            Ok(())
        };
        for path in &manifest.sources {
            add(path, DraftFileKind::Source)?;
        }
        add(&manifest.schema, DraftFileKind::Schema)?;
        let mut ids = BTreeSet::new();
        for (id, path) in &manifest.profiles {
            portable_component(id)?;
            if !ids.insert(id.to_ascii_lowercase()) {
                return Err(invalid("case-colliding profile IDs"));
            }
            add(path, DraftFileKind::Profile)?;
        }
        ids.clear();
        for (id, asset) in &manifest.assets {
            portable_component(id)?;
            if !ids.insert(id.to_ascii_lowercase()) {
                return Err(invalid("case-colliding asset IDs"));
            }
            add(&asset.path, DraftFileKind::Asset)?;
            validate_asset(id, asset, &files[&asset.path])?;
        }
        for (module, path) in &manifest.source_maps {
            if !manifest.sources.contains(module) {
                return Err(invalid("source map refers to undeclared source"));
            }
            add(path, DraftFileKind::SourceMap)?;
        }
        if files.len() != declared.len() {
            let path = files.keys().find(|path| !declared.contains(*path));
            return Err(invalid("undeclared package file").with_context(json!({"path":path})));
        }
        let (approved, _) = catalog(&manifest)?;
        if files.len() + approved.len() > limits.snapshot_files
            || approved.values().map(String::len).sum::<usize>() > limits.snapshot_bytes - total
        {
            return Err(invalid(
                "approved dependency closure exceeds snapshot bounds",
            ));
        }
        image_budget.check_total(total + approved.values().map(String::len).sum::<usize>())?;
        if paths.len() > limits.snapshot_files.saturating_mul(2) {
            return Err(invalid("snapshot directory bound exceeded"));
        }
        let directories = paths
            .into_values()
            .filter(|path| !files.contains_key(path))
            .collect();
        let identity: BTreeMap<_, _> = files
            .iter()
            .map(|(path, bytes)| (path, sha256(bytes)))
            .collect();
        let revision =
            sha256(&serde_json::to_vec(&identity).map_err(|error| invalid(error.to_string()))?);
        Ok(Self {
            files,
            kinds,
            directories,
            manifest,
            revision,
            limits: limits.clone(),
        })
    }

    /// Catalog removal leaves directories in place. Count those retained empty
    /// directories as well as new parents before starting a coordinated write.
    pub fn check_directory_budget(&self, previous: &Self) -> Result<(), Fault> {
        if self.files.len() + self.directories.union(&previous.directories).count()
            > self.limits.snapshot_files.saturating_mul(2)
        {
            return Err(invalid("prospective snapshot directory bound exceeded"));
        }
        Ok(())
    }

    pub fn files(&self) -> &BTreeMap<String, PayloadBytes> {
        &self.files
    }
    pub fn kinds(&self) -> &BTreeMap<String, DraftFileKind> {
        &self.kinds
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn package_id(&self) -> &str {
        &self.manifest.package_id
    }
    pub fn manifest(&self) -> Result<Value, Fault> {
        serde_json::to_value(&self.manifest).map_err(|error| invalid(error.to_string()))
    }
    pub fn validate(&self) -> Result<Inventory, Fault> {
        inventory_from_files(self.files.clone(), &self.limits)
    }
}
