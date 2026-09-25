use super::profiles::{
    check_webview_value, desktop_options, normalize_editor_numbers, validated_profiles,
};
use super::{
    Application, InspectionKind, InspectionOutcome, MAX_SESSION_COUNTER, ProfileCatalog,
    ProfileRecoveryOutcome, RecoverableProfile, RecoveryMutation, RecoveryRef, RecoveryStatus,
    RecoveryView, Selected, Workspace, WorkspaceRef, WorkspaceView, Workspaces, lock,
};
use crate::storage::{
    PackageReference, PackageSource, ProfileStore, RecoveryRecord, Store, portable_values,
};
use mado_runtime_comparison::model::Fault;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

struct RecoveryEntry {
    original: RecoveryRecord,
    issue: Fault,
}

#[derive(Clone)]
pub(super) struct RecoveryContext {
    reference: RecoveryRef,
    selected: Selected,
    source_reference: Option<PackageReference>,
    selected_package_id: Option<String>,
    relocation: bool,
    binding_required: bool,
    entries: Vec<Arc<RecoveryEntry>>,
    profiles_error: Option<Fault>,
    outcomes: Vec<ProfileRecoveryOutcome>,
}

impl RecoveryContext {
    fn check_source_reference(&self, store: &Store) -> Result<(), Fault> {
        let tab = store.tab(&self.selected.internal_name)?;
        if !tab.open {
            return Err(Fault::new(
                "TabClosed",
                "Reopen the Tab before recovering profiles",
            ));
        }
        let current = tab
            .packages
            .iter()
            .find(|reference| reference.package_id == self.selected.inventory.package_id);
        if current != self.source_reference.as_ref()
            || tab.selected_package_id != self.selected_package_id
        {
            return Err(Fault::new(
                "StaleIdentity",
                "Saved package ownership changed; inspect it again",
            ));
        }
        Ok(())
    }

    pub(super) fn view(&self) -> RecoveryView {
        RecoveryView {
            context: self.reference.clone(),
            relocation: self.relocation,
            binding_required: self.binding_required,
            package_path: self.selected.path.to_string_lossy().into_owned(),
            package: self.selected.package.clone(),
            profiles: self
                .entries
                .iter()
                .map(|entry| RecoverableProfile {
                    profile: entry.original.profile.clone(),
                    issue: entry.issue.clone(),
                })
                .collect(),
            profiles_error: self.profiles_error.clone(),
        }
    }
}

impl Workspaces {
    fn resolve_recovery(&self, reference: &RecoveryRef) -> Result<Arc<RecoveryContext>, Fault> {
        self.resolve_workspace(&reference.workspace)?
            .recovery
            .as_ref()
            .filter(|context| context.reference == *reference)
            .cloned()
            .ok_or_else(|| {
                Fault::new(
                    "StaleIdentity",
                    "Recovery context is closed, discarded, or superseded",
                )
            })
    }
}

fn next_revision(workspace: &WorkspaceRef) -> Result<WorkspaceRef, Fault> {
    let revision = workspace
        .revision
        .checked_add(1)
        .filter(|revision| *revision <= MAX_SESSION_COUNTER)
        .ok_or_else(|| Fault::new("WorkspaceLimit", "Workspace revision limit reached"))?;
    Ok(WorkspaceRef {
        workspace_id: workspace.workspace_id.clone(),
        revision,
    })
}

fn compatibility(store: &ProfileStore, selected: &Selected) -> Result<(), Fault> {
    let (_, incompatible) = validated_profiles(store, &selected.inventory)?;
    if let Some(cause) = incompatible {
        return Err(Fault::new(
            "PackageCompatibility",
            "Replacement source is incompatible with this Tab's saved profiles",
        )
        .with_context(json!({
            "internal_name":selected.internal_name,
            "package_id":selected.inventory.package_id,
            "cause":cause,
        })));
    }
    Ok(())
}

fn bind(store: &Store, selected: &Selected) -> Result<Option<PackageReference>, Fault> {
    Ok(store
        .bind_package(
            &selected.internal_name,
            &selected.inventory.package_id,
            &selected.path,
        )?
        .packages
        .into_iter()
        .find(|reference| reference.package_id == selected.inventory.package_id))
}

fn reconcile(store: &ProfileStore, context: &mut RecoveryContext) {
    let records = match store.recovery_records() {
        Ok(records) => records,
        Err(error) => {
            context.profiles_error = Some(error);
            return;
        }
    };
    for original in records {
        let profile = &original.profile;
        // Unsafe originals never gain reset authority or cross the WebView boundary.
        if let Err(error) = check_webview_value(&profile.values, "$") {
            context.outcomes.push(ProfileRecoveryOutcome {
                profile_id: profile.id.clone(),
                name: profile.name.clone(),
                status: RecoveryStatus::StorageFailed,
                issue: Some(error.clone()),
            });
            context.profiles_error = Some(error);
            continue;
        }
        let candidate = desktop_options(&context.selected.inventory, profile.values.clone())
            .and_then(|values| portable_values(&values).map(|()| values));
        if profile.schema_identity == context.selected.package.schema_identity && candidate.is_ok()
        {
            continue;
        }
        let (status, issue) = match candidate {
            Ok(values) => {
                match store.replace_recovery(&context.selected.inventory, &original, values) {
                    Ok(_) => (RecoveryStatus::Saved, None),
                    Err(error) => (RecoveryStatus::StorageFailed, Some(error)),
                }
            }
            Err(error) => (RecoveryStatus::RepairRequired, Some(error)),
        };
        context.outcomes.push(ProfileRecoveryOutcome {
            profile_id: profile.id.clone(),
            name: profile.name.clone(),
            status,
            issue: issue.clone(),
        });
        if let Some(issue) = issue {
            context
                .entries
                .push(Arc::new(RecoveryEntry { original, issue }));
        }
    }
}

impl Application {
    pub fn inspect(
        &self,
        path: &Path,
        workspace: &WorkspaceRef,
    ) -> Result<InspectionOutcome, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            state.resolve_workspace(workspace)?;
            self.collect(&mut state);
            state.idle()?;
            let previous = state.resolve_workspace(workspace)?.clone();
            let reference = next_revision(workspace)?;
            self.invalidate_target_observation(Some(workspace));
            drop(state);
            let selected = Self::inspect_selection(
                &self.runner,
                path,
                reference.clone(),
                &previous.internal_name,
                &previous.display_name,
            )?;
            let mut context = RecoveryContext {
                reference: RecoveryRef {
                    token: format!("recovery-{}-{}", reference.workspace_id, reference.revision),
                    workspace: reference,
                },
                selected,
                source_reference: None,
                selected_package_id: None,
                relocation: false,
                binding_required: true,
                entries: Vec::new(),
                profiles_error: None,
                outcomes: Vec::new(),
            };
            let binding = {
                let store = lock(&self.store);
                let tab = store.tab(&previous.internal_name)?;
                let existing = tab.packages.iter().find(|reference| {
                    reference.package_id == context.selected.inventory.package_id
                });
                context.source_reference = existing.cloned();
                context.selected_package_id = tab.selected_package_id;
                context.relocation = existing.is_some_and(|reference| !matches!(
                    &reference.source,
                    PackageSource::Directory { path } if Path::new(path) == context.selected.path.as_path()
                ));
                if existing.is_some() {
                    let profiles = store.profile_store(
                        &previous.internal_name,
                        &context.selected.inventory.package_id,
                    )?;
                    reconcile(&profiles, &mut context);
                    if context.relocation {
                        compatibility(&profiles, &context.selected)
                            .and_then(|()| bind(&store, &context.selected))
                    } else {
                        bind(&store, &context.selected)
                    }
                } else {
                    bind(&store, &context.selected)
                }
            };
            Ok(self.publish_inspection(workspace, previous, context, binding))
        })();
        self.reported(workspace, "inspect", result)
    }

    /// Inspection and binding retry succeed as commands even when the candidate was
    /// not bound; the record follows the binding result, not the transport.
    fn reported(
        &self,
        workspace: &WorkspaceRef,
        action: &str,
        result: Result<InspectionOutcome, Fault>,
    ) -> Result<InspectionOutcome, Fault> {
        let error = match &result {
            Ok(outcome) => outcome.binding_error.as_ref(),
            Err(error) => Some(error),
        };
        self.record(
            Some(workspace),
            action,
            Some(("workspace.reinspected", "Workspace package inspected")),
            error,
        );
        result
    }

    fn publish_inspection(
        &self,
        previous_ref: &WorkspaceRef,
        mut workspace: Workspace,
        mut context: RecoveryContext,
        binding: Result<Option<PackageReference>, Fault>,
    ) -> InspectionOutcome {
        let binding_error = binding.as_ref().err().cloned();
        let kind = match &binding {
            Ok(_) => InspectionKind::Bound,
            Err(error)
                if error.category == "PackageCompatibility" && !context.entries.is_empty() =>
            {
                InspectionKind::RecoveryRequired
            }
            Err(_) => InspectionKind::BindingFailed,
        };
        workspace.workspace = context.reference.workspace.clone();
        context.binding_required = binding.is_err();
        match binding {
            Ok(saved_package) => {
                context.source_reference = saved_package.clone();
                context.selected_package_id =
                    saved_package.as_ref().map(|saved| saved.package_id.clone());
                // The saved reference now names the candidate directory.
                context.relocation = false;
                workspace.saved_package = saved_package;
                workspace.selected = Some(context.selected.clone());
                workspace.source_error = None;
            }
            // A recovery-only candidate is not runnable and supersedes the prior selection
            // until it is bound or discarded.
            Err(_) if kind == InspectionKind::RecoveryRequired => workspace.selected = None,
            // A failed durable mutation grants the candidate nothing: the prior selection
            // stays runnable under the new revision, and the saved reference keeps its
            // own source fault; `binding_error` alone names this attempt's failure.
            Err(_) => {
                if let Some(selected) = &mut workspace.selected {
                    selected.workspace = workspace.workspace.clone();
                }
            }
        }
        let outcomes = context.outcomes.clone();
        workspace.recovery = (binding_error.is_some()
            || !context.entries.is_empty()
            || context.profiles_error.is_some())
        .then(|| Arc::new(context));
        let view = self.workspace_view(&workspace);
        let mut state = lock(&self.workspaces);
        let current = state
            .open
            .iter_mut()
            .find(|value| value.workspace == *previous_ref)
            .expect("command admission retains the initiating workspace");
        *current = workspace;
        InspectionOutcome {
            kind,
            workspace: view,
            outcomes,
            binding_error,
        }
    }

    fn check_recovery_inventory(&self, context: &RecoveryContext) -> Result<(), Fault> {
        let current = self.runner.inspect(&context.selected.path)?;
        if current.inventory_identity != context.selected.inventory.identity {
            return Err(Fault::new(
                "InventoryChanged",
                "Recovery package changed; inspect it again before continuing",
            ));
        }
        Ok(())
    }

    pub fn repair_profile(
        &self,
        context: &RecoveryRef,
        id: &str,
        values: Value,
    ) -> Result<RecoveryMutation, Fault> {
        self.mutate_recovery(context, id, Some(values))
    }

    pub fn reset_profile(
        &self,
        context: &RecoveryRef,
        id: &str,
        confirm: bool,
    ) -> Result<RecoveryMutation, Fault> {
        if !confirm {
            return Err(Fault::new(
                "ConfirmationRequired",
                "Confirm replacement of this profile's saved values",
            ));
        }
        self.mutate_recovery(context, id, None)
    }

    fn mutate_recovery(
        &self,
        reference: &RecoveryRef,
        id: &str,
        values: Option<Value>,
    ) -> Result<RecoveryMutation, Fault> {
        let reset = values.is_none();
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let context = state.resolve_recovery(reference)?;
            self.collect(&mut state);
            state.idle()?;
            drop(state);
            self.check_recovery_inventory(&context)?;
            let store = lock(&self.store);
            context.check_source_reference(&store)?;
            let entry = context
                .entries
                .iter()
                .find(|entry| entry.original.profile.id == id)
                .ok_or_else(|| {
                    Fault::new(
                        "ProfileNotFound",
                        "Profile does not belong to this recovery context",
                    )
                })?;
            let mut values =
                values.unwrap_or_else(|| top_level_defaults(&context.selected.inventory.schema));
            if !reset {
                normalize_editor_numbers(&context.selected.inventory.schema, &mut values);
            }
            let mut response = RecoveryMutation {
                saved: None,
                draft: None,
                issue: None,
                recovery: None,
                catalog: None,
                refresh_error: None,
            };
            let candidate = match desktop_options(&context.selected.inventory, values) {
                Ok(candidate) => candidate,
                Err(error) => {
                    response.issue = Some(error);
                    response.draft =
                        reset.then(|| top_level_defaults(&context.selected.inventory.schema));
                    response.recovery = Some(context.view());
                    return Ok(response);
                }
            };
            let profiles = store.profile_store(
                &context.selected.internal_name,
                &context.selected.inventory.package_id,
            )?;
            match profiles.replace_recovery(&context.selected.inventory, &entry.original, candidate)
            {
                Ok(saved) => response.saved = Some(saved),
                Err(error) => {
                    // The store marks candidate validation at its existing boundary;
                    // aggregate limits, conflicts, and I/O cannot be fixed by a default draft.
                    if reset && error.context["stage"] == "profile_validation" {
                        response.draft =
                            Some(top_level_defaults(&context.selected.inventory.schema));
                    }
                    response.issue = Some(error);
                    response.recovery = Some(context.view());
                    return Ok(response);
                }
            }
            // Keep the committed fact even if the independent catalog read fails.
            match validated_profiles(&profiles, &context.selected.inventory) {
                Ok((profiles, profiles_error)) => {
                    response.catalog = Some(ProfileCatalog {
                        profiles,
                        profiles_error,
                    })
                }
                Err(error) => response.refresh_error = Some(error),
            }
            drop(store);
            let mut updated = (*context).clone();
            updated
                .entries
                .retain(|entry| entry.original.profile.id != id);
            for outcome in &mut updated.outcomes {
                if outcome.profile_id == id {
                    outcome.status = RecoveryStatus::Saved;
                    outcome.issue = None;
                }
            }
            let mut state = lock(&self.workspaces);
            let workspace = state
                .open
                .iter_mut()
                .find(|workspace| workspace.workspace == reference.workspace)
                .expect("command admission retains the recovery workspace");
            // Keep the bounded context until explicit close/reinspect so late replies cannot erase newer drafts.
            response.recovery = Some(updated.view());
            workspace.recovery = Some(Arc::new(updated));
            Ok(response)
        })();
        let action = if reset {
            "reset_profile"
        } else {
            "repair_profile"
        };
        // A refused replacement returns a typed outcome; record what was saved or refused.
        let error = match &result {
            Ok(mutation) => mutation.issue.as_ref(),
            Err(error) => Some(error),
        };
        self.record(
            Some(&reference.workspace),
            action,
            Some(("profile.saved", "Profile saved")),
            error,
        );
        result
    }

    pub fn retry_binding(&self, reference: &RecoveryRef) -> Result<InspectionOutcome, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let context = state.resolve_recovery(reference)?;
            self.collect(&mut state);
            state.idle()?;
            let previous = state.resolve_workspace(&reference.workspace)?.clone();
            drop(state);
            self.check_recovery_inventory(&context)?;
            let revision = next_revision(&reference.workspace)?;
            let binding = {
                let store = lock(&self.store);
                (|| {
                    context.check_source_reference(&store)?;
                    // Only a moved source must pass the strict replacement checks; an
                    // in-place binding never requires unrelated profiles to be repaired.
                    if context.relocation {
                        let profiles = store.profile_store(
                            &context.selected.internal_name,
                            &context.selected.inventory.package_id,
                        )?;
                        compatibility(&profiles, &context.selected)?;
                    }
                    bind(&store, &context.selected)
                })()
            };
            // Failed retries retain the same repair authority and expected originals.
            if let Err(error) = binding {
                return Ok(InspectionOutcome {
                    kind: if error.category == "PackageCompatibility" && !context.entries.is_empty()
                    {
                        InspectionKind::RecoveryRequired
                    } else {
                        InspectionKind::BindingFailed
                    },
                    workspace: self.workspace_view(&previous),
                    outcomes: context.outcomes.clone(),
                    binding_error: Some(error),
                });
            }
            // Rejected profiles keep their repair authority under the bound selection.
            let mut context = (*context).clone();
            context.reference.workspace = revision;
            context.selected.workspace = context.reference.workspace.clone();
            Ok(self.publish_inspection(&reference.workspace, previous, context, binding))
        })();
        self.reported(&reference.workspace, "retry_binding", result)
    }

    pub fn discard_recovery(&self, reference: &RecoveryRef) -> Result<WorkspaceView, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            state.resolve_recovery(reference)?;
            self.collect(&mut state);
            state.idle()?;
            let mut workspace = state.resolve_workspace(&reference.workspace)?.clone();
            if workspace.selected.is_none() {
                workspace.workspace = next_revision(&workspace.workspace)?;
            }
            workspace.recovery = None;
            drop(state);
            let view = self.workspace_view(&workspace);
            let mut state = lock(&self.workspaces);
            let current = state
                .open
                .iter_mut()
                .find(|value| value.workspace == reference.workspace)
                .expect("command admission retains the recovery workspace");
            *current = workspace;
            Ok(view)
        })();
        self.outcome(Some(&reference.workspace), "discard_recovery", None, result)
    }
}

fn top_level_defaults(schema: &Value) -> Value {
    Value::Object(
        schema["properties"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, node)| {
                node.get("default")
                    .map(|value| (name.clone(), value.clone()))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
