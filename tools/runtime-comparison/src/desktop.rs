//! One application-owned controlled run, with independent lifecycle and log queues.

use crate::host::resolve_options;
use crate::inventory::Inventory;
use crate::model::{Control, Fault, Limits, Plan, encode_bounded, identity};
use crate::runner::{Observer, run_once_with_executable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOG_CAPACITY: usize = 64;
const PROGRESS_CAPACITY: usize = 32;
const REQUEST_BYTES: usize = 65_536;
const SHUTDOWN_MS: u64 = 14_000;
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
}

#[derive(Debug, Serialize)]
pub struct ControllerView {
    pub run: Option<String>,
    pub state: String,
    pub result: Option<Value>,
    pub error: Option<Fault>,
    pub progress: Vec<Value>,
    pub logs: Vec<Value>,
    pub dropped_logs: u64,
}

struct Active {
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<Value, Fault>>,
    progress: mpsc::Receiver<Value>,
    logs: mpsc::Receiver<Value>,
    dropped_logs: Arc<AtomicU64>,
}

struct State {
    closed: bool,
    run: Option<String>,
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
                "run worker panicked; inspect containment evidence",
            ))
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
            Err(mut error) => {
                error.bound_diagnostics();
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
    runner_path: PathBuf,
    state: Mutex<State>,
}

impl DesktopController {
    pub fn new(runner_path: PathBuf) -> Self {
        Self {
            runner_path,
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
        let info = PackageInfo {
            schema_identity: identity(&inventory.schema)?,
            package_id: inventory.package_id,
            inventory_identity: inventory.identity,
            runtime,
            schema: inventory.schema,
            profiles: inventory.profiles,
            effective_defaults,
        };
        encode_bounded(&info, 2 * 1024 * 1024)?;
        Ok(info)
    }

    /// Reserve synchronously; package capture and execution never run on the UI thread.
    pub fn start(&self, request: StartRequest) -> Result<String, Fault> {
        let mut state = self.state();
        state.refresh();
        if state.closed {
            return Err(Fault::new(
                "ControllerClosed",
                "controller is shutting down",
            ));
        }
        if state.active.is_some() {
            return Err(
                Fault::new("RunActive", "previous run has not settled and been reaped")
                    .with_context(json!({"run":state.run})),
            );
        }
        encode_bounded(&request, REQUEST_BYTES)?;
        let plan = requested_plan(&request)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Fault::new("Clock", error.to_string()))?
            .as_nanos();
        let run = format!(
            "desktop-{}-{nonce}-{}",
            std::process::id(),
            NEXT_RUN.fetch_add(1, Ordering::Relaxed)
        );
        let stop = Arc::new(AtomicBool::new(false));
        let (progress_send, progress) = mpsc::sync_channel(PROGRESS_CAPACITY);
        let (log_send, logs) = mpsc::sync_channel(LOG_CAPACITY);
        let dropped_logs = Arc::new(AtomicU64::new(0));
        let observer = Observer {
            progress: progress_send,
            logs: log_send,
            dropped_logs: dropped_logs.clone(),
        };
        let worker_stop = stop.clone();
        let executable = self.runner_path.clone();
        let app_run = run.clone();
        let worker = thread::Builder::new()
            .name("desktop-runner".into())
            .spawn(move || {
                execute(
                    &executable,
                    &request,
                    plan,
                    &app_run,
                    &worker_stop,
                    &observer,
                )
            })
            .map_err(|error| Fault::new("Startup", error.to_string()))?;
        state.run = Some(run.clone());
        state.phase = "preparing";
        state.result = None;
        state.error = None;
        state.progress = vec![json!({"event":"Preparing","run":run})];
        state.logs.clear();
        state.seen_logs.clear();
        state.dropped_logs = 0;
        state.active = Some(Active {
            stop,
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
            active.stop.store(true, Ordering::Release);
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
                active.stop.store(true, Ordering::Release);
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
        "controlled" => {}
        "replay" => {
            return Err(Fault::new(
                "Blocked",
                "desktop replay requires an explicitly configured engine/model and saved-frame identity",
            ));
        }
        "native" => {
            return Err(Fault::new(
                "NativeRefused",
                "desktop M1 grants no native capture, OCR, target, or input authority",
            ));
        }
        _ => return Err(Fault::new("InvalidPlan", "unknown desktop execution lane")),
    }
    if request.package_path.is_empty() || request.package_path.len() > 4096 {
        return Err(Fault::new(
            "Inventory",
            "package location must be nonempty and at most 4096 bytes",
        ));
    }
    let mut plan = manual_plan()?;
    plan.id = "desktop".into();
    plan.lane = "controlled".into();
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

fn cancelled(stop: &AtomicBool) -> Result<(), Fault> {
    if stop.load(Ordering::Acquire) {
        Err(Fault::new(
            "Cancelled",
            "Stop requested during preparation; no child started",
        ))
    } else {
        Ok(())
    }
}

fn execute(
    executable: &Path,
    request: &StartRequest,
    mut plan: Plan,
    app_run: &str,
    stop: &AtomicBool,
    observer: &Observer,
) -> Result<Value, Fault> {
    cancelled(stop)?;
    let mut inventory =
        Inventory::capture_with_stop(Path::new(&request.package_path), &plan.limits, Some(stop))?;
    cancelled(stop)?;
    plan.candidate = runtime(&inventory)?;
    select_profile(&mut inventory, request)?;
    cancelled(stop)?;
    let mut poll_stop = || stop.load(Ordering::Acquire);
    let record = run_once_with_executable(executable, &plan, &inventory, &mut poll_stop, observer)?;
    let mut result =
        serde_json::to_value(record).map_err(|error| Fault::new("Encoding", error.to_string()))?;
    result["app_run"] = json!(app_run);
    result["package_inventory_identity"] = json!(request.inventory_identity);
    result["schema_identity"] = json!(request.schema_identity);
    result["profile_id"] = json!(request.profile_id);
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
        let controller = DesktopController::new(PathBuf::from("unused-runner"));
        let stop = Arc::new(AtomicBool::new(false));
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
                stop: stop.clone(),
                worker,
                progress,
                logs,
                dropped_logs: Arc::new(AtomicU64::new(0)),
            });
        }
        assert_eq!(
            controller.start(request(&fixture())).unwrap_err().category,
            "RunActive"
        );
        assert_eq!(
            controller.stop("predecessor").unwrap_err().category,
            "StaleIdentity"
        );
        assert!(!stop.load(Ordering::Acquire));
        controller.stop("successor").unwrap();
        assert_eq!(controller.poll().state, "stopping");
        assert!(stop.load(Ordering::Acquire));
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
        let stop = AtomicBool::new(true);
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
            requested_plan(&request).unwrap(),
            "cancelled-before-capture",
            &stop,
            &observer,
        );
        assert_eq!(result.unwrap_err().category, "Cancelled");
        assert_eq!(
            Inventory::capture_with_stop(
                Path::new(&request.package_path),
                &manual_plan().unwrap().limits,
                Some(&stop),
            )
            .unwrap_err()
            .category,
            "Cancelled"
        );
    }
}
