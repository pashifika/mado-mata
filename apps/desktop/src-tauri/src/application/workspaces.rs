use super::profiles::{check_webview_value, validated_profiles};
use super::{
    Application, InstalledSelection, MAX_SESSION_COUNTER, Selected, Selection, Workspace,
    WorkspaceCatalog, WorkspaceRef, WorkspaceView, Workspaces, lock,
};
use crate::authoring::Publisher;
use crate::distribution::{self, InstalledIdentity, InstalledRelease, VerifiedInstalled};
use crate::storage::{MAX_OPEN_TABS, PackageSource, TabRecord};
use mado_runtime_comparison::desktop::{DesktopController, PackageInfo};
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, Plan};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

impl Workspaces {
    pub(super) fn resolve_workspace(&self, workspace: &WorkspaceRef) -> Result<&Workspace, Fault> {
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
            let workspace = Self::restore_workspace(&self.runner, &self.publisher, tab, reference);
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
            let workspace = Self::restore_workspace(&self.runner, &self.publisher, tab, reference);
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
        publisher: &Publisher,
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
                .and_then(|reference| {
                    let mut session = workspace.clone();
                    session.revision = 1;
                    let selected = match &reference.source {
                        PackageSource::Directory { path } => Self::inspect_selection(
                            runner,
                            publisher,
                            Path::new(path),
                            session,
                            &tab.internal_name,
                            &tab.display_name,
                        )?,
                        // Restart reverifies the retained archive; no earlier result is reused.
                        PackageSource::Installed {
                            package_id: installed,
                            artifact_id,
                        } => Self::inspect_installed_selection(
                            runner,
                            publisher,
                            &InstalledIdentity {
                                package_id: installed.clone(),
                                artifact_id: artifact_id.clone(),
                            },
                            session,
                            &tab.internal_name,
                            &tab.display_name,
                        )?,
                        PackageSource::CustomArchive { .. } => {
                            return Err(Fault::new(
                                "UnsupportedPackageSource",
                                "Custom package archives are not supported",
                            ));
                        }
                    };
                    if selected.inventory.package_id != *package_id {
                        return Err(Fault::new(
                            "PackageIdentity",
                            "Saved source now identifies another package",
                        ));
                    }
                    Ok(selected)
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
            recovery: None,
        }
    }

    /// A path naming an installed release (its archive, or a former expanded container)
    /// keeps its installed identity and verification; it is never admitted as a directory.
    pub(super) fn inspect_selection(
        runner: &DesktopController,
        publisher: &Publisher,
        path: &Path,
        workspace: WorkspaceRef,
        internal_name: &str,
        display_name: &str,
    ) -> Result<Selected, Fault> {
        publisher.check_admission(path)?;
        if let Some(identity) = distribution::managed_identity(publisher.data_root(), path)? {
            return Self::inspect_installed_selection(
                runner,
                publisher,
                &identity,
                workspace,
                internal_name,
                display_name,
            );
        }
        let path = path
            .canonicalize()
            .map_err(|_| Fault::new("Package", "Package location cannot be resolved"))?;
        let package = runner.inspect(&path)?;
        check_package_values(&package)?;
        let mut plan: Plan = serde_json::from_str(include_str!(
            "../../../../../tools/runtime-comparison/fixtures/manual-plan.json"
        ))
        .map_err(|error| Fault::new("Application", error.to_string()))?;
        plan.limits.snapshot_bytes = mado_runtime_comparison::images::PACKAGE_BYTES;
        let inventory = Inventory::capture(&path, &plan.limits)?;
        publisher.check_admission(&path)?;
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
            installed: None,
            inventory: Arc::new(inventory),
            package: Arc::new(package),
        })
    }

    /// Verify the installed archive, then statically inspect that same capture. The
    /// selection's inventory is exactly the verified bytes; nothing is extracted.
    pub(super) fn inspect_installed_selection(
        runner: &DesktopController,
        publisher: &Publisher,
        identity: &InstalledIdentity,
        workspace: WorkspaceRef,
        internal_name: &str,
        display_name: &str,
    ) -> Result<Selected, Fault> {
        let (verified, inventory, package) = Self::verify_installed(runner, publisher, identity)?;
        check_package_values(&package)?;
        Ok(Selected {
            workspace,
            internal_name: internal_name.to_owned(),
            display_name: display_name.to_owned(),
            path: verified.archive_path().to_path_buf(),
            installed: Some(InstalledSelection {
                identity: identity.clone(),
                key_id: verified.facts().key_id.clone(),
            }),
            inventory: Arc::new(inventory),
            package: Arc::new(package),
        })
    }

    fn verify_installed(
        runner: &DesktopController,
        publisher: &Publisher,
        identity: &InstalledIdentity,
    ) -> Result<(VerifiedInstalled, Inventory, PackageInfo), Fault> {
        let verified = distribution::verify_installed(
            publisher.data_root(),
            &identity.package_id,
            &identity.artifact_id,
        )?;
        let inventory = verified.draft().validate()?;
        if inventory.package_id != identity.package_id {
            return Err(Fault::new(
                "InstalledIntegrity",
                "Installed payload names another package than its release identity",
            )
            .with_context(json!({
                "package_id":identity.package_id,"artifact_id":identity.artifact_id,
            })));
        }
        let package = runner.inspect_inventory(inventory.clone(), None)?;
        publisher.check_admission(verified.archive_path())?;
        Ok((verified, inventory, package))
    }

    /// Capture a selection's current package for a later mutation check. Installed
    /// releases are reverified in full rather than reread as a directory.
    pub(super) fn reinspect(&self, selected: &Selected) -> Result<PackageInfo, Fault> {
        self.publisher.check_admission(&selected.path)?;
        match &selected.installed {
            Some(installed) => {
                Ok(Self::verify_installed(&self.runner, &self.publisher, &installed.identity)?.2)
            }
            None => self.runner.inspect(&selected.path),
        }
    }

    /// Installed releases for explicit selection; each archive is reverified per call.
    pub fn package_installed_list(&self) -> Result<Vec<InstalledRelease>, Fault> {
        self.package_installed_list_with(distribution::installed_releases)
    }

    fn package_installed_list_with(
        &self,
        read: impl FnOnce(&Path) -> Result<Vec<InstalledRelease>, Fault>,
    ) -> Result<Vec<InstalledRelease>, Fault> {
        let result = (|| {
            {
                let (_command, _state) = self.command_state()?;
            }
            // A catalog observation grants no authority; verification must not hold Stop's lock.
            read(&self.root)
        })();
        self.outcome(None, "package_installed_list", None, result)
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
            source: match &selected.installed {
                Some(installed) => PackageSource::Installed {
                    package_id: installed.identity.package_id.clone(),
                    artifact_id: installed.identity.artifact_id.clone(),
                },
                None => PackageSource::Directory {
                    path: selected.path.to_string_lossy().into_owned(),
                },
            },
            package: selected.package.clone(),
            profiles,
            profiles_error,
        }
    }

    pub(super) fn workspace_view(&self, workspace: &Workspace) -> WorkspaceView {
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
            recovery: workspace.recovery.as_ref().map(|context| context.view()),
        }
    }

    pub fn close_workspace(&self, workspace: &WorkspaceRef) -> Result<(), Fault> {
        let result =
            (|| {
                let (_command, mut state) = self.command_state()?;
                let internal_name = state.resolve_workspace(workspace)?.internal_name.clone();
                self.collect(&mut state);
                if state
                    .authoring
                    .as_ref()
                    .is_some_and(|lease| lease.owner.workspace == *workspace)
                {
                    return Err(Fault::new(
                        "AuthoringActive",
                        "Exit the authoring session before closing its workspace",
                    ));
                }
                if state.owner.as_ref().is_some_and(|owner| {
                    !owner.terminal && owner.workspace.as_ref() == Some(workspace)
                }) {
                    return Err(Fault::new(
                        "RunActive",
                        "Workspace owns an unsettled operation",
                    ));
                }
                self.invalidate_target_observation(Some(workspace));
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

pub(super) fn check_package_values(package: &PackageInfo) -> Result<(), Fault> {
    check_webview_value(&package.schema, "$.schema")?;
    for (name, preset) in &package.profiles {
        check_webview_value(preset, &format!("$.presets[{name:?}]"))?;
    }
    if let Some(defaults) = &package.effective_defaults {
        check_webview_value(defaults, "$.effective_defaults")?;
    }
    Ok(())
}

#[cfg(test)]
mod installed_tests;
#[cfg(test)]
mod tests;
