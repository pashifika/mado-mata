use crate::host::{Host, resolve_options, run_rust};
use crate::inventory::Inventory;
use crate::model::{Control, Fault, MAX_TRANSPORT_BYTES, Plan, RuntimeMetrics, identity};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    run: String,
    attempt: u64,
    plan: Plan,
    inventory: Inventory,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub version: u32,
    pub run: String,
    pub candidate: String,
    pub lane: String,
    pub scenario: String,
    pub profile: String,
    pub plan_identity: String,
    pub inventory_identity: String,
    pub status: String,
    pub reason: String,
    pub primary: Option<Fault>,
    pub entry_outcome: String,
    pub cleanup: Value,
    pub observations: Value,
    pub metrics: Value,
    pub milestones: Vec<Value>,
    pub exit_code: Option<i32>,
    pub forced: bool,
    pub build: Value,
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

pub fn read_json<T: DeserializeOwned>(path: &Path, bound: usize) -> Result<T, Fault> {
    let file = File::open(path).map_err(|e| Fault::new("Read", e.to_string()))?;
    let mut bytes = Vec::new();
    file.take(bound as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Fault::new("Read", e.to_string()))?;
    if bytes.len() > bound {
        return Err(Fault::new(
            "LimitExceeded",
            "JSON input exceeds its byte bound",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| Fault::new("InvalidJson", e.to_string()))
}

fn frame(reader: &mut impl BufRead, bound: usize) -> Result<Option<Vec<u8>>, Fault> {
    let mut bytes = Vec::new();
    reader
        .take(bound as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|e| Fault::new("Transport", e.to_string()))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > bound || bytes.last() != Some(&b'\n') {
        return Err(Fault::new(
            "Transport",
            "oversized or incomplete protocol frame",
        ));
    }
    Ok(Some(bytes))
}

fn emit(value: &Value) -> Result<(), Fault> {
    let bytes = serde_json::to_vec(value).map_err(|e| Fault::new("Encoding", e.to_string()))?;
    if bytes.len() >= MAX_TRANSPORT_BYTES {
        return Err(Fault::new(
            "LimitExceeded",
            "terminal record exceeds transport bound",
        ));
    }
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&bytes)
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
        .map_err(|e| Fault::new("Transport", e.to_string()))
}

fn process_metrics() -> Value {
    let mut system = System::new();
    let pid = Pid::from_u32(std::process::id());
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory().with_cpu(),
    );
    system.process(pid).map_or(Value::Null, |p| {
        json!({
            "rss_bytes":p.memory(), "cpu_ms":p.accumulated_cpu_time(),
            "method":"sysinfo process snapshot; RSS is not a live-owner count"
        })
    })
}

pub fn child() -> Result<bool, Fault> {
    let mut input = BufReader::new(std::io::stdin());
    let bytes = frame(&mut input, MAX_TRANSPORT_BYTES)?
        .ok_or_else(|| Fault::new("Transport", "missing child invocation"))?;
    let invocation: Invocation =
        serde_json::from_slice(&bytes).map_err(|e| Fault::new("Transport", e.to_string()))?;
    invocation.plan.validate()?;
    invocation.inventory.validate()?;
    let control = Arc::new(Control::new(&invocation.plan.limits));
    let finished = Arc::new(AtomicBool::new(false));
    let run = invocation.run.clone();
    let attempt = invocation.attempt;
    emit(
        &json!({"event":"ChildStarted","run":run,"attempt":attempt,"pid":std::process::id(),"at_us":control.elapsed_us()}),
    )?;

    // These threads never acquire a VM, host, native-work, or ordinary-log lock.
    let watch_control = control.clone();
    let watch_finished = finished.clone();
    let cleanup_ms = invocation.plan.limits.cleanup_ms;
    thread::spawn(move || {
        loop {
            if watch_finished.load(Ordering::Acquire) {
                return;
            }
            let _ = watch_control.check();
            let stop = watch_control.stop_us.load(Ordering::Acquire);
            if stop != 0 && watch_control.elapsed_us().saturating_sub(stop) > cleanup_ms * 1000 {
                // The observer records the exit; this is never a clean acknowledgement.
                std::process::exit(124);
            }
            thread::sleep(Duration::from_millis(2));
        }
    });
    let input_control = control.clone();
    thread::spawn(move || {
        let reason = match frame(&mut input, 1024) {
            Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) if value == json!({"command":"Stop","run":run,"attempt":attempt}) => {
                    "Stop"
                }
                _ => "InvalidControl",
            },
            Ok(None) => "ControlLost",
            Err(_) => "InvalidControl",
        };
        input_control.cancel();
        let _ = emit(
            &json!({"event":"StopRequested","run":run,"attempt":attempt,"reason":reason,
            "at_us":input_control.stop_us.load(Ordering::Acquire)}),
        );
        let _ = emit(
            &json!({"event":"AdmissionClosed","run":run,"attempt":attempt,
            "at_us":input_control.closed_us.load(Ordering::Acquire)}),
        );
    });

    let preflight_start = Instant::now();
    let selected = invocation
        .inventory
        .profiles
        .get(&invocation.plan.profile)
        .ok_or_else(|| Fault::new("Profile", "selected profile is not in the inventory"));
    let prepared = selected
        .and_then(|profile| {
            resolve_options(
                &invocation.inventory.schema,
                profile,
                &invocation.inventory.package_id,
            )
        })
        .and_then(|options| {
            Host::new(
                invocation.plan.clone(),
                options,
                invocation.inventory.assets.clone(),
                control.clone(),
            )
        });
    let host = match prepared {
        Ok(host) => host,
        Err(error) => {
            control.cancel();
            let release = crate::engine::release_runner_resources();
            emit(
                &json!({"event":"Terminal","run":invocation.run,"attempt":attempt,"primary":error,"entry_outcome":"NotStarted",
                "cleanup":{"clean":release.is_ok(),"stage":"preflight-no-host","runner_release":release.err()},"observations":{},
                "runtime":null,"preflight_us":preflight_start.elapsed().as_micros(),"workflow_us":null,
                "process":process_metrics()}),
            )?;
            finished.store(true, Ordering::Release);
            return Ok(true);
        }
    };
    let mut inventory = invocation.inventory;
    let compilation = if invocation.plan.candidate == "typescript" {
        crate::typescript::compile(&inventory, &invocation.plan.limits)
            .map(|compiled| inventory = compiled)
    } else {
        Ok(())
    };
    let preflight_us = preflight_start.elapsed().as_micros();
    let started = Instant::now();
    let inventory = Arc::new(inventory);
    let runtime = compilation.and_then(|()| match invocation.plan.candidate.as_str() {
        "rust" if invocation.plan.scenario == "held-work" => held_work(&host, &invocation.run),
        "rust" => run_rust(&host),
        "javascript" | "typescript" => crate::javascript::run(inventory.clone(), host.clone()),
        "lua" => crate::lua::run(inventory.clone(), host.clone()),
        _ => Err(Fault::new("InvalidPlan", "unknown candidate")),
    });
    let workflow_us = started.elapsed().as_micros();
    let (metrics, primary) = match runtime {
        Ok(metrics) => (Some(metrics), host.failure()),
        Err(error) => {
            // Adapters retain the host-owned primary and enrich it at the live
            // language boundary. Replacing it with the latch loses attribution.
            let error = if invocation.plan.candidate == "typescript" {
                crate::typescript::map_fault(&inventory, error)
            } else {
                error
            };
            (None::<RuntimeMetrics>, Some(error))
        }
    };
    control.cancel();
    let entry_outcome = if metrics.is_some() {
        "Returned"
    } else {
        "FailedOrNotStarted"
    };
    // Retain the settled entry before cleanup can block or the watchdog can exit.
    emit(
        &json!({"event":"EntrySettled","run":invocation.run,"attempt":attempt,
        "entry_outcome":entry_outcome,"primary":primary,"runtime":metrics,
        "compiled_inventory_identity":inventory.identity,"preflight_us":preflight_us,
        "workflow_us":workflow_us,"at_us":control.elapsed_us()}),
    )?;
    let mut cleanup = host.finish();
    let observations = host.snapshot();
    drop(host);
    if cleanup["clean"] == true {
        if let Err(error) = crate::engine::release_runner_resources() {
            cleanup["clean"] = Value::Bool(false);
            cleanup["runner_release_failure"] = json!(error);
        }
    }
    emit(
        &json!({"event":"Terminal","run":invocation.run,"attempt":attempt,
        "primary":primary,"entry_outcome":entry_outcome,"cleanup":cleanup,"observations":observations,"runtime":metrics,
        "compiled_inventory_identity":inventory.identity,"preflight_us":preflight_us,"workflow_us":workflow_us,
        "stop_at_us":control.stop_us.load(Ordering::Acquire),
        "admission_closed_at_us":control.closed_us.load(Ordering::Acquire),"process":process_metrics()}),
    )?;
    if cleanup["clean"] != true {
        // Keep retained work alive until the independent containment path exits.
        loop {
            thread::park_timeout(Duration::from_millis(10));
        }
    }
    finished.store(true, Ordering::Release);
    Ok(true)
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
    mut operator_stop: Option<&mut dyn FnMut() -> bool>,
) -> Result<RunRecord, Fault> {
    plan.validate()?;
    inventory.validate()?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Fault::new("Clock", e.to_string()))?
        .as_nanos();
    let run = format!("{}-{}-{nonce}", plan.id, std::process::id());
    let invocation = Invocation {
        run: run.clone(),
        attempt: 1,
        plan: plan.clone(),
        inventory: inventory.clone(),
    };
    let mut bytes =
        serde_json::to_vec(&invocation).map_err(|e| Fault::new("Encoding", e.to_string()))?;
    if bytes.len() >= MAX_TRANSPORT_BYTES {
        return Err(Fault::new(
            "LimitExceeded",
            "child invocation exceeds byte bound",
        ));
    }
    bytes.push(b'\n');
    let started = Instant::now();
    let executable = std::env::current_exe().map_err(|e| Fault::new("Startup", e.to_string()))?;
    let mut child = OwnedChild(
        Command::new(executable)
            .arg("child")
            // The ORT API opt-out leaves the POSIX uploader alive. Suppress its
            // initialization before any native library or child thread starts.
            .env("ORT_DISABLE_TELEMETRY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Fault::new("Startup", e.to_string()))?,
    );
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
    // At most sixteen records plus one bound-error; joining never needs a drain.
    let (sender, receiver) = mpsc::sync_channel(17);
    let reader = thread::spawn(move || {
        let mut input = BufReader::new(stdout);
        for _ in 0..16 {
            match frame(&mut input, MAX_TRANSPORT_BYTES) {
                Ok(Some(bytes)) => {
                    let result = serde_json::from_slice::<Value>(&bytes)
                        .map_err(|e| Fault::new("Transport", e.to_string()));
                    if sender.send(result).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            }
        }
        let _ = sender.send(Err(Fault::new(
            "Transport",
            "child exceeded protocol record bound",
        )));
    });
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
    let mut system = System::new();
    let child_pid = Pid::from_u32(child.0.id());
    let parent_pid = Pid::from_u32(std::process::id());
    let mut sampled_child_rss = 0u64;
    let mut sampled_parent_rss = 0u64;
    let mut metric_samples = 0usize;
    let exit;
    loop {
        while let Ok(message) = receiver.try_recv() {
            match message {
                Ok(mut value) if value["run"] == run && value["attempt"] == 1 => {
                    value["supervisor_received_us"] = json!(started.elapsed().as_micros());
                    if value["event"] == "Terminal" {
                        if terminal.replace(value).is_some() {
                            protocol_fault = Some(Fault::new("Transport", "duplicate terminal"));
                        }
                    } else {
                        if value["event"] == "ChildStarted" {
                            startup_us = Some(started.elapsed().as_micros());
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
        if stop_sent_at.is_none()
            && (operator_requested
                || elapsed >= Duration::from_millis(stop_at)
                || protocol_fault.is_some())
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
                value["supervisor_received_us"] = json!(started.elapsed().as_micros());
                if value["event"] == "ChildStarted" {
                    startup_us = Some(started.elapsed().as_micros());
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
    let terminal = terminal.unwrap_or_else(|| {
        milestones
            .iter()
            .find(|value| value["event"] == "EntrySettled")
            .cloned()
            .unwrap_or_else(|| json!({}))
    });
    let primary = protocol_fault.or_else(|| {
        terminal
            .get("primary")
            .filter(|v| !v.is_null())
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    });
    let clean = terminal["cleanup"]["clean"] == true && exit.success() && !forced;
    let status = if primary.as_ref().is_some_and(|e| e.category == "Blocked") {
        "BLOCKED"
    } else if clean && primary.is_none() {
        "PASS"
    } else {
        "FAIL"
    };
    let cleanup = if terminal.get("cleanup").is_some() {
        terminal["cleanup"].clone()
    } else {
        json!({"clean":false,"outcome":"ForcedOrIncomplete","cleanup_finished_us":null,
            "external_effects":"unknown; no automatic continuation"})
    };
    let reason = primary.as_ref().map_or_else(
        || {
            if clean {
                "entry returned and cleanup settled".into()
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
        entry_outcome: terminal["entry_outcome"]
            .as_str()
            .unwrap_or("Unobserved")
            .to_owned(),
        cleanup,
        observations: terminal["observations"].clone(),
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
            "owner_method":"attempt owners after cleanup, not peak","budgets":plan.budgets,
            "comparison_identity":identity(&(&plan.lane,&plan.scenario,&plan.profile,&plan.limits,&plan.budgets,
                (plan.samples,plan.warmups,plan.repetitions),&inventory.metadata["runtime_scenario"],
                &plan.native_config,&inventory.package_id,&inventory.schema,&inventory.profiles,&inventory.assets))?,
            "stderr_bytes_retained":stderr.len(),"stderr":String::from_utf8_lossy(&stderr),
            "compiled_inventory_identity":terminal["compiled_inventory_identity"]}),
        milestones,
        exit_code: exit.code(),
        forced: forced || exit.code() == Some(124),
        build: crate::report::build_identity(),
    })
}

fn held_work(host: &Host, run: &str) -> Result<RuntimeMetrics, Fault> {
    host.begin_readiness()?;
    host.begin_workflow()?;
    let observation = host.call("observe", json!({}))?;
    let query = host.call(
        "query",
        json!({"observation":observation,"kind":"ocr",
        "roi":{"x":0,"y":0,"width":640,"height":480},"expected":"READY"}),
    )?;
    let deadline = Instant::now() + Duration::from_millis(host.limits().wait_ms);
    while host.snapshot()["in_flight_native"].as_u64().unwrap_or(0) == 0 {
        host.control().check()?;
        if Instant::now() >= deadline {
            return Err(Fault::new(
                "Fixture",
                "held worker never retained its owner",
            ));
        }
        thread::sleep(Duration::from_millis(1));
    }
    emit(
        &json!({"event":"WorkHeld","run":run,"attempt":1,"pid":std::process::id(),
        "physical_owners":host.snapshot()["in_flight_native"]}),
    )?;
    host.call(
        "query_wait",
        json!({"id":query["id"],"timeout_ms":host.limits().wait_ms}),
    )?;
    Err(Fault::new(
        "Fixture",
        "non-returning controlled work unexpectedly completed",
    ))
}

pub fn parent_probe(intentional: bool) -> Result<bool, Fault> {
    let mut input = BufReader::new(std::io::stdin());
    let bytes = frame(&mut input, MAX_TRANSPORT_BYTES)?
        .ok_or_else(|| Fault::new("Fixture", "missing probe invocation"))?;
    if intentional {
        let invocation: Invocation = serde_json::from_slice(&bytes)
            .map_err(|error| Fault::new("Transport", error.to_string()))?;
        let record = run_once(
            &invocation.plan,
            &invocation.inventory,
            Some(100),
            false,
            None,
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
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().map_err(|e| Fault::new("Startup", e.to_string()))?)
            .arg("child")
            .env("ORT_DISABLE_TELEMETRY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Fault::new("Startup", e.to_string()))?,
    );
    let mut control = child
        .0
        .stdin
        .take()
        .ok_or_else(|| Fault::new("Transport", "no probe control"))?;
    control
        .write_all(&bytes)
        .map_err(|e| Fault::new("Transport", e.to_string()))?;
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
    let invocation = Invocation {
        run: run.clone(),
        attempt: 1,
        plan: plan.clone(),
        inventory: inventory.clone(),
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
    writeln!(
        input,
        "{}",
        serde_json::to_string(&invocation).map_err(|e| Fault::new("Encoding", e.to_string()))?
    )
    .map_err(|e| Fault::new("Transport", e.to_string()))?;
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
    let invocation = Invocation {
        run: format!("intentional-exit-{}", std::process::id()),
        attempt: 1,
        plan: plan.clone(),
        inventory: inventory.clone(),
    };
    let mut bytes = serde_json::to_vec(&invocation)
        .map_err(|error| Fault::new("Encoding", error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_TRANSPORT_BYTES {
        return Err(Fault::new(
            "LimitExceeded",
            "probe invocation exceeds its bound",
        ));
    }
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
    let writer = thread::spawn(move || input.write_all(&bytes));
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
        && record["milestones"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["event"] == "StopRequested"));
    Ok(
        json!({"id":format!("{}-intentional-supervisor-exit",plan.candidate),"candidate":plan.candidate,
        "lane":"controlled","os":std::env::consts::OS,"status":if passed{"PASS"}else{"FAIL"},
        "oracle":"owned supervisor requests Stop, retains clean cleanup, reaps its child, exits successfully and leaves the inert target alive",
        "parent_exit":status.code(),"parent_forced":forced,"target_survived":target_alive,
        "observed":response,"build":crate::report::build_identity()}),
    )
}
