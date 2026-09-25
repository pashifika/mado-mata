use super::{
    Application, MAX_SESSION_COUNTER, OperationOwner, WorkspaceRef, WorkspaceView, Workspaces, lock,
};
use crate::authoring::{AuthoringFile, Candidate, CatalogEdit, Edit};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthoringRef {
    pub workspace: WorkspaceRef,
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct AuthoringView {
    pub owner: AuthoringRef,
    pub package_path: String,
    pub package_id: String,
    pub revision: String,
    pub files: Vec<AuthoringFile>,
}

#[derive(Debug, Serialize)]
pub struct AuthoringMutation {
    pub owner: AuthoringRef,
    pub committed_revision: String,
    pub view: Option<AuthoringView>,
    pub refresh_error: Option<Fault>,
}

#[derive(Debug, Serialize)]
pub struct AuthoringValidation {
    pub owner: AuthoringRef,
    pub revision: String,
    pub valid: bool,
    pub diagnostics: Vec<Fault>,
}

pub(super) struct Lease {
    pub owner: AuthoringRef,
    candidate: Arc<Candidate>,
    revision: String,
    pub containment: Option<Fault>,
    // Duplicate changes the lease source; every previously edited source still
    // needs explicit reinspection when the session exits.
    roots: BTreeSet<PathBuf>,
}

pub(super) struct StopOwner {
    owner: AuthoringRef,
    run: Option<String>,
}

impl Workspaces {
    fn authoring(&self, owner: &AuthoringRef) -> Result<&Lease, Fault> {
        self.authoring
            .as_ref()
            .filter(|lease| lease.owner == *owner)
            .ok_or_else(|| {
                Fault::new(
                    "StaleAuthoring",
                    "Authoring owner is expired or belongs to another session",
                )
            })
    }

    fn authoring_revision(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<Arc<Candidate>, Fault> {
        let lease = self.authoring(owner)?;
        if lease.revision != revision || lease.candidate.revision() != revision {
            return Err(Fault::new(
                "AuthoringConflict",
                "Source revision changed; refresh before continuing",
            )
            .with_context(json!({"expected_revision":revision,"revision":lease.revision})));
        }
        Ok(lease.candidate.clone())
    }

    fn next_authoring_owner(&self, workspace: &WorkspaceRef) -> Result<AuthoringRef, Fault> {
        if self.next_authoring > MAX_SESSION_COUNTER {
            return Err(Fault::new(
                "AuthoringLimit",
                "Authoring session counter exhausted",
            ));
        }
        Ok(AuthoringRef {
            workspace: workspace.clone(),
            token: format!("authoring-{}-{}", self.session, self.next_authoring),
        })
    }
}

fn view(owner: &AuthoringRef, candidate: &Candidate) -> AuthoringView {
    AuthoringView {
        owner: owner.clone(),
        package_path: candidate.root().to_string_lossy().into_owned(),
        package_id: candidate.package_id().to_owned(),
        revision: candidate.revision().to_owned(),
        files: candidate.files(),
    }
}

impl Application {
    pub fn authoring_open(
        &self,
        workspace: &WorkspaceRef,
        package_path: &Path,
    ) -> Result<AuthoringView, Fault> {
        self.enter_authoring(workspace, || self.publisher.open(package_path))
    }

    pub fn authoring_create(
        &self,
        workspace: &WorkspaceRef,
        package_path: &Path,
        package_id: &str,
    ) -> Result<AuthoringView, Fault> {
        self.enter_authoring(workspace, || {
            self.publisher.create(package_path, package_id)
        })
    }

    fn enter_authoring(
        &self,
        workspace: &WorkspaceRef,
        capture: impl FnOnce() -> Result<Candidate, Fault>,
    ) -> Result<AuthoringView, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.resolve_workspace(workspace)?;
        self.collect(&mut state);
        state.idle()?;
        let owner = state.next_authoring_owner(workspace)?;
        drop(state);
        let candidate = capture()?;
        self.invalidate_target_observation(Some(workspace));
        let result = view(&owner, &candidate);
        let roots = BTreeSet::from([candidate.root().to_path_buf()]);
        let mut state = lock(&self.workspaces);
        state.next_authoring += 1;
        state.authoring = Some(Lease {
            owner: owner.clone(),
            revision: candidate.revision().to_owned(),
            candidate: Arc::new(candidate),
            containment: None,
            roots,
        });
        *lock(&self.authoring_stop) = Some(StopOwner { owner, run: None });
        Ok(result)
    }

    pub fn authoring_duplicate(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        package_path: &Path,
        package_id: &str,
    ) -> Result<AuthoringView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let next = state.next_authoring_owner(&owner.workspace)?;
        drop(state);
        let candidate = self
            .publisher
            .duplicate(&candidate, revision, package_path, package_id)?;
        let result = view(&next, &candidate);
        let mut state = lock(&self.workspaces);
        state.next_authoring += 1;
        let lease = state
            .authoring
            .as_mut()
            .expect("command retains authoring lease");
        lease.roots.insert(candidate.root().to_path_buf());
        lease.owner = next.clone();
        lease.revision = candidate.revision().to_owned();
        lease.candidate = Arc::new(candidate);
        *lock(&self.authoring_stop) = Some(StopOwner {
            owner: next,
            run: None,
        });
        Ok(result)
    }

    pub fn authoring_save(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        path: &str,
        text: String,
    ) -> Result<AuthoringMutation, Fault> {
        self.publish_authoring(
            owner,
            revision,
            Edit::Text {
                path: path.to_owned(),
                text,
            },
        )
    }

    pub fn authoring_catalog(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        edit: CatalogEdit,
    ) -> Result<AuthoringMutation, Fault> {
        self.publish_authoring(owner, revision, Edit::Catalog(edit))
    }

    fn publish_authoring(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        edit: Edit,
    ) -> Result<AuthoringMutation, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        drop(state);
        let committed = self.publisher.publish(&candidate, revision, edit)?;
        let refreshed = committed
            .candidate
            .as_ref()
            .map(|candidate| view(owner, candidate));
        let mut state = lock(&self.workspaces);
        let lease = state
            .authoring
            .as_mut()
            .expect("command retains authoring lease");
        lease.revision = committed.committed_revision.clone();
        if let Some(candidate) = committed.candidate {
            lease.candidate = Arc::new(candidate);
        }
        Ok(AuthoringMutation {
            owner: owner.clone(),
            committed_revision: committed.committed_revision,
            view: refreshed,
            refresh_error: committed.refresh_error,
        })
    }

    pub fn authoring_refresh(&self, owner: &AuthoringRef) -> Result<AuthoringView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let previous = state.authoring(owner)?.candidate.clone();
        self.collect(&mut state);
        state.work_idle()?;
        drop(state);
        let candidate = self.publisher.open(previous.root())?;
        if candidate.package_id() != previous.package_id() || candidate.root() != previous.root() {
            return Err(Fault::new(
                "AuthoringConflict",
                "Authoring source identity changed; exit and open it explicitly",
            ));
        }
        let result = view(owner, &candidate);
        let mut state = lock(&self.workspaces);
        let lease = state
            .authoring
            .as_mut()
            .expect("command retains authoring lease");
        lease.revision = candidate.revision().to_owned();
        lease.candidate = Arc::new(candidate);
        Ok(result)
    }

    pub fn authoring_validate(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<AuthoringValidation, Fault> {
        let (_command, mut state) = self.command_state()?;
        let previous = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let publisher = self.publisher.clone();
        let expected = revision.to_owned();
        #[cfg(test)]
        let gate = lock(&self.authoring_validation_gate).take();
        let mut cancellation = lock(&self.authoring_stop);
        let run = self.runner.validate_authoring(move |control| {
            #[cfg(test)]
            if let Some((started, released)) = gate {
                let _ = started.send(());
                let _ = released.recv();
            }
            control.check()?;
            let candidate = publisher.open(previous.root())?;
            if candidate.revision() != expected || candidate.package_id() != previous.package_id() {
                return Err(Fault::new(
                    "AuthoringConflict",
                    "Source changed before validation; refresh the editor",
                ));
            }
            control.check()?;
            candidate.validate()
        })?;
        state.owner = Some(OperationOwner {
            run: run.clone(),
            workspace: Some(owner.workspace.clone()),
            check: None,
            terminal: false,
        });
        cancellation
            .as_mut()
            .expect("lease owns cancellation slot")
            .run = Some(run);
        drop(cancellation);
        loop {
            self.collect(&mut state);
            if state
                .owner
                .as_ref()
                .is_some_and(|operation| operation.terminal)
            {
                let controller = state.controller.clone();
                lock(&self.authoring_stop)
                    .as_mut()
                    .expect("lease owns cancellation slot")
                    .run = None;
                let diagnostics: Vec<Fault> =
                    if !controller["error"].is_null() {
                        vec![serde_json::from_value(controller["error"].clone()).map_err(
                            |error| Fault::new("AuthoringValidation", error.to_string()),
                        )?]
                    } else {
                        serde_json::from_value(controller["result"]["diagnostics"].clone())
                            .map_err(|error| Fault::new("AuthoringValidation", error.to_string()))?
                    };
                state
                    .authoring
                    .as_mut()
                    .expect("command retains lease")
                    .containment = diagnostics
                    .iter()
                    .find(|fault| {
                        matches!(
                            fault.category.as_str(),
                            "CompilerContainment" | "Controller"
                        )
                    })
                    .cloned();
                drop(state);
                return Ok(AuthoringValidation {
                    owner: owner.clone(),
                    revision: revision.to_owned(),
                    valid: controller["result"]["valid"] == true && diagnostics.is_empty(),
                    diagnostics,
                });
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(5));
            state = lock(&self.workspaces);
        }
    }

    pub fn authoring_stop(&self, owner: &AuthoringRef) -> Result<bool, Fault> {
        let slot = lock(&self.authoring_stop);
        let owned = slot
            .as_ref()
            .filter(|slot| slot.owner == *owner)
            .ok_or_else(|| {
                Fault::new(
                    "StaleAuthoring",
                    "Stop does not name the current authoring owner",
                )
            })?;
        let Some(run) = &owned.run else {
            return Ok(false);
        };
        self.runner.stop(run)?;
        Ok(true)
    }

    pub fn authoring_exit(&self, owner: &AuthoringRef) -> Result<WorkspaceView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let lease = state.authoring(owner)?;
        self.publisher.check_admission(lease.candidate.root())?;
        self.collect(&mut state);
        state.work_idle()?;
        let lease = state.authoring.as_ref().expect("owner checked");
        let affected = |workspace: &super::Workspace| {
            workspace.workspace == owner.workspace
                || workspace
                    .selected
                    .as_ref()
                    .is_some_and(|selected| lease.roots.contains(&selected.path))
                || workspace
                    .recovery
                    .as_ref()
                    .is_some_and(|recovery| lease.roots.contains(recovery.package_path()))
        };
        if state
            .open
            .iter()
            .filter(|workspace| affected(workspace))
            .any(|workspace| workspace.workspace.revision >= MAX_SESSION_COUNTER)
        {
            return Err(Fault::new("WorkspaceLimit", "Workspace revision exhausted"));
        }
        let affected: Vec<_> = state
            .open
            .iter()
            .enumerate()
            .filter_map(|(index, workspace)| affected(workspace).then_some(index))
            .collect();
        for index in affected {
            let workspace = &mut state.open[index];
            self.invalidate_target_observation(Some(&workspace.workspace));
            workspace.workspace.revision += 1;
            workspace.selected = None;
            workspace.recovery = None;
            workspace.source_error = Some(Fault::new(
                "AuthoringReinspectRequired",
                "Inspect the saved package explicitly before running",
            ));
        }
        state.authoring = None;
        *lock(&self.authoring_stop) = None;
        let workspace = state
            .open
            .iter()
            .find(|workspace| workspace.workspace.workspace_id == owner.workspace.workspace_id)
            .expect("owner workspace retained")
            .clone();
        drop(state);
        Ok(self.workspace_view(&workspace))
    }

    pub fn authoring_recover(&self, package_path: &Path) -> Result<(), Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.work_idle()?;
        drop(state);
        self.publisher.recover(package_path)
    }

    pub fn authoring_owner(&self) -> Option<AuthoringRef> {
        lock(&self.authoring_stop)
            .as_ref()
            .map(|slot| slot.owner.clone())
    }

    /// Retire admission after the WebView resolves drafts. Keep the lease until
    /// shutdown settles work and reports any containment failure.
    pub fn prepare_close(&self, resolved_owner: Option<&AuthoringRef>) -> Result<(), Fault> {
        if self.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        {
            let slot = lock(&self.authoring_stop);
            if let Some(lease) = slot.as_ref() {
                if resolved_owner != Some(&lease.owner) {
                    return Err(Fault::new(
                        "AuthoringActive",
                        "Resolve editor drafts before closing",
                    )
                    .with_context(json!({"owner":lease.owner})));
                }
                self.closing.store(true, Ordering::Release);
                return Ok(());
            }
        }
        let (_command, state) = match self.command_state() {
            Ok(state) => state,
            Err(fault) if fault.category == "Closing" => return Ok(()),
            Err(fault) => return Err(fault),
        };
        if let Some(lease) = &state.authoring {
            return Err(
                Fault::new("AuthoringActive", "Resolve editor drafts before closing")
                    .with_context(json!({"owner":lease.owner})),
            );
        }
        self.closing.store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
