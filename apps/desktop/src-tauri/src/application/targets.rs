use super::{
    Application, AuthoringRef, Selected, TargetCheckResponse, TargetContext, TargetSaveResponse,
    TargetView, WorkspaceRef, Workspaces, lock,
};
use crate::authoring::Candidate;
use crate::target::{TargetConfiguration, TargetExpectation, TargetRecord, TargetResolution};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::Fault;
use std::sync::Arc;

/// Who admits a target command: an inspected Workspace outside Edit, or the current
/// Edit owner at its saved revision. Both use the same machine-local target store.
#[derive(Clone, Debug)]
pub(super) enum TargetOwner {
    Workspace(WorkspaceRef),
    Authoring {
        owner: AuthoringRef,
        revision: String,
    },
}

/// The package declaration and store scope a target command acts on.
#[derive(Clone, Debug)]
pub(super) struct TargetScope {
    pub(super) workspace: WorkspaceRef,
    pub(super) internal_name: String,
    pub(super) package_id: String,
    pub(super) declaration: Option<TargetDeclaration>,
    pub(super) declaration_identity: Option<String>,
}

/// An admitted owner whose declaration is read after the workspace lock is released.
pub(super) enum ResolvedTarget {
    Selected(Selected),
    Authoring {
        workspace: WorkspaceRef,
        internal_name: String,
        candidate: Arc<Candidate>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TargetAccess {
    Read,
    Use,
    Mutate,
}

impl TargetOwner {
    pub(super) fn workspace(&self) -> &WorkspaceRef {
        match self {
            Self::Workspace(workspace) => workspace,
            Self::Authoring { owner, .. } => &owner.workspace,
        }
    }
}

impl TargetScope {
    pub(super) fn selected(selected: &Selected) -> Self {
        Self {
            workspace: selected.workspace.clone(),
            internal_name: selected.internal_name.clone(),
            package_id: selected.inventory.package_id.clone(),
            declaration: selected.package.target.clone(),
            declaration_identity: selected.package.target_identity.clone(),
        }
    }

    pub(super) fn declared(&self) -> Result<&TargetDeclaration, Fault> {
        self.declaration.as_ref().ok_or_else(target_undeclared)
    }
}

impl ResolvedTarget {
    pub(super) fn scope(self) -> Result<TargetScope, Fault> {
        match self {
            Self::Selected(selected) => Ok(TargetScope::selected(&selected)),
            Self::Authoring {
                workspace,
                internal_name,
                candidate,
            } => {
                let declaration = candidate.validate()?.target()?;
                Ok(TargetScope {
                    workspace,
                    internal_name,
                    package_id: candidate.package_id().to_owned(),
                    declaration_identity: declaration
                        .as_ref()
                        .map(TargetDeclaration::identity)
                        .transpose()?,
                    declaration,
                })
            }
        }
    }
}

impl Workspaces {
    /// Admission for target commands. A Workspace keeps the ordinary idle rules; the
    /// Edit owner uses its lease and, for mutations, a released native capture.
    pub(super) fn target_owner(
        &self,
        owner: &TargetOwner,
        access: TargetAccess,
    ) -> Result<ResolvedTarget, Fault> {
        match owner {
            TargetOwner::Workspace(workspace) => {
                let selected = self.resolve(workspace)?.clone();
                if access != TargetAccess::Read {
                    self.idle()?;
                }
                Ok(ResolvedTarget::Selected(selected))
            }
            TargetOwner::Authoring { owner, revision } => {
                let candidate = self.authoring_revision(owner, revision)?;
                if access != TargetAccess::Read {
                    self.work_idle()?;
                }
                if access == TargetAccess::Mutate {
                    self.native_capture_released(owner)?;
                }
                Ok(ResolvedTarget::Authoring {
                    workspace: owner.workspace.clone(),
                    internal_name: self
                        .resolve_workspace(&owner.workspace)?
                        .internal_name
                        .clone(),
                    candidate,
                })
            }
        }
    }

    /// Whether the owner and declaration a pending observation was admitted for remain current.
    pub(super) fn target_current(
        &self,
        owner: &TargetOwner,
        scope: &TargetScope,
    ) -> Result<bool, Fault> {
        Ok(match owner {
            TargetOwner::Workspace(workspace) => {
                let selected = self.resolve(workspace)?;
                selected.package.target_identity == scope.declaration_identity
                    && selected.inventory.package_id == scope.package_id
            }
            // One saved revision has exactly one declaration.
            TargetOwner::Authoring { owner, revision } => {
                self.authoring_revision(owner, revision)?;
                true
            }
        })
    }
}

impl Application {
    pub fn read_target(&self, workspace: &WorkspaceRef) -> Result<TargetView, Fault> {
        self.read_target_for(TargetOwner::Workspace(workspace.clone()))
    }

    pub fn authoring_read_target(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<TargetView, Fault> {
        self.read_target_for(TargetOwner::Authoring {
            owner: owner.clone(),
            revision: revision.to_owned(),
        })
    }

    fn read_target_for(&self, owner: TargetOwner) -> Result<TargetView, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let resolved = state.target_owner(&owner, TargetAccess::Read)?;
            drop(state);
            let scope = resolved.scope()?;
            let record = lock(&self.store).read_target(&scope.internal_name, &scope.package_id)?;
            Ok(target_view(&scope, record))
        })();
        self.outcome(Some(owner.workspace()), "read_target", None, result)
    }

    pub fn check_target(
        &self,
        workspace: &WorkspaceRef,
        expected: &TargetExpectation,
        configuration: &TargetConfiguration,
    ) -> Result<TargetCheckResponse, Fault> {
        self.check_target_for(
            TargetOwner::Workspace(workspace.clone()),
            expected,
            configuration,
        )
    }

    pub fn authoring_check_target(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        expected: &TargetExpectation,
        configuration: &TargetConfiguration,
    ) -> Result<TargetCheckResponse, Fault> {
        self.check_target_for(
            TargetOwner::Authoring {
                owner: owner.clone(),
                revision: revision.to_owned(),
            },
            expected,
            configuration,
        )
    }

    fn check_target_for(
        &self,
        owner: TargetOwner,
        expected: &TargetExpectation,
        configuration: &TargetConfiguration,
    ) -> Result<TargetCheckResponse, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            self.collect(&mut state);
            let resolved = state.target_owner(&owner, TargetAccess::Use)?;
            drop(state);
            let scope = resolved.scope()?;
            let check = lock(&self.store).check_target(
                &scope.internal_name,
                &scope.package_id,
                scope.declared()?,
                expected,
                configuration,
            )?;
            Ok(TargetCheckResponse {
                context: target_context(&scope),
                revision: expected.revision,
                binding_id: expected.binding_id.clone(),
                check,
            })
        })();
        self.outcome(Some(owner.workspace()), "check_target", None, result)
    }

    pub fn save_target(
        &self,
        workspace: &WorkspaceRef,
        expected: &TargetExpectation,
        configuration: TargetConfiguration,
        reviewed_resolution: Option<&TargetResolution>,
    ) -> Result<TargetSaveResponse, Fault> {
        self.save_target_for(
            TargetOwner::Workspace(workspace.clone()),
            expected,
            configuration,
            reviewed_resolution,
        )
    }

    pub fn authoring_save_target(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        expected: &TargetExpectation,
        configuration: TargetConfiguration,
        reviewed_resolution: Option<&TargetResolution>,
    ) -> Result<TargetSaveResponse, Fault> {
        self.save_target_for(
            TargetOwner::Authoring {
                owner: owner.clone(),
                revision: revision.to_owned(),
            },
            expected,
            configuration,
            reviewed_resolution,
        )
    }

    fn save_target_for(
        &self,
        owner: TargetOwner,
        expected: &TargetExpectation,
        configuration: TargetConfiguration,
        reviewed_resolution: Option<&TargetResolution>,
    ) -> Result<TargetSaveResponse, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            self.collect(&mut state);
            let resolved = state.target_owner(&owner, TargetAccess::Mutate)?;
            self.invalidate_target_observation(Some(owner.workspace()));
            drop(state);
            let scope = resolved.scope()?;
            let (record, check) = lock(&self.store).save_target(
                &scope.internal_name,
                &scope.package_id,
                scope.declared()?,
                expected,
                configuration,
                reviewed_resolution,
            )?;
            self.target_changed(&owner);
            Ok(TargetSaveResponse {
                view: target_view(&scope, record),
                check,
            })
        })();
        self.outcome(
            Some(owner.workspace()),
            "save_target",
            Some(("target.saved", "Target configuration saved")),
            result,
        )
    }

    pub fn remove_target(
        &self,
        workspace: &WorkspaceRef,
        expected: &TargetExpectation,
    ) -> Result<TargetView, Fault> {
        self.remove_target_for(TargetOwner::Workspace(workspace.clone()), expected)
    }

    pub fn authoring_remove_target(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        expected: &TargetExpectation,
    ) -> Result<TargetView, Fault> {
        self.remove_target_for(
            TargetOwner::Authoring {
                owner: owner.clone(),
                revision: revision.to_owned(),
            },
            expected,
        )
    }

    fn remove_target_for(
        &self,
        owner: TargetOwner,
        expected: &TargetExpectation,
    ) -> Result<TargetView, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            self.collect(&mut state);
            let resolved = state.target_owner(&owner, TargetAccess::Mutate)?;
            self.invalidate_target_observation(Some(owner.workspace()));
            drop(state);
            let scope = resolved.scope()?;
            let record = lock(&self.store).remove_target(
                &scope.internal_name,
                &scope.package_id,
                expected,
            )?;
            self.target_changed(&owner);
            Ok(target_view(&scope, record))
        })();
        self.outcome(
            Some(owner.workspace()),
            "remove_target",
            Some(("target.removed", "Target binding removed")),
            result,
        )
    }

    /// A saved binding change makes the Edit owner's capture target state stale.
    fn target_changed(&self, owner: &TargetOwner) {
        if let TargetOwner::Authoring { owner, .. } = owner
            && let Some(lease) = lock(&self.workspaces)
                .authoring
                .as_mut()
                .filter(|lease| lease.owner == *owner)
        {
            lease.native.invalidate_target();
        }
    }
}

pub(super) fn target_context(scope: &TargetScope) -> TargetContext {
    TargetContext {
        workspace: scope.workspace.clone(),
        internal_name: scope.internal_name.clone(),
        package_id: scope.package_id.clone(),
        declaration_identity: scope.declaration_identity.clone(),
    }
}

pub(super) fn target_view(scope: &TargetScope, record: TargetRecord) -> TargetView {
    let compatible = record.binding.as_ref().is_some_and(|binding| {
        scope
            .declaration
            .as_ref()
            .zip(scope.declaration_identity.as_deref())
            .is_some_and(|(declaration, identity)| {
                binding.compatible(&scope.package_id, &declaration.id, identity)
            })
    });
    TargetView {
        platform: std::env::consts::OS,
        context: target_context(scope),
        record,
        compatible,
    }
}

fn target_undeclared() -> Fault {
    Fault::new("TargetUndeclared", "Package has no target declaration")
}

#[cfg(test)]
mod tests;
