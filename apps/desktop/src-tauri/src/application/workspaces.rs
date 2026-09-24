use super::profiles::{check_webview_value, validated_profiles};
use super::{
    Application, MAX_SESSION_COUNTER, Selected, Selection, Workspace, WorkspaceCatalog,
    WorkspaceRef, WorkspaceView, Workspaces, lock,
};
use crate::storage::{MAX_OPEN_TABS, PackageSource, TabRecord};
use mado_runtime_comparison::desktop::DesktopController;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, Plan};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

impl Workspaces {
    fn resolve_workspace(&self, workspace: &WorkspaceRef) -> Result<&Workspace, Fault> {
        self.open
            .iter()
            .find(|selected| selected.workspace == *workspace)
            .ok_or_else(|| {
                Fault::new(
                    "StaleIdentity",
                    "Workspace is closed, unknown, or reinspected",
                )
            })
    }

    pub(super) fn resolve(&self, workspace: &WorkspaceRef) -> Result<&Selected, Fault> {
        self.resolve_workspace(workspace)?
            .selected
            .as_ref()
            .ok_or_else(|| {
                Fault::new(
                    "WorkspaceUnbound",
                    "Workspace has no available inspected package",
                )
            })
    }

    pub(super) fn next_workspace(&self) -> Result<WorkspaceRef, Fault> {
        if self.open.len() >= MAX_OPEN_TABS || self.next_id > MAX_SESSION_COUNTER {
            return Err(Fault::new(
                "WorkspaceLimit",
                "Workspace session or open limit reached",
            ));
        }
        Ok(WorkspaceRef {
            workspace_id: format!("workspace-{}-{}", self.session, self.next_id),
            revision: 0,
        })
    }
}

impl Application {
    pub fn create_workspace(
        &self,
        internal_name: &str,
        display_name: &str,
    ) -> Result<WorkspaceView, Fault> {
        let result: Result<WorkspaceView, Fault> = (|| {
            let (_command, state) = self.command_state()?;
            let reference = state.next_workspace()?;
            drop(state);
            let tab = lock(&self.store).create_tab(internal_name, display_name)?;
            let workspace = Self::restore_workspace(&self.runner, tab, reference);
            let view = self.workspace_view(&workspace);
            let mut state = lock(&self.workspaces);
            state.next_id += 1;
            state.open.push(workspace);
            Ok(view)
        })();
        // The session exists only on success; a refusal has no workspace to attribute.
        let opened = result.as_ref().ok().map(|view| WorkspaceRef {
            workspace_id: view.workspace_id.clone(),
            revision: view.revision,
        });
        self.outcome(
            opened.as_ref(),
            "create_workspace",
            Some(("workspace.opened", "Workspace opened")),
            result,
        )
    }

    pub fn reopen_workspace(&self, internal_name: &str) -> Result<WorkspaceView, Fault> {
        let result: Result<WorkspaceView, Fault> = (|| {
            let (_command, mut state) = self.command_state()?;
            if state
                .open
                .iter()
                .any(|tab| tab.internal_name == internal_name)
            {
                return Err(Fault::new("WorkspaceConflict", "Workspace is already open"));
            }
            self.collect(&mut state);
            state.idle()?;
            let reference = state.next_workspace()?;
            drop(state);
            let tab = lock(&self.store).tab(internal_name)?;
            if tab.open {
                return Err(Fault::new(
                    "WorkspaceConflict",
                    "Saved open state changed; reload configuration",
                ));
            }
            let workspace = Self::restore_workspace(&self.runner, tab, reference);
            lock(&self.store).set_tab_open(internal_name, true)?;
            let view = self.workspace_view(&workspace);
            let mut state = lock(&self.workspaces);
            state.next_id += 1;
            state.open.push(workspace);
            Ok(view)
        })();
        let opened = result.as_ref().ok().map(|view| WorkspaceRef {
            workspace_id: view.workspace_id.clone(),
            revision: view.revision,
        });
        self.outcome(
            opened.as_ref(),
            "reopen_workspace",
            Some(("workspace.opened", "Workspace opened")),
            result,
        )
    }

    pub fn workspace_catalog(&self) -> Result<WorkspaceCatalog, Fault> {
        let (_command, state) = self.command_state()?;
        let open = state.open.clone();
        drop(state);
        let listing = lock(&self.store).tabs()?;
        Ok(WorkspaceCatalog {
            open: open
                .iter()
                .map(|workspace| self.workspace_view(workspace))
                .collect(),
            closed: listing.tabs.into_iter().filter(|tab| !tab.open).collect(),
            faults: listing.faults,
        })
    }

    pub(super) fn restore_workspace(
        runner: &DesktopController,
        tab: TabRecord,
        mut workspace: WorkspaceRef,
    ) -> Workspace {
        let mut source_error = None;
        let mut selected = None;
        // The durable reference is projected as stored; inspection alone grants authority.
        let saved_package = tab.selected_package_id.as_ref().and_then(|id| {
            tab.packages
                .iter()
                .find(|package| &package.package_id == id)
                .cloned()
        });
        if let Some(package_id) = &tab.selected_package_id {
            let result = tab
                .packages
                .iter()
                .find(|package| &package.package_id == package_id)
                .ok_or_else(|| {
                    Fault::new(
                        "PackageSource",
                        "Saved selected package reference is missing",
                    )
                })
                .and_then(|reference| match &reference.source {
                    PackageSource::Directory { path } => {
                        let mut reference = workspace.clone();
                        reference.revision = 1;
                        let selected = Self::inspect_selection(
                            runner,
                            Path::new(path),
                            reference,
                            &tab.internal_name,
                            &tab.display_name,
                        )?;
                        if selected.inventory.package_id != *package_id {
                            return Err(Fault::new(
                                "PackageIdentity",
                                "Saved source now identifies another package",
                            ));
                        }
                        Ok(selected)
                    }
                    PackageSource::CustomArchive { .. } => Err(Fault::new(
                        "UnsupportedPackageSource",
                        "Custom package archives are not supported",
                    )),
                });
            match result {
                Ok(value) => {
                    workspace = value.workspace.clone();
                    selected = Some(value);
                }
                Err(mut error) => {
                    let cause = std::mem::take(&mut error.context);
                    source_error = Some(error.with_context(json!({
                        "internal_name":tab.internal_name,"package_id":package_id,"cause":cause,
                    })));
                }
            }
        }
        Workspace {
            workspace,
            internal_name: tab.internal_name,
            display_name: tab.display_name,
            saved_package,
            selected,
            source_error,
            terminal: None,
        }
    }

    fn inspect_selection(
        runner: &DesktopController,
        path: &Path,
        workspace: WorkspaceRef,
        internal_name: &str,
        display_name: &str,
    ) -> Result<Selected, Fault> {
        let path = path
            .canonicalize()
            .map_err(|_| Fault::new("Package", "Package location cannot be resolved"))?;
        let package = runner.inspect(&path)?;
        check_webview_value(&package.schema, "$.schema")?;
        for (name, preset) in &package.profiles {
            check_webview_value(preset, &format!("$.presets[{name:?}]"))?;
        }
        if let Some(defaults) = &package.effective_defaults {
            check_webview_value(defaults, "$.effective_defaults")?;
        }
        let plan: Plan = serde_json::from_str(include_str!(
            "../../../../../tools/runtime-comparison/fixtures/manual-plan.json"
        ))
        .map_err(|error| Fault::new("Application", error.to_string()))?;
        let inventory = Inventory::capture(&path, &plan.limits)?;
        if inventory.identity != package.inventory_identity {
            return Err(Fault::new(
                "InventoryChanged",
                "Package changed during inspection; inspect it again",
            ));
        }
        Ok(Selected {
            workspace,
            internal_name: internal_name.to_owned(),
            display_name: display_name.to_owned(),
            path,
            inventory: Arc::new(inventory),
            package: Arc::new(package),
        })
    }

    pub fn inspect(&self, path: &Path, workspace: &WorkspaceRef) -> Result<Selection, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let previous = state.resolve_workspace(workspace)?.clone();
            self.collect(&mut state);
            state.idle()?;
            let revision = workspace
                .revision
                .checked_add(1)
                .filter(|revision| *revision <= MAX_SESSION_COUNTER)
                .ok_or_else(|| Fault::new("WorkspaceLimit", "Workspace revision limit reached"))?;
            drop(state);
            let selected = Self::inspect_selection(
                &self.runner,
                path,
                WorkspaceRef {
                    workspace_id: workspace.workspace_id.clone(),
                    revision,
                },
                &previous.internal_name,
                &previous.display_name,
            )?;
            let saved_package = {
                let store = lock(&self.store);
                let tab = store.tab(&selected.internal_name)?;
                let replacing_source = tab.packages.iter()
                    .find(|reference| reference.package_id == selected.inventory.package_id)
                    .is_some_and(|reference| !matches!(
                        &reference.source,
                        PackageSource::Directory { path } if Path::new(path) == selected.path.as_path()
                    ));
                if replacing_source {
                    let profiles = store
                        .profile_store(&selected.internal_name, &selected.inventory.package_id)?;
                    let (_, incompatible) = validated_profiles(&profiles, &selected.inventory)?;
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
                }
                let bound = store.bind_package(
                    &selected.internal_name,
                    &selected.inventory.package_id,
                    &selected.path,
                )?;
                bound
                    .packages
                    .into_iter()
                    .find(|reference| reference.package_id == selected.inventory.package_id)
            };
            let selection = self.selection(&selected);
            let mut state = lock(&self.workspaces);
            let current = state
                .open
                .iter_mut()
                .find(|value| value.workspace == *workspace)
                .expect("command admission retains the initiating workspace");
            current.workspace = selected.workspace.clone();
            current.saved_package = saved_package;
            current.selected = Some(selected);
            current.source_error = None;
            // A previous revision's outcome remains attributable, not applicable.
            Ok(selection)
        })();
        self.outcome(
            Some(workspace),
            "inspect",
            Some(("workspace.reinspected", "Workspace package inspected")),
            result,
        )
    }

    fn selection(&self, selected: &Selected) -> Selection {
        let store = lock(&self.store);
        let listing = store
            .profile_store(&selected.internal_name, &selected.inventory.package_id)
            .and_then(|store| validated_profiles(&store, &selected.inventory));
        let (profiles, profiles_error) = match listing {
            Ok(listing) => listing,
            Err(error) => (Vec::new(), Some(error)),
        };
        Selection {
            workspace_id: selected.workspace.workspace_id.clone(),
            revision: selected.workspace.revision,
            internal_name: selected.internal_name.clone(),
            display_name: selected.display_name.clone(),
            package_path: selected.path.to_string_lossy().into_owned(),
            package: selected.package.clone(),
            profiles,
            profiles_error,
        }
    }

    fn workspace_view(&self, workspace: &Workspace) -> WorkspaceView {
        WorkspaceView {
            workspace_id: workspace.workspace.workspace_id.clone(),
            revision: workspace.workspace.revision,
            internal_name: workspace.internal_name.clone(),
            display_name: workspace.display_name.clone(),
            saved_package: workspace.saved_package.clone(),
            selection: workspace
                .selected
                .as_ref()
                .map(|selected| self.selection(selected)),
            source_error: workspace.source_error.clone(),
        }
    }

    pub fn close_workspace(&self, workspace: &WorkspaceRef) -> Result<(), Fault> {
        let result =
            (|| {
                let (_command, mut state) = self.command_state()?;
                let internal_name = state.resolve_workspace(workspace)?.internal_name.clone();
                self.collect(&mut state);
                if state.owner.as_ref().is_some_and(|owner| {
                    !owner.terminal && owner.workspace.as_ref() == Some(workspace)
                }) {
                    return Err(Fault::new(
                        "RunActive",
                        "Workspace owns an unsettled operation",
                    ));
                }
                drop(state);
                lock(&self.store).set_tab_open(&internal_name, false)?;
                lock(&self.workspaces)
                    .open
                    .retain(|selected| selected.workspace != *workspace);
                Ok(())
            })();
        self.outcome(
            Some(workspace),
            "close_workspace",
            Some(("workspace.closed", "Workspace closed")),
            result,
        )
    }
}

#[cfg(test)]
mod tests;
