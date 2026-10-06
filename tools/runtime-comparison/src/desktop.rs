//! One application-owned operation, with independent lifecycle and log queues.

mod native;
mod operation;
mod packages;
mod recognition;

#[cfg(test)]
mod test_support;

use crate::inventory::TargetDeclaration;
use crate::model::{Control, Fault, Plan};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;

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
    pub operation: String,
    pub visible_postcondition: String,
    pub limits: NativeLimits,
}

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
    pub fn budgets(&self) -> crate::model::NativeBudgets {
        crate::model::NativeBudgets {
            startup_ms: self.startup_ms,
            readiness_ms: self.readiness_ms,
            workflow_ms: self.workflow_ms,
        }
    }
}

/// Per-Start defaults and ceilings; containment is the fixed supervisor bound.
pub fn native_limits() -> NativeLimits {
    NativeLimits {
        startup_ms: 60_000,
        readiness_ms: 30_000,
        workflow_ms: 30_000,
        max_frames: 300,
        wait_ms: 1_000,
        interval_ms: 100,
        max_actions: 64,
        cleanup_ms: 1_000,
        containment_ms: 2_000,
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

/// Fresh application correspondence, never accepted from public IPC.
#[derive(Debug)]
pub struct NativeTarget {
    pub executable: PathBuf,
    pub process_id: u32,
    pub process_lifetime: String,
    pub window_title: String,
    pub input: NativeInputPolicy,
}

#[derive(Debug, Default)]
pub struct StartPreparation<P = ()> {
    pub environment: Option<crate::environment::OcrEnvironment>,
    pub native: P,
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
    Workflow,
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
    pub logs: Vec<Value>,
    pub dropped_logs: u64,
}

struct Active {
    control: Arc<Control>,
    worker: JoinHandle<Result<Value, Fault>>,
    progress: mpsc::Receiver<Value>,
    logs: mpsc::Receiver<Value>,
    dropped_logs: Arc<AtomicU64>,
}

struct State {
    closed: bool,
    run: Option<String>,
    operation: &'static str,
    phase: &'static str,
    active: Option<Active>,
    result: Option<Value>,
    error: Option<Fault>,
    native_preparation: Option<NativeProgress>,
    progress: Vec<Value>,
    logs: VecDeque<Value>,
    seen_logs: BTreeSet<u64>,
    dropped_logs: u64,
}

impl State {
    fn new() -> Self {
        Self {
            closed: false,
            run: None,
            operation: "run",
            phase: "idle",
            active: None,
            result: None,
            error: None,
            native_preparation: None,
            progress: Vec::new(),
            logs: VecDeque::new(),
            seen_logs: BTreeSet::new(),
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
        controller.start(native, None).unwrap();
        let terminal = settled(&controller);
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "NativeRefused");
        assert_eq!(
            fault.context["cleanup"],
            json!({"clean":true,"child_started":false})
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
