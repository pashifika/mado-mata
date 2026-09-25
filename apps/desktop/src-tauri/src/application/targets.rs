use super::{
    Application, Selected, TargetCheckResponse, TargetContext, TargetSaveResponse, TargetView,
    WorkspaceRef, lock,
};
use crate::target::{TargetConfiguration, TargetExpectation, TargetRecord, TargetResolution};
use mado_runtime_comparison::model::Fault;

impl Application {
    pub fn read_target(&self, workspace: &WorkspaceRef) -> Result<TargetView, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let record = lock(&self.store)
                .read_target(&selected.internal_name, &selected.inventory.package_id)?;
            Ok(target_view(&selected, record))
        })();
        self.outcome(Some(workspace), "read_target", None, result)
    }

    pub fn check_target(
        &self,
        workspace: &WorkspaceRef,
        expected: &TargetExpectation,
        configuration: &TargetConfiguration,
    ) -> Result<TargetCheckResponse, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            self.collect(&mut state);
            state.idle()?;
            drop(state);
            let declaration = selected
                .package
                .target
                .as_ref()
                .ok_or_else(target_undeclared)?;
            let check = lock(&self.store).check_target(
                &selected.internal_name,
                &selected.inventory.package_id,
                declaration,
                expected,
                configuration,
            )?;
            Ok(TargetCheckResponse {
                context: target_context(&selected),
                revision: expected.revision,
                binding_id: expected.binding_id.clone(),
                check,
            })
        })();
        self.outcome(Some(workspace), "check_target", None, result)
    }

    pub fn save_target(
        &self,
        workspace: &WorkspaceRef,
        expected: &TargetExpectation,
        configuration: TargetConfiguration,
        reviewed_resolution: Option<&TargetResolution>,
    ) -> Result<TargetSaveResponse, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            self.collect(&mut state);
            state.idle()?;
            self.invalidate_target_observation(Some(workspace));
            drop(state);
            let declaration = selected
                .package
                .target
                .as_ref()
                .ok_or_else(target_undeclared)?;
            let (record, check) = lock(&self.store).save_target(
                &selected.internal_name,
                &selected.inventory.package_id,
                declaration,
                expected,
                configuration,
                reviewed_resolution,
            )?;
            Ok(TargetSaveResponse {
                view: target_view(&selected, record),
                check,
            })
        })();
        self.outcome(
            Some(workspace),
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
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            self.collect(&mut state);
            state.idle()?;
            self.invalidate_target_observation(Some(workspace));
            drop(state);
            let record = lock(&self.store).remove_target(
                &selected.internal_name,
                &selected.inventory.package_id,
                expected,
            )?;
            Ok(target_view(&selected, record))
        })();
        self.outcome(
            Some(workspace),
            "remove_target",
            Some(("target.removed", "Target binding removed")),
            result,
        )
    }
}

pub(super) fn target_context(selected: &Selected) -> TargetContext {
    TargetContext {
        workspace: selected.workspace.clone(),
        internal_name: selected.internal_name.clone(),
        package_id: selected.inventory.package_id.clone(),
        declaration_identity: selected.package.target_identity.clone(),
    }
}

fn target_view(selected: &Selected, record: TargetRecord) -> TargetView {
    let compatible = record.binding.as_ref().is_some_and(|binding| {
        selected
            .package
            .target
            .as_ref()
            .zip(selected.package.target_identity.as_deref())
            .is_some_and(|(declaration, identity)| {
                binding.compatible(&selected.inventory.package_id, &declaration.id, identity)
            })
    });
    TargetView {
        context: target_context(selected),
        record,
        compatible,
    }
}

fn target_undeclared() -> Fault {
    Fault::new("TargetUndeclared", "Package has no target declaration")
}

#[cfg(test)]
mod tests;
