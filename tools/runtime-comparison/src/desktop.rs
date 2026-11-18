//! One application-owned operation, with independent lifecycle and log queues.

mod lifecycle;
mod native;
mod operation;
mod packages;
mod recognition;

#[cfg(test)]
mod test_support;

use crate::inventory::{Inventory, TargetDeclaration};
use crate::model::{Control, Fault, NativeBudgets, NativeWorkflow, Plan};
use lifecycle::Lifecycle;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;

pub use crate::typescript::FunctionInvocation;

const LOG_CAPACITY: usize = 64;
const PROGRESS_CAPACITY: usize = 32;
const REQUEST_BYTES: usize = 65_536;
const SHUTDOWN_MS: u64 = 14_000;
const REPLAY_DURATION_MS: u64 = 30_000;
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub package_path: String,
    pub inventory_identity: String,
    pub package_id: String,
    pub schema_identity: String,
    pub profile_id: String,
    /// Raw options, not the package fixture's profile envelope.
    pub values: Value,
    pub lane: String,
    #[serde(default = "workflow")]
    pub scenario: String,
    #[serde(default)]
    pub replay_descriptor_path: Option<String>,
    #[serde(default)]
    pub native_intent: Option<NativeIntent>,
    #[serde(default)]
    pub function: Option<FunctionInvocation>,
}

fn workflow() -> String {
    "workflow".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIntent {
    pub target_revision: u64,
    pub target_binding_id: String,
    pub target_declaration_identity: String,
    pub capture_approved: bool,
    pub input_approved: bool,
    #[serde(default)]
    pub launch_approved: bool,
    #[serde(default)]
    pub max_exit_recoveries: u8,
    pub review: NativeReview,
    pub limits: NativeRunLimits,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeReview {
    Operation {
        operation: String,
        visible_postcondition: String,
    },
    // A unit variant would silently discard unknown fields during deserialization.
    Script {},
}

/// Finite stage/lifetime limits, the default reviewed lifecycle.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLimits {
    pub startup_ms: u64,
    pub readiness_ms: u64,
    pub workflow_ms: u64,
    pub max_frames: u64,
    pub wait_ms: u64,
    pub interval_ms: u64,
    pub max_actions: usize,
    pub cleanup_ms: u64,
    pub containment_ms: u64,
}

impl NativeLimits {
    pub fn budgets(&self) -> NativeBudgets {
        NativeBudgets {
            startup_ms: self.startup_ms,
            readiness_ms: self.readiness_ms,
            workflow: NativeWorkflow::Finite {
                workflow_ms: self.workflow_ms,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuousLifecycle {
    Continuous,
}

/// Ordinary continuous Workflow: finite Startup/Readiness, bounded per-call waits
/// and expanded-event pace, no Workflow lifetime, optional overall deadline.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousLimits {
    pub lifecycle: ContinuousLifecycle,
    pub startup_ms: u64,
    pub readiness_ms: u64,
    pub wait_ms: u64,
    pub interval_ms: u64,
    pub cleanup_ms: u64,
    pub containment_ms: u64,
    pub max_batch_events: usize,
    pub event_interval_ms: u64,
    /// Absent means no overall deadline; never a sentinel value.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub overall_ms: Option<u64>,
}

fn present<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(input).map(Some)
}

impl ContinuousLimits {
    pub fn budgets(&self) -> NativeBudgets {
        NativeBudgets {
            startup_ms: self.startup_ms,
            readiness_ms: self.readiness_ms,
            workflow: NativeWorkflow::Continuous {
                overall_ms: self.overall_ms,
            },
        }
    }
}

/// The finite object is unchanged; only `lifecycle: "continuous"` selects the other.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NativeRunLimits {
    Continuous(ContinuousLimits),
    Finite(NativeLimits),
}

impl NativeRunLimits {
    pub fn budgets(&self) -> NativeBudgets {
        match self {
            Self::Continuous(limits) => limits.budgets(),
            Self::Finite(limits) => limits.budgets(),
        }
    }
}

impl From<NativeLimits> for NativeRunLimits {
    fn from(limits: NativeLimits) -> Self {
        Self::Finite(limits)
    }
}

impl From<ContinuousLimits> for NativeRunLimits {
    fn from(limits: ContinuousLimits) -> Self {
        Self::Continuous(limits)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCapability {
    pub default_limits: NativeLimits,
    pub max_workflow_ms: u64,
    pub continuous_limits: ContinuousLimits,
}

pub fn native_capability() -> NativeCapability {
    NativeCapability {
        default_limits: native_limits(),
        max_workflow_ms: NativeBudgets::CEILINGS
            .workflow_ms()
            .expect("finite workflow ceiling"),
        continuous_limits: continuous_limits(),
    }
}

/// Per-Start defaults; containment is the fixed supervisor bound.
pub fn native_limits() -> NativeLimits {
    NativeLimits {
        startup_ms: NativeBudgets::DEFAULT.startup_ms,
        readiness_ms: NativeBudgets::DEFAULT.readiness_ms,
        workflow_ms: NativeBudgets::DEFAULT
            .workflow_ms()
            .expect("finite workflow default"),
        max_frames: 300,
        wait_ms: 1_000,
        interval_ms: 100,
        max_actions: 64,
        cleanup_ms: 1_000,
        containment_ms: 2_000,
    }
}

/// Continuous template and ceilings; `overall_ms` is chosen per Start.
pub fn continuous_limits() -> ContinuousLimits {
    let finite = native_limits();
    ContinuousLimits {
        lifecycle: ContinuousLifecycle::Continuous,
        startup_ms: finite.startup_ms,
        readiness_ms: finite.readiness_ms,
        wait_ms: finite.wait_ms,
        interval_ms: finite.interval_ms,
        cleanup_ms: finite.cleanup_ms,
        containment_ms: finite.containment_ms,
        max_batch_events: finite.max_actions,
        event_interval_ms: 1_000,
        overall_ms: None,
    }
}

/// Saved binding policy, constructed only by the application host.
#[derive(Debug)]
pub struct NativeInputPolicy {
    pub route: String,
    pub focus: String,
    pub pointer_mode: Option<String>,
    pub click_hold_ms: u64,
}

/// Host-selected target policy and fresh process correspondence.
#[derive(Debug)]
pub struct NativeTarget {
    pub executable: PathBuf,
    /// Windows requires PID/creation FILETIME; only macOS Script may defer selection to the SDK.
    pub process: Option<(u32, String)>,
    pub window_title: String,
    pub input: NativeInputPolicy,
}

#[derive(Debug, Default)]
pub struct StartPreparation<P = ()> {
    pub environment: Option<crate::environment::OcrEnvironment>,
    pub native: P,
    /// A package the host captured and verified itself. When present, execution uses
    /// exactly this inventory and never rereads `package_path`.
    pub inventory: Option<Inventory>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePhase {
    Preflight,
    TargetDiscovery,
    LaunchSubmission,
    WaitingForProcess,
    WaitingForWindow,
    NativeInitialization,
    Readiness,
    HostAttachment,
    Instantiation,
    Function,
    Workflow,
    Settling,
    Recovering,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchDisposition {
    NotRequested,
    Accepted,
    Rejected,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTargetStatus {
    NotRequested,
    Pending,
    CaptureReady,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeProgress {
    pub attempt: u64,
    pub status: NativeTargetStatus,
    pub phase: NativePhase,
    pub launch: LaunchDisposition,
}

#[derive(Debug, Serialize)]
pub struct PackageInfo {
    pub package_id: String,
    pub inventory_identity: String,
    pub schema_identity: String,
    pub runtime: String,
    pub schema: Value,
    pub profiles: BTreeMap<String, Value>,
    pub effective_defaults: Option<Value>,
    pub target: Option<TargetDeclaration>,
    pub target_identity: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControllerView {
    pub run: Option<String>,
    pub operation: String,
    pub state: String,
    pub result: Option<Value>,
    pub error: Option<Fault>,
    pub progress: Vec<Value>,
    pub native_preparation: Option<NativeProgress>,
    pub attempts: Vec<Value>,
    pub logs: Vec<Value>,
    pub dropped_logs: u64,
}

struct Active {
    control: Arc<Control>,
    worker: JoinHandle<Result<Value, Fault>>,
    progress: mpsc::Receiver<Value>,
    logs: mpsc::Receiver<Value>,
    dropped_logs: Arc<AtomicU64>,
    attempts: Arc<Mutex<Vec<Value>>>,
    native_preparation: Arc<Mutex<Option<NativeProgress>>>,
}

struct State {
    lifecycle: Lifecycle,
    run: Option<String>,
    operation: &'static str,
    active: Option<Active>,
    result: Option<Value>,
    error: Option<Fault>,
    native_preparation: Option<NativeProgress>,
    progress: Vec<Value>,
    logs: VecDeque<Value>,
    attempts: Vec<Value>,
    seen_logs: BTreeSet<(u64, u64)>,
    dropped_logs: u64,
}

impl State {
    fn new() -> Self {
        Self {
            lifecycle: Lifecycle::new(),
            run: None,
            operation: "run",
            active: None,
            result: None,
            error: None,
            native_preparation: None,
            progress: Vec::new(),
            logs: VecDeque::new(),
            seen_logs: BTreeSet::new(),
            attempts: Vec::new(),
            dropped_logs: 0,
        }
    }
}

pub struct DesktopController {
    controlled_path: PathBuf,
    engine_path: PathBuf,
    state: Mutex<State>,
}

impl DesktopController {
    pub fn new(controlled_path: PathBuf, engine_path: PathBuf) -> Self {
        Self {
            controlled_path,
            engine_path,
            state: Mutex::new(State::new()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl Drop for DesktopController {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn manual_plan() -> Result<Plan, Fault> {
    let mut plan: Plan = serde_json::from_str(include_str!("../fixtures/manual-plan.json"))
        .map_err(|error| Fault::new("InvalidPlan", error.to_string()))?;
    plan.limits.snapshot_bytes = crate::images::PACKAGE_BYTES;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::test_support::{fixture, request, settled};
    use super::*;
    use serde_json::json;

    #[test]
    fn ipc_cannot_select_native_authority_or_runner_artifacts() {
        let request = request(&fixture());
        for field in ["native_config", "environment", "executable", "limits"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = json!({});
            assert!(
                serde_json::from_value::<StartRequest>(value).is_err(),
                "{field}"
            );
        }
        let controller =
            DesktopController::new("missing-controlled".into(), "missing-engine".into());
        let mut native = request.clone();
        native.lane = "native".into();
        assert_eq!(
            controller.start(native, None).unwrap_err().category,
            "NativeRefused"
        );
        let mut replay = request;
        replay.lane = "replay".into();
        replay.scenario = "partial".into();
        controller.start(replay, None).unwrap();
        let fault = settled(&controller).error.unwrap();
        assert_eq!(fault.category, "InvalidPlan");
        assert_eq!(fault.context["cleanup"]["child_started"], false);
    }
}
