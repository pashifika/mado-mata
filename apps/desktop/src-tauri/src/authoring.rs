//! Owner-admitted directory authoring. The Application authenticates its lease and
//! serializes calls; this module owns source revisions and durable publication.
mod catalog;
mod publication;

#[cfg(test)]
mod tests;

use mado_runtime_comparison::inventory::{DraftFileKind, Inventory, PackageDraft};
use mado_runtime_comparison::model::{Fault, Limits, Plan};
use serde::Serialize;
use serde_json::json;
use std::path::{Path, PathBuf};

pub use catalog::{CatalogEdit, CatalogFileKind};

pub const MAX_FILES: usize = 128;
pub const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Candidate {
    root: PathBuf,
    draft: PackageDraft,
    root_identity: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthoringFile {
    pub path: String,
    pub kind: DraftFileKind,
    pub text: Option<String>,
    pub bytes: usize,
}

#[derive(Debug)]
pub struct Commit {
    pub committed_revision: String,
    pub candidate: Option<Candidate>,
    pub refresh_error: Option<Fault>,
}

#[derive(Debug)]
pub enum Edit {
    Text { path: String, text: String },
    Catalog(CatalogEdit),
}

pub struct Publisher {
    data_root: PathBuf,
}

impl Candidate {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn package_id(&self) -> &str {
        self.draft.package_id()
    }
    pub fn revision(&self) -> &str {
        self.draft.revision()
    }
    pub fn files(&self) -> Vec<AuthoringFile> {
        self.draft
            .files()
            .iter()
            .map(|(path, bytes)| {
                let kind = self.draft.kinds()[path];
                AuthoringFile {
                    path: path.clone(),
                    kind,
                    text: if kind == DraftFileKind::Asset {
                        None
                    } else {
                        // PackageDraft has already checked every textual file's encoding.
                        Some(
                            std::str::from_utf8(bytes)
                                .expect("PackageDraft checked text encoding")
                                .to_owned(),
                        )
                    },
                    bytes: bytes.len(),
                }
            })
            .collect()
    }
    pub fn validate(&self) -> Result<Inventory, Fault> {
        self.draft.validate()
    }
}

impl Publisher {
    pub fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    pub fn open(&self, root: &Path) -> Result<Candidate, Fault> {
        self.check_admission(root)?;
        let root = PackageDraft::canonical_root(root)?;
        self.separate_root(&root)?;
        let root_identity = publication::root_identity(&root)?;
        let draft = PackageDraft::capture(&root, &limits()?)?;
        if root_identity != publication::root_identity(&root)? {
            return Err(stale());
        }
        if !matches!(
            draft.manifest()?["runtime"].as_str(),
            Some("typescript" | "javascript")
        ) {
            return Err(Fault::new(
                "RuntimeRefused",
                "desktop authoring supports JavaScript and TypeScript only",
            ));
        }
        Ok(Candidate {
            root,
            draft,
            root_identity,
        })
    }

    pub fn create(&self, root: &Path, id: &str) -> Result<Candidate, Fault> {
        PackageDraft::check_id(id)?;
        self.check_admission(root)?;
        self.create_files(root, catalog::starter(id)?)
    }

    pub fn duplicate(
        &self,
        candidate: &Candidate,
        expected_revision: &str,
        root: &Path,
        id: &str,
    ) -> Result<Candidate, Fault> {
        self.check_current(candidate, expected_revision)?;
        PackageDraft::check_id(id)?;
        if id.eq_ignore_ascii_case(candidate.package_id()) {
            return Err(Fault::new(
                "AuthoringIdentity",
                "Duplicate requires a distinct package ID",
            ));
        }
        let draft = catalog::duplicate(&candidate.draft, id)?;
        self.create_files(root, draft)
    }

    pub fn publish(
        &self,
        candidate: &Candidate,
        expected_revision: &str,
        edit: Edit,
    ) -> Result<Commit, Fault> {
        self.publish_with(candidate, expected_revision, edit, |_| Ok(()), || Ok(()))
    }

    fn publish_with(
        &self,
        candidate: &Candidate,
        expected_revision: &str,
        edit: Edit,
        after_change: impl FnMut(usize) -> Result<(), Fault>,
        before_refresh: impl FnOnce() -> Result<(), Fault>,
    ) -> Result<Commit, Fault> {
        let current = self.check_current(candidate, expected_revision)?;
        let next = catalog::apply(&current.draft, edit)?;
        next.check_directory_budget(&current.draft)?;
        let committed_revision = next.revision().to_owned();
        if let Some(fault) = self.publish_files(&current, &next, after_change)? {
            return Ok(Commit {
                committed_revision,
                candidate: None,
                refresh_error: Some(fault),
            });
        }
        let refreshed = before_refresh()
            .and_then(|()| self.open(candidate.root()))
            .and_then(|view| {
                if view.revision() != committed_revision {
                    Err(stale())
                } else {
                    Ok(view)
                }
            });
        Ok(match refreshed {
            Ok(candidate) => Commit {
                committed_revision,
                candidate: Some(candidate),
                refresh_error: None,
            },
            Err(fault) => Commit {
                committed_revision,
                candidate: None,
                refresh_error: Some(fault),
            },
        })
    }

    fn check_current(
        &self,
        candidate: &Candidate,
        expected_revision: &str,
    ) -> Result<Candidate, Fault> {
        self.check_admission(candidate.root())?;
        if expected_revision != candidate.revision() {
            return Err(stale());
        }
        let current = self.open(candidate.root()).map_err(|cause| {
            stale().with_context(json!({"package_path":candidate.root(),"cause":cause}))
        })?;
        if current.revision() != expected_revision
            || current.root_identity != candidate.root_identity
        {
            return Err(stale());
        }
        Ok(current)
    }

    fn separate_root(&self, root: &Path) -> Result<(), Fault> {
        let mut ancestor = std::path::absolute(&self.data_root)
            .map_err(|error| io_fault("resolve private storage", error))?;
        if ancestor
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(Fault::new(
                "AuthoringPath",
                "private storage may not contain parent traversal",
            ));
        }
        let mut missing = Vec::new();
        while !crate::storage::exists(&ancestor)? {
            missing.push(ancestor.file_name().ok_or_else(stale)?.to_owned());
            if !ancestor.pop() {
                return Err(stale());
            }
        }
        let mut data = ancestor
            .canonicalize()
            .map_err(|error| io_fault("resolve private storage", error))?;
        for name in missing.into_iter().rev() {
            data.push(name);
        }
        if data.starts_with(root) || root.starts_with(&data) {
            return Err(Fault::new(
                "AuthoringPath",
                "package source and application configuration must be separate",
            ));
        }
        Ok(())
    }
}

fn limits() -> Result<Limits, Fault> {
    let plan: Plan = serde_json::from_str(include_str!(
        "../../../../tools/runtime-comparison/fixtures/manual-plan.json"
    ))
    .map_err(|error| Fault::new("InvalidPlan", error.to_string()))?;
    Ok(plan.limits)
}

fn stale() -> Fault {
    Fault::new(
        "AuthoringConflict",
        "source revision changed; retain drafts and deliberately refresh before saving",
    )
}

fn io_fault(action: &str, error: std::io::Error) -> Fault {
    Fault::new("AuthoringStorage", action).with_context(json!({"cause":error.to_string()}))
}
