use super::RunRecord;
use super::evidence::{Observer, receive_evidence};
use super::protocol::{Invocation, Operation, emit, frame};
use crate::inventory::Inventory;
use crate::model::{Fault, MAX_TRANSPORT_BYTES, Plan, identity};
use serde_json::{Value, json};
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

pub(super) struct OwnedChild(pub(super) Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

pub fn sample(plan: &Plan, inventory: &Inventory) -> Result<Vec<RunRecord>, Fault> {
    plan.validate()?;
    let mut records = Vec::new();
    for i in 0..plan.warmups + plan.samples * plan.repetitions {
        let mut record = run_once(plan, inventory, None, false, None)?;
        record.metrics["warmup"] = Value::Bool(i < plan.warmups);
        record.metrics["sample_index"] = json!(i);
        let failed = record.status != "PASS";
        records.push(record);
        if failed && plan.lane == "native" {
            break;
        }
    }
    Ok(records)
}

/// The optional operator callback must be a nonblocking poll. A true result
/// requests the same bounded Stop/cleanup path as the existing timed control.
pub fn run_once(
    plan: &Plan,
    inventory: &Inventory,
    stop_after_ms: Option<u64>,
    disconnect: bool,
    operator_stop: Option<&mut dyn FnMut() -> bool>,
) -> Result<RunRecord, Fault> {
    let executable = std::env::current_exe().map_err(|e| Fault::new("Startup", e.to_string()))?;
    supervise(
        plan,
        inventory,
        stop_after_ms,
        disconnect,
        operator_stop,
        None,
        &executable,
        None,
        Operation::Run,
    )
}

/// The application chooses the executable; observer queues never block supervision.
pub fn run_once_with_executable(
    executable: &Path,
    plan: &Plan,
    inventory: &Inventory,
    operator_stop: &mut dyn FnMut() -> bool,
    observer: &Observer,
) -> Result<RunRecord, Fault> {
    supervise(
        plan,
        inventory,
        None,
        false,
        Some(operator_stop),
        None,
        executable,
        Some(observer),
        Operation::Run,
    )
}

/// Initializes the selected real replay backend without executing package code.
pub fn run_environment_check_with_executable(
    executable: &Path,
    plan: &Plan,
    inventory: &Inventory,
    operator_stop: &mut dyn FnMut() -> bool,
    observer: &Observer,
) -> Result<RunRecord, Fault> {
    supervise(
        plan,
        inventory,
        None,
        false,
        Some(operator_stop),
        None,
        executable,
        Some(observer),
        Operation::EnvironmentCheck,
    )
}

pub(crate) fn run_once_after_milestone(
    plan: &Plan,
    inventory: &Inventory,
    event: &str,
    delay_ms: u64,
    disconnect: bool,
) -> Result<RunRecord, Fault> {
    let executable = std::env::current_exe().map_err(|e| Fault::new("Startup", e.to_string()))?;
    supervise(
        plan,
        inventory,
        None,
        disconnect,
        None,
        Some((event, delay_ms)),
        &executable,
        None,
        Operation::Run,
    )
}

fn child_loader_environment(command: &mut Command, plan: &Plan) -> Result<Value, Fault> {
    // The ORT API opt-out leaves the POSIX uploader alive. Suppress its
    // initialization before any native library or child thread starts.
    command.env("ORT_DISABLE_TELEMETRY", "1");
    #[cfg(windows)]
    {
        command.env_remove("MADO_COMPILER_NODE");
        // Resolve Node before restricting the engine's DLL search path.
        if let Some(path) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&path) {
                let node = directory.join("node.exe");
                if node.is_file()
                    && let Ok(node) = node.canonicalize()
                {
                    command.env("MADO_COMPILER_NODE", node);
                    break;
                }
            }
        }
    }
    if plan.lane == "controlled" {
        return Ok(json!({"ORT_DISABLE_TELEMETRY":"1"}));
    }
    let raw = plan.native_config.as_ref().ok_or_else(|| {
        crate::environment::blocked(
            "configuration_unset",
            "engine configuration is required before child startup",
        )
    })?;
    let configuration: crate::environment::Configuration<Value> =
        serde_json::from_value(raw.clone()).map_err(|error| {
            crate::environment::blocked("configuration_validation", &error.to_string())
        })?;
    configure_engine_loader(command, &configuration)
}

pub(super) fn configure_engine_loader(
    command: &mut Command,
    configuration: &crate::environment::Configuration<Value>,
) -> Result<Value, Fault> {
    command.env("ORT_DISABLE_TELEMETRY", "1");
    let mut directories = Vec::new();
    for library in
        std::iter::once(&configuration.ocr.runtime).chain(&configuration.native_libraries)
    {
        if !library.path.is_absolute() {
            return Err(crate::environment::blocked(
                "child_loader_configuration",
                "selected library paths must be absolute",
            ));
        }
        let directory = library.path.parent().ok_or_else(|| {
            crate::environment::blocked(
                "child_loader_configuration",
                "selected library has no parent directory",
            )
        })?;
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    let paths = std::env::join_paths(&directories).map_err(|error| {
        crate::environment::blocked("child_loader_configuration", &error.to_string())
    })?;
    let variable = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    // Do not inherit arbitrary search directories or mutate the GUI process.
    command.env(variable, &paths);
    Ok(json!({"ORT_DISABLE_TELEMETRY":"1","search_variable":variable,"directories":directories}))
}

fn loader_prerequisite_record(
    run: String,
    plan: &Plan,
    inventory: &Inventory,
    operation: Operation,
    primary: Fault,
    started: Instant,
) -> Result<RunRecord, Fault> {
    Ok(RunRecord {
        version: 1,
        run,
        candidate: plan.candidate.clone(),
        lane: plan.lane.clone(),
        scenario: plan.scenario.clone(),
        profile: plan.profile.clone(),
        plan_identity: identity(plan)?,
        inventory_identity: inventory.identity.clone(),
        status: "BLOCKED".into(),
        reason: primary.message.clone(),
        entry_outcome: "NotExecuted".into(),
        cleanup: json!({"clean":true,"child_started":false}),
        observations: json!({"operation":operation.name(),"stage":primary.context["stage"],
            "child_started":false}),
        primary: Some(primary),
        metrics: json!({"elapsed_us":started.elapsed().as_micros(),"runtime":null,
            "workflow_us":null,"vm_bytes":null,"child_process":null}),
        milestones: Vec::new(),
        exit_code: None,
        forced: false,
        build: Value::Null,
    })
}

fn child_startup_fault(
    exit_code: Option<i32>,
    forced: bool,
    stop_requested: bool,
    stderr: &[u8],
) -> Fault {
    let retained = &stderr[..stderr.len().min(4096)];
    Fault::new(
        "ChildStartup",
        "child exited before an authenticated startup record was observed",
    )
    .with_context(json!({
        "stage":"child_startup","boundary":"before_child_started","child_started":true,
        "rust_startup_observed":false,"exit_code":exit_code,"forced":forced,
        "stop_requested":stop_requested,"stderr":String::from_utf8_lossy(retained),
        "stderr_bytes_retained":retained.len(),"stderr_truncated":stderr.len() > retained.len()
    }))
}

fn supervise(
    plan: &Plan,
    inventory: &Inventory,
    stop_after_ms: Option<u64>,
    disconnect: bool,
    mut operator_stop: Option<&mut dyn FnMut() -> bool>,
    stop_milestone: Option<(&str, u64)>,
    executable: &Path,
    observer: Option<&Observer>,
    operation: Operation,
) -> Result<RunRecord, Fault> {
    plan.validate()?;
    inventory.validate()?;
    operation.validate(plan)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Fault::new("Clock", e.to_string()))?
        .as_nanos();
    let run = format!("{}-{}-{nonce}", plan.id, std::process::id());
    let mut invocation = Invocation {
        run: run.clone(),
        attempt: 1,
        operation,
        plan: plan.clone(),
        inventory: inventory.clone(),
        observe_logs: observer.is_some(),
    };
    let bytes = super::payload::header(&mut invocation)?;
    let transfer_assets = invocation.inventory.assets.clone();
    let _image_reservation = super::payload::reserve_child_images(plan, inventory)?;
    if observer.is_some() && operator_stop.as_mut().is_some_and(|poll| poll()) {
        return Err(
            Fault::new("Cancelled", "Stop requested before child startup").with_context(json!({
                "stage":"child_startup","boundary":"before_spawn","child_started":false,
                "cleanup":{"clean":true,"child_started":false}
            })),
        );
    }
    let started = Instant::now();
    let mut command = Command::new(executable);
    command
        .arg("child")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let loader_environment = match child_loader_environment(&mut command, plan) {
        Ok(environment) => environment,
        Err(error) => {
            return loader_prerequisite_record(run, plan, inventory, operation, error, started);
        }
    };
    if operator_stop.as_mut().is_some_and(|poll| poll()) {
        return Err(
            Fault::new("Cancelled", "Stop requested before child startup").with_context(json!({
                "stage":"child_startup","boundary":"before_spawn","child_started":false,
                "cleanup":{"clean":true,"child_started":false}
            })),
        );
    }
    let mut child = OwnedChild(command.spawn().map_err(|error| {
        Fault::new("ChildStartup", error.to_string()).with_context(json!({
            "stage":"child_startup","boundary":"spawn","io_kind":format!("{:?}",error.kind()),
            "child_started":false,"cleanup":{"clean":true,"child_started":false}
        }))
    })?);
    let mut input = child
        .0
        .stdin
        .take()
        .ok_or_else(|| Fault::new("Transport", "child stdin unavailable"))?;
    let (commands, command_receive) = mpsc::sync_channel::<Value>(1);
    let mut commands = Some(commands);
    // A child stuck before reading cannot stall the supervisor's deadline.
    let writer = thread::spawn(move || -> Result<(), Fault> {
        input
            .write_all(&bytes)
            .map_err(|e| Fault::new("Startup", e.to_string()))?;
        for asset in transfer_assets.values() {
            input
                .write_all(asset)
                .map_err(|e| Fault::new("Startup", e.to_string()))?;
        }
        if let Ok(command) = command_receive.recv() {
            writeln!(input, "{command}").map_err(|e| Fault::new("Transport", e.to_string()))?;
        }
        Ok(())
    });
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| Fault::new("Transport", "child stdout unavailable"))?;
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| Fault::new("Transport", "child stderr unavailable"))?;
    let (receiver, reader) = receive_evidence(stdout, observer, &invocation);
    let errors = thread::spawn(move || {
        let mut input = stderr;
        let mut retained = Vec::new();
        let mut buffer = [0u8; 4096];
        while let Ok(count) = input.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let keep = count.min(65_536usize.saturating_sub(retained.len()));
            retained.extend_from_slice(&buffer[..keep]);
        }
        retained
    });
    let mut milestones = Vec::new();
    let mut terminal = None;
    let mut protocol_fault = None;
    let mut forced = false;
    let mut stop_sent_at = None;
    let mut startup_us = None;
    let mut child_build = None;
    let mut system = System::new();
    let child_pid = Pid::from_u32(child.0.id());
    let mut milestone_received_at = None;
    let parent_pid = Pid::from_u32(std::process::id());
    let mut sampled_child_rss = 0u64;
    let mut sampled_parent_rss = 0u64;
    let mut metric_samples = 0usize;
    let exit;
    loop {
        while let Ok(message) = receiver.try_recv() {
            match message {
                Ok(mut value) if value["run"] == run && value["attempt"] == 1 => {
                    if value["event"] == "ChildStarted" {
                        if let Err(error) =
                            retain_child_build(&value, child.0.id(), &mut child_build)
                        {
                            protocol_fault = Some(error);
                            continue;
                        }
                        startup_us = Some(started.elapsed().as_micros());
                    }
                    value["supervisor_received_us"] = json!(started.elapsed().as_micros());
                    if let Some(observer) = observer {
                        observer.progress(&value);
                    }
                    if value["event"] == "Terminal" {
                        if terminal.replace(value).is_some() {
                            protocol_fault = Some(Fault::new("Transport", "duplicate terminal"));
                        }
                    } else {
                        if stop_milestone.is_some_and(|(event, _)| value["event"] == event) {
                            milestone_received_at.get_or_insert_with(Instant::now);
                        }
                        milestones.push(value);
                    }
                }
                Ok(_) => {
                    protocol_fault = Some(Fault::new("StaleIdentity", "foreign child evidence"))
                }
                Err(error) => protocol_fault = Some(error),
            }
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|e| Fault::new("Containment", e.to_string()))?
        {
            exit = status;
            break;
        }
        let elapsed = started.elapsed();
        // The optional operator poll is nonblocking and runs in the supervisor,
        // independently of compilation, VM execution, and native-work locks.
        let operator_requested =
            stop_sent_at.is_none() && operator_stop.as_mut().is_some_and(|poll| poll());
        let stop_at = stop_after_ms.unwrap_or(plan.limits.duration_ms);
        let milestone_stop_due = stop_milestone.is_some_and(|(_, delay_ms)| {
            milestone_received_at
                .is_some_and(|at: Instant| at.elapsed() >= Duration::from_millis(delay_ms))
        });
        if stop_sent_at.is_none()
            && (operator_requested
                || elapsed >= Duration::from_millis(stop_at)
                || protocol_fault.is_some()
                || milestone_stop_due)
        {
            stop_sent_at = Some(Instant::now());
            if disconnect {
                drop(commands.take());
            } else if let Some(sender) = commands.take() {
                let _ = sender.send(json!({"command":"Stop","run":run,"attempt":1}));
            }
        }
        if stop_sent_at
            .is_some_and(|stop| stop.elapsed() >= Duration::from_millis(plan.limits.cleanup_ms))
            || elapsed
                >= Duration::from_millis(plan.limits.duration_ms + plan.limits.containment_ms)
        {
            forced = true;
            child
                .0
                .kill()
                .map_err(|e| Fault::new("Containment", e.to_string()))?;
            exit = child
                .0
                .wait()
                .map_err(|e| Fault::new("Containment", e.to_string()))?;
            break;
        }
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[child_pid, parent_pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        if let Some(process) = system.process(child_pid) {
            sampled_child_rss = sampled_child_rss.max(process.memory());
            metric_samples += 1;
        }
        if let Some(process) = system.process(parent_pid) {
            sampled_parent_rss = sampled_parent_rss.max(process.memory());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let exit_us = started.elapsed().as_micros() as u64;
    drop(commands);
    if let Ok(Err(error)) = writer.join() {
        protocol_fault.get_or_insert(error);
    }
    let _ = reader.join();
    while let Ok(message) = receiver.try_recv() {
        match message {
            Ok(mut value) if value["run"] == run && value["attempt"] == 1 => {
                if value["event"] == "ChildStarted" {
                    if let Err(error) = retain_child_build(&value, child.0.id(), &mut child_build) {
                        protocol_fault = Some(error);
                        continue;
                    }
                    startup_us = Some(started.elapsed().as_micros());
                }
                value["supervisor_received_us"] = json!(started.elapsed().as_micros());
                if let Some(observer) = observer {
                    observer.progress(&value);
                }
                if value["event"] == "Terminal" {
                    if terminal.replace(value).is_some() {
                        protocol_fault = Some(Fault::new("Transport", "duplicate terminal"));
                    }
                } else {
                    milestones.push(value);
                }
            }
            Ok(_) => protocol_fault = Some(Fault::new("StaleIdentity", "foreign child evidence")),
            Err(error) => protocol_fault = Some(error),
        }
    }
    let stderr = errors.join().unwrap_or_default();
    if stop_milestone
        .is_some_and(|(event, _)| !milestones.iter().any(|value| value["event"] == event))
    {
        protocol_fault.get_or_insert_with(|| {
            Fault::new("Fixture", "required execution milestone was not observed")
        });
    }
    if child_build.is_none() && terminal.is_some() {
        protocol_fault.get_or_insert_with(|| {
            Fault::new("Transport", "terminal evidence lacks child build identity")
        });
    }
    let terminal = settled_evidence(terminal, &milestones);
    let primary = if child_build.is_none() {
        Some(child_startup_fault(
            exit.code(),
            forced || exit.code() == Some(124),
            stop_sent_at.is_some(),
            &stderr,
        ))
    } else {
        terminal_primary(
            &terminal,
            protocol_fault.as_ref(),
            forced || exit.code() == Some(124),
        )
    };
    let clean =
        child_build.is_some() && terminal["cleanup"]["clean"] == true && exit.success() && !forced;
    let status = if primary.as_ref().is_some_and(|e| e.category == "Blocked") {
        "BLOCKED"
    } else if clean && primary.is_none() {
        "PASS"
    } else {
        "FAIL"
    };
    let cleanup = if child_build.is_some() && terminal.get("cleanup").is_some() {
        terminal["cleanup"].clone()
    } else {
        json!({"clean":false,"status":"IncompleteCleanup","outcome":"ForcedOrIncomplete","cleanup_finished_us":null,
            "external_effects":"unknown; no automatic continuation"})
    };
    let reason = primary.as_ref().map_or_else(
        || {
            if clean {
                if operation == Operation::EnvironmentCheck {
                    "backend initialized and cleanup settled without executing package code".into()
                } else {
                    "entry returned and cleanup settled".into()
                }
            } else {
                "child exited without verified clean cleanup".into()
            }
        },
        |fault| fault.to_string(),
    );
    let stop_sent_us = stop_sent_at.map(|at| at.duration_since(started).as_micros() as u64);
    let receipt_latency = |event: &str| {
        stop_sent_us.and_then(|sent| {
            milestones
                .iter()
                .find(|value| value["event"] == event)
                .and_then(|value| value["supervisor_received_us"].as_u64())
                .map(|received| received.saturating_sub(sent))
        })
    };
    let host_call_us = terminal["observations"]["operation_metrics"]
        .as_object()
        .and_then(|operations| {
            operations
                .values()
                .filter_map(|value| value["max_us"].as_u64())
                .max()
        });
    let cpu_percent = terminal["process"]["cpu_ms"]
        .as_f64()
        .map(|cpu| cpu * 100_000.0 / exit_us.max(1) as f64);
    let mut observations = terminal["observations"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    observations.insert("operation".into(), json!(operation.name()));
    if operation == Operation::EnvironmentCheck && !observations.contains_key("stage") {
        observations.insert(
            "stage".into(),
            json!(if child_build.is_none() {
                "child_startup"
            } else if milestones
                .iter()
                .any(|value| value["event"] == "BackendInitializationStarted")
            {
                "backend_initialization"
            } else {
                "engine_preparation"
            }),
        );
    }
    Ok(RunRecord {
        version: 1,
        run,
        candidate: plan.candidate.clone(),
        lane: plan.lane.clone(),
        scenario: plan.scenario.clone(),
        profile: plan.profile.clone(),
        plan_identity: identity(plan)?,
        inventory_identity: inventory.identity.clone(),
        status: status.into(),
        reason,
        primary,
        entry_outcome: if operation == Operation::EnvironmentCheck {
            "NotExecuted".into()
        } else {
            terminal["entry_outcome"]
                .as_str()
                .unwrap_or("Unobserved")
                .to_owned()
        },
        cleanup,
        observations: Value::Object(observations),
        metrics: json!({"elapsed_us":started.elapsed().as_micros(),"startup_us":startup_us,
            "preflight_us":terminal["preflight_us"],"workflow_us":terminal["workflow_us"],
            "runtime":terminal["runtime"],"child_process":terminal["process"],
            "child_rss_bytes":if metric_samples == 0 { None } else { Some(sampled_child_rss) },
            "supervisor_rss_bytes":if metric_samples == 0 { None } else { Some(sampled_parent_rss) },"process_samples":metric_samples,
            "rss_method":"5ms periodic samples, observed maximum not OS peak",
            "stop_receipt_us":receipt_latency("StopRequested"),"admission_close_us":receipt_latency("AdmissionClosed"),
            "stop_metric_method":"supervisor-clock upper bound including pipe delivery and polling; null when no external Stop",
            "child_stop_at_us":terminal["stop_at_us"],"child_admission_closed_at_us":terminal["admission_closed_at_us"],
            "cleanup_us":terminal["cleanup"]["elapsed_us"],"containment_us":stop_sent_us.or_else(|| {
                (!clean).then(||milestones.iter().find(|value|value["event"]=="EntrySettled")
                    .and_then(|value|value["supervisor_received_us"].as_u64())).flatten()
            }).map(|sent|exit_us.saturating_sub(sent)),
            "containment_metric_method":"supervisor-clock Stop-to-exit; absent external Stop, incomplete entry-settlement receipt-to-exit",
            "host_call_us":host_call_us,"cpu_percent":cpu_percent,"vm_bytes":terminal["runtime"]["vm_bytes"],
            "live_owners":terminal["observations"]["attempt_owners"],
            "owner_method":if terminal["observations"]["snapshot_stage"] == "pre_cleanup" {
                "attempt owners before cleanup; completion unverified, not peak"
            } else {
                "attempt owners after cleanup, not peak"
            },"budgets":plan.budgets,
            "comparison_identity":identity(&(&plan.lane,&plan.scenario,&plan.profile,&plan.limits,&plan.budgets,
                (plan.samples,plan.warmups,plan.repetitions),&inventory.metadata["runtime_scenario"],
                &plan.native_config,&inventory.package_id,&inventory.schema,&inventory.profiles,&inventory.assets))?,
            "protocol_fault":protocol_fault,"entry_emission_failure":terminal["entry_emission_failure"],
            "diagnostic_details_omitted":terminal["diagnostic_details_omitted"],
            "stderr_bytes_retained":stderr.len(),"stderr":String::from_utf8_lossy(&stderr),
            "loader_environment":loader_environment,
            "compiled_inventory_identity":terminal["compiled_inventory_identity"]}),
        milestones,
        exit_code: exit.code(),
        forced: forced || exit.code() == Some(124),
        build: child_build.unwrap_or(Value::Null),
    })
}

// Call only after run/attempt correlation. Metadata is evidence, never a source
// of executable paths or authority; the PID comes from the supervisor's child.
pub(super) fn retain_child_build(
    started: &Value,
    pid: u32,
    build: &mut Option<Value>,
) -> Result<(), Fault> {
    if build.is_some() {
        return Err(Fault::new("Transport", "duplicate child startup identity"));
    }
    if started["pid"].as_u64() != Some(u64::from(pid)) {
        return Err(Fault::new(
            "StaleIdentity",
            "foreign child startup identity",
        ));
    }
    let identity = started
        .get("build")
        .filter(|value| value.is_object())
        .ok_or_else(|| Fault::new("Transport", "missing child build identity"))?;
    *build = Some(identity.clone());
    Ok(())
}

pub(super) fn settled_evidence(terminal: Option<Value>, milestones: &[Value]) -> Value {
    terminal.unwrap_or_else(|| {
        milestones
            .iter()
            .find(|value| value["event"] == "EntrySettled")
            .cloned()
            .unwrap_or_else(|| json!({}))
    })
}

pub(super) fn terminal_primary(
    terminal: &Value,
    protocol_fault: Option<&Fault>,
    forced: bool,
) -> Option<Fault> {
    terminal
        .get("primary")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .or_else(|| {
            protocol_fault
                .filter(|error| {
                    // Forced containment may cut the final write. Keep that
                    // diagnostic in protocol_fault, not as a new entry failure;
                    // the unverified cleanup and overall run still fail.
                    !(forced
                        && terminal["entry_outcome"] == "Returned"
                        && error.category == "Transport"
                        && error.context["frame_error"] == "Incomplete")
                })
                .cloned()
        })
        .or_else(|| {
            terminal
                .get("entry_emission_failure")
                .filter(|value| !value.is_null())
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        })
}

pub fn parent_probe(intentional: bool) -> Result<bool, Fault> {
    let mut input = BufReader::new(std::io::stdin());
    let mut invocation = super::payload::read(&mut input)?;
    if intentional {
        let record = run_once_after_milestone(
            &invocation.plan,
            &invocation.inventory,
            "VmHookReached",
            100,
            false,
        )?;
        let clean = record.cleanup["clean"] == true
            && !record.forced
            && record
                .primary
                .as_ref()
                .is_some_and(|fault| fault.category == "Cancelled");
        emit(&json!({"event":"SupervisorExit","record":record}))?;
        return Ok(clean);
    }
    let mut command =
        Command::new(std::env::current_exe().map_err(|e| Fault::new("Startup", e.to_string()))?);
    command
        .arg("child")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    child_loader_environment(&mut command, &invocation.plan)?;
    let mut child = OwnedChild(
        command
            .spawn()
            .map_err(|e| Fault::new("Startup", e.to_string()))?,
    );
    let mut control = child
        .0
        .stdin
        .take()
        .ok_or_else(|| Fault::new("Transport", "no probe control"))?;
    super::payload::write(&mut invocation, &mut control)?;
    let mut output = BufReader::new(
        child
            .0
            .stdout
            .take()
            .ok_or_else(|| Fault::new("Transport", "no probe evidence"))?,
    );
    while let Some(bytes) = frame(&mut output, MAX_TRANSPORT_BYTES)? {
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| Fault::new("Transport", e.to_string()))?;
        emit(&value)?;
    }
    drop(control);
    Ok(child
        .0
        .wait()
        .map_err(|e| Fault::new("Containment", e.to_string()))?
        .success())
}

pub fn parent_loss_evidence(plan: &Plan, inventory: &Inventory) -> Result<Value, Fault> {
    let run = format!("parent-loss-{}", std::process::id());
    let mut invocation = Invocation {
        run: run.clone(),
        attempt: 1,
        operation: Operation::Run,
        plan: plan.clone(),
        inventory: inventory.clone(),
        observe_logs: false,
    };
    let executable = std::env::current_exe().map_err(|e| Fault::new("Startup", e.to_string()))?;
    let mut target = OwnedChild(
        Command::new(&executable)
            .arg("target-probe")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Fault::new("Startup", e.to_string()))?,
    );
    let mut parent = OwnedChild(
        Command::new(executable)
            .arg("parent-probe")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Fault::new("Startup", e.to_string()))?,
    );
    let mut input = parent
        .0
        .stdin
        .take()
        .ok_or_else(|| Fault::new("Transport", "no parent input"))?;
    super::payload::write(&mut invocation, &mut input)?;
    drop(input);
    let output = parent
        .0
        .stdout
        .take()
        .ok_or_else(|| Fault::new("Transport", "no parent output"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut reader = BufReader::new(output);
        while let Ok(Some(bytes)) = frame(&mut reader, MAX_TRANSPORT_BYTES) {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if value["event"] == "WorkHeld" {
                    let _ = sender.try_send(value);
                    return;
                }
            }
        }
    });
    let held = receiver
        .recv_timeout(Duration::from_millis(plan.limits.duration_ms))
        .map_err(|_| Fault::new("Fixture", "parent-loss fixture did not confirm held work"))?;
    let child_pid = held["pid"]
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .map(Pid::from_u32)
        .ok_or_else(|| Fault::new("Fixture", "held child identity missing"))?;
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[child_pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let child_start = system.process(child_pid).map(sysinfo::Process::start_time);
    let started = Instant::now();
    parent
        .0
        .kill()
        .map_err(|e| Fault::new("Containment", e.to_string()))?;
    let parent_status = parent
        .0
        .wait()
        .map_err(|e| Fault::new("Containment", e.to_string()))?;
    let mut child_exited = false;
    while started.elapsed() < Duration::from_millis(plan.limits.containment_ms) {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[child_pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        child_exited = system.process(child_pid).is_none_or(|process| {
            Some(process.start_time()) != child_start
                || matches!(
                    process.status(),
                    sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
                )
        });
        if child_exited {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let target_alive = target
        .0
        .try_wait()
        .map_err(|e| Fault::new("Fixture", e.to_string()))?
        .is_none();
    Ok(
        json!({"id":"parent-death-held-work","candidate":"rust","lane":"controlled","os":std::env::consts::OS,
        "status":if child_exited && target_alive {"PASS"}else{"FAIL"},
        "oracle":"supervisor death while physical work held: independently bounded child exit, inert target survives",
        "held":held,"parent_exit":parent_status.code(),"child_exit_observed":child_exited,"target_survived":target_alive,
        "elapsed_us":started.elapsed().as_micros(),"cleanup":"ForcedOrIncomplete","cleanup_acknowledgement":null,
        "observation_method":"owned PID/start-time liveness; exited or zombie counts as stopped, not clean cleanup",
        "build":crate::report::build_identity()}),
    )
}

/// Observe an owned supervisor completing Stop/cleanup/reaping before it exits.
pub fn intentional_exit_evidence(plan: &Plan, inventory: &Inventory) -> Result<Value, Fault> {
    let executable =
        std::env::current_exe().map_err(|error| Fault::new("Startup", error.to_string()))?;
    let mut invocation = Invocation {
        run: format!("intentional-exit-{}", std::process::id()),
        attempt: 1,
        operation: Operation::Run,
        plan: plan.clone(),
        inventory: inventory.clone(),
        observe_logs: false,
    };
    let mut target = OwnedChild(
        Command::new(&executable)
            .arg("target-probe")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Fault::new("Startup", error.to_string()))?,
    );
    let mut parent = OwnedChild(
        Command::new(&executable)
            .arg("parent-stop-probe")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Fault::new("Startup", error.to_string()))?,
    );
    let mut input = parent
        .0
        .stdin
        .take()
        .ok_or_else(|| Fault::new("Transport", "no probe input"))?;
    let output = parent
        .0
        .stdout
        .take()
        .ok_or_else(|| Fault::new("Transport", "no probe output"))?;
    let writer = thread::spawn(move || super::payload::write(&mut invocation, &mut input));
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .take(MAX_TRANSPORT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let started = Instant::now();
    let mut forced = false;
    let status = loop {
        if let Some(status) = parent
            .0
            .try_wait()
            .map_err(|error| Fault::new("Containment", error.to_string()))?
        {
            break status;
        }
        if started.elapsed()
            >= Duration::from_millis(plan.limits.duration_ms + plan.limits.containment_ms)
        {
            forced = true;
            parent
                .0
                .kill()
                .map_err(|error| Fault::new("Containment", error.to_string()))?;
            break parent
                .0
                .wait()
                .map_err(|error| Fault::new("Containment", error.to_string()))?;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let written = matches!(writer.join(), Ok(Ok(())));
    let bytes = reader
        .join()
        .map_err(|_| Fault::new("Transport", "probe reader panicked"))?
        .map_err(|error| Fault::new("Transport", error.to_string()))?;
    let response: Value = if bytes.len() <= MAX_TRANSPORT_BYTES {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let record = &response["record"];
    let target_alive = target
        .0
        .try_wait()
        .map_err(|error| Fault::new("Containment", error.to_string()))?
        .is_none();
    let passed = written
        && !forced
        && status.success()
        && target_alive
        && response["event"] == "SupervisorExit"
        && record["cleanup"]["clean"] == true
        && record["forced"] == false
        && record["exit_code"] == 0
        && record["primary"]["category"] == "Cancelled"
        && record["milestones"].as_array().is_some_and(|rows| {
            let vm_hook = rows.iter().position(|row| row["event"] == "VmHookReached");
            let stop_requested = rows
                .iter()
                .position(|row| row["event"] == "StopRequested" && row["reason"] == "Stop");
            matches!((vm_hook, stop_requested), (Some(hook), Some(stop)) if hook < stop)
        });
    Ok(
        json!({"id":format!("{}-intentional-supervisor-exit",plan.candidate),"candidate":plan.candidate,
        "lane":"controlled","os":std::env::consts::OS,"status":if passed{"PASS"}else{"FAIL"},
        "oracle":"owned supervisor requests Stop, retains clean cleanup, reaps its child, exits successfully and leaves the inert target alive",
        "parent_exit":status.code(),"parent_forced":forced,"target_survived":target_alive,
        "observed":response,"build":crate::report::build_identity()}),
    )
}
