//! One application-owned operation, with independent lifecycle and log queues.

use crate::environment::{OcrEnvironment, capture_environment, capture_replay};
use crate::host::resolve_options;
use crate::inventory::{Inventory, TargetDeclaration};
use crate::model::{Control, Fault, Limits, Plan, encode_bounded, identity};
use crate::runner::{Observer, run_environment_check_with_executable, run_once_with_executable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOG_CAPACITY: usize = 64;
const PROGRESS_CAPACITY: usize = 32;
const REQUEST_BYTES: usize = 65_536;
const SHUTDOWN_MS: u64 = 14_000;
const REPLAY_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
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
}

fn workflow() -> String {
    "workflow".into()
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
            progress: Vec::new(),
            logs: VecDeque::new(),
            seen_logs: BTreeSet::new(),
            dropped_logs: 0,
        }
    }

    fn progress(&mut self, mut value: Value) {
        let Some(run) = &self.run else { return };
        value["child_run"] = value["run"].clone();
        value["run"] = json!(run);
        value["operation"] = json!(self.operation);
        if value["event"] == "ChildStarted" && self.phase == "preparing" {
            self.phase = "running";
        }
        if self.progress.len() < PROGRESS_CAPACITY {
            self.progress.push(value);
        }
    }

    fn log(&mut self, mut value: Value) {
        let Some(sequence) = value["sequence"].as_u64() else {
            return;
        };
        if !self.seen_logs.insert(sequence) {
            return;
        }
        value["child_run"] = value["run"].clone();
        value["run"] = json!(self.run);
        if self.logs.len() == LOG_CAPACITY {
            self.logs.pop_front();
            self.dropped_logs = self.dropped_logs.saturating_add(1);
        }
        self.logs.push_back(value);
    }

    fn refresh(&mut self) {
        let Some(active) = &self.active else { return };
        let progress: Vec<_> = active.progress.try_iter().take(PROGRESS_CAPACITY).collect();
        let logs: Vec<_> = active.logs.try_iter().take(LOG_CAPACITY).collect();
        let dropped = active.dropped_logs.swap(0, Ordering::Relaxed);
        let finished = active.worker.is_finished();
        self.dropped_logs = self.dropped_logs.saturating_add(dropped);
        for value in progress {
            self.progress(value);
        }
        for value in logs {
            self.log(value);
        }
        if !finished {
            return;
        }
        let active = self.active.take().expect("finished worker remains owned");
        // Join only a finished worker. It returns after the supervisor reaps its child.
        let result = active.worker.join().unwrap_or_else(|_| {
            Err(Fault::new(
                "Controller",
                "operation worker panicked; inspect containment evidence",
            )
            .with_context(json!({
                "operation":self.operation,"app_run":self.run,"stage":"worker",
                "environment_identity":null,"corpus_identity":null,
                "cleanup":{"clean":false,"child_started":null}
            })))
        });
        // A final event may have arrived between the first drain and is_finished().
        for value in active.progress.try_iter() {
            self.progress(value);
        }
        for value in active.logs.try_iter() {
            self.log(value);
        }
        self.dropped_logs = self
            .dropped_logs
            .saturating_add(active.dropped_logs.load(Ordering::Relaxed));
        match result {
            Ok(value) => {
                // Retained Script records recover delivery races without double-importing.
                if let Some(logs) = value
                    .pointer("/observations/script_logs")
                    .and_then(Value::as_array)
                {
                    for log in logs {
                        let mut log = log.clone();
                        log["run"] = value["run"].clone();
                        self.log(log);
                    }
                }
                for field in ["dropped_logs", "script_logs_dropped"] {
                    self.dropped_logs = self
                        .dropped_logs
                        .saturating_add(value["observations"][field].as_u64().unwrap_or(0));
                }
                self.result = Some(value);
            }
            Err(error) => {
                // Workers bound diagnostics before adding bounded operation correlation.
                self.error = Some(error);
            }
        }
        self.phase = "terminal";
        if self.progress.len() < PROGRESS_CAPACITY {
            self.progress
                .push(json!({"event":"ControllerSettled","run":self.run,"worker_finished":true}));
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

    /// Capture and statically validate only; no package module is evaluated.
    pub fn inspect(&self, package: &Path) -> Result<PackageInfo, Fault> {
        if self.state().closed {
            return Err(Fault::new(
                "ControllerClosed",
                "controller is shutting down",
            ));
        }
        let limits = manual_plan()?.limits;
        let inventory = Inventory::capture(package, &limits)?;
        let runtime = runtime(&inventory)?;
        let inventory = match runtime.as_str() {
            "typescript" => {
                crate::typescript::compile(&inventory, &limits)?;
                inventory
            }
            "javascript" => inspect_javascript(inventory, &limits)?,
            _ => unreachable!("runtime() admits only QuickJS languages"),
        };
        let defaults = profile(&inventory.package_id, &inventory.schema, json!({}));
        let effective_defaults =
            resolve_options(&inventory.schema, &defaults, &inventory.package_id).ok();
        let target = inventory.target()?;
        let target_identity = target
            .as_ref()
            .map(TargetDeclaration::identity)
            .transpose()?;
        let info = PackageInfo {
            schema_identity: identity(&inventory.schema)?,
            package_id: inventory.package_id,
            inventory_identity: inventory.identity,
            runtime,
            schema: inventory.schema,
            profiles: inventory.profiles,
            effective_defaults,
            target,
            target_identity,
        };
        encode_bounded(&info, 2 * 1024 * 1024)?;
        Ok(info)
    }

    /// Reserve synchronously; package capture and execution never run on the UI thread.
    pub fn start(
        &self,
        request: StartRequest,
        environment: Option<OcrEnvironment>,
    ) -> Result<String, Fault> {
        self.start_with_preparation(request, move |_, _| Ok(environment))
    }

    /// Application-owned profile checks share the reservation with resource capture.
    pub fn start_with_preparation(
        &self,
        mut request: StartRequest,
        prepare: impl FnOnce(&mut StartRequest, &Control) -> Result<Option<OcrEnvironment>, Fault>
        + Send
        + 'static,
    ) -> Result<String, Fault> {
        self.reserve(
            "run",
            request.lane == "replay",
            move |app_run, control, observer, controlled, engine| {
                let mut evidence = Evidence::new(
                    "run",
                    app_run,
                    None,
                    request.replay_descriptor_path.as_deref(),
                )?;
                evidence.stage("request_validation", observer);
                let prepared = (|| {
                    control.check()?;
                    encode_bounded(&request, REQUEST_BYTES)?;
                    evidence.fields["package_inventory_identity"] =
                        json!(request.inventory_identity);
                    evidence.fields["schema_identity"] = json!(request.schema_identity);
                    evidence.fields["profile_id"] = json!(request.profile_id);
                    let plan = requested_plan(&request)?;
                    evidence.complete();
                    evidence.stage("input_capture", observer);
                    let environment = prepare(&mut request, control)?;
                    evidence.fields["selection_identity"] =
                        json!(environment.as_ref().map(identity).transpose()?);
                    control.check()?;
                    Ok((plan, environment))
                })();
                let (plan, environment) = prepared.map_err(|error| evidence.fault(error, true))?;
                evidence.complete();
                let executable = if request.lane == "replay" {
                    engine
                } else {
                    controlled
                };
                execute(
                    executable,
                    &request,
                    environment.as_ref(),
                    plan,
                    control,
                    observer,
                    evidence,
                )
            },
        )
    }

    pub fn check_environment(
        &self,
        environment: Option<OcrEnvironment>,
        package: Option<(PathBuf, String)>,
        replay_descriptor_path: Option<String>,
    ) -> Result<String, Fault> {
        self.check_environment_with_preparation(package, replay_descriptor_path, move |_| {
            Ok(environment)
        })
    }

    pub fn check_environment_with_preparation(
        &self,
        package: Option<(PathBuf, String)>,
        replay_descriptor_path: Option<String>,
        prepare: impl FnOnce(&Control) -> Result<Option<OcrEnvironment>, Fault> + Send + 'static,
    ) -> Result<String, Fault> {
        self.reserve(
            "environment_check",
            true,
            move |app_run, control, observer, _, engine| {
                let mut evidence = Evidence::new(
                    "environment_check",
                    app_run,
                    None,
                    replay_descriptor_path.as_deref(),
                )?;
                evidence.stage("input_capture", observer);
                let environment = control
                    .check()
                    .and_then(|()| prepare(control))
                    .map_err(|error| evidence.fault(error, true))?;
                evidence.fields["selection_identity"] =
                    json!(environment.as_ref().map(identity).transpose()?);
                evidence.complete();
                execute_check(
                    engine,
                    environment.as_ref(),
                    package.as_ref(),
                    replay_descriptor_path.as_deref(),
                    evidence,
                    control,
                    observer,
                )
            },
        )
    }

    fn reserve(
        &self,
        operation: &'static str,
        replay: bool,
        work: impl FnOnce(&str, &Control, &Observer, &Path, &Path) -> Result<Value, Fault>
        + Send
        + 'static,
    ) -> Result<String, Fault> {
        let mut state = self.state();
        state.refresh();
        if state.closed {
            return Err(Fault::new(
                "ControllerClosed",
                "controller is shutting down",
            ));
        }
        if state.active.is_some() {
            return Err(Fault::new(
                "RunActive",
                "previous operation has not settled and been reaped",
            )
            .with_context(json!({"run":state.run,"operation":state.operation})));
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Fault::new("Clock", error.to_string()))?
            .as_nanos();
        let run = format!(
            "desktop-{}-{nonce}-{}",
            std::process::id(),
            NEXT_RUN.fetch_add(1, Ordering::Relaxed)
        );
        let mut limits = manual_plan()?.limits;
        if replay {
            limits.duration_ms = REPLAY_DURATION_MS;
        }
        let control = Arc::new(Control::new(&limits));
        let (progress_send, progress) = mpsc::sync_channel(PROGRESS_CAPACITY);
        let (log_send, logs) = mpsc::sync_channel(LOG_CAPACITY);
        let dropped_logs = Arc::new(AtomicU64::new(0));
        let observer = Observer {
            progress: progress_send,
            logs: log_send,
            dropped_logs: dropped_logs.clone(),
        };
        let worker_control = control.clone();
        let controlled = self.controlled_path.clone();
        let engine = self.engine_path.clone();
        let app_run = run.clone();
        let worker = thread::Builder::new()
            .name("desktop-runner".into())
            .spawn(move || work(&app_run, &worker_control, &observer, &controlled, &engine))
            .map_err(|error| Fault::new("Startup", error.to_string()))?;
        state.run = Some(run.clone());
        state.operation = operation;
        state.phase = "preparing";
        state.result = None;
        state.error = None;
        state.progress = vec![json!({"event":"Preparing","run":run,"operation":operation})];
        state.logs.clear();
        state.seen_logs.clear();
        state.dropped_logs = 0;
        state.active = Some(Active {
            control,
            worker,
            progress,
            logs,
            dropped_logs,
        });
        Ok(run)
    }

    pub fn stop(&self, run: &str) -> Result<(), Fault> {
        let mut state = self.state();
        if state.run.as_deref() != Some(run) {
            return Err(Fault::new(
                "StaleIdentity",
                "Stop does not name the current application run",
            ));
        }
        if let Some(active) = &state.active {
            active.control.cancel();
            if state.phase != "stopping" {
                state.phase = "stopping";
                if state.progress.len() < PROGRESS_CAPACITY {
                    state
                        .progress
                        .push(json!({"event":"StopRequestedByApplication","run":run}));
                }
            }
        }
        Ok(())
    }

    /// Only ordinary logs drain; progress and terminal evidence remain retrievable.
    pub fn poll(&self) -> ControllerView {
        let mut state = self.state();
        state.refresh();
        ControllerView {
            run: state.run.clone(),
            operation: state.operation.into(),
            state: state.phase.into(),
            result: state.result.clone(),
            error: state.error.clone(),
            progress: state.progress.clone(),
            logs: state.logs.drain(..).collect(),
            dropped_logs: state.dropped_logs,
        }
    }

    /// Call off the UI thread. Failure never releases an unsettled run reservation.
    pub fn shutdown(&self) -> Result<(), Fault> {
        {
            let mut state = self.state();
            state.closed = true;
            if let Some(active) = &state.active {
                active.control.cancel();
                state.phase = "stopping";
            }
        }
        let deadline = Instant::now() + Duration::from_millis(SHUTDOWN_MS);
        loop {
            {
                let mut state = self.state();
                state.refresh();
                if state.active.is_none() {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(Fault::new(
                    "Containment",
                    "shutdown deadline expired; worker still owned, cleanup unverified",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for DesktopController {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn manual_plan() -> Result<Plan, Fault> {
    serde_json::from_str(include_str!("../fixtures/manual-plan.json"))
        .map_err(|error| Fault::new("InvalidPlan", error.to_string()))
}

fn requested_plan(request: &StartRequest) -> Result<Plan, Fault> {
    match request.lane.as_str() {
        "controlled" | "replay" => {}
        "native" => {
            return Err(Fault::new(
                "NativeRefused",
                "desktop grants no live capture, target, or input authority",
            ));
        }
        _ => return Err(Fault::new("InvalidPlan", "unknown desktop execution lane")),
    }
    if request.lane == "replay" && request.scenario != "workflow" {
        return Err(Fault::new(
            "InvalidPlan",
            "recorded replay accepts only the package workflow, not controlled fixture scenarios",
        ));
    }
    if request.package_path.is_empty() || request.package_path.len() > 4096 {
        return Err(Fault::new(
            "Inventory",
            "package location must be nonempty and at most 4096 bytes",
        ));
    }
    let mut plan = manual_plan()?;
    plan.id = "desktop".into();
    plan.lane = request.lane.clone();
    if plan.lane == "replay" {
        plan.limits.snapshot_bytes = REPLAY_SNAPSHOT_BYTES;
        plan.limits.duration_ms = REPLAY_DURATION_MS;
    }
    plan.native_config = None;
    plan.samples = 1;
    plan.warmups = 0;
    plan.repetitions = 1;
    plan.profile = request.profile_id.clone();
    plan.scenario = if request.scenario == "workflow" {
        "success".into()
    } else {
        request.scenario.clone()
    };
    plan.validate()?;
    Ok(plan)
}

fn inspect_javascript(inventory: Inventory, limits: &Limits) -> Result<Inventory, Fault> {
    let inventory = Arc::new(inventory);
    let control = Arc::new(Control::new(limits));
    crate::typescript::validate_javascript_modules(&inventory, limits, &control)?;
    Arc::try_unwrap(inventory)
        .map_err(|_| Fault::new("Runtime", "inspection retained the captured inventory"))
}

fn runtime(inventory: &Inventory) -> Result<String, Fault> {
    match inventory.metadata["runtime"].as_str() {
        Some(value @ ("javascript" | "typescript")) => Ok(value.into()),
        _ => Err(Fault::new(
            "RuntimeRefused",
            "desktop M1 supports JavaScript and TypeScript through QuickJS only",
        )),
    }
}

fn profile(package_id: &str, schema: &Value, values: Value) -> Value {
    json!({"package_id":package_id,"schema_version":schema["version"],"options":values})
}

fn select_profile(inventory: &mut Inventory, request: &StartRequest) -> Result<(), Fault> {
    if inventory.identity != request.inventory_identity
        || inventory.package_id != request.package_id
    {
        return Err(Fault::new(
            "StaleIdentity",
            "package changed since inspection; inspect it again",
        )
        .with_context(
            json!({"expected_inventory":request.inventory_identity,"inventory":inventory.identity}),
        ));
    }
    if identity(&inventory.schema)? != request.schema_identity {
        return Err(Fault::new(
            "ProfileIdentity",
            "profile schema identity no longer matches the package",
        ));
    }
    let selected = profile(
        &inventory.package_id,
        &inventory.schema,
        request.values.clone(),
    );
    resolve_options(&inventory.schema, &selected, &inventory.package_id)?;
    inventory.metadata["desktop_profile"] = json!({
        "id":request.profile_id,"package_inventory_identity":inventory.identity,
        "schema_identity":request.schema_identity
    });
    // The external profile is a captured application input, never a package file read.
    inventory.metadata["manifest"]["profiles"][&request.profile_id] =
        json!("application-profile.json");
    inventory
        .profiles
        .insert(request.profile_id.clone(), selected);
    inventory.refresh_identity()
}

struct Evidence {
    fields: Value,
    stage: &'static str,
    completed: Vec<&'static str>,
}

impl Evidence {
    fn new(
        operation: &'static str,
        app_run: &str,
        environment: Option<&OcrEnvironment>,
        descriptor: Option<&str>,
    ) -> Result<Self, Fault> {
        Ok(Self {
            fields: json!({
                "operation":operation,"app_run":app_run,
                "selection_identity":environment.map(identity).transpose()?,
                "replay_descriptor_path":descriptor.filter(|path| path.len() <= 4096),
                "environment_identity":null,"corpus_identity":null
            }),
            stage: "request_validation",
            completed: Vec::new(),
        })
    }

    fn stage(&mut self, stage: &'static str, observer: &Observer) {
        self.stage = stage;
        let _ = observer.progress.try_send(json!({
            "event":"PreparationStage","stage":stage
        }));
    }

    fn complete(&mut self) {
        self.completed.push(self.stage);
    }

    fn attach(&self, value: &mut Value) {
        for (key, field) in self.fields.as_object().expect("evidence object") {
            value[key] = field.clone();
        }
        value["stage"] = json!(self.stage);
        value["completed_stages"] = json!(self.completed);
    }

    fn fault(&self, mut fault: Fault, before_child: bool) -> Fault {
        fault.bound_diagnostics();
        if !fault.context.is_object() {
            fault.context = json!({"cause":fault.context});
        }
        let source_stage = fault.context.get("stage").cloned();
        self.attach(&mut fault.context);
        if let Some(stage) = source_stage {
            fault.context["operation_stage"] = json!(self.stage);
            fault.context["stage"] = stage;
        }
        // Only the returning preparation worker can attest no child was started.
        if before_child {
            fault.context["cleanup"] = json!({"clean":true,"child_started":false});
        }
        fault
    }
}

fn replay_configuration(
    environment: Option<&OcrEnvironment>,
    control: &Control,
    observer: &Observer,
    evidence: &mut Evidence,
) -> Result<Value, Fault> {
    evidence.stage("environment_validation", observer);
    control.check()?;
    let environment = environment.ok_or_else(|| {
        Fault::new(
            "EnvironmentUnset",
            "Save an OCR environment before checking or replaying",
        )
    })?;
    let snapshot = capture_environment(environment, control)?;
    evidence.fields["environment_identity"] = json!(snapshot.identity);
    evidence.complete();
    Ok(snapshot.configuration)
}

fn project_replay(
    plan: &mut Plan,
    mut configuration: Value,
    descriptor: Option<&str>,
    inventory: &Inventory,
    control: &Control,
    observer: &Observer,
    evidence: &mut Evidence,
) -> Result<(), Fault> {
    evidence.stage("corpus_validation", observer);
    control.check()?;
    let descriptor = descriptor
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            Fault::new(
                "ReplayPrerequisite",
                "Select a recorded corpus before backend initialization",
            )
        })?;
    if descriptor.len() > 4096 || descriptor.chars().any(char::is_control) {
        return Err(Fault::new(
            "ReplayPrerequisite",
            "Corpus descriptor path exceeds accepted bounds",
        ));
    }
    let replay = capture_replay(Path::new(descriptor), inventory, &plan.limits, control)?;
    evidence.fields["corpus_identity"] = json!(replay.identity);
    evidence.fields["corpus_id"] = json!(replay.corpus_id);
    configuration["replay"] = replay.configuration;
    configuration["native"] = Value::Null;
    plan.native_config = Some(configuration);
    plan.validate()?;
    evidence.complete();
    Ok(())
}

fn engine_available(executable: &Path) -> Result<(), Fault> {
    match std::fs::metadata(executable) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        _ => Err(Fault::new(
            "EngineUnavailable",
            "The fixed engine runner artifact is unavailable; build it in the documented checkout location",
        )),
    }
}

fn execute(
    executable: &Path,
    request: &StartRequest,
    environment: Option<&OcrEnvironment>,
    mut plan: Plan,
    control: &Control,
    observer: &Observer,
    mut evidence: Evidence,
) -> Result<Value, Fault> {
    let prepared = (|| {
        evidence.stage("package_validation", observer);
        control.check()?;
        let mut inventory = Inventory::capture_with_stop(
            Path::new(&request.package_path),
            &manual_plan()?.limits,
            Some(&control.cancelled),
        )?;
        control.check()?;
        plan.candidate = runtime(&inventory)?;
        evidence.complete();
        if plan.lane == "replay" {
            let configuration =
                replay_configuration(environment, control, observer, &mut evidence)?;
            project_replay(
                &mut plan,
                configuration,
                request.replay_descriptor_path.as_deref(),
                &inventory,
                control,
                observer,
                &mut evidence,
            )?;
            evidence.stage("engine_availability", observer);
            engine_available(executable)?;
            evidence.complete();
        }
        evidence.stage("profile_validation", observer);
        select_profile(&mut inventory, request)?;
        evidence.complete();
        control.check()?;
        Ok(inventory)
    })();
    let inventory = prepared.map_err(|error| evidence.fault(error, true))?;
    evidence.stage("execution", observer);
    let mut poll_stop = || control.check().is_err();
    let record = run_once_with_executable(executable, &plan, &inventory, &mut poll_stop, observer)
        .map_err(|error| evidence.fault(error, false))?;
    let mut result = serde_json::to_value(record)
        .map_err(|error| evidence.fault(Fault::new("Encoding", error.to_string()), false))?;
    evidence.attach(&mut result);
    Ok(result)
}

fn execute_check(
    executable: &Path,
    environment: Option<&OcrEnvironment>,
    package: Option<&(PathBuf, String)>,
    descriptor: Option<&str>,
    mut evidence: Evidence,
    control: &Control,
    observer: &Observer,
) -> Result<Value, Fault> {
    evidence.fields["package_inventory_identity"] = json!(package.map(|(_, identity)| identity));
    let prepared = (|| {
        let mut plan = manual_plan()?;
        plan.id = "desktop-environment-check".into();
        plan.lane = "replay".into();
        plan.limits.snapshot_bytes = REPLAY_SNAPSHOT_BYTES;
        plan.limits.duration_ms = REPLAY_DURATION_MS;
        plan.samples = 1;
        plan.warmups = 0;
        plan.repetitions = 1;
        let configuration = replay_configuration(environment, control, observer, &mut evidence)?;
        evidence.stage("corpus_validation", observer);
        let (path, expected) = package.ok_or_else(|| {
            Fault::new(
                "ReplayPrerequisite",
                "Select a package and recorded corpus before backend initialization",
            )
        })?;
        if descriptor.is_none_or(|path| path.trim().is_empty()) {
            return Err(Fault::new(
                "ReplayPrerequisite",
                "Select a recorded corpus before backend initialization",
            ));
        }
        control.check()?;
        // Package capture remains lane-independent; replay's larger bound covers expanded frames.
        let inventory =
            Inventory::capture_with_stop(path, &manual_plan()?.limits, Some(&control.cancelled))?;
        control.check()?;
        if inventory.identity != *expected {
            return Err(Fault::new(
                "StaleIdentity",
                "Package changed; inspect it again before checking",
            ));
        }
        plan.candidate = runtime(&inventory)?;
        project_replay(
            &mut plan,
            configuration,
            descriptor,
            &inventory,
            control,
            observer,
            &mut evidence,
        )?;
        evidence.stage("engine_availability", observer);
        engine_available(executable)?;
        evidence.complete();
        control.check()?;
        Ok((plan, inventory))
    })();
    let (plan, inventory) = prepared.map_err(|error| evidence.fault(error, true))?;
    evidence.stage("child_startup", observer);
    let mut poll_stop = || control.check().is_err();
    let record = run_environment_check_with_executable(
        executable,
        &plan,
        &inventory,
        &mut poll_stop,
        observer,
    )
    .map_err(|error| evidence.fault(error, false))?;
    let observed_stage = record
        .primary
        .as_ref()
        .and_then(|fault| fault.context.get("stage"))
        .or_else(|| record.observations.get("stage"))
        .filter(|stage| stage.is_string())
        .cloned();
    if record.observations["stage"] == "initialized" {
        evidence.completed.push("backend_initialization");
    }
    let mut result = serde_json::to_value(record)
        .map_err(|error| evidence.fault(Fault::new("Encoding", error.to_string()), false))?;
    evidence.attach(&mut result);
    if let Some(stage) = observed_stage {
        result["stage"] = stage;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Inventory {
        Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/typescript")),
            &manual_plan().unwrap().limits,
        )
        .unwrap()
    }

    fn request(inventory: &Inventory) -> StartRequest {
        StartRequest {
            package_path: concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/typescript").into(),
            inventory_identity: inventory.identity.clone(),
            package_id: inventory.package_id.clone(),
            schema_identity: identity(&inventory.schema).unwrap(),
            profile_id: "saved-choice".into(),
            values: inventory.profiles["ocr-first"]["options"].clone(),
            lane: "controlled".into(),
            scenario: workflow(),
            replay_descriptor_path: None,
        }
    }

    #[test]
    fn javascript_preflight_refuses_missing_static_dependencies_without_evaluation() {
        let limits = manual_plan().unwrap().limits;
        let mut inventory = Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &limits,
        )
        .unwrap();
        inventory
            .sources
            .get_mut("main.js")
            .unwrap()
            .push_str("\nthrow new Error('must not evaluate during inspect');\n");
        let mut inventory = inspect_javascript(inventory, &limits).unwrap();
        inventory
            .sources
            .get_mut("main.js")
            .unwrap()
            .push_str("\nexport { missing } from './missing.js';\n");
        let fault = inspect_javascript(inventory, &limits).unwrap_err();
        assert_eq!(fault.category, "ImportRefused");
        assert_eq!(fault.context["specifier"], "./missing.js");
    }

    #[test]
    fn javascript_preflight_links_present_modules_without_evaluation() {
        let limits = manual_plan().unwrap().limits;
        let mut inventory = Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &limits,
        )
        .unwrap();
        inventory.sources.get_mut("main.js").unwrap().push_str(
            "\nexport { decide as chooseDecision } from './decisions.js';\n\
             export * from './decisions.js';\n\
             throw new Error('entry must not evaluate during inspect');\n",
        );
        inventory.sources.get_mut("decisions.js").unwrap().push_str(
            "\nimport { readiness } from './main.js';\n\
             export { readiness };\n\
             throw new Error('dependency must not evaluate during inspect');\n",
        );
        let inventory = inspect_javascript(inventory, &limits).unwrap();
        for invalid_binding in [
            "import { notExported as unavailable } from './decisions.js';",
            "export { notExported } from './decisions.js';",
        ] {
            let mut invalid = inventory.clone();
            invalid
                .sources
                .get_mut("main.js")
                .unwrap()
                .push_str(invalid_binding);
            let fault = inspect_javascript(invalid, &limits).unwrap_err();
            assert_eq!(
                fault.category, "ImportRefused",
                "{invalid_binding}: {fault:?}"
            );
            assert!(fault.message.contains("notExported"), "{fault:?}");
        }
    }

    #[test]
    fn javascript_preflight_follows_requested_declaration_modules_only() {
        let limits = manual_plan().unwrap().limits;
        let mut inventory = Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &limits,
        )
        .unwrap();
        inventory.sources.insert(
            "runtime.d.ts".into(),
            "export { decide } from './bridge.d.ts';\n\
             import { readiness } from './main.js';\n\
             export { readiness };\n\
             throw new Error('runtime declaration must not evaluate during inspect');\n"
                .into(),
        );
        inventory.sources.insert(
            "bridge.d.ts".into(),
            "import { decide } from './decisions.js';\n\
             export { decide };\n\
             throw new Error('transitive declaration must not evaluate during inspect');\n"
                .into(),
        );
        inventory.sources.insert(
            "unused.d.ts".into(),
            "export { Unused } from './missing-types.d.ts';\n\
             export declare const unused: import('./missing-query.d.ts').Unused;\n"
                .into(),
        );
        for request in [
            "import { decide } from './runtime.d.ts';",
            "export function load() { return import('./runtime.d.ts'); }",
        ] {
            let mut selected = inventory.clone();
            selected.sources.insert(
                "main.js".into(),
                format!(
                    "{request}\n\
                     export function readiness() {{ return 'Ready'; }}\n\
                     export function workflow() {{}}\n\
                     throw new Error('entry must not evaluate during inspect');\n"
                ),
            );
            let selected = inspect_javascript(selected, &limits).unwrap();

            let mut missing_export = selected.clone();
            missing_export.sources.insert(
                "bridge.d.ts".into(),
                "export { notExported as decide } from './decisions.js';".into(),
            );
            let fault = inspect_javascript(missing_export, &limits).unwrap_err();
            assert_eq!(fault.category, "ImportRefused", "{request}: {fault:?}");
            assert!(fault.message.contains("notExported"), "{fault:?}");

            let mut invalid_syntax = selected;
            invalid_syntax.sources.insert(
                "bridge.d.ts".into(),
                "export function decide() {\n  const value = ;\n}\n".into(),
            );
            let fault = inspect_javascript(invalid_syntax, &limits).unwrap_err();
            assert_eq!(fault.category, "Syntax", "{request}: {fault:?}");
            assert_eq!(fault.context["module"], "bridge.d.ts");
            assert_eq!(fault.context["line"], 2);
        }
    }

    #[test]
    fn javascript_preflight_preserves_target_engine_syntax_location() {
        let limits = manual_plan().unwrap().limits;
        let mut inventory = Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &limits,
        )
        .unwrap();
        inventory.sources.insert(
            "decisions.js".into(),
            "export function decide() {\n  const value = ;\n}\n".into(),
        );
        let fault = inspect_javascript(inventory, &limits).unwrap_err();
        assert_eq!(fault.category, "Syntax");
        assert_eq!(fault.context["module"], "decisions.js");
        assert_eq!(fault.context["line"], 2);
    }

    #[test]
    fn profile_capture_preserves_values_and_rejects_stale_inputs() {
        let inventory = fixture();
        let mut request = request(&inventory);
        let mut selected = inventory.clone();
        select_profile(&mut selected, &request).unwrap();
        request.values["priorities"] = json!(["template", "ocr"]);
        let effective = resolve_options(
            &selected.schema,
            &selected.profiles["saved-choice"],
            &selected.package_id,
        )
        .unwrap();
        assert_eq!(effective["priorities"], json!(["ocr", "template"]));
        assert_ne!(selected.identity, inventory.identity);
        let mut changed = inventory.clone();
        changed
            .sources
            .get_mut("main.ts")
            .unwrap()
            .push_str("\n// changed\n");
        changed.refresh_identity().unwrap();
        assert_eq!(
            select_profile(&mut changed, &request).unwrap_err().category,
            "StaleIdentity"
        );
        request.schema_identity = "old-schema".into();
        assert_eq!(
            select_profile(&mut inventory.clone(), &request)
                .unwrap_err()
                .category,
            "ProfileIdentity"
        );
    }

    #[test]
    fn pending_worker_reserves_run_and_stale_stop_cannot_cancel_successor() {
        let controller = DesktopController::new(
            PathBuf::from("unused-runner"),
            PathBuf::from("unused-engine"),
        );
        let control = Arc::new(Control::new(&manual_plan().unwrap().limits));
        let (release, wait) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            wait.recv().unwrap();
            Err(Fault::new("Cancelled", "preparation stopped"))
        });
        let (_, progress) = mpsc::sync_channel(1);
        let (_, logs) = mpsc::sync_channel(1);
        {
            let mut state = controller.state();
            state.run = Some("successor".into());
            state.phase = "preparing";
            state.active = Some(Active {
                control: control.clone(),
                worker,
                progress,
                logs,
                dropped_logs: Arc::new(AtomicU64::new(0)),
            });
        }
        assert_eq!(
            controller
                .start(request(&fixture()), None)
                .unwrap_err()
                .category,
            "RunActive"
        );
        assert_eq!(
            controller
                .check_environment(None, None, None)
                .unwrap_err()
                .category,
            "RunActive"
        );
        assert_eq!(
            controller.stop("predecessor").unwrap_err().category,
            "StaleIdentity"
        );
        assert!(!control.cancelled.load(Ordering::Acquire));
        controller.stop("successor").unwrap();
        assert_eq!(controller.poll().state, "stopping");
        assert!(control.cancelled.load(Ordering::Acquire));
        release.send(()).unwrap();
        controller.shutdown().unwrap();
        let terminal = controller.poll();
        assert_eq!(terminal.state, "terminal");
        assert_eq!(terminal.error.unwrap().category, "Cancelled");
        assert_eq!(controller.poll().progress, terminal.progress);
    }

    #[test]
    fn preparation_stop_never_attempts_to_launch_the_runner() {
        let request = request(&fixture());
        let control = Control::new(&manual_plan().unwrap().limits);
        control.cancel();
        let (progress, _) = mpsc::sync_channel(PROGRESS_CAPACITY);
        let (logs, _) = mpsc::sync_channel(LOG_CAPACITY);
        let observer = Observer {
            progress,
            logs,
            dropped_logs: Arc::new(AtomicU64::new(0)),
        };
        let result = execute(
            Path::new("runner-must-not-be-launched"),
            &request,
            None,
            requested_plan(&request).unwrap(),
            &control,
            &observer,
            Evidence::new("run", "cancelled-before-capture", None, None).unwrap(),
        );
        let error = result.unwrap_err();
        assert_eq!(error.category, "Cancelled");
        assert_eq!(
            error.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
        assert_eq!(
            Inventory::capture_with_stop(
                Path::new(&request.package_path),
                &manual_plan().unwrap().limits,
                Some(&control.cancelled),
            )
            .unwrap_err()
            .category,
            "Cancelled"
        );
    }

    fn settled(controller: &DesktopController) -> ControllerView {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let view = controller.poll();
            if view.state == "terminal" {
                return view;
            }
            assert!(Instant::now() < deadline, "operation did not settle");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn reserved_preparation_excludes_check_and_stop_prevents_launch() {
        let controller =
            DesktopController::new("missing-controlled".into(), "missing-engine".into());
        let (entered, started) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let run = controller
            .start_with_preparation(request(&fixture()), move |_, _| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                Ok(None)
            })
            .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let refusal = controller.check_environment(None, None, None).unwrap_err();
        assert_eq!(refusal.category, "RunActive");
        assert_eq!(refusal.context["run"], run);
        controller.stop(&run).unwrap();
        release.send(()).unwrap();
        let terminal = settled(&controller);
        let fault = terminal.error.unwrap();
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        assert_eq!(fault.category, "Cancelled");
        assert_eq!(
            fault.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
        assert!(
            !terminal
                .progress
                .iter()
                .any(|event| event["event"] == "ChildStarted")
        );
        let next = controller.check_environment(None, None, None).unwrap();
        assert_eq!(controller.stop(&run).unwrap_err().category, "StaleIdentity");
        let terminal = settled(&controller);
        assert_eq!(terminal.run.as_deref(), Some(next.as_str()));
        assert_eq!(terminal.operation, "environment_check");
        assert_eq!(terminal.error.unwrap().category, "EnvironmentUnset");
    }

    #[test]
    fn replay_without_environment_never_falls_back_to_controlled() {
        let controller =
            DesktopController::new("missing-controlled".into(), "missing-engine".into());
        let mut request = request(&fixture());
        request.lane = "replay".into();
        request.replay_descriptor_path = Some("not-read-without-environment.json".into());
        controller.start(request, None).unwrap();
        let terminal = settled(&controller);
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "EnvironmentUnset");
        assert_eq!(fault.context["stage"], "environment_validation");
        assert_eq!(
            fault.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
        assert!(
            !terminal
                .progress
                .iter()
                .any(|event| event["event"] == "ChildStarted")
        );
    }

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
