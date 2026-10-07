use super::packages::{runtime, select_profile};
use super::{
    Active, ControllerView, DesktopController, LOG_CAPACITY, LaunchDisposition, NEXT_RUN,
    NativePhase, NativeProgress, NativeTarget, NativeTargetStatus, PROGRESS_CAPACITY,
    REPLAY_DURATION_MS, REQUEST_BYTES, SHUTDOWN_MS, StartPreparation, StartRequest, State,
    manual_plan,
};
use crate::environment::{
    EnvironmentSnapshot, Library, OcrEnvironment, capture_environment, capture_replay,
};
use crate::inventory::Inventory;
use crate::model::{Control, Fault, Plan, encode_bounded, identity};
use crate::runner::{
    Observer, run_environment_check_with_executable, run_prepared_with_executable,
};
use serde_json::{Value, json};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

impl State {
    fn progress(&mut self, mut value: Value) {
        let Some(run) = &self.run else { return };
        let attempt = value["attempt"].as_u64().unwrap_or(1);
        if self
            .native_preparation
            .is_some_and(|current| attempt < current.attempt)
        {
            return;
        }
        if value["event"] == "NativePreparation" {
            if value
                .get("app_run")
                .is_some_and(|owner| owner.as_str() != Some(run.as_str()))
            {
                return;
            }
            if let (Ok(mut phase), Ok(mut status), Ok(incoming_launch)) = (
                serde_json::from_value(value["phase"].clone()),
                serde_json::from_value(value["status"].clone()),
                serde_json::from_value(value["launch"].clone()),
            ) {
                let previous = self
                    .native_preparation
                    .filter(|current| current.attempt == attempt);
                let launch = previous
                    .map(|progress| progress.launch)
                    .filter(|launch| *launch != LaunchDisposition::NotRequested)
                    .unwrap_or(incoming_launch);
                if let Some(previous) = previous
                    && ((previous.status == NativeTargetStatus::CaptureReady
                        && status != NativeTargetStatus::CaptureReady)
                        || (previous.status == NativeTargetStatus::Pending
                            && status == NativeTargetStatus::NotRequested))
                {
                    status = previous.status;
                    phase = previous.phase;
                }
                self.native_preparation = Some(NativeProgress {
                    attempt,
                    status,
                    phase,
                    launch,
                });
                value["status"] = json!(status);
                value["phase"] = json!(phase);
                value["launch"] = json!(launch);
                if self.phase != "stopping" {
                    self.phase = match phase {
                        NativePhase::Settling | NativePhase::Recovering => "recovering",
                        NativePhase::Workflow | NativePhase::Readiness => "running",
                        _ => "preparing",
                    };
                }
            }
        }
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
        if !self
            .seen_logs
            .insert((value["attempt"].as_u64().unwrap_or(1), sequence))
        {
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

    fn retained_logs(&mut self, record: &Value) {
        if let Some(logs) = record
            .pointer("/observations/script_logs")
            .and_then(Value::as_array)
        {
            for log in logs {
                let attempt = log["attempt"]
                    .as_u64()
                    .unwrap_or_else(|| record["attempt"].as_u64().unwrap_or(1));
                let Some(sequence) = log["sequence"].as_u64() else {
                    continue;
                };
                if self.seen_logs.contains(&(attempt, sequence)) {
                    continue;
                }
                let mut value = log.clone();
                value["run"] = record["run"].clone();
                value["attempt"] = json!(attempt);
                self.log(value);
            }
        }
    }

    fn refresh(&mut self) {
        let Some(active) = &self.active else { return };
        let progress: Vec<_> = active.progress.try_iter().take(PROGRESS_CAPACITY).collect();
        let logs: Vec<_> = active.logs.try_iter().take(LOG_CAPACITY).collect();
        let dropped = active.dropped_logs.swap(0, Ordering::Relaxed);
        let finished = active.worker.is_finished();
        let imported_attempts = self.attempts.len();
        {
            let attempts = active.attempts.lock().unwrap_or_else(|e| e.into_inner());
            if attempts.len() != self.attempts.len() {
                self.attempts.clone_from(&attempts);
            }
        }
        let native_preparation = *active
            .native_preparation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.dropped_logs = self.dropped_logs.saturating_add(dropped);
        for value in progress {
            self.progress(value);
        }
        for value in logs {
            self.log(value);
        }
        let attempts = std::mem::take(&mut self.attempts);
        for attempt in attempts.iter().skip(imported_attempts) {
            self.retained_logs(attempt);
        }
        self.attempts = attempts;
        if let Some(progress) = native_preparation
            && self.native_preparation != Some(progress)
        {
            self.progress(
                json!({"event":"NativePreparation","attempt":progress.attempt,
                "status":progress.status,"phase":progress.phase,"launch":progress.launch}),
            );
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
        self.attempts = active
            .attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
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
        let attempts = std::mem::take(&mut self.attempts);
        for attempt in &attempts {
            self.retained_logs(attempt);
            for field in ["dropped_logs", "script_logs_dropped"] {
                self.dropped_logs = self
                    .dropped_logs
                    .saturating_add(attempt["observations"][field].as_u64().unwrap_or(0));
            }
        }
        self.attempts = attempts;
        match result {
            Ok(value) => {
                self.retained_logs(&value);
                if self.attempts.is_empty() {
                    for field in ["dropped_logs", "script_logs_dropped"] {
                        self.dropped_logs = self
                            .dropped_logs
                            .saturating_add(value["observations"][field].as_u64().unwrap_or(0));
                    }
                }
                if let Ok(progress) = serde_json::from_value(value["native_preparation"].clone()) {
                    self.native_preparation = Some(progress);
                }
                self.result = Some(value);
            }
            Err(error) => {
                // Workers bound diagnostics before adding bounded operation correlation.
                if let Ok(progress) =
                    serde_json::from_value(error.context["native_preparation"].clone())
                {
                    self.native_preparation = Some(progress);
                }
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

impl DesktopController {
    /// Reserve synchronously; package capture and execution never run on the UI thread.
    pub fn start(
        &self,
        request: StartRequest,
        environment: Option<OcrEnvironment>,
    ) -> Result<String, Fault> {
        self.start_with_preparation(
            request,
            move |request, _| {
                if request.lane == "native" {
                    return Err(Fault::new(
                        "NativeRefused",
                        "Native requires host-owned target preparation",
                    ));
                }
                Ok(StartPreparation {
                    environment,
                    native: (),
                })
            },
            |_, _| {
                |_: &Control, _: &dyn Fn(NativeProgress), _: &dyn Fn() -> Result<(), Fault>| {
                    Ok(None)
                }
            },
        )
    }

    /// Capture once; construct a fresh resolver from that immutable capture for
    /// each admitted attempt. No successor rereads application settings.
    pub fn start_with_preparation<P: Send + 'static, R>(
        &self,
        mut request: StartRequest,
        capture: impl FnOnce(&mut StartRequest, &Control) -> Result<StartPreparation<P>, Fault>
        + Send
        + 'static,
        mut create_resolver: impl FnMut(&P, u64) -> R + Send + 'static,
    ) -> Result<String, Fault>
    where
        R: FnMut(
                &Control,
                &dyn Fn(NativeProgress),
                &dyn Fn() -> Result<(), Fault>,
            ) -> Result<Option<NativeTarget>, Fault>
            + Send
            + 'static,
    {
        let duration_ms = match request.lane.as_str() {
            "native" => {
                let intent = request.native_intent.as_ref().ok_or_else(|| {
                    Fault::new("NativeRefused", "Native requires explicit per-Start review")
                })?;
                super::native::validate_intent(intent)?;
                Some(
                    intent
                        .limits
                        .budgets()
                        .operation_ms(intent.max_exit_recoveries)?,
                )
            }
            "replay" => Some(REPLAY_DURATION_MS),
            _ => None,
        };
        self.reserve(
            "run",
            duration_ms,
            move |app_run, owner, observer, controlled, engine| {
                let initial_control;
                let control = if request.lane == "native" {
                    initial_control = Arc::new(Control::for_attempt(Arc::clone(owner), &manual_plan()?.limits, true));
                    &initial_control
                } else {
                    owner
                };
                let mut evidence = Evidence::new(
                    "run",
                    app_run,
                    None,
                    request.replay_descriptor_path.as_deref(),
                )?;
                if request.lane == "native" {
                    evidence.native_progress(
                        NativeProgress {
                            attempt: 1,
                            status: NativeTargetStatus::NotRequested,
                            phase: NativePhase::Preflight,
                            launch: LaunchDisposition::NotRequested,
                        },
                        observer,
                    );
                }
                evidence.stage("request_validation", observer);
                let prepared = (|| {
                    control.check()?;
                    encode_bounded(&request, REQUEST_BYTES)?;
                    evidence.fields["package_inventory_identity"] =
                        json!(request.inventory_identity);
                    evidence.fields["schema_identity"] = json!(request.schema_identity);
                    evidence.fields["profile_id"] = json!(request.profile_id);
                    let plan = requested_plan(&request)?;
                    if let Some(budgets) = plan.native_budgets {
                        control.start_native(budgets, None)?;
                        control.check()?;
                    }
                    evidence.complete();
                    evidence.stage("input_capture", observer);
                    let preparation = capture(&mut request, control)?;
                    evidence.fields["selection_identity"] =
                        json!(preparation.environment.as_ref().map(identity).transpose()?);
                    control.check()?;
                    Ok((plan, preparation))
                })();
                let (plan, preparation) = prepared.map_err(|error| evidence.fault(error, true))?;
                evidence.complete();
                let executable = if matches!(request.lane.as_str(), "replay" | "native") {
                    engine
                } else {
                    controlled
                };
                let mut plan = plan;
                let prepared = prepare_run(
                    executable,
                    &request,
                    preparation.environment.as_ref(),
                    &mut plan,
                    control,
                    observer,
                    &mut evidence,
                )
                .map_err(|error| evidence.fault(error, true))?;
                let PreparedRun { inventory, environment, engine, templates, modules } = prepared;
                let executable = engine.as_ref().map_or(executable, |artifact| artifact.path.as_path()).to_owned();
                let templates = templates.map(Arc::new);
                let environment = environment.map(Arc::new);
                let engine = engine.map(Arc::new);
                if plan.lane == "native" {
                    plan.native_config = Some(environment.as_ref()
                        .expect("native preflight captures environment").configuration.clone());
                    evidence.fields["native_intent_identity"] = json!(identity(&request.native_intent)?);
                }
                let request = Arc::new(request);
                run_attempts(
                    owner,
                    Arc::clone(control),
                    &plan,
                    &request,
                    observer,
                    &evidence,
                    |attempt, attempt_control, remaining| {
                    let startup = if plan.lane == "native" {
                        let mut resolve = create_resolver(&preparation.native, attempt);
                        let mut projected = plan.clone();
                        let probe_templates = Arc::clone(templates.as_ref().expect("native templates"));
                        let probe_executable = executable.clone();
                        let progress_observer = observer.clone();
                        let app_run = app_run.to_owned();
                        let environment = environment.clone();
                        let engine = engine.clone();
                        let request = Arc::clone(&request);
                        let (frames, actions) = remaining.expect("native allowances");
                        Some(crate::runner::NativePreparation::new(Arc::clone(attempt_control), attempt, move |control, report| {
                            let report = |mut progress: NativeProgress| {
                                progress.attempt = attempt;
                                report(progress);
                                progress_observer.progress(&json!({
                                    "event":"NativePreparation", "app_run":app_run,"attempt":attempt,
                                    "status":progress.status,"phase":progress.phase,"launch":progress.launch,
                                }));
                            };
                            let verify = || verify_resources(environment.as_deref(), engine.as_deref(), control);
                            let target = resolve(control, &report, &verify)?;
                            control.check()?;
                            let Some(target) = target else { return Ok(None); };
                            super::native::validate_target(Some(&target))?;
                            verify()?;
                            super::native::project(
                                &mut projected, &request, &target, &probe_executable, &probe_templates,
                                environment.as_ref().expect("native environment").configuration.clone(),
                            )?;
                            let config = projected.native_config.as_mut().expect("native projection");
                            config["native"]["capture"]["max_frames"] = json!(frames);
                            config["native"]["input"]["max_actions"] = json!(actions);
                            verify()?;
                            Ok(projected.native_config.take())
                        }))
                    } else {
                        None
                    };
                    verify_resources(environment.as_deref(), engine.as_deref(), attempt_control)
                        .map_err(|fault| evidence.fault(fault, true))
                        .and_then(|()| execute(
                        &executable, &inventory, plan.clone(),
                        crate::runner::PreparedExecution {
                            control: attempt_control,
                            modules: modules.as_ref(),
                            images: templates.as_ref().map(|templates| &templates.images),
                            startup: startup.as_ref(),
                            identity: Some((app_run, attempt)),
                        },
                        observer, evidence.clone(),
                    ))
                })
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
            Some(REPLAY_DURATION_MS),
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

    /// Validate one immutable authoring candidate without evaluating package code.
    /// The compiler child uses the same finite reservation and Stop latch as runs.
    pub fn validate_authoring(
        &self,
        capture: impl FnOnce(&Control) -> Result<Inventory, Fault> + Send + 'static,
    ) -> Result<String, Fault> {
        self.reserve("authoring_validate", None, move |_, control, _, _, _| {
            let outcome = (|| {
                control.check()?;
                let inventory = capture(control)?;
                control.check()?;
                let limits = manual_plan()?.limits;
                match runtime(&inventory)?.as_str() {
                    "typescript" => {
                        crate::typescript::compile_with_control(&inventory, &limits, control)?;
                    }
                    "javascript" => {
                        crate::typescript::validate_javascript_modules(
                            &Arc::new(inventory),
                            &limits,
                            control,
                        )?;
                    }
                    _ => unreachable!("runtime admits only supported source languages"),
                }
                control.check()
            })();
            let diagnostics = outcome.err().map(|mut error| {
                error.bound_diagnostics();
                error
            });
            Ok(json!({
                "valid":diagnostics.is_none(),
                "diagnostics":diagnostics.into_iter().collect::<Vec<_>>(),
            }))
        })
    }

    pub(super) fn reserve(
        &self,
        operation: &'static str,
        duration_ms: Option<u64>,
        work: impl FnOnce(&str, &Arc<Control>, &Observer, &Path, &Path) -> Result<Value, Fault>
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
        if let Some(duration_ms) = duration_ms {
            limits.duration_ms = duration_ms;
        }
        let control = Arc::new(Control::reserved(&limits)?);
        let (progress_send, progress) = mpsc::sync_channel(PROGRESS_CAPACITY);
        let (log_send, logs) = mpsc::sync_channel(LOG_CAPACITY);
        let dropped_logs = Arc::new(AtomicU64::new(0));
        let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let native_preparation = Arc::new(std::sync::Mutex::new(None));
        let observer = Observer {
            attempts: Arc::clone(&attempts),
            native_preparation: Arc::clone(&native_preparation),
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
        state.native_preparation = None;
        state.progress = vec![json!({"event":"Preparing","run":run,"operation":operation})];
        state.logs.clear();
        state.seen_logs.clear();
        state.dropped_logs = 0;
        state.attempts.clear();
        state.active = Some(Active {
            control,
            worker,
            progress,
            logs,
            attempts,
            native_preparation,
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
            attempts: state.attempts.clone(),
            native_preparation: state.native_preparation,
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

/// Keep the reservation and immutable Run authority while each owned executor
/// returns only after child/native settlement and terminal accounting.
fn run_attempts(
    owner: &Arc<Control>,
    mut attempt_control: Arc<Control>,
    plan: &Plan,
    request: &StartRequest,
    observer: &Observer,
    evidence: &Evidence,
    mut execute_attempt: impl FnMut(u64, &Arc<Control>, Option<(u64, usize)>) -> Result<Value, Fault>,
) -> Result<Value, Fault> {
    let mut attempt = 1;
    let mut remaining = request
        .native_intent
        .as_ref()
        .map(|intent| (intent.limits.max_frames, intent.limits.max_actions));
    let mut attempts = Vec::with_capacity(2);
    loop {
        let mut result = match execute_attempt(attempt, &attempt_control, remaining) {
            Ok(result) => result,
            Err(mut fault) => {
                let failed = json!({
                    "run":evidence.fields["app_run"],"attempt":attempt,"status":"FAIL","primary":fault,
                    "reason":fault.to_string(),"entry_outcome":"Unobserved",
                    "stage":fault.context["stage"],"completed_stages":fault.context["completed_stages"],
                    "native_preparation":fault.context["native_preparation"],
                    "cleanup":fault.context["cleanup"],"observations":{},
                    "forced":null,"exit_code":null,
                });
                attempts.push(failed);
                *observer.attempts.lock().unwrap_or_else(|e| e.into_inner()) = attempts.clone();
                fault.context["attempts"] = json!(attempts);
                fault.context["attempt"] = json!(attempt);
                fault.context["recovery_count"] = json!(attempt - 1);
                return Err(fault);
            }
        };
        result["attempt"] = json!(attempt);
        let next = remaining.and_then(|(frames, actions)| {
            crate::runner::recovery_allowance(&result, frames, actions)
        });
        attempts.push(crate::runner::attempt_summary(&result));
        *observer.attempts.lock().unwrap_or_else(|e| e.into_inner()) = attempts.clone();
        let approved = attempt == 1
            && request
                .native_intent
                .as_ref()
                .is_some_and(|intent| intent.max_exit_recoveries == 1);
        let progress =
            serde_json::from_value::<NativeProgress>(result["native_preparation"].clone()).ok();
        if approved && let (Some((frames, actions)), Some(mut progress)) = (next, progress) {
            // EntrySettled retired the ordinary stage clock. This check retains
            // earlier attempt stops; owner admission still serializes Run Stop
            // and the original absolute deadline against successor creation.
            let admission = attempt_control.check().and_then(|()| {
                if frames == 0 || actions == 0 {
                    return Err(Fault::new(
                        if frames == 0 {
                            "CaptureLimit"
                        } else {
                            "ActionLimit"
                        },
                        "Aggregate Run allowance is exhausted",
                    ));
                }
                owner.admit_recovery()
            });
            if let Err(fault) = admission {
                result["reason"] = json!(fault.to_string());
                result["primary"] = json!(fault);
                result["status"] = json!("FAIL");
            } else {
                progress.phase = NativePhase::Recovering;
                evidence.native_progress(progress, observer);
                remaining = next;
                attempt = 2;
                attempt_control =
                    Arc::new(Control::for_attempt(Arc::clone(owner), &plan.limits, false));
                attempt_control.start_native(plan.native_budgets.expect("native budgets"), None)?;
                evidence.native_progress(
                    NativeProgress {
                        attempt,
                        status: NativeTargetStatus::NotRequested,
                        phase: NativePhase::Preflight,
                        launch: LaunchDisposition::NotRequested,
                    },
                    observer,
                );
                continue;
            }
        }
        result["attempts"] = json!(attempts);
        result["recovery_count"] = json!(attempt - 1);
        return Ok(result);
    }
}

pub(super) fn requested_plan(request: &StartRequest) -> Result<Plan, Fault> {
    match request.lane.as_str() {
        "controlled" | "replay" => {
            if request.native_intent.is_some() {
                return Err(Fault::new(
                    "NativeRefused",
                    "Non-native lanes reject native approval",
                ));
            }
        }
        "native" => {
            let intent = request.native_intent.as_ref().ok_or_else(|| {
                Fault::new("NativeRefused", "Native requires explicit per-Start review")
            })?;
            super::native::validate_intent(intent)?;
            if !cfg!(target_os = "macos") {
                return Err(Fault::new(
                    "NativeRefused",
                    "Desktop Native is available only on macOS",
                ));
            }
            if request.replay_descriptor_path.is_some() {
                return Err(Fault::new(
                    "NativeRefused",
                    "Native rejects recorded replay sources",
                ));
            }
        }
        _ => return Err(Fault::new("InvalidPlan", "unknown desktop execution lane")),
    }
    if request.lane != "controlled" && request.scenario != "workflow" {
        return Err(Fault::new(
            "InvalidPlan",
            "Engine lanes accept only the package workflow, not controlled fixture scenarios",
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
        plan.limits.duration_ms = REPLAY_DURATION_MS;
    }
    if let Some(intent) = &request.native_intent {
        let limits = &intent.limits;
        plan.native_budgets = Some(limits.budgets());
        plan.limits.duration_ms = limits.budgets().total_ms()?;
        plan.limits.readiness_ms = limits.readiness_ms;
        plan.limits.wait_ms = limits.wait_ms;
        plan.limits.max_actions = limits.max_actions;
        plan.limits.cleanup_ms = limits.cleanup_ms;
        plan.limits.containment_ms = limits.containment_ms;
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

#[derive(Clone)]
struct Evidence {
    fields: Value,
    stage: &'static str,
    completed: Vec<&'static str>,
    native: Cell<Option<NativeProgress>>,
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
            native: Cell::new(None),
        })
    }

    fn native_progress(&self, progress: NativeProgress, observer: &Observer) {
        self.native.set(Some(progress));
        observer.progress(&json!({
            "event":"NativePreparation","app_run":self.fields["app_run"],
            "attempt":progress.attempt,
            "status":progress.status,"phase":progress.phase,"launch":progress.launch,
        }));
    }
    fn stage(&mut self, stage: &'static str, observer: &Observer) {
        self.stage = stage;
        let _ = observer.progress.try_send(json!({
            "event":"PreparationStage","stage":stage,"app_run":self.fields["app_run"],
            "attempt":self.native.get().map_or(1, |progress| progress.attempt),
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
        if let Some(progress) = self.native.get() {
            value["native_preparation"] = json!(progress);
        }
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

fn engine_configuration(
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
            "Save an OCR environment before checking or running an engine lane",
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

fn engine_available(executable: &Path, control: &Control) -> Result<Library, Fault> {
    crate::environment::capture_executable(executable, control).map_err(|mut fault| {
        if !matches!(fault.category.as_str(), "Cancelled" | "Timeout") {
            fault.category = "EngineUnavailable".into();
        }
        fault
    })
}

struct PreparedRun {
    inventory: Inventory,
    environment: Option<EnvironmentSnapshot>,
    engine: Option<Library>,
    templates: Option<super::native::Templates>,
    modules: Option<crate::typescript::PreparedModules>,
}

fn verify_resources(
    environment: Option<&EnvironmentSnapshot>,
    engine: Option<&Library>,
    control: &Control,
) -> Result<(), Fault> {
    control.check()?;
    if let Some(environment) = environment {
        environment.verify(control)?;
    }
    if let Some(engine) = engine {
        crate::environment::verify_executable(engine, control)?;
    }
    control.check()
}

fn prepare_run(
    executable: &Path,
    request: &StartRequest,
    environment: Option<&OcrEnvironment>,
    plan: &mut Plan,
    control: &Arc<Control>,
    observer: &Observer,
    evidence: &mut Evidence,
) -> Result<PreparedRun, Fault> {
    evidence.stage("package_validation", observer);
    control.check()?;
    let mut inventory = Inventory::capture_with_stop(
        Path::new(&request.package_path),
        &manual_plan()?.limits,
        Some(&control.cancelled),
    )?;
    control.check()?;
    evidence.complete();
    evidence.stage("profile_validation", observer);
    select_profile(&mut inventory, request)?;
    plan.candidate = runtime(&inventory)?;
    evidence.complete();
    let mut prepared = PreparedRun {
        inventory,
        environment: None,
        engine: None,
        templates: None,
        modules: None,
    };
    if plan.lane == "native" {
        evidence.stage("static_preflight", observer);
        prepared.templates = Some(super::native::prepare_templates(
            &prepared.inventory,
            &plan.limits,
        )?);
        if plan.candidate == "typescript" {
            prepared.inventory = crate::typescript::compile_with_control(
                &prepared.inventory,
                &plan.limits,
                control,
            )?;
        }
        let inventory = Arc::new(prepared.inventory);
        prepared.modules = Some(crate::typescript::PreparedModules::capture(
            &inventory,
            &plan.limits,
            control,
        )?);
        prepared.inventory = Arc::try_unwrap(inventory).map_err(|_| {
            Fault::new(
                "Runtime",
                "static preflight retained the captured inventory",
            )
        })?;
        evidence.complete();
        evidence.stage("engine_availability", observer);
        prepared.engine = Some(engine_available(executable, control)?);
        evidence.complete();
    }
    if matches!(plan.lane.as_str(), "native" | "replay") {
        evidence.stage("environment_validation", observer);
        let environment = environment.ok_or_else(|| {
            Fault::new(
                "EnvironmentUnset",
                "Save an OCR environment before checking or running an engine lane",
            )
        })?;
        let snapshot = capture_environment(environment, control)?;
        evidence.fields["environment_identity"] = json!(snapshot.identity);
        evidence.complete();
        if plan.lane == "replay" {
            project_replay(
                plan,
                snapshot.configuration.clone(),
                request.replay_descriptor_path.as_deref(),
                &prepared.inventory,
                control,
                observer,
                evidence,
            )?;
        }
        prepared.environment = Some(snapshot);
        if prepared.engine.is_none() {
            evidence.stage("engine_availability", observer);
            prepared.engine = Some(engine_available(executable, control)?);
            evidence.complete();
        }
    }
    control.check()?;
    Ok(prepared)
}

fn execute(
    executable: &Path,
    inventory: &Inventory,
    plan: Plan,
    preparation: crate::runner::PreparedExecution<'_>,
    observer: &Observer,
    mut evidence: Evidence,
) -> Result<Value, Fault> {
    evidence.stage("execution", observer);
    preparation
        .control
        .check()
        .map_err(|error| evidence.fault(error, true))?;
    let startup = preparation.startup;
    let mut record =
        run_prepared_with_executable(executable, &plan, inventory, preparation, observer);
    if let Some(startup) = startup {
        // An OS callback can report acceptance while an execution failure settles.
        if let Err(error) = &mut record
            && let Some(mut late) = startup.settle()
        {
            let unverified = late.context["native_cleanup"] == "unverified";
            late.bound_diagnostics();
            if !error.context.is_object() {
                error.context = json!({"cause":std::mem::take(&mut error.context)});
            }
            error.context["native_preparation_fault"] = json!(late);
            if unverified {
                error.context["native_cleanup"] = json!("unverified");
                if !error.context["cleanup"].is_object() {
                    error.context["cleanup"] = json!({});
                }
                error.context["cleanup"]["clean"] = json!(false);
            }
        }
        let progress = startup.progress();
        if progress.status != NativeTargetStatus::NotRequested {
            evidence.native.set(Some(progress));
        }
    }
    let record = record.map_err(|error| evidence.fault(error, false))?;
    if let Some(mut progress) = evidence.native.get() {
        for milestone in &record.milestones {
            if milestone["event"] == "NativePreparation"
                && let Ok(phase) = serde_json::from_value(milestone["phase"].clone())
            {
                progress.phase = phase;
                if let Ok(status) = serde_json::from_value(milestone["status"].clone()) {
                    progress.status = status;
                }
            }
        }
        if let Ok(phase) = serde_json::from_value(record.observations["native_phase"].clone()) {
            progress.phase = phase;
        }
        if let Ok(status) = serde_json::from_value(record.observations["native_status"].clone()) {
            progress.status = status;
        }
        evidence.native.set(Some(progress));
    }
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
        plan.limits.duration_ms = REPLAY_DURATION_MS;
        plan.samples = 1;
        plan.warmups = 0;
        plan.repetitions = 1;
        let configuration = engine_configuration(environment, control, observer, &mut evidence)?;
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
        engine_available(executable, control)?;
        evidence.complete();
        control.check()?;
        Ok((plan, inventory))
    })();
    let (plan, inventory) = prepared.map_err(|error| evidence.fault(error, true))?;
    evidence.stage("child_startup", observer);
    let mut poll_stop = || control.stop_reason();
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
mod tests;

#[cfg(test)]
mod preflight_tests;
