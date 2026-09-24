use crate::configuration::{self, Capture};
use crate::logging::{LogBatch, Logger};
use crate::storage::{EditableSettings, PackageReference, Profile, Settings, Store, TabRecord};
use crate::target::{TargetCheck, TargetRecord};
use mado_runtime_comparison::desktop::{DesktopController, PackageInfo};
use mado_runtime_comparison::environment::OcrEnvironment;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, OnceLock, TryLockError,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod operations;
mod profiles;
mod targets;
mod workspaces;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

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

/// `saved_package` is the Tab's currently selected durable reference exactly as stored,
/// present whether or not inspection succeeded; only `selection` conveys inspected authority.
#[derive(Debug, Serialize)]
pub struct WorkspaceView {
    pub workspace_id: String,
    pub revision: u64,
    pub internal_name: String,
    pub display_name: String,
    pub saved_package: Option<PackageReference>,
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

#[derive(Debug, Serialize)]
pub struct TargetContext {
    pub workspace: WorkspaceRef,
    pub internal_name: String,
    pub package_id: String,
    pub declaration_identity: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TargetView {
    pub context: TargetContext,
    pub record: TargetRecord,
    pub compatible: bool,
}

#[derive(Debug, Serialize)]
pub struct TargetCheckResponse {
    pub context: TargetContext,
    pub revision: u64,
    pub binding_id: Option<String>,
    pub check: TargetCheck,
}

#[derive(Debug, Serialize)]
pub struct TargetSaveResponse {
    pub view: TargetView,
    pub check: TargetCheck,
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
    saved_package: Option<PackageReference>,
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
            Err(error) => {
                let (level, code, message) =
                    if action == "save_target" && error.category == "TargetResolutionChanged" {
                        (
                            "warn",
                            "target.review_required",
                            "Review changed target resolution before saving",
                        )
                    } else {
                        ("error", "command.failed", "Command failed")
                    };
                self.logger.emit(
                    "Rust",
                    level,
                    None,
                    workspace.map(|value| value.workspace_id.as_str()),
                    code,
                    message,
                    json!({"action":action,"category":error.category}),
                );
            }
        }
        result
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
}
