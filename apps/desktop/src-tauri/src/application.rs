use crate::configuration::{self, Capture};
use crate::logging::{LogBatch, LogStatus, Logger};
use crate::storage::{
    EditableSettings, LegacyImport, PackageSource, Profile, ProfileStore, Settings, Store,
    TabRecord,
};
use mado_runtime_comparison::desktop::{DesktopController, PackageInfo, StartRequest};
use mado_runtime_comparison::environment::OcrEnvironment;
use mado_runtime_comparison::host::resolve_options;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, Plan};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock, TryLockError,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_WORKSPACES: usize = 8;
const MAX_SESSION_COUNTER: u64 = 9_007_199_254_740_991;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRef {
    pub workspace_id: String,
    pub revision: u64,
}

#[derive(Debug, Serialize)]
pub struct Selection {
    pub workspace_id: String,
    pub revision: u64,
    pub internal_name: String,
    pub display_name: String,
    pub package_path: String,
    #[serde(serialize_with = "serialize_shared")]
    pub package: Arc<PackageInfo>,
    pub profiles: Vec<Profile>,
    pub profiles_error: Option<Fault>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceView {
    pub workspace_id: String,
    pub revision: u64,
    pub internal_name: String,
    pub display_name: String,
    pub selection: Option<Selection>,
    pub source_error: Option<Fault>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceCatalog {
    pub open: Vec<WorkspaceView>,
    pub closed: Vec<TabRecord>,
    pub faults: Vec<Fault>,
}

#[derive(Debug, Serialize)]
pub struct ProfileCatalog {
    pub profiles: Vec<Profile>,
    pub profiles_error: Option<Fault>,
}

#[derive(Clone, Serialize)]
pub struct WorkspaceResult {
    pub workspace: WorkspaceRef,
    #[serde(serialize_with = "serialize_shared")]
    pub controller: Arc<Value>,
}

#[derive(Serialize)]
pub struct RetainedCheck {
    pub workspace: Option<WorkspaceRef>,
    pub environment: Option<OcrEnvironment>,
    pub descriptor_path: Option<String>,
    pub package_inventory_identity: Option<String>,
    #[serde(serialize_with = "serialize_shared")]
    pub controller: Arc<Value>,
}

#[derive(Serialize)]
pub struct Poll {
    #[serde(serialize_with = "serialize_shared")]
    pub controller: Arc<Value>,
    pub logs: LogBatch,
    pub workspace_results: Vec<WorkspaceResult>,
    #[serde(serialize_with = "serialize_optional_shared")]
    pub last_check: Option<Arc<RetainedCheck>>,
}

fn serialize_shared<T: Serialize, S: Serializer>(
    value: &Arc<T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.as_ref().serialize(serializer)
}

fn serialize_optional_shared<T: Serialize, S: Serializer>(
    value: &Option<Arc<T>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.as_deref().serialize(serializer)
}

#[derive(Clone)]
struct Selected {
    workspace: WorkspaceRef,
    internal_name: String,
    display_name: String,
    path: PathBuf,
    inventory: Arc<Inventory>,
    package: Arc<PackageInfo>,
}

#[derive(Clone)]
struct Workspace {
    workspace: WorkspaceRef,
    internal_name: String,
    display_name: String,
    selected: Option<Selected>,
    source_error: Option<Fault>,
    terminal: Option<WorkspaceResult>,
}

struct CheckInputs {
    environment: Arc<OnceLock<Option<OcrEnvironment>>>,
    descriptor_path: Option<String>,
    package_inventory_identity: Option<String>,
}

struct OperationOwner {
    run: String,
    workspace: Option<WorkspaceRef>,
    check: Option<CheckInputs>,
    terminal: bool,
}

struct Workspaces {
    open: Vec<Workspace>,
    session: String,
    next_id: u64,
    owner: Option<OperationOwner>,
    controller: Arc<Value>,
    last_check: Option<Arc<RetainedCheck>>,
}

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

    fn resolve(&self, workspace: &WorkspaceRef) -> Result<&Selected, Fault> {
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

    fn next_workspace(&self) -> Result<WorkspaceRef, Fault> {
        if self.open.len() >= MAX_WORKSPACES || self.next_id > MAX_SESSION_COUNTER {
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

    fn idle(&self) -> Result<(), Fault> {
        if self.owner.as_ref().is_some_and(|owner| !owner.terminal) {
            Err(Fault::new(
                "RunActive",
                "Previous operation has not settled and been reaped",
            ))
        } else {
            Ok(())
        }
    }
}

pub struct Application {
    runner: DesktopController,
    store: Arc<Mutex<Store>>,
    commands: Mutex<()>,
    #[cfg(test)]
    command_admitted: AtomicBool,
    // Admission/collection share this lock; workers never acquire it.
    workspaces: Mutex<Workspaces>,
    logger: Logger,
    closing: AtomicBool,
    bridge: Mutex<Option<JoinHandle<()>>>,
    shutdown_outcome: OnceLock<Result<(), Fault>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Application {
    pub fn new(root: PathBuf, controlled: PathBuf, engine: PathBuf) -> Result<Arc<Self>, Fault> {
        #[cfg(not(test))]
        {
            Self::build(root, controlled, engine, Logger::new)
        }
        #[cfg(test)]
        {
            Self::build(root, controlled, engine, Logger::new, None)
        }
    }

    fn build(
        root: PathBuf,
        controlled: PathBuf,
        engine: PathBuf,
        make_logger: impl FnOnce(PathBuf) -> Result<Logger, Fault>,
        #[cfg(test)] bridge_exit: Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>,
    ) -> Result<Arc<Self>, Fault> {
        let store = Store::new(root.clone())?;
        store.settings()?;
        let runner = DesktopController::new(controlled, engine);
        let mut workspaces = Workspaces {
            open: Vec::new(),
            session: format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| Fault::new("Application", error.to_string()))?
                    .as_nanos(),
                NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            ),
            next_id: 1,
            owner: None,
            controller: Arc::new(json!({
                "state":"idle","run":null,"operation":"run","result":null,
                "error":null,"progress":[],"logs":[],"dropped_logs":0,
                "workspace_id":null,"workspace_revision":null
            })),
            last_check: None,
        };
        for tab in store.tabs()?.tabs.into_iter().filter(|tab| tab.open) {
            let workspace = workspaces.next_workspace()?;
            workspaces
                .open
                .push(Self::restore_workspace(&runner, tab, workspace));
            workspaces.next_id += 1;
        }
        // Start the bridge first, but give it no Application until all workers
        // exist. A logger-start failure can join this waiting bridge immediately,
        // without leaving a detached file writer for Retry to duplicate.
        let (publish, ready) = mpsc::sync_channel::<std::sync::Weak<Self>>(1);
        let bridge = std::thread::Builder::new()
            .name("desktop-log-bridge".into())
            .spawn(move || {
                if let Ok(weak) = ready.recv() {
                    loop {
                        let Some(application) = weak.upgrade() else {
                            break;
                        };
                        if application.closing.load(Ordering::Acquire) {
                            break;
                        }
                        application.collect(&mut lock(&application.workspaces));
                        drop(application);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                #[cfg(test)]
                if let Some((exiting, released)) = bridge_exit {
                    let _ = exiting.send(());
                    let _ = released.recv();
                }
            })
            .map_err(|error| Fault::new("Application", error.to_string()))?;
        let logger = match make_logger(root.join("logs")) {
            Ok(logger) => logger,
            Err(error) => {
                drop(publish);
                let _ = bridge.join();
                return Err(error);
            }
        };
        let application = Arc::new(Self {
            runner,
            store: Arc::new(Mutex::new(store)),
            commands: Mutex::new(()),
            #[cfg(test)]
            command_admitted: AtomicBool::new(false),
            workspaces: Mutex::new(workspaces),
            logger,
            closing: AtomicBool::new(false),
            bridge: Mutex::new(Some(bridge)),
            shutdown_outcome: OnceLock::new(),
        });
        if publish.send(Arc::downgrade(&application)).is_err() {
            let cleanup = application.shutdown();
            return Err(
                Fault::new("Application", "Log bridge stopped before publication").with_context(
                    json!({"cleanup":cleanup.err(),"logging":application.logger.status()}),
                ),
            );
        }
        application.logger.with_dispatch(|| {
            tracing::info!(
                code = "application.ready",
                lane = "controlled",
                "Application ready"
            )
        });
        Ok(application)
    }

    fn command_state(
        &self,
    ) -> Result<
        (
            std::sync::MutexGuard<'_, ()>,
            std::sync::MutexGuard<'_, Workspaces>,
        ),
        Fault,
    > {
        if self.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        let command = match self.commands.try_lock() {
            Ok(command) => command,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(Fault::new(
                    "WorkspaceBusy",
                    "Another workspace command is still in progress",
                ));
            }
        };
        // Recheck under admission: reconstruction may have closed the application
        // between the optimistic check above and acquiring this mutex.
        if self.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        #[cfg(test)]
        self.command_admitted.store(true, Ordering::Release);
        Ok((command, lock(&self.workspaces)))
    }

    fn outcome<T>(
        &self,
        workspace: Option<&WorkspaceRef>,
        action: &str,
        success: Option<(&str, &str)>,
        result: Result<T, Fault>,
    ) -> Result<T, Fault> {
        match &result {
            Ok(_) => {
                if let Some((code, message)) = success {
                    self.logger.emit(
                        "Rust",
                        "info",
                        None,
                        workspace.map(|value| value.workspace_id.as_str()),
                        code,
                        message,
                        json!({"action":action}),
                    );
                }
            }
            Err(error) => self.logger.emit(
                "Rust",
                "error",
                None,
                workspace.map(|value| value.workspace_id.as_str()),
                "command.failed",
                "Command failed",
                json!({"action":action,"category":error.category}),
            ),
        }
        result
    }

    fn collect(&self, state: &mut Workspaces) {
        let Some(owner) = &mut state.owner else {
            return;
        };
        if owner.terminal {
            return;
        }
        let mut view = self.runner.poll();
        let workspace = owner.workspace.as_ref();
        let workspace_id = workspace.map(|value| value.workspace_id.as_str());
        for event in view.logs.drain(..) {
            self.logger.emit(
                "Script",
                event["severity"].as_str().unwrap_or("info"),
                Some(&owner.run),
                workspace_id,
                "script.log",
                event["message"].as_str().unwrap_or("Script log"),
                event.clone(),
            );
        }
        let mut value = serde_json::to_value(view).expect("serializable controller view");
        value["workspace_id"] = json!(workspace_id);
        value["workspace_revision"] = json!(workspace.map(|value| value.revision));
        if state.controller["run"] != value["run"] || state.controller["state"] != value["state"] {
            self.logger.emit(
                "Rust",
                "info",
                Some(&owner.run),
                workspace_id,
                "run.state",
                "Run state changed",
                json!({"state":value["state"],"operation":value["operation"]}),
            );
        }
        let terminal = value["state"] == "terminal";
        let controller = Arc::new(value);
        if terminal {
            let outcome = TerminalOutcome::from_view(&controller);
            let (level, message) = outcome.notice();
            self.logger.emit(
                "Rust",
                level,
                Some(&owner.run),
                workspace_id,
                "run.terminal",
                message,
                outcome.fields(&controller["operation"]),
            );
            if let Some(workspace) = workspace {
                if let Some(selected) = state
                    .open
                    .iter_mut()
                    .find(|value| value.workspace == *workspace)
                {
                    selected.terminal = Some(WorkspaceResult {
                        workspace: workspace.clone(),
                        controller: controller.clone(),
                    });
                }
            }
            if let Some(check) = &owner.check {
                state.last_check = Some(Arc::new(RetainedCheck {
                    workspace: owner.workspace.clone(),
                    environment: check.environment.get().cloned().flatten(),
                    descriptor_path: check.descriptor_path.clone(),
                    package_inventory_identity: check.package_inventory_identity.clone(),
                    controller: controller.clone(),
                }));
            }
            owner.terminal = true;
        }
        state.controller = controller;
    }

    pub fn settings(&self) -> Result<Settings, Fault> {
        self.outcome(None, "settings", None, lock(&self.store).settings())
    }

    pub fn save_settings(&self, settings: EditableSettings) -> Result<Settings, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            drop(state);
            lock(&self.store).save_preferences(settings)
        })();
        self.outcome(
            None,
            "save_settings",
            Some(("settings.saved", "Settings saved")),
            result,
        )
    }

    pub fn capture_configuration(&self) -> Result<Capture, Fault> {
        let store = lock(&self.store);
        configuration::capture(store.root())
    }

    pub fn prepare_reconstruction(&self) -> Result<(), Fault> {
        {
            let (_command, mut state) = self.command_state().map_err(|error| {
                if self.closing.load(Ordering::Acquire) {
                    retired_fault(error)
                } else {
                    error
                }
            })?;
            self.collect(&mut state);
            state.idle()?;
            self.closing.store(true, Ordering::Release);
        }
        self.shutdown().map_err(retired_fault)?;
        let status = self.logger.status();
        if status.shutdown_timed_out || status.file_pending != 0 {
            return Err(Fault::new(
                "LoggingShutdown",
                "Configuration was not replaced because log-writer shutdown was incomplete; exit and relaunch before retrying",
            ).with_context(json!({"logging":status,"application_retired":true})));
        }
        Ok(())
    }

    pub fn create_workspace(
        &self,
        internal_name: &str,
        display_name: &str,
    ) -> Result<WorkspaceView, Fault> {
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
    }

    pub fn reopen_workspace(&self, internal_name: &str) -> Result<WorkspaceView, Fault> {
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

    fn restore_workspace(
        runner: &DesktopController,
        tab: TabRecord,
        mut workspace: WorkspaceRef,
    ) -> Workspace {
        let mut source_error = None;
        let mut selected = None;
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
            "../../../../tools/runtime-comparison/fixtures/manual-plan.json"
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
            {
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
                store.bind_package(
                    &selected.internal_name,
                    &selected.inventory.package_id,
                    &selected.path,
                )?;
            }
            let selection = self.selection(&selected);
            let mut state = lock(&self.workspaces);
            let current = state
                .open
                .iter_mut()
                .find(|value| value.workspace == *workspace)
                .expect("command admission retains the initiating workspace");
            current.workspace = selected.workspace.clone();
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

    pub fn import_legacy_profiles(&self, workspace: &WorkspaceRef) -> Result<LegacyImport, Fault> {
        let (_command, state) = self.command_state()?;
        let selected = state.resolve(workspace)?.clone();
        drop(state);
        lock(&self.store).import_legacy_profiles(&selected.internal_name, &selected.inventory)
    }

    pub fn validate(&self, workspace: &WorkspaceRef, mut values: Value) -> Result<Value, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            lock(&self.store)
                .profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            normalize_editor_numbers(&selected.inventory.schema, &mut values);
            desktop_options(&selected.inventory, values)
        })();
        self.outcome(Some(workspace), "validate", None, result)
    }

    pub fn profiles(&self, workspace: &WorkspaceRef) -> Result<ProfileCatalog, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let store = lock(&self.store);
            let scoped =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            let (profiles, profiles_error) = validated_profiles(&scoped, &selected.inventory)?;
            Ok(ProfileCatalog {
                profiles,
                profiles_error,
            })
        })();
        self.outcome(Some(workspace), "profiles", None, result)
    }

    pub fn save_profile(
        &self,
        workspace: &WorkspaceRef,
        id: Option<&str>,
        name: &str,
        mut values: Value,
    ) -> Result<Profile, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let current = self.runner.inspect(&selected.path)?;
            if current.inventory_identity != selected.inventory.identity {
                return Err(Fault::new(
                    "InventoryChanged",
                    "Package changed; inspect it again before saving",
                ));
            }
            normalize_editor_numbers(&selected.inventory.schema, &mut values);
            desktop_options(&selected.inventory, values.clone())?;
            let store = lock(&self.store);
            let store =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            if let Some(id) = id {
                checked_profile(&store, &current.package_id, &current.schema_identity, id)?;
            }
            store.save(&selected.inventory, id, name, values)
        })();
        self.outcome(
            Some(workspace),
            "save_profile",
            Some(("profile.saved", "Profile saved")),
            result,
        )
    }

    pub fn rename_profile(
        &self,
        workspace: &WorkspaceRef,
        id: &str,
        name: &str,
    ) -> Result<Profile, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let store = lock(&self.store);
            let store =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            let profile = checked_profile(
                &store,
                &selected.inventory.package_id,
                &selected.package.schema_identity,
                id,
            )?;
            desktop_options(&selected.inventory, profile.values.clone())?;
            store.rename(&selected.inventory, id, name)
        })();
        self.outcome(
            Some(workspace),
            "rename_profile",
            Some(("profile.renamed", "Profile renamed")),
            result,
        )
    }

    pub fn delete_profile(&self, workspace: &WorkspaceRef, id: &str) -> Result<(), Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let store = lock(&self.store);
            let store =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            let belongs = store
                .list(
                    &selected.inventory.package_id,
                    &selected.package.schema_identity,
                )?
                .profiles
                .iter()
                .any(|profile| profile.id == id);
            if !belongs {
                return Err(Fault::new(
                    "ProfileNotFound",
                    "Saved profile does not belong to this workspace",
                ));
            }
            store.delete(id)
        })();
        self.outcome(
            Some(workspace),
            "delete_profile",
            Some(("profile.deleted", "Profile deleted")),
            result,
        )
    }

    pub fn start(
        &self,
        workspace: &WorkspaceRef,
        mut request: StartRequest,
    ) -> Result<String, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let selected = state.resolve(workspace)?;
            if request.inventory_identity != selected.inventory.identity
                || request.package_id != selected.inventory.package_id
                || request.schema_identity != selected.package.schema_identity
            {
                return Err(Fault::new(
                    "StaleIdentity",
                    "Workspace package changed; inspect it again",
                ));
            }
            // The caller's location is never an execution authority.
            request.package_path = selected.path.to_string_lossy().into_owned();
            normalize_editor_numbers(&selected.inventory.schema, &mut request.values);
            desktop_options(&selected.inventory, request.values.clone())?;
            let internal_name = selected.internal_name.clone();
            self.collect(&mut state);
            // Never let reserve refresh away an uncollected terminal outcome.
            state.idle()?;
            let store = self.store.clone();
            let (acquired, ready) = mpsc::sync_channel(1);
            let run = self
                .runner
                .start_with_preparation(request, move |request, control| {
                    let store = lock(&store);
                    let _ = acquired.send(());
                    control.check()?;
                    let environment = if request.lane == "replay" {
                        store.settings()?.ocr_environment
                    } else {
                        None
                    };
                    let profiles = store.profile_store(&internal_name, &request.package_id)?;
                    if request.profile_id != "draft" {
                        let profile = checked_profile(
                            &profiles,
                            &request.package_id,
                            &request.schema_identity,
                            &request.profile_id,
                        )?;
                        if !same_json_values(&profile.values, &request.values) {
                            return Err(Fault::new(
                                "ProfileIdentity",
                                "Saved profile values changed; select it again",
                            )
                            .with_context(json!({"profile_id":profile.id})));
                        }
                    }
                    control.check()?;
                    Ok(environment)
                })?;
            state.owner = Some(OperationOwner {
                run: run.clone(),
                workspace: Some(workspace.clone()),
                check: None,
                terminal: false,
            });
            self.collect(&mut state);
            drop(_command);
            drop(state);
            // Publication precedes collection; store capture precedes admission return.
            let _ = ready.recv();
            Ok(run)
        })();
        self.outcome(Some(workspace), "start", None, result)
    }

    pub fn check_environment(
        &self,
        workspace: Option<&WorkspaceRef>,
        replay_descriptor_path: Option<String>,
    ) -> Result<String, Fault> {
        let result = (|| {
            if replay_descriptor_path
                .as_ref()
                .is_some_and(|path| path.len() > 4096)
            {
                return Err(Fault::new(
                    "ReplayPrerequisite",
                    "Recorded corpus location exceeds the path limit",
                ));
            }
            let (_command, mut state) = self.command_state()?;
            let selected = workspace
                .map(|workspace| state.resolve(workspace))
                .transpose()?;
            let package = selected
                .map(|selected| (selected.path.clone(), selected.inventory.identity.clone()));
            let scope = selected.map(|selected| {
                (
                    selected.internal_name.clone(),
                    selected.inventory.package_id.clone(),
                )
            });
            self.collect(&mut state);
            state.idle()?;
            let environment = Arc::new(OnceLock::new());
            let captured = environment.clone();
            let check = CheckInputs {
                environment,
                descriptor_path: replay_descriptor_path.clone(),
                package_inventory_identity: package.as_ref().map(|(_, identity)| identity.clone()),
            };
            let store = self.store.clone();
            let (acquired, ready) = mpsc::sync_channel(1);
            let run = self.runner.check_environment_with_preparation(
                package,
                replay_descriptor_path,
                move |control| {
                    let store = lock(&store);
                    let _ = acquired.send(());
                    control.check()?;
                    if let Some((internal_name, package_id)) = &scope {
                        store.profile_store(internal_name, package_id)?;
                    }
                    let environment = store.settings()?.ocr_environment;
                    let _ = captured.set(environment.clone());
                    Ok(environment)
                },
            )?;
            state.owner = Some(OperationOwner {
                run: run.clone(),
                workspace: workspace.cloned(),
                check: Some(check),
                terminal: false,
            });
            self.collect(&mut state);
            drop(_command);
            drop(state);
            let _ = ready.recv();
            Ok(run)
        })();
        self.outcome(workspace, "check_environment", None, result)
    }

    pub fn stop(&self, run: &str) -> Result<(), Fault> {
        // No workspace/store locks on the cancellation path.
        self.outcome(None, "stop", None, self.runner.stop(run))
    }

    pub fn poll(&self) -> Poll {
        let mut state = lock(&self.workspaces);
        self.collect(&mut state);
        Poll {
            controller: state.controller.clone(),
            logs: self.logger.drain(),
            workspace_results: state
                .open
                .iter()
                .filter_map(|selected| selected.terminal.clone())
                .collect(),
            last_check: state.last_check.clone(),
        }
    }

    pub fn shutdown(&self) -> Result<(), Fault> {
        self.shutdown_outcome
            .get_or_init(|| {
                self.closing.store(true, Ordering::Release);
                let outcome = self.runner.shutdown();
                if let Some(bridge) = lock(&self.bridge).take() {
                    let _ = bridge.join();
                }
                self.collect(&mut lock(&self.workspaces));
                report_log_shutdown(self.logger.shutdown());
                outcome
            })
            .clone()
    }
}

fn retired_fault(mut error: Fault) -> Fault {
    let cause = std::mem::take(&mut error.context);
    error.with_context(json!({"application_retired":true,"cause":cause}))
}

/// Display-independent facts of a settled operation for the durable record.
/// Absent evidence stays absent; no field is a path, message, or recognized text.
struct TerminalOutcome<'a> {
    status: Option<&'a str>,
    category: Option<&'a str>,
    entry_outcome: Option<&'a str>,
    cleanup_clean: Option<bool>,
    child_started: Option<bool>,
    forced: Option<bool>,
}

impl<'a> TerminalOutcome<'a> {
    fn from_view(controller: &'a Value) -> Self {
        let result = &controller["result"];
        let error = &controller["error"];
        // A result record owns its cleanup and containment facts; without one, only
        // the returning worker's fault context can attest them (never inferred).
        let (cleanup, boundary) = if result.is_null() {
            (&error["context"]["cleanup"], &error["context"])
        } else {
            (&result["cleanup"], result)
        };
        Self {
            status: result["status"].as_str(),
            category: result["primary"]["category"]
                .as_str()
                .or_else(|| error["category"].as_str()),
            entry_outcome: result["entry_outcome"].as_str(),
            cleanup_clean: cleanup["clean"].as_bool(),
            child_started: cleanup["child_started"]
                .as_bool()
                .or_else(|| boundary["child_started"].as_bool()),
            forced: boundary["forced"].as_bool(),
        }
    }

    fn notice(&self) -> (&'static str, &'static str) {
        if self.category.is_none() && self.cleanup_clean == Some(true) {
            return if self.status == Some("PASS") {
                ("info", "Operation completed")
            } else {
                ("warn", "Operation settled without success")
            };
        }
        let message = match (self.cleanup_clean, self.forced, self.child_started) {
            (Some(false), _, _) | (_, Some(true), _) => {
                "Operation failed; cleanup is incomplete or forced"
            }
            (Some(true), Some(false), _) => "Operation failed; cleanup is clean",
            (Some(true), None, Some(false)) => {
                "Operation failed before a child started; cleanup is clean"
            }
            _ => "Operation failed; cleanup is unverified",
        };
        ("error", message)
    }

    fn fields(&self, action: &Value) -> Value {
        json!({
            "action":action,
            "state":"terminal",
            "status":self.status,
            "category":self.category,
            "entry_outcome":self.entry_outcome,
            "cleanup_clean":self.cleanup_clean,
            "child_started":self.child_started,
            "forced":self.forced,
        })
    }
}

fn report_log_shutdown(status: LogStatus) {
    if !status.shutdown_timed_out && status.file_pending == 0 && status.file_errors == 0 {
        return;
    }
    // Do not use the failed/closed logger, expose private error text, or wait on a
    // blocked stderr pipe. The diagnostic is best-effort and never changes cleanup.
    let (done, wait) = mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("log-shutdown-diagnostic".into())
        .spawn(move || {
            let _ = writeln!(
                std::io::stderr(),
                "Log shutdown: shutdown_timed_out={} file_pending={} file_errors={}",
                status.shutdown_timed_out,
                status.file_pending,
                status.file_errors,
            );
            let _ = done.send(());
        })
        .is_ok()
    {
        let _ = wait.recv_timeout(Duration::from_millis(50));
    }
}

// i128 avoids the saturating i64/u64 cast that would accept their rounded maxima.
fn exact_integer(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

fn check_webview_value(value: &Value, path: &str) -> Result<(), Fault> {
    match value {
        Value::Number(number) => {
            let floating = number.as_f64().expect("JSON numbers are finite");
            if (floating == 0.0 && floating.is_sign_negative())
                || exact_integer(number).is_some_and(|integer| {
                    !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&integer)
                })
            {
                return Err(Fault::new(
                    "NumericPrecision",
                    format!("{path}: number is outside the desktop's lossless JSON contract; source data was preserved"),
                )
                .with_context(json!({"path":path,"value":number.to_string()})));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check_webview_value(item, &format!("{path}[{index}]"))?;
            }
        }
        Value::Object(fields) => {
            for (name, value) in fields {
                check_webview_value(value, &format!("{path}.{name}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

// A number editor intentionally uses f64 semantics. JSON.stringify can emit an
// integer spelling for a float (1.0, 1e18, etc.); restore that schema-known type
// only on incoming editor values, never on unchecked package or stored data.
fn normalize_editor_numbers(schema: &Value, value: &mut Value) {
    match schema["type"].as_str() {
        Some("number") => {
            if let Some(number) = value.as_f64().and_then(serde_json::Number::from_f64) {
                *value = Value::Number(number);
            }
        }
        Some("object") => {
            if let Some(fields) = value.as_object_mut() {
                for (name, value) in fields {
                    if let Some(node) = schema["properties"].get(name) {
                        normalize_editor_numbers(node, value);
                    }
                }
            }
        }
        Some("array") => {
            if let Some(items) = value.as_array_mut() {
                for value in items {
                    normalize_editor_numbers(&schema["items"], value);
                }
            }
        }
        _ => {}
    }
}

fn desktop_options(inventory: &Inventory, values: Value) -> Result<Value, Fault> {
    check_webview_value(&values, "$")?;
    let effective = resolve_options(
        &inventory.schema,
        &json!({"package_id":inventory.package_id,"schema_version":inventory.schema["version"],"options":values}),
        &inventory.package_id,
    )?;
    check_webview_value(&effective, "$")?;
    Ok(effective)
}

fn checked_profile(
    store: &ProfileStore,
    package_id: &str,
    schema_identity: &str,
    id: &str,
) -> Result<Profile, Fault> {
    let profile = store
        .list(package_id, schema_identity)?
        .profiles
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| {
            Fault::new(
                "ProfileNotFound",
                "Saved profile is unavailable; select or save it again",
            )
        })?;
    check_webview_value(&profile.values, "$").map_err(|error| {
        Fault::new(
            "ProfileRejected",
            "Saved numbers cannot be used by the desktop; the file was preserved",
        )
        .with_context(json!({"profile_id":profile.id,"cause":error}))
    })?;
    Ok(profile)
}

// JSON normalizes 1.0 to 1 in the WebView, but must never equate rounded integers.
fn same_json_values(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            match (exact_integer(left), exact_integer(right)) {
                (Some(left), Some(right)) => left == right,
                (Some(integer), None) | (None, Some(integer)) => {
                    let floating = if left.is_f64() { left } else { right };
                    floating
                        .as_f64()
                        .is_some_and(|value| value.fract() == 0.0 && value as i128 == integer)
                }
                (None, None) => left == right,
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_json_values(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|other| same_json_values(value, other))
                })
        }
        _ => left == right,
    }
}

fn validated_profiles(
    store: &ProfileStore,
    inventory: &Inventory,
) -> Result<(Vec<Profile>, Option<Fault>), Fault> {
    let listing = store.list(
        &inventory.package_id,
        &mado_runtime_comparison::model::identity(&inventory.schema)?,
    )?;
    let mut profiles = Vec::with_capacity(listing.profiles.len());
    let mut rejected = listing.rejected;
    for profile in listing.profiles {
        match desktop_options(inventory, profile.values.clone()) {
            Ok(_) => profiles.push(profile),
            Err(error) => rejected.push(
                Fault::new("Profile", "Saved values do not match the selected schema")
                    .with_context(json!({"profile_id":profile.id, "cause":error})),
            ),
        }
    }
    let error = (!rejected.is_empty()).then(|| {
        Fault::new(
            "ProfileRejected",
            "Some saved profiles are incompatible; their files were preserved",
        )
        .with_context(json!({"rejected":rejected}))
    });
    Ok((profiles, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        application: Arc<Application>,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "mado-application-{}-{nonce}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
            ));
            Store::new(root.clone())
                .unwrap()
                .initialize(preferences())
                .unwrap();
            let application = Application::new(
                root.clone(),
                root.join("runner-must-not-be-launched"),
                root.join("engine-must-not-be-launched"),
            )
            .unwrap();
            Self { root, application }
        }

        fn package_at(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            for relative in [
                "package.json",
                "main.js",
                "decisions.js",
                "schema.json",
                "profiles/template-first.json",
                "profiles/ocr-first.json",
                "assets/marker.rgba",
            ] {
                let destination = path.join(relative);
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                fs::copy(package_path().join(relative), destination).unwrap();
            }
            path
        }

        fn numeric_package(&self) -> PathBuf {
            let path = self.package_at("package");
            let schema_path = path.join("schema.json");
            let mut schema: Value =
                serde_json::from_slice(&fs::read(&schema_path).unwrap()).unwrap();
            schema["properties"]["amount"] = json!({"type":"number","default":1});
            schema["properties"]["numbers"] = json!({"type":"array","items":{"type":"number"}});
            fs::write(schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.application.shutdown();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn package_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tools/runtime-comparison/fixtures/javascript")
            .canonicalize()
            .unwrap()
    }

    fn workspace_ref(selection: &Selection) -> WorkspaceRef {
        WorkspaceRef {
            workspace_id: selection.workspace_id.clone(),
            revision: selection.revision,
        }
    }

    fn view_ref(workspace: &WorkspaceView) -> WorkspaceRef {
        WorkspaceRef {
            workspace_id: workspace.workspace_id.clone(),
            revision: workspace.revision,
        }
    }

    fn inspect_named(
        application: &Application,
        internal_name: &str,
        path: &Path,
    ) -> Result<Selection, Fault> {
        let workspace = application.create_workspace(internal_name, internal_name)?;
        application.inspect(path, &view_ref(&workspace))
    }

    fn preferences() -> EditableSettings {
        let settings = Settings::default();
        EditableSettings {
            locale: settings.locale,
            gui_log_limit: settings.gui_log_limit,
            ocr_environment: settings.ocr_environment,
            notifications: settings.notifications,
            backup_directory: settings.backup_directory,
        }
    }

    fn profile_path(fixture: &Fixture, internal_name: &str, profile: &Profile) -> PathBuf {
        fixture
            .root
            .join("tabs")
            .join(internal_name)
            .join(&profile.package_id)
            .join(format!("{}.config", profile.id))
    }

    fn request(selection: &Selection) -> StartRequest {
        StartRequest {
            package_path: selection.package_path.clone(),
            inventory_identity: selection.package.inventory_identity.clone(),
            package_id: selection.package.package_id.clone(),
            schema_identity: selection.package.schema_identity.clone(),
            profile_id: "draft".into(),
            values: selection.package.profiles["template-first"]["options"].clone(),
            lane: "controlled".into(),
            scenario: "workflow".into(),
            replay_descriptor_path: None,
        }
    }

    #[test]
    fn unbound_commands_refuse_without_borrowing_another_tabs_inventory() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let bound = inspect_named(application, "Bound", &package_path()).unwrap();
        let empty = application.create_workspace("Empty", "No package").unwrap();
        let workspace = view_ref(&empty);
        assert_eq!(workspace.revision, 0);
        assert!(empty.selection.is_none());
        assert!(empty.source_error.is_none());
        let errors = [
            application.validate(&workspace, json!({})).unwrap_err(),
            application.profiles(&workspace).unwrap_err(),
            application
                .save_profile(&workspace, None, "No owner", json!({}))
                .unwrap_err(),
            application
                .rename_profile(&workspace, "missing", "No owner")
                .unwrap_err(),
            application
                .delete_profile(&workspace, "missing")
                .unwrap_err(),
            application.start(&workspace, request(&bound)).unwrap_err(),
            application
                .check_environment(Some(&workspace), None)
                .unwrap_err(),
            application
                .import_legacy_profiles(&workspace)
                .err()
                .unwrap(),
        ];
        for error in errors {
            assert_eq!(error.category, "WorkspaceUnbound");
        }
        assert!(application.runner.poll().run.is_none());
        assert!(
            application
                .inspect(&fixture.root.join("absent"), &workspace)
                .is_err()
        );
        let catalog = application.workspace_catalog().unwrap();
        let unchanged = catalog
            .open
            .iter()
            .find(|tab| tab.internal_name == "Empty")
            .unwrap();
        assert_eq!(view_ref(unchanged), workspace);
        assert!(unchanged.selection.is_none());
        let run = application.check_environment(None, None).unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        assert_eq!(terminal.error.unwrap().category, "EnvironmentUnset");
        assert!(application.poll().controller["workspace_id"].is_null());
    }

    #[test]
    fn naming_boundaries_preserve_scalars_and_closed_name_ownership() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        for (internal, display) in [
            ("", "Name"),
            ("name１", "Name"),
            ("../escape", "Name"),
            (" white", "Name"),
            ("Valid", " \t"),
            ("Valid", "line\nbreak"),
        ] {
            assert!(application.create_workspace(internal, display).is_err());
        }
        assert!(
            application
                .create_workspace(&"a".repeat(65), "Name")
                .is_err()
        );
        assert!(
            application
                .create_workspace("LongDisplay", &"\u{10400}".repeat(81))
                .is_err()
        );
        assert!(application.workspace_catalog().unwrap().open.is_empty());
        let internal = "A".repeat(64);
        let display = "\u{10400}".repeat(80);
        let saved = application.create_workspace(&internal, &display).unwrap();
        assert_eq!(saved.display_name, display);
        application.close_workspace(&view_ref(&saved)).unwrap();
        assert!(
            application
                .create_workspace(&internal.to_lowercase(), "Alias")
                .is_err()
        );
        let duplicate = application.create_workspace("Duplicate", &display).unwrap();
        assert_eq!(duplicate.display_name, display);
        let reopened = application.reopen_workspace(&internal).unwrap();
        assert_eq!(reopened.internal_name, internal);
        assert_eq!(reopened.display_name, display);
        assert_ne!(reopened.workspace_id, saved.workspace_id);
        assert!(reopened.selection.is_none());
    }

    #[test]
    fn omitted_display_name_survives_close_reopen_and_restart() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let saved = application.create_workspace("0-copy_9", "").unwrap();
        assert_eq!(saved.display_name, "0-copy_9");
        application.close_workspace(&view_ref(&saved)).unwrap();
        let catalog = application.workspace_catalog().unwrap();
        assert_eq!(catalog.closed[0].display_name, "0-copy_9");
        let reopened = application.reopen_workspace("0-copy_9").unwrap();
        assert_eq!(reopened.display_name, "0-copy_9");
        assert_ne!(reopened.workspace_id, saved.workspace_id);
        assert_eq!(
            Store::new(fixture.root.clone())
                .unwrap()
                .tab("0-copy_9")
                .unwrap()
                .display_name,
            "0-copy_9"
        );
    }

    #[test]
    fn failed_durable_bind_close_and_reopen_preserve_host_and_saved_authority() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let empty = application.create_workspace("Main", "Saved name").unwrap();
        let reference = view_ref(&empty);
        let path = fixture.root.join("tabs/Main/tab.config");
        let pending = path.with_extension("pending");
        let before = fs::read(&path).unwrap();
        fs::write(&pending, b"interrupted Tab write").unwrap();
        assert!(application.inspect(&package_path(), &reference).is_err());
        assert!(application.close_workspace(&reference).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let current = application.workspace_catalog().unwrap().open.remove(0);
        assert_eq!(view_ref(&current), reference);
        assert!(current.selection.is_none());
        assert_eq!(fs::read(&pending).unwrap(), b"interrupted Tab write");
        fs::remove_file(&pending).unwrap();
        let bound = application.inspect(&package_path(), &reference).unwrap();
        let bound_ref = workspace_ref(&bound);
        let bound_bytes = fs::read(&path).unwrap();
        fs::write(&pending, b"interrupted Tab write").unwrap();
        assert!(
            application
                .inspect(&fixture.numeric_package(), &bound_ref)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bound_bytes);
        assert_eq!(
            application
                .validate(&bound_ref, json!({"priorities":["ocr"]}))
                .unwrap()["priorities"],
            json!(["ocr"])
        );
        fs::remove_file(&pending).unwrap();
        application.close_workspace(&bound_ref).unwrap();
        let closed_bytes = fs::read(&path).unwrap();
        fs::write(&pending, b"interrupted reopen").unwrap();
        assert!(application.reopen_workspace("Main").is_err());
        assert!(application.workspace_catalog().unwrap().open.is_empty());
        assert_eq!(fs::read(&path).unwrap(), closed_bytes);
        fs::remove_file(pending).unwrap();
        let reopened = application.reopen_workspace("Main").unwrap();
        assert_ne!(reopened.workspace_id, reference.workspace_id);
        assert!(reopened.selection.is_some());
    }

    #[test]
    fn restart_revalidates_saved_open_sources_and_scopes_faults_without_rewriting_them() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let good = inspect_named(application, "Good", &package_path()).unwrap();
        let old = workspace_ref(&good);
        let saved = application
            .save_profile(&old, None, "Durable", json!({"priorities":["ocr"]}))
            .unwrap();
        let missing_path = fixture.package_at("disappearing-source");
        inspect_named(application, "Missing", &missing_path).unwrap();
        inspect_named(application, "Archive", &package_path()).unwrap();
        inspect_named(application, "Changed", &package_path()).unwrap();
        application.create_workspace("Empty", "No package").unwrap();
        let closed = application
            .create_workspace("Closed", "Retained closed")
            .unwrap();
        application.close_workspace(&view_ref(&closed)).unwrap();
        application.create_workspace("Broken", "Malformed").unwrap();
        application.start(&old, request(&good)).unwrap();
        settled(application);
        application.shutdown().unwrap();
        fs::rename(&missing_path, fixture.root.join("relocated-source")).unwrap();
        let archive_path = fixture.root.join("tabs/Archive/tab.config");
        let mut archive = lock(&application.store).tab("Archive").unwrap();
        archive.packages[0].source = PackageSource::CustomArchive {
            path: fixture
                .root
                .join("unsupported.czip")
                .to_string_lossy()
                .into_owned(),
        };
        let archive_bytes = serde_json::to_vec(&archive).unwrap();
        fs::write(&archive_path, &archive_bytes).unwrap();
        let changed_path = fixture.root.join("tabs/Changed/tab.config");
        let mut changed = lock(&application.store).tab("Changed").unwrap();
        changed.packages[0].package_id = "another-package".into();
        changed.selected_package_id = Some("another-package".into());
        let changed_bytes = serde_json::to_vec(&changed).unwrap();
        fs::write(&changed_path, &changed_bytes).unwrap();
        let broken_path = fixture.root.join("tabs/Broken/tab.config");
        fs::write(&broken_path, b"malformed retained Tab").unwrap();
        let capture = application.capture_configuration().unwrap();
        assert_eq!(
            capture.files["tabs/Broken/tab.config"],
            b"malformed retained Tab"
        );
        assert!(capture.files.contains_key("tabs/Closed/tab.config"));
        assert!(!capture.files.keys().any(|path| path.starts_with("logs/")));
        let restarted = Application::new(
            fixture.root.clone(),
            fixture.root.join("absent-runner"),
            fixture.root.join("absent-engine"),
        )
        .unwrap();
        let catalog = restarted.workspace_catalog().unwrap();
        let restored = catalog
            .open
            .iter()
            .find(|tab| tab.internal_name == "Good")
            .unwrap();
        assert_ne!(restored.workspace_id, old.workspace_id);
        assert_eq!(
            restored.selection.as_ref().unwrap().profiles[0].id,
            saved.id
        );
        assert_eq!(
            restarted.validate(&old, json!({})).unwrap_err().category,
            "StaleIdentity"
        );
        let missing = catalog
            .open
            .iter()
            .find(|tab| tab.internal_name == "Missing")
            .unwrap();
        assert!(missing.selection.is_none());
        assert_eq!(
            missing.source_error.as_ref().unwrap().context["internal_name"],
            "Missing"
        );
        assert_eq!(
            restarted.profiles(&view_ref(missing)).unwrap_err().category,
            "WorkspaceUnbound"
        );
        let unsupported = catalog
            .open
            .iter()
            .find(|tab| tab.internal_name == "Archive")
            .unwrap();
        assert!(unsupported.selection.is_none());
        assert_eq!(
            unsupported.source_error.as_ref().unwrap().category,
            "UnsupportedPackageSource"
        );
        let changed = catalog
            .open
            .iter()
            .find(|tab| tab.internal_name == "Changed")
            .unwrap();
        assert!(changed.selection.is_none());
        assert_eq!(
            changed.source_error.as_ref().unwrap().category,
            "PackageIdentity"
        );
        assert_eq!(fs::read(changed_path).unwrap(), changed_bytes);
        let empty = catalog
            .open
            .iter()
            .find(|tab| tab.internal_name == "Empty")
            .unwrap();
        assert!(empty.selection.is_none() && empty.source_error.is_none());
        assert_eq!(catalog.closed[0].internal_name, "Closed");
        assert_eq!(catalog.faults.len(), 1);
        assert_eq!(fs::read(archive_path).unwrap(), archive_bytes);
        assert_eq!(fs::read(broken_path).unwrap(), b"malformed retained Tab");
        let poll = restarted.poll();
        assert!(poll.workspace_results.is_empty());
        assert!(poll.last_check.is_none());
        assert!(poll.controller["run"].is_null());
        restarted.shutdown().unwrap();
    }

    #[test]
    fn reconstruction_requires_settled_commands_and_closes_all_future_admission() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let workspace = application.create_workspace("Empty", "Empty").unwrap();
        let store = lock(&application.store);
        application.command_admitted.store(false, Ordering::Release);
        let saving = application.clone();
        let writer = std::thread::spawn(move || saving.save_settings(preferences()));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !application.command_admitted.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let refusal = application.prepare_reconstruction().unwrap_err();
        assert_eq!(refusal.category, "WorkspaceBusy");
        assert_ne!(refusal.context["application_retired"], true);
        assert!(!application.closing.load(Ordering::Acquire));
        drop(store);
        writer.join().unwrap().unwrap();
        application.prepare_reconstruction().unwrap();
        assert_eq!(
            application
                .create_workspace("Late", "Late")
                .unwrap_err()
                .category,
            "Closing"
        );
        assert_eq!(
            application
                .close_workspace(&view_ref(&workspace))
                .unwrap_err()
                .category,
            "Closing"
        );
        assert_eq!(
            application
                .save_settings(preferences())
                .unwrap_err()
                .category,
            "Closing"
        );
        assert_eq!(
            application.prepare_reconstruction().unwrap_err().context["application_retired"],
            true
        );
        assert!(lock(&application.store).tab("Empty").unwrap().open);
        assert!(application.poll().controller["run"].is_null());
    }

    #[test]
    fn invalid_settings_precede_worker_creation() {
        let fixture = Fixture::new();
        fixture.application.shutdown().unwrap();
        let settings_path = fixture.root.join("settings.json");
        let bytes = fs::read(&settings_path).unwrap();
        for invalid in [None, Some(b"invalid settings".as_slice())] {
            match invalid {
                Some(bytes) => {
                    Store::new(fixture.root.clone())
                        .unwrap()
                        .initialize(preferences())
                        .unwrap();
                    fs::write(&settings_path, bytes).unwrap();
                }
                None => {
                    fs::remove_file(&settings_path).unwrap();
                }
            }
            let result = Application::build(
                fixture.root.clone(),
                fixture.root.join("runner"),
                fixture.root.join("engine"),
                |_| panic!("invalid settings must be rejected before starting workers"),
                None,
            );
            assert!(result.is_err());
        }
        fs::write(settings_path, bytes).unwrap();
    }

    #[test]
    fn writer_start_failure_waits_for_bridge_termination_before_retry() {
        let fixture = Fixture::new();
        fixture.application.shutdown().unwrap();
        let root = fixture.root.clone();
        let (exiting, exit) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let (returned, result) = mpsc::sync_channel(1);
        let constructor = std::thread::spawn(move || {
            let failure = Application::build(
                root.clone(),
                root.join("runner"),
                root.join("engine"),
                |_| {
                    Err(Fault::new(
                        "LoggingInitialization",
                        "Injected writer creation failure",
                    ))
                },
                Some((exiting, released)),
            );
            let _ = returned.send(failure);
        });
        // The actual bridge has observed failed publication, but cannot exit
        // until released. Construction must still own and await that thread.
        exit.recv_timeout(Duration::from_secs(2)).unwrap();
        let premature = result.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        assert!(matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)));
        let failure = result.recv_timeout(Duration::from_secs(2)).unwrap();
        constructor.join().unwrap();
        assert_eq!(failure.err().unwrap().category, "LoggingInitialization");
        let retried = Application::new(
            fixture.root.clone(),
            fixture.root.join("runner"),
            fixture.root.join("engine"),
        )
        .unwrap();
        assert!(retried.workspace_catalog().unwrap().open.is_empty());
        retried.shutdown().unwrap();
    }

    fn settled(application: &Application) -> mado_runtime_comparison::desktop::ControllerView {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let view = application.runner.poll();
            if view.state == "terminal" {
                return view;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "operation did not settle"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn environment_check_refuses_missing_or_stale_package_selection() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let unknown = WorkspaceRef {
            workspace_id: "unknown".into(),
            revision: 1,
        };
        let error = application
            .check_environment(Some(&unknown), None)
            .unwrap_err();
        assert_eq!(error.category, "StaleIdentity");
        let selection = inspect_named(application, "Main", &package_path()).unwrap();
        let workspace = workspace_ref(&selection);
        let replacement = application
            .inspect(&fixture.numeric_package(), &workspace)
            .unwrap();
        assert_eq!(replacement.workspace_id, workspace.workspace_id);
        let error = application
            .check_environment(Some(&workspace), None)
            .unwrap_err();
        assert_eq!(error.category, "StaleIdentity");
        assert!(application.runner.poll().run.is_none());
    }

    #[test]
    fn environment_check_without_package_does_not_inherit_cached_selection() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let selection = inspect_named(application, "Main", &package_path()).unwrap();
        let run = application.check_environment(None, None).unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "EnvironmentUnset");
        assert!(fault.context["package_inventory_identity"].is_null());
        let poll = serde_json::to_value(application.poll()).unwrap();
        assert!(poll["controller"]["workspace_id"].is_null());
        assert!(poll["controller"]["workspace_revision"].is_null());
        assert!(poll["last_check"]["workspace"].is_null());
        assert_eq!(poll["last_check"]["controller"]["run"], run);
        assert_eq!(poll["workspace_results"], json!([]));

        // Omitting the package must not discard the cached inspection either.
        let identity = selection.package.inventory_identity.clone();
        let run = application
            .check_environment(Some(&workspace_ref(&selection)), None)
            .unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "EnvironmentUnset");
        assert_eq!(fault.context["package_inventory_identity"], identity);
    }

    #[test]
    fn stale_saved_values_are_refused_before_runner_startup() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = package_path();
        let selection = inspect_named(application, "Main", &path).unwrap();
        let workspace = workspace_ref(&selection);
        let plan: Plan = serde_json::from_str(include_str!(
            "../../../../tools/runtime-comparison/fixtures/manual-plan.json"
        ))
        .unwrap();
        let inventory = Inventory::capture(&path, &plan.limits).unwrap();
        let values = inventory.profiles["template-first"]["options"].clone();
        let saved = application
            .save_profile(&workspace, None, "Saved choice", values.clone())
            .unwrap();
        let mut replacement = values.clone();
        replacement["priorities"] = json!(["ocr", "template"]);
        application
            .save_profile(
                &workspace,
                Some(&saved.id),
                "Saved choice",
                replacement.clone(),
            )
            .unwrap();
        let stored_path = profile_path(&fixture, "Main", &saved);
        let before = fs::read(&stored_path).unwrap();
        let run = application
            .start(
                &workspace,
                StartRequest {
                    package_path: path.to_string_lossy().into_owned(),
                    inventory_identity: inventory.identity,
                    package_id: saved.package_id.clone(),
                    schema_identity: saved.schema_identity.clone(),
                    profile_id: saved.id.clone(),
                    values,
                    lane: "controlled".into(),
                    scenario: "workflow".into(),
                    replay_descriptor_path: None,
                },
            )
            .unwrap();
        let controller = settled(application);
        let fault = controller.error.unwrap();
        assert_eq!(fault.category, "ProfileIdentity");
        assert_eq!(fault.context["profile_id"], saved.id);
        assert_eq!(controller.run.as_deref(), Some(run.as_str()));
        assert_eq!(
            fault.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
        assert_eq!(fs::read(&stored_path).unwrap(), before);
        let profiles = application.profiles(&workspace).unwrap();
        assert_eq!(profiles.profiles[0].values, replacement);
    }

    #[test]
    fn admission_reserves_before_store_io_and_owns_the_profile_snapshot() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = package_path();
        let selection = inspect_named(application, "Main", &path).unwrap();
        let workspace = workspace_ref(&selection);
        let values = selection.package.profiles["template-first"]["options"].clone();
        let saved = application
            .save_profile(&workspace, None, "Before", values.clone())
            .unwrap();
        let request = StartRequest {
            package_path: path.to_string_lossy().into_owned(),
            inventory_identity: selection.package.inventory_identity.clone(),
            package_id: saved.package_id.clone(),
            schema_identity: saved.schema_identity.clone(),
            profile_id: saved.id.clone(),
            values: values.clone(),
            lane: "controlled".into(),
            scenario: "workflow".into(),
            replay_descriptor_path: None,
        };
        let store = lock(&application.store);
        let (admitted, admission) = mpsc::sync_channel(1);
        let starting = application.clone();
        let starting_workspace = workspace.clone();
        let starter = std::thread::spawn(move || {
            let _ = admitted.send(starting.start(&starting_workspace, request));
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while application.runner.poll().state != "preparing" {
            assert!(
                std::time::Instant::now() < deadline,
                "Start did not reserve"
            );
            std::thread::yield_now();
        }
        let published = application.poll();
        assert_eq!(published.controller["workspace_id"], workspace.workspace_id);
        assert_eq!(
            published.controller["workspace_revision"],
            workspace.revision
        );
        let (checked, check) = mpsc::sync_channel(1);
        let checking = application.clone();
        let competitor = std::thread::spawn(move || {
            let _ = checked.send(checking.check_environment(None, None));
        });
        let refusal = check.recv_timeout(Duration::from_secs(2));
        let premature = admission.recv_timeout(Duration::from_millis(100));
        drop(store);
        competitor.join().unwrap();
        starter.join().unwrap();
        assert_eq!(refusal.unwrap().unwrap_err().category, "RunActive");
        assert!(
            matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)),
            "Start must acquire snapshot ownership before returning admission"
        );
        let run = admission
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let mut next_values = values;
        next_values["priorities"] = json!(["ocr", "template"]);
        application
            .save_profile(&workspace, Some(&saved.id), "After", next_values)
            .unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        // The absent fixture executable is the expected boundary, not a changed-profile refusal.
        assert_eq!(terminal.error.unwrap().category, "ChildStartup");
    }

    #[test]
    fn inspection_preserves_unrelated_app_preferences_and_legacy_hint() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let settings_path = fixture.root.join("settings.json");
        let mut legacy = application.settings().unwrap();
        legacy.package_path = Some("previous-package".into());
        fs::write(&settings_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let before = fs::read(&settings_path).unwrap();
        let selection = inspect_named(application, "Main", &package_path()).unwrap();
        let workspace = workspace_ref(&selection);
        assert_eq!(
            application
                .validate(&workspace, json!({"priorities":["ocr"]}))
                .unwrap()["priorities"],
            json!(["ocr"])
        );
        assert_eq!(fs::read(settings_path).unwrap(), before);
        assert_eq!(
            application.settings().unwrap().package_path.as_deref(),
            Some("previous-package")
        );
        let tab = lock(&application.store).tab("Main").unwrap();
        assert_eq!(
            tab.selected_package_id.as_deref(),
            Some(selection.package.package_id.as_str())
        );
    }

    #[test]
    fn unsafe_package_numbers_are_refused_before_ipc_export() {
        for (source, pointer, expected_path) in [
            (
                "schema.json",
                "/properties/amount/default",
                "$.schema.properties.amount.default",
            ),
            (
                "schema.json",
                "/properties/amount/maximum",
                "$.schema.properties.amount.maximum",
            ),
            (
                "profiles/template-first.json",
                "/options/amount",
                "$.presets[\"template-first\"].options.amount",
            ),
        ] {
            let fixture = Fixture::new();
            let package = fixture.numeric_package();
            let path = package.join(source);
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            if source == "schema.json" {
                value["properties"]["amount"]["maximum"] = json!(u64::MAX);
            } else {
                value["options"]["amount"] = Value::Null;
            }
            *value.pointer_mut(pointer).unwrap() = json!(9_007_199_254_740_993_u64);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let before = fs::read(&path).unwrap();
            let error = inspect_named(&fixture.application, "Main", &package)
                .err()
                .expect("unsafe source must be refused");
            assert_eq!(error.category, "NumericPrecision", "{source}");
            assert_eq!(error.context["path"], expected_path);
            assert_eq!(error.context["value"], "9007199254740993");
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn unsafe_stored_numbers_remain_preserved_and_unavailable_to_commands() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = fixture.numeric_package();
        let selection = inspect_named(application, "Main", &path).unwrap();
        let workspace = workspace_ref(&selection);
        let inventory = lock(&application.workspaces)
            .resolve(&workspace)
            .unwrap()
            .inventory
            .as_ref()
            .clone();
        let saved = lock(&application.store)
            .profile_store(&selection.internal_name, &inventory.package_id)
            .unwrap()
            .save(
                &inventory,
                None,
                "Exact imported number",
                json!({"amount":9_007_199_254_740_993_u64}),
            )
            .unwrap();
        let stored_path = profile_path(&fixture, "Main", &saved);
        let before = fs::read(&stored_path).unwrap();
        let reopened = application.profiles(&workspace).unwrap();
        assert!(reopened.profiles.is_empty());
        let error = reopened.profiles_error.unwrap();
        assert_eq!(
            error.context["rejected"][0]["context"]["profile_id"],
            saved.id
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["path"],
            "$.amount"
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["value"],
            "9007199254740993"
        );
        let catalog = application.profiles(&workspace).unwrap();
        assert!(catalog.profiles.is_empty());
        let warning = catalog.profiles_error.unwrap();
        assert_eq!(warning.category, "ProfileRejected");
        assert_eq!(
            warning.context["rejected"][0]["context"]["profile_id"],
            saved.id
        );
        assert_eq!(
            application
                .save_profile(
                    &workspace,
                    Some(&saved.id),
                    "Rounded replacement",
                    json!({"amount":1})
                )
                .unwrap_err()
                .category,
            "ProfileRejected",
        );
        assert_eq!(
            application
                .rename_profile(&workspace, &saved.id, "Renamed")
                .unwrap_err()
                .category,
            "ProfileRejected"
        );
        application
            .start(
                &workspace,
                StartRequest {
                    package_path: path.to_string_lossy().into_owned(),
                    inventory_identity: inventory.identity,
                    package_id: saved.package_id,
                    schema_identity: saved.schema_identity,
                    profile_id: saved.id,
                    values: json!({"amount":9_007_199_254_740_992_u64}),
                    lane: "controlled".into(),
                    scenario: "workflow".into(),
                    replay_descriptor_path: None,
                },
            )
            .unwrap();
        let controller = settled(application);
        assert_eq!(controller.error.unwrap().category, "ProfileRejected");
        assert_eq!(fs::read(stored_path).unwrap(), before);
    }

    #[test]
    fn omitted_defaults_are_checked_without_replacing_explicit_values() {
        let fixture = Fixture::new();
        let path = fixture.numeric_package();
        inspect_named(&fixture.application, "Main", &path).unwrap();
        let mut selected = lock(&fixture.application.workspaces);
        let inventory = Arc::make_mut(&mut selected.open[0].selected.as_mut().unwrap().inventory);
        inventory.schema["properties"]["amount"]["default"] = json!(9_007_199_254_740_993_u64);
        let error = desktop_options(inventory, json!({})).unwrap_err();
        assert_eq!(error.category, "NumericPrecision");
        assert_eq!(error.context["path"], "$.amount");
        let explicit = desktop_options(inventory, json!({"amount":0.1})).unwrap();
        assert_eq!(explicit["amount"], json!(0.1));
    }

    #[test]
    fn integer_precision_checks_do_not_saturate_or_conflate_values() {
        for text in [
            "9007199254740992",
            "9007199254740993",
            "-9007199254740993",
            "9223372036854775807",
            "18446744073709551615",
            "-9223372036854775808",
        ] {
            let value: Value = serde_json::from_str(text).unwrap();
            let error = check_webview_value(&json!({"items":[value]}), "$").unwrap_err();
            assert_eq!(error.context["path"], "$.items[0]");
            assert_eq!(error.context["value"], text);
        }
        let safe: Value =
            serde_json::from_str("[9007199254740991,-9007199254740991,0.1,1e18,1e100]").unwrap();
        check_webview_value(&safe, "$").unwrap();
        assert!(same_json_values(
            &json!({"items":[1.0]}),
            &json!({"items":[1]})
        ));
        assert!(!same_json_values(
            &json!(9_007_199_254_740_993_u64),
            &json!(9_007_199_254_740_992_u64)
        ));
        assert!(!same_json_values(
            &json!(9_007_199_254_740_993_u64),
            &json!(9_007_199_254_740_992_f64)
        ));
        assert!(!same_json_values(&json!(u64::MAX), &json!(u64::MAX as f64)));
    }

    #[test]
    fn floating_profiles_survive_webview_normalization_save_reopen_and_start() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = fixture.numeric_package();
        let selection = inspect_named(application, "Main", &path).unwrap();
        let workspace = workspace_ref(&selection);
        let inventory = lock(&application.workspaces)
            .resolve(&workspace)
            .unwrap()
            .inventory
            .as_ref()
            .clone();
        let source: Value = serde_json::from_str(
            r#"{"amount":1000000000000000100.0,"numbers":[1.0,1e18,1e100,0.1,9007199254740991.0]}"#,
        )
        .unwrap();
        let stored = lock(&application.store)
            .profile_store(&selection.internal_name, &inventory.package_id)
            .unwrap()
            .save(&inventory, None, "Floating values", source.clone())
            .unwrap();
        let selection = application.profiles(&workspace).unwrap();
        assert_eq!(selection.profiles[0].values, source);
        // These are the integer spellings emitted by WebView JSON.stringify.
        let submitted: Value = serde_json::from_str(
            r#"{"amount":1000000000000000100,"numbers":[1,1000000000000000000,1e100,0.1,9007199254740991]}"#
        ).unwrap();
        let effective = application.validate(&workspace, submitted.clone()).unwrap();
        assert_eq!(effective["amount"], source["amount"]);
        let saved = application
            .save_profile(
                &workspace,
                Some(&stored.id),
                "Floating values",
                submitted.clone(),
            )
            .unwrap();
        assert_eq!(saved.values, source);
        let reopened = application.profiles(&workspace).unwrap();
        assert!(reopened.profiles_error.is_none());
        assert_eq!(reopened.profiles[0].values, source);
        let stored_path = profile_path(&fixture, "Main", &saved);
        let before = fs::read(&stored_path).unwrap();
        let run = application
            .start(
                &workspace,
                StartRequest {
                    package_path: path.to_string_lossy().into_owned(),
                    inventory_identity: inventory.identity,
                    package_id: saved.package_id,
                    schema_identity: saved.schema_identity,
                    profile_id: saved.id.clone(),
                    values: submitted,
                    lane: "controlled".into(),
                    scenario: "workflow".into(),
                    replay_descriptor_path: None,
                },
            )
            .unwrap();
        assert_eq!(application.runner.poll().run.as_deref(), Some(run.as_str()));
        let terminal = settled(application);
        assert_eq!(terminal.error.as_ref().unwrap().category, "ChildStartup");
        assert_eq!(
            terminal.error.unwrap().context["operation_stage"],
            "execution"
        );
        assert_eq!(fs::read(stored_path).unwrap(), before);
    }

    #[test]
    fn controlled_start_ignores_unavailable_environment_settings() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = package_path();
        let selection = inspect_named(application, "Main", &path).unwrap();
        let workspace = workspace_ref(&selection);
        let settings_path = fixture.root.join("settings.json");
        let preserved = b"unreadable OCR settings must not disable controlled runs";
        fs::write(&settings_path, preserved).unwrap();
        let package = selection.package;
        application
            .start(
                &workspace,
                StartRequest {
                    package_path: path.to_string_lossy().into_owned(),
                    inventory_identity: package.inventory_identity.clone(),
                    package_id: package.package_id.clone(),
                    schema_identity: package.schema_identity.clone(),
                    profile_id: "draft".into(),
                    values: package.profiles["template-first"]["options"].clone(),
                    lane: "controlled".into(),
                    scenario: "workflow".into(),
                    replay_descriptor_path: None,
                },
            )
            .unwrap();
        let terminal = settled(application);
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "ChildStartup");
        assert_eq!(fault.context["operation_stage"], "execution");
        assert!(fault.context["environment_identity"].is_null());
        assert_eq!(fs::read(settings_path).unwrap(), preserved);
    }

    #[test]
    fn signed_zero_sources_are_refused_without_rewriting_profiles() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = fixture.numeric_package();
        let selection = inspect_named(application, "Main", &path).unwrap();
        let workspace = workspace_ref(&selection);
        let inventory = lock(&application.workspaces)
            .resolve(&workspace)
            .unwrap()
            .inventory
            .as_ref()
            .clone();
        let saved = lock(&application.store)
            .profile_store(&selection.internal_name, &inventory.package_id)
            .unwrap()
            .save(&inventory, None, "Signed zero", json!({"amount":-0.0}))
            .unwrap();
        let stored_path = profile_path(&fixture, "Main", &saved);
        let before = fs::read(&stored_path).unwrap();
        let reopened = application.profiles(&workspace).unwrap();
        assert!(reopened.profiles.is_empty());
        let error = reopened.profiles_error.unwrap();
        assert_eq!(
            error.context["rejected"][0]["context"]["profile_id"],
            saved.id
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["path"],
            "$.amount"
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["value"],
            "-0.0"
        );
        assert_eq!(
            application
                .rename_profile(&workspace, &saved.id, "Unsigned")
                .unwrap_err()
                .category,
            "ProfileRejected"
        );
        assert_eq!(
            application
                .validate(&workspace, json!({"amount":-0.0}))
                .unwrap_err()
                .category,
            "NumericPrecision",
        );
        let schema_path = path.join("schema.json");
        let mut schema = inventory.schema;
        schema["properties"]["amount"]["default"] = json!(-0.0);
        fs::write(&schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
        let source_before = fs::read(&schema_path).unwrap();
        let error = application
            .inspect(&path, &workspace)
            .err()
            .expect("signed-zero default must be refused");
        assert_eq!(error.category, "NumericPrecision");
        assert_eq!(error.context["path"], "$.schema.properties.amount.default");
        assert_eq!(error.context["value"], "-0.0");
        assert_eq!(fs::read(schema_path).unwrap(), source_before);
        assert_eq!(fs::read(stored_path).unwrap(), before);
    }

    #[test]
    fn same_id_source_replacement_checks_profiles_before_durable_binding() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let original = inspect_named(application, "Main", &package_path()).unwrap();
        let workspace = workspace_ref(&original);
        let saved = application
            .save_profile(
                &workspace,
                None,
                "Retained profile",
                json!({"priorities":["ocr"]}),
            )
            .unwrap();
        let tab_path = fixture.root.join("tabs/Main/tab.config");
        let profile_path = profile_path(&fixture, "Main", &saved);
        let original_tab = fs::read(&tab_path).unwrap();
        let original_profile = fs::read(&profile_path).unwrap();
        let error = application
            .inspect(&fixture.numeric_package(), &workspace)
            .unwrap_err();
        assert_eq!(error.category, "PackageCompatibility");
        assert_eq!(fs::read(&tab_path).unwrap(), original_tab);
        assert_eq!(fs::read(&profile_path).unwrap(), original_profile);
        let current = application.workspace_catalog().unwrap().open.remove(0);
        assert_eq!(view_ref(&current), workspace);
        assert_eq!(
            current.selection.unwrap().package_path,
            original.package_path
        );
        let relocated = fixture.package_at("compatible-relocation");
        let replacement = application.inspect(&relocated, &workspace).unwrap();
        assert_eq!(replacement.workspace_id, workspace.workspace_id);
        assert_eq!(replacement.revision, workspace.revision + 1);
        assert_eq!(replacement.profiles[0].id, saved.id);
        assert_eq!(replacement.profiles[0].values, saved.values);
        assert_eq!(fs::read(profile_path).unwrap(), original_profile);
        let tab = lock(&application.store).tab("Main").unwrap();
        assert_eq!(tab.packages.len(), 1);
        assert_eq!(
            tab.packages[0].source,
            PackageSource::Directory {
                path: relocated
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            }
        );
    }

    #[test]
    fn same_source_tabs_keep_independent_profiles_and_fresh_reopened_sessions() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let first = inspect_named(application, "Main", &package_path()).unwrap();
        let second = inspect_named(application, "Other", &package_path().join(".")).unwrap();
        let original = workspace_ref(&first);
        let other = workspace_ref(&second);
        assert_ne!(original.workspace_id, other.workspace_id);
        let saved = application
            .save_profile(&original, None, "First", json!({"priorities":["ocr"]}))
            .unwrap();
        assert!(application.profiles(&other).unwrap().profiles.is_empty());
        assert_eq!(
            application
                .rename_profile(&other, &saved.id, "Foreign")
                .unwrap_err()
                .category,
            "ProfileNotFound"
        );
        assert_eq!(
            application
                .delete_profile(&other, &saved.id)
                .unwrap_err()
                .category,
            "ProfileNotFound"
        );
        let mut foreign_request = request(&second);
        foreign_request.profile_id = saved.id.clone();
        foreign_request.values = saved.values.clone();
        application.start(&other, foreign_request).unwrap();
        assert_eq!(
            settled(application).error.unwrap().category,
            "ProfileNotFound"
        );
        let independent = application
            .save_profile(&other, None, "Second", json!({"priorities":["template"]}))
            .unwrap();
        application
            .rename_profile(&original, &saved.id, "Renamed")
            .unwrap();
        assert_eq!(
            application.profiles(&other).unwrap().profiles[0].name,
            independent.name
        );
        assert_eq!(
            application.profiles(&other).unwrap().profiles[0].values["priorities"],
            json!(["template"])
        );
        let stored_path = profile_path(&fixture, "Main", &saved);
        let before = fs::read(&stored_path).unwrap();
        fs::write(&stored_path, b"malformed owner profile").unwrap();
        assert!(application.profiles(&original).is_err());
        assert_eq!(
            application.profiles(&other).unwrap().profiles[0].id,
            independent.id
        );
        assert_eq!(fs::read(&stored_path).unwrap(), b"malformed owner profile");
        fs::write(stored_path, before).unwrap();
        application.close_workspace(&original).unwrap();
        let reopened = application.reopen_workspace("Main").unwrap();
        assert_ne!(reopened.workspace_id, original.workspace_id);
        assert_eq!(
            application
                .validate(&original, json!({}))
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(reopened.selection.unwrap().profiles[0].id, saved.id);
        for name in ["A", "B", "C", "D", "E", "F"] {
            application.create_workspace(name, name).unwrap();
        }
        assert_eq!(
            application
                .create_workspace("Ninth", "Ninth")
                .unwrap_err()
                .category,
            "WorkspaceLimit"
        );
        application.close_workspace(&other).unwrap();
        let last = application.create_workspace("Ninth", "Ninth").unwrap();
        assert!(last.selection.is_none());
        assert_eq!(
            application.reopen_workspace("Other").unwrap_err().category,
            "WorkspaceLimit"
        );
        assert!(!lock(&application.store).tab("Other").unwrap().open);
    }

    #[test]
    fn stale_revisions_and_foreign_profiles_cannot_mutate_saved_values() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let first = inspect_named(application, "Main", &package_path()).unwrap();
        let original = workspace_ref(&first);
        let second = inspect_named(application, "Other", &fixture.numeric_package()).unwrap();
        let foreign = workspace_ref(&second);
        let values = first.package.profiles["template-first"]["options"].clone();
        let profile = application
            .save_profile(&original, None, "Original", values.clone())
            .unwrap();
        let path = profile_path(&fixture, "Main", &profile);
        let before = fs::read(&path).unwrap();
        assert_eq!(
            application
                .save_profile(&foreign, Some(&profile.id), "Foreign", values.clone())
                .unwrap_err()
                .category,
            "ProfileNotFound"
        );
        assert_eq!(
            application
                .rename_profile(&foreign, &profile.id, "Foreign")
                .unwrap_err()
                .category,
            "ProfileNotFound"
        );
        assert_eq!(
            application
                .delete_profile(&foreign, &profile.id)
                .unwrap_err()
                .category,
            "ProfileNotFound"
        );
        let replacement = application.inspect(&package_path(), &original).unwrap();
        let current = workspace_ref(&replacement);
        assert_eq!(current.revision, original.revision + 1);
        assert_eq!(
            application
                .validate(&original, values.clone())
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(
            application.profiles(&original).unwrap_err().category,
            "StaleIdentity"
        );
        assert_eq!(
            application
                .save_profile(&original, Some(&profile.id), "Stale", values)
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(
            application
                .rename_profile(&original, &profile.id, "Stale")
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(
            application
                .delete_profile(&original, &profile.id)
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(
            application
                .start(&original, request(&first))
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(
            application
                .check_environment(Some(&original), None)
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        application.delete_profile(&current, &profile.id).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn busy_owner_refuses_close_reinspection_and_competitors_without_blocking_stop() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let first = inspect_named(application, "Main", &package_path()).unwrap();
        let origin = workspace_ref(&first);
        let other = inspect_named(application, "Other", &fixture.numeric_package()).unwrap();
        let other_ref = workspace_ref(&other);
        let store = lock(&application.store);
        let starting = application.clone();
        let starting_ref = origin.clone();
        let start_request = request(&first);
        let starter = std::thread::spawn(move || starting.start(&starting_ref, start_request));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let preparing = loop {
            let poll = application.poll();
            if poll.controller["state"] == "preparing" {
                break poll;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        let run = preparing.controller["run"].as_str().unwrap().to_owned();
        let damaged_settings = b"settings became invalid after operation admission";
        fs::write(fixture.root.join("settings.json"), damaged_settings).unwrap();
        let close = application.close_workspace(&origin);
        let inspect = application.inspect(&package_path(), &origin);
        let competing_start = application.start(&other_ref, request(&other));
        let competing_check = application.check_environment(Some(&other_ref), None);
        let reconstruct = application.prepare_reconstruction();
        assert!(!application.closing.load(Ordering::Acquire));
        let capturing = application.clone();
        let (entered, entry) = mpsc::sync_channel(1);
        let snapshot = std::thread::spawn(move || {
            entered.send(()).unwrap();
            capturing.capture_configuration()
        });
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        let stopped = application.stop(&run);
        let stopped_state = application.poll();
        drop(store);
        starter.join().unwrap().unwrap();
        let captured = snapshot.join().unwrap().unwrap();
        assert_eq!(captured.files["settings.json"], damaged_settings);
        assert!(application.settings().is_err());
        assert_eq!(reconstruct.unwrap_err().category, "RunActive");
        assert_eq!(close.unwrap_err().category, "RunActive");
        assert_eq!(inspect.unwrap_err().category, "RunActive");
        assert_eq!(competing_start.unwrap_err().category, "RunActive");
        assert_eq!(competing_check.unwrap_err().category, "RunActive");
        stopped.unwrap();
        assert_eq!(stopped_state.controller["state"], "stopping");
        assert_eq!(
            stopped_state.controller["workspace_id"],
            origin.workspace_id
        );
        let terminal = settled(application);
        assert_eq!(terminal.error.unwrap().category, "Cancelled");
        application.close_workspace(&origin).unwrap();
        let closed = application.poll();
        assert_eq!(closed.controller["workspace_id"], origin.workspace_id);
        assert!(
            closed
                .workspace_results
                .iter()
                .all(|value| value.workspace != origin)
        );
    }

    #[test]
    fn successor_retains_unpolled_terminal_and_independent_check_association() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        // Disable only automatic collection, not the real controller or workers.
        application.closing.store(true, Ordering::Release);
        lock(&application.bridge).take().unwrap().join().unwrap();
        application.closing.store(false, Ordering::Release);
        let first = inspect_named(application, "Main", &package_path()).unwrap();
        let origin = workspace_ref(&first);
        let second = inspect_named(application, "Other", &fixture.numeric_package()).unwrap();
        let next = workspace_ref(&second);
        let environment = OcrEnvironment {
            model: mado_runtime_comparison::environment::G004_PROFILE.into(),
            profile: mado_runtime_comparison::environment::G004_PROFILE.into(),
            language: mado_runtime_comparison::environment::LANGUAGE.into(),
            provider: mado_runtime_comparison::environment::PROVIDER.into(),
            runtime_profile: mado_runtime_comparison::environment::RUNTIME_PROFILE.into(),
            model_root: fixture
                .root
                .join("absent-model")
                .to_string_lossy()
                .into_owned(),
            runtime_path: fixture
                .root
                .join("absent-runtime")
                .to_string_lossy()
                .into_owned(),
            native_library_paths: vec![
                fixture
                    .root
                    .join("absent-library")
                    .to_string_lossy()
                    .into_owned(),
            ],
        };
        let settings = application.settings().unwrap();
        application
            .save_settings(EditableSettings {
                locale: settings.locale,
                backup_directory: settings.backup_directory,
                gui_log_limit: settings.gui_log_limit,
                ocr_environment: Some(environment.clone()),
                notifications: settings.notifications,
            })
            .unwrap();
        let descriptor = "private-recorded-corpus.json".to_owned();
        let store = lock(&application.store);
        let checking = application.clone();
        let check_origin = origin.clone();
        let check_descriptor = descriptor.clone();
        let checker = std::thread::spawn(move || {
            checking.check_environment(Some(&check_origin), Some(check_descriptor))
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while application.poll().controller["state"] != "preparing" {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        drop(store);
        let checked_run = checker.join().unwrap().unwrap();
        let check_terminal = settled(application);
        assert_eq!(check_terminal.run.as_deref(), Some(checked_run.as_str()));
        let settings = application.settings().unwrap();
        application
            .save_settings(EditableSettings {
                locale: settings.locale,
                backup_directory: settings.backup_directory,
                gui_log_limit: settings.gui_log_limit,
                ocr_environment: None,
                notifications: settings.notifications,
            })
            .unwrap();
        let mut next_request = request(&second);
        next_request.package_path = fixture
            .root
            .join("foreign-missing-package")
            .to_string_lossy()
            .into_owned();
        let next_run = application.start(&next, next_request).unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(next_run.as_str()));
        assert_eq!(terminal.error.unwrap().category, "ChildStartup");
        let poll = application.poll();
        let retained = poll
            .workspace_results
            .iter()
            .find(|result| result.workspace == origin)
            .unwrap();
        assert_eq!(retained.controller["run"], checked_run);
        assert_eq!(retained.controller["state"], "terminal");
        let check = poll.last_check.unwrap();
        assert_eq!(check.workspace.as_ref(), Some(&origin));
        assert_eq!(check.environment.as_ref(), Some(&environment));
        assert_eq!(check.descriptor_path.as_deref(), Some(descriptor.as_str()));
        assert_eq!(
            check.package_inventory_identity.as_deref(),
            Some(first.package.inventory_identity.as_str())
        );
        assert_eq!(check.controller["workspace_revision"], origin.revision);
        assert_eq!(poll.controller["workspace_id"], next.workspace_id);
        application.close_workspace(&origin).unwrap();
        let closed = application.poll();
        assert_eq!(closed.last_check.unwrap().controller["run"], checked_run);
        assert!(
            closed
                .workspace_results
                .iter()
                .all(|result| result.workspace != origin)
        );
        let terminal_events: Vec<_> = poll
            .logs
            .entries
            .iter()
            .filter(|entry| entry.code == "run.terminal")
            .collect();
        assert_eq!(terminal_events.len(), 2);
        assert!(terminal_events.iter().all(|entry| entry.level == "error"));
        assert!(
            terminal_events
                .iter()
                .any(
                    |entry| entry.workspace_id.as_deref() == Some(origin.workspace_id.as_str())
                        && entry.run.as_deref() == Some(checked_run.as_str())
                )
        );
        assert!(
            terminal_events
                .iter()
                .any(
                    |entry| entry.workspace_id.as_deref() == Some(next.workspace_id.as_str())
                        && entry.run.as_deref() == Some(next_run.as_str())
                )
        );
        assert!(
            terminal_events
                .iter()
                .all(|entry| !entry.message.contains(&descriptor))
        );
        assert!(
            application
                .poll()
                .logs
                .entries
                .iter()
                .all(|entry| entry.code != "run.terminal")
        );
    }

    #[test]
    fn close_refuses_inflight_profile_save_without_deleting_saved_data() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let selection = inspect_named(application, "Main", &package_path()).unwrap();
        let workspace = workspace_ref(&selection);
        let values = selection.package.profiles["template-first"]["options"].clone();
        let store = lock(&application.store);
        // Observing admission must not contend for the nonblocking command mutex.
        application.command_admitted.store(false, Ordering::Release);
        let saving = application.clone();
        let save_workspace = workspace.clone();
        let saver = std::thread::spawn(move || {
            saving.save_profile(&save_workspace, None, "Keep on close", values)
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !application.command_admitted.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let close = application.close_workspace(&workspace);
        drop(store);
        let profile = saver.join().unwrap().unwrap();
        assert_eq!(close.unwrap_err().category, "WorkspaceBusy");
        application.close_workspace(&workspace).unwrap();
        let reopened = application
            .reopen_workspace("Main")
            .unwrap()
            .selection
            .unwrap();
        assert_ne!(reopened.workspace_id, workspace.workspace_id);
        assert_eq!(reopened.profiles[0].id, profile.id);
        assert_eq!(reopened.profiles[0].values, profile.values);
    }

    #[test]
    fn terminal_outcomes_keep_severity_and_distinguish_cleanup_without_private_detail() {
        let passed = json!({"operation":"run","error":null,"result":{
            "status":"PASS","primary":null,"entry_outcome":"Returned",
            "cleanup":{"clean":true,"status":"CleanupFinished"},"forced":false,"exit_code":0}});
        let outcome = TerminalOutcome::from_view(&passed);
        assert_eq!(outcome.notice().0, "info");
        assert_eq!(
            outcome.fields(&passed["operation"]),
            json!({"action":"run","state":"terminal","status":"PASS","category":null,
                "entry_outcome":"Returned","cleanup_clean":true,"child_started":null,"forced":false})
        );

        // Script failure with clean cleanup: an error, but not an incomplete cleanup.
        let clean_failure = json!({"operation":"run","error":null,"result":{
            "status":"FAIL","entry_outcome":"Returned","forced":false,"exit_code":0,
            "primary":{"category":"Script","message":"recognized private words","context":{"path":"/private/root"}},
            "cleanup":{"clean":true,"status":"CleanupFinished"}}});
        let outcome = TerminalOutcome::from_view(&clean_failure);
        assert_eq!(outcome.notice().0, "error");
        let fields = outcome.fields(&clean_failure["operation"]);
        assert_eq!(fields["status"], "FAIL");
        assert_eq!(fields["category"], "Script");
        assert_eq!(fields["cleanup_clean"], true);
        assert_eq!(fields["forced"], false);
        assert!(!fields.to_string().contains("private"));

        // Forced containment with a returned entry: distinct from the clean failure above.
        let forced = json!({"operation":"run","error":null,"result":{
            "status":"FAIL","primary":null,"entry_outcome":"Returned","forced":true,"exit_code":124,
            "cleanup":{"clean":false,"status":"IncompleteCleanup","outcome":"ForcedOrIncomplete"}}});
        let outcome = TerminalOutcome::from_view(&forced);
        assert_eq!(outcome.notice().0, "error");
        let fields = outcome.fields(&forced["operation"]);
        assert_eq!(fields["category"], Value::Null);
        assert_eq!(fields["entry_outcome"], "Returned");
        assert_eq!(fields["cleanup_clean"], false);
        assert_eq!(fields["forced"], true);

        // Settled without success but with clean cleanup and no primary stays a warning.
        let unsuccessful = json!({"operation":"run","error":null,"result":{
            "status":"FAIL","primary":null,"entry_outcome":"Returned","forced":false,"exit_code":3,
            "cleanup":{"clean":true}}});
        assert_eq!(TerminalOutcome::from_view(&unsuccessful).notice().0, "warn");

        // Pre-child fault: the worker attests clean cleanup and no child; no status exists.
        let pre_child = json!({"operation":"environment_check","result":null,"error":{
            "category":"EnvironmentUnset","message":"Save an OCR environment","context":{
                "stage":"environment_validation","cleanup":{"clean":true,"child_started":false}}}});
        let outcome = TerminalOutcome::from_view(&pre_child);
        assert_eq!(outcome.notice().0, "error");
        assert_eq!(
            outcome.fields(&pre_child["operation"]),
            json!({"action":"environment_check","state":"terminal","status":null,
                "category":"EnvironmentUnset","entry_outcome":null,"cleanup_clean":true,
                "child_started":false,"forced":null})
        );

        // A started child without cleanup evidence stays unverified, never clean or false.
        let unattested = json!({"operation":"run","result":null,"error":{
            "category":"ChildStartup","message":"child exited before startup record",
            "context":{"child_started":true,"forced":false,"exit_code":1}}});
        let outcome = TerminalOutcome::from_view(&unattested);
        assert_eq!(outcome.notice().0, "error");
        let fields = outcome.fields(&unattested["operation"]);
        assert_eq!(fields["cleanup_clean"], Value::Null);
        assert_eq!(fields["child_started"], true);
        assert_eq!(fields["forced"], false);
    }

    #[test]
    fn shutdown_during_preparation_persists_a_distinguishable_terminal_record() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let selection = inspect_named(application, "Main", &package_path()).unwrap();
        let workspace = workspace_ref(&selection);
        let store = lock(&application.store);
        let starting = application.clone();
        let starting_ref = workspace.clone();
        let start_request = request(&selection);
        let starter = std::thread::spawn(move || starting.start(&starting_ref, start_request));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let run = loop {
            let poll = application.poll();
            if poll.controller["state"] == "preparing" {
                break poll.controller["run"].as_str().unwrap().to_owned();
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        let closing = application.clone();
        let shutdown = std::thread::spawn(move || closing.shutdown());
        // Release the blocked worker only once shutdown has cancelled it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while application.runner.poll().state != "stopping" {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        drop(store);
        starter.join().unwrap().unwrap();
        shutdown.join().unwrap().unwrap();
        let status = application.logger.status();
        assert_eq!(status.file_errors, 0);
        assert_eq!(status.file_pending, 0);
        let persisted =
            fs::read_to_string(fixture.root.join("logs").join("application.jsonl")).unwrap();
        let terminal: Vec<crate::logging::LogEntry> = persisted
            .lines()
            .map(|line| serde_json::from_str::<crate::logging::LogEntry>(line).unwrap())
            .filter(|entry| entry.code == "run.terminal")
            .collect();
        assert_eq!(terminal.len(), 1);
        let record = &terminal[0];
        assert_eq!(record.run.as_deref(), Some(run.as_str()));
        assert_eq!(
            record.workspace_id.as_deref(),
            Some(workspace.workspace_id.as_str())
        );
        assert_eq!(record.level, "error");
        assert_eq!(record.fields["action"], "run");
        assert_eq!(record.fields["category"], "Cancelled");
        assert_eq!(record.fields["cleanup_clean"], true);
        assert_eq!(record.fields["child_started"], false);
        assert_eq!(record.fields["status"], Value::Null);
        assert_eq!(record.fields["forced"], Value::Null);
        assert!(!persisted.contains(&selection.package_path));
    }
}
