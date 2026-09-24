use super::evidence::{
    BACKEND_INITIALIZATION_STARTED, emit_settlement, retain_script_logs, start_evidence,
};
use super::protocol::{Invocation, Operation, emit, frame};
use crate::host::{Host, resolve_options, run_rust};
use crate::model::{Control, Fault, MAX_TRANSPORT_BYTES, RuntimeMetrics};
use serde_json::{Value, json};
use std::io::BufReader;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

fn preflight_cleanup(primary: &Fault, release: Result<(), Fault>) -> Value {
    let native_unverified = primary.context["native_cleanup"] == "unverified";
    let clean = !native_unverified && release.is_ok();
    json!({
        "clean":clean,
        "status":if clean {"CleanupFinished"} else {"IncompleteCleanup"},
        "stage":"preflight-no-host",
        "native_cleanup":if native_unverified {Some("unverified")} else {None},
        "runner_release":release.err()
    })
}

fn complete_child(
    clean: bool,
    finished: &AtomicBool,
    emission: Result<(), Fault>,
) -> Result<bool, Fault> {
    if !clean {
        // Retained/quarantined work lives until independent containment. A
        // broken evidence pipe must not imply that physical cleanup completed.
        loop {
            thread::park_timeout(Duration::from_millis(10));
        }
    }
    finished.store(true, Ordering::Release);
    emission.map(|()| true)
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
    invocation.operation.validate(&invocation.plan)?;
    let control = Arc::new(Control::new(&invocation.plan.limits));
    let finished = Arc::new(AtomicBool::new(false));
    let run = invocation.run.clone();
    let attempt = invocation.attempt;

    // These threads never acquire a VM, host, native-work, or ordinary-log lock.
    // Start control before identity collection: hashing or hardware discovery
    // must not delay Stop admission closure or the cleanup watchdog.
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
    let input_run = run.clone();
    let input_control = control.clone();
    thread::spawn(move || {
        let reason = match frame(&mut input, 1024) {
            Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value)
                    if value == json!({"command":"Stop","run":input_run,"attempt":attempt}) =>
                {
                    "Stop"
                }
                _ => "InvalidControl",
            },
            Ok(None) => "ControlLost",
            Err(_) => "InvalidControl",
        };
        input_control.cancel();
        let _ = emit(
            &json!({"event":"StopRequested","run":input_run,"attempt":attempt,"reason":reason,
            "at_us":input_control.stop_us.load(Ordering::Acquire)}),
        );
        let _ = emit(
            &json!({"event":"AdmissionClosed","run":input_run,"attempt":attempt,
            "at_us":input_control.closed_us.load(Ordering::Acquire)}),
        );
    });
    emit(
        &json!({"event":"ChildStarted","run":run,"attempt":attempt,"pid":std::process::id(),
            "build":crate::report::build_identity(),"operation":invocation.operation.name(),"at_us":control.elapsed_us()}),
    )?;
    start_evidence(&invocation);

    let preflight_start = Instant::now();
    let checking = invocation.operation == Operation::EnvironmentCheck;
    if checking {
        emit(
            &json!({"event":"EnginePreparationStarted","run":run,"attempt":attempt,
            "operation":"environment_check","stage":"engine_preparation","at_us":control.elapsed_us()}),
        )?;
    }
    let options = if checking {
        // No profile readiness, package evaluation, compiler, or VM is entered.
        Ok(json!({}))
    } else {
        invocation
            .inventory
            .profiles
            .get(&invocation.plan.profile)
            .ok_or_else(|| Fault::new("Profile", "selected profile is not in the inventory"))
            .and_then(|profile| {
                resolve_options(
                    &invocation.inventory.schema,
                    profile,
                    &invocation.inventory.package_id,
                )
            })
    };
    let prepared = options.and_then(|options| {
        Host::new(
            invocation.plan.clone(),
            options,
            invocation.inventory.assets.clone(),
            control.clone(),
        )
    });
    let host = match prepared {
        Ok(host) => host,
        Err(mut error) => {
            if checking && error.context.get("stage").is_none() {
                if !error.context.is_object() {
                    error.context = json!({});
                }
                error.context["stage"] = json!(if !cfg!(feature = "engine") {
                    "engine_unavailable"
                } else if BACKEND_INITIALIZATION_STARTED.load(Ordering::Acquire) {
                    "backend_initialization"
                } else {
                    "engine_preparation"
                });
            }
            let observations = json!({"operation":invocation.operation.name(),
                "stage":error.context["stage"],"initialized":false});
            let entry_outcome = if checking {
                "NotExecuted"
            } else {
                "NotStarted"
            };
            error.bound_diagnostics();
            let entry_emission = emit_settlement(
                json!({"event":"EntrySettled","run":invocation.run,"attempt":attempt,
                "primary":error,"entry_outcome":entry_outcome,"observations":observations,
                "preflight_us":preflight_start.elapsed().as_micros(),"at_us":control.elapsed_us()}),
            )
            .err();
            control.cancel();
            let cleanup = preflight_cleanup(&error, crate::engine::release_runner_resources());
            let emission = emit_settlement(
                json!({"event":"Terminal","run":invocation.run,"attempt":attempt,"primary":error,"entry_outcome":entry_outcome,
                "cleanup":cleanup,"observations":observations,"entry_emission_failure":entry_emission,
                "runtime":null,"preflight_us":preflight_start.elapsed().as_micros(),"workflow_us":null,
                "process":process_metrics()}),
            );
            return complete_child(cleanup["clean"] == true, &finished, emission);
        }
    };
    if checking {
        return finish_environment_check(host, &invocation, &control, &finished, preflight_start);
    }
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
    let (metrics, mut primary) = match runtime {
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
    if let Some(primary) = primary.as_mut() {
        primary.bound_diagnostics();
    }
    let entry_outcome = if metrics.is_some() {
        "Returned"
    } else {
        "FailedOrNotStarted"
    };
    // snapshot() takes only short-lived host bookkeeping locks; the engine
    // snapshot reads immutable facts and atomics, never its native-work lock.
    // Publish known facts before starting our local cleanup clock. External
    // Stop/deadline cancellation remains independent throughout serialization.
    let mut observations = host.snapshot();
    observations["snapshot_stage"] = json!("pre_cleanup");
    retain_script_logs(&mut observations);
    let entry_emission = emit_settlement(
        json!({"event":"EntrySettled","run":invocation.run,"attempt":attempt,
        "entry_outcome":entry_outcome,"primary":primary,"runtime":metrics,"observations":observations,
        "compiled_inventory_identity":inventory.identity,"preflight_us":preflight_us,
        "workflow_us":workflow_us,"at_us":control.elapsed_us()}),
    )
    .err();
    control.cancel();
    // An emission error is secondary evidence, never an early return before
    // cleanup of accepted input, held keys, queued work, or native ownership.
    let mut cleanup = host.finish();
    let mut observations = host.snapshot();
    observations["snapshot_stage"] = json!("post_cleanup");
    retain_script_logs(&mut observations);
    drop(host);
    if cleanup["clean"] == true {
        if let Err(error) = crate::engine::release_runner_resources() {
            cleanup["clean"] = Value::Bool(false);
            cleanup["runner_release_failure"] = json!(error);
        }
    }
    let emission = emit_settlement(
        json!({"event":"Terminal","run":invocation.run,"attempt":attempt,
        "primary":primary,"entry_outcome":entry_outcome,"cleanup":cleanup,"observations":observations,"runtime":metrics,
        "entry_emission_failure":entry_emission,
        "compiled_inventory_identity":inventory.identity,"preflight_us":preflight_us,"workflow_us":workflow_us,
        "stop_at_us":control.stop_us.load(Ordering::Acquire),
        "admission_closed_at_us":control.closed_us.load(Ordering::Acquire),"process":process_metrics()}),
    );
    complete_child(cleanup["clean"] == true, &finished, emission)
}

fn finish_environment_check(
    host: Host,
    invocation: &Invocation,
    control: &Control,
    finished: &AtomicBool,
    started: Instant,
) -> Result<bool, Fault> {
    let primary = control.check().err();
    let initialization_us = started.elapsed().as_micros();
    let mut observations = host.snapshot();
    observations["operation"] = json!("environment_check");
    observations["stage"] = json!("initialized");
    observations["initialized"] = json!(true);
    observations["snapshot_stage"] = json!("pre_cleanup");
    // Publication errors must not bypass the owned session's cleanup path.
    let milestone_emission = emit(&json!({"event":"BackendInitialized","run":invocation.run,
        "attempt":invocation.attempt,"operation":"environment_check","stage":"initialized",
        "at_us":control.elapsed_us()}))
    .err();
    let entry_emission = emit_settlement(json!({"event":"EntrySettled","run":invocation.run,
        "attempt":invocation.attempt,"primary":primary,"entry_outcome":"NotExecuted",
        "observations":observations,"runtime":null,"preflight_us":initialization_us,
        "workflow_us":null,"at_us":control.elapsed_us()}))
    .err()
    .or(milestone_emission);
    control.cancel();
    let mut cleanup = host.finish();
    observations = host.snapshot();
    observations["operation"] = json!("environment_check");
    observations["stage"] = json!("initialized");
    observations["initialized"] = json!(true);
    observations["snapshot_stage"] = json!("post_cleanup");
    drop(host);
    if cleanup["clean"] == true {
        if let Err(error) = crate::engine::release_runner_resources() {
            cleanup["clean"] = json!(false);
            cleanup["runner_release_failure"] = json!(error);
        }
    }
    let emission = emit_settlement(json!({"event":"Terminal","run":invocation.run,
        "attempt":invocation.attempt,"primary":primary,"entry_outcome":"NotExecuted",
        "cleanup":cleanup,"observations":observations,"runtime":null,
        "entry_emission_failure":entry_emission,"preflight_us":initialization_us,"workflow_us":null,
        "stop_at_us":control.stop_us.load(Ordering::Acquire),
        "admission_closed_at_us":control.closed_us.load(Ordering::Acquire),"process":process_metrics()}));
    complete_child(cleanup["clean"] == true, finished, emission)
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

#[cfg(test)]
mod tests;
