use super::*;
use crate::desktop::test_support::{fixture, request, settled};

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
            native_preparation: Default::default(),
            attempts: Default::default(),
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
        native_preparation: Default::default(),
        attempts: Default::default(),
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
    };
    let result = execute(
        Path::new("runner-must-not-be-launched"),
        &fixture(),
        requested_plan(&request).unwrap(),
        crate::runner::PreparedExecution {
            identity: None,
            control: &control,
            modules: None,
            images: None,
            startup: None,
        },
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

#[test]
fn reserved_preparation_excludes_check_and_stop_prevents_launch() {
    let controller = DesktopController::new("missing-controlled".into(), "missing-engine".into());
    let (entered, started) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let run = controller
        .start_with_preparation(
            request(&fixture()),
            move |_, _| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                Ok(StartPreparation::<()>::default())
            },
            |_, _| {
                |_: &Control, _: &dyn Fn(NativeProgress), _: &dyn Fn() -> Result<(), Fault>| {
                    panic!("cancelled capture must not reach target resolution")
                }
            },
        )
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
    let controller = DesktopController::new("missing-controlled".into(), "missing-engine".into());
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
fn execution_error_retains_late_launch_and_unverified_callback_cleanup() {
    let inventory = fixture();
    let plan = requested_plan(&request(&inventory)).unwrap();
    let control = Arc::new(Control::new(&plan.limits));
    let (entered, arrival) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let startup =
        crate::runner::NativePreparation::new(Arc::clone(&control), 1, move |control, report| {
            control.admit_launch()?;
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            report(NativeProgress {
                attempt: 1,
                status: NativeTargetStatus::Pending,
                phase: NativePhase::LaunchSubmission,
                launch: LaunchDisposition::Accepted,
            });
            panic!("callback cleanup is unknown after late acceptance");
        });
    startup.request().unwrap();
    arrival.recv_timeout(Duration::from_secs(5)).unwrap();
    let (progress, events) = mpsc::sync_channel(PROGRESS_CAPACITY);
    let (logs, _) = mpsc::sync_channel(LOG_CAPACITY);
    let observer = Observer {
        native_preparation: Default::default(),
        attempts: Default::default(),
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
    };
    let evidence = Evidence::new("run", "late-native-callback", None, None).unwrap();
    evidence.native_progress(
        NativeProgress {
            attempt: 1,
            status: NativeTargetStatus::NotRequested,
            phase: NativePhase::Preflight,
            launch: LaunchDisposition::NotRequested,
        },
        &observer,
    );
    let worker = thread::spawn(move || {
        let result = execute(
            Path::new("missing-owned-runner"),
            &inventory,
            plan,
            crate::runner::PreparedExecution {
                identity: None,
                control: &control,
                modules: None,
                images: None,
                startup: Some(&startup),
            },
            &observer,
            evidence,
        );
        drop(startup);
        result.unwrap_err()
    });
    loop {
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        if event["event"] == "PreparationStage" && event["stage"] == "execution" {
            break;
        }
    }
    release.send(()).unwrap();
    let fault = worker.join().unwrap();
    assert_ne!(fault.category, "Controller");
    assert_eq!(fault.context["native_preparation"]["launch"], "accepted");
    assert_eq!(
        fault.context["native_preparation_fault"]["category"],
        "Controller"
    );
    assert_eq!(fault.context["native_cleanup"], "unverified");
    assert_eq!(fault.context["cleanup"]["clean"], false);
}

// This executor seam owns a real controlled Host, not a subprocess or startup
// worker. Its cleanup/accounting are real; the two absent external owners are
// settled by construction. Real child transport is covered by startup_protocol.
fn settle_controlled_host(
    run: &str,
    attempt: u64,
    host: &crate::host::Host,
    primary: Option<Fault>,
) -> Value {
    let cleanup = host.finish();
    let mut observations = host.snapshot();
    observations["workflow_entered"] = json!(host.workflow_entered());
    observations["snapshot_stage"] = json!("post_cleanup");
    observations["terminal_accounted"] = json!(true);
    observations["child_reaped"] = json!(true);
    observations["startup_settled"] = json!(true);
    if cleanup["clean"] == true {
        observations["accounting"] = host.terminal_accounting().unwrap();
    }
    json!({
        "run":run,"attempt":attempt,
        "status":if primary.is_none() && cleanup["clean"] == true {"PASS"} else {"FAIL"},
        "entry_outcome":if primary.is_none() {"Returned"} else {"FailedOrNotStarted"},
        "primary":primary,"cleanup":cleanup,"observations":observations,
        "forced":false,"exit_code":0,
        "native_preparation":{
            "attempt":attempt,"status":"capture_ready","phase":"settling","launch":"not_requested"
        }
    })
}

#[test]
fn owned_recovery_loop_settles_past_workflow_without_renewing_run_authority() {
    for case in ["clean", "stop", "stage_timeout", "incomplete"] {
        let controller = DesktopController::new("unused-controlled".into(), "unused-engine".into());
        let (entered, started) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let run = controller
            .reserve("run", Some(30_000), move |run, owner, observer, _, _| {
                let mut plan = manual_plan().unwrap();
                let mut request = request(&fixture());
                let mut limits = crate::desktop::native_limits();
                limits.startup_ms = 5_000;
                limits.readiness_ms = 5_000;
                limits.workflow_ms = 100;
                let budgets = limits.budgets();
                request.native_intent = Some(crate::desktop::NativeIntent {
                    target_revision: 1,
                    target_binding_id: "controlled-owner-loop".into(),
                    target_declaration_identity: "controlled-only".into(),
                    capture_approved: true,
                    input_approved: true,
                    launch_approved: true,
                    max_exit_recoveries: 1,
                    operation: "Controlled owner-loop regression".into(),
                    visible_postcondition: "Fresh attempt completes".into(),
                    limits,
                });
                plan.native_budgets = Some(budgets);
                let initial = Arc::new(Control::for_attempt(Arc::clone(owner), &plan.limits, true));
                initial.start_native(budgets, None).unwrap();
                let outer = owner.outer_deadline();
                let evidence = Evidence::new("run", run, None, None).unwrap();
                run_attempts(
                    owner,
                    Arc::clone(&initial),
                    &plan,
                    &request,
                    observer,
                    &evidence,
                    |attempt, control, remaining| {
                        assert_eq!(control.outer_deadline(), outer);
                        assert_eq!(owner.outer_deadline(), outer);
                        assert_eq!(remaining, Some((if attempt == 1 { 300 } else { 298 }, 64)));
                        if attempt == 2 {
                            assert_eq!(case, "clean");
                            assert!(!Arc::ptr_eq(control, &initial));
                            assert!(owner.check().is_ok());
                        }
                        let mut controlled_plan = plan.clone();
                        controlled_plan.lane = "controlled".into();
                        controlled_plan.native_budgets = None;
                        let host = crate::host::Host::new_attempt(
                            controlled_plan,
                            json!({}),
                            Default::default(),
                            Arc::clone(control),
                            attempt,
                        )
                        .unwrap();
                        assert_eq!(host.snapshot()["observations"], 0);
                        control.native_transition(1, None).unwrap();
                        host.begin_readiness().unwrap();
                        host.begin_workflow().unwrap();
                        let observation = host.call("observe", json!({})).unwrap();
                        host.call("release", json!({"id":observation["id"]}))
                            .unwrap();
                        let old_workflow = control.native_transition(2, None).unwrap();
                        let primary = if attempt == 1 {
                            host.call("fixture", json!({"event":"confirmed_exit"}))
                                .unwrap();
                            let fault = host.call("observe", json!({})).unwrap_err();
                            assert_eq!(fault.category, "TargetExited");
                            assert_eq!(fault.context["exit_reason"], "absent");
                            if case == "stage_timeout" {
                                thread::sleep(
                                    old_workflow.saturating_duration_since(Instant::now()),
                                );
                                assert_eq!(control.check().unwrap_err().category, "Timeout");
                            }
                            Some(fault)
                        } else {
                            None
                        };
                        // Mirror authenticated EntrySettled, before cleanup.
                        control.settle_native();
                        if attempt == 1 {
                            evidence.native_progress(
                                NativeProgress {
                                    attempt,
                                    status: NativeTargetStatus::CaptureReady,
                                    phase: NativePhase::Settling,
                                    launch: LaunchDisposition::NotRequested,
                                },
                                observer,
                            );
                            entered.send(old_workflow).unwrap();
                            wait.recv_timeout(Duration::from_secs(5)).unwrap();
                            assert!(Instant::now() >= old_workflow);
                            if case == "incomplete" {
                                host.fail(
                                    Fault::new("Cleanup", "unverified ownership")
                                        .with_context(json!({"native_cleanup":"unverified"})),
                                );
                            }
                        }
                        Ok(settle_controlled_host(run, attempt, &host, primary))
                    },
                )
            })
            .unwrap();
        let old_workflow = started.recv_timeout(Duration::from_secs(5)).unwrap();
        thread::sleep(old_workflow.saturating_duration_since(Instant::now()));
        let pending = controller.poll();
        assert_eq!(pending.run.as_deref(), Some(run.as_str()));
        assert_ne!(pending.state, "terminal");
        assert_eq!(
            controller
                .check_environment(None, None, None)
                .unwrap_err()
                .category,
            "RunActive"
        );
        if case == "stop" {
            controller.stop(&run).unwrap();
            assert_eq!(controller.poll().state, "stopping");
        }
        release.send(()).unwrap();
        let terminal = settled(&controller);
        assert!(terminal.error.is_none(), "{case}: {:?}", terminal.error);
        let result = terminal.result.unwrap();
        assert_eq!(result["attempts"][0]["primary"]["category"], "TargetExited");
        assert_eq!(
            result["attempts"][0]["primary"]["context"]["exit_reason"],
            "absent"
        );
        assert_eq!(
            result["recovery_count"],
            if case == "clean" { 1 } else { 0 }
        );
        match case {
            "clean" => {
                assert_eq!(terminal.attempts.len(), 2);
                assert_eq!(result["status"], "PASS");
                assert_eq!(result["attempt"], 2);
                assert_eq!(result["cleanup"]["clean"], true);
            }
            "stop" => assert_eq!(result["primary"]["category"], "Cancelled"),
            "stage_timeout" => {
                assert_eq!(result["primary"]["category"], "Timeout");
                assert_eq!(result["primary"]["context"]["stage"], "workflow");
            }
            "incomplete" => {
                assert_eq!(result["primary"]["category"], "TargetExited");
                assert_eq!(result["cleanup"]["clean"], false);
                assert_eq!(result["cleanup"]["native_cleanup"], "unverified");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn mapped_typescript_exit_survives_bounding_recovery_and_retention() {
    let mut plan = manual_plan().unwrap();
    plan.limits.duration_ms = 30_000;
    let control = Arc::new(Control::new(&plan.limits));
    let mut source = fixture();
    let function = format!("observe_exit_{}", "x".repeat(6_000));
    source.sources.insert(
        "main.ts".into(),
        r"
        function observeExitedTarget(depth: number): void {
            if (depth > 0) {
                observeExitedTarget(depth - 1);
                return;
            }
            // @ts-expect-error Controlled-only regression fixture, not Native authority.
            host.call('fixture', {event:'confirmed_exit'});
            host.call('observe', {});
        }
        export function readiness(): MadoReady { return 'Ready'; }
        export function workflow(): void { observeExitedTarget(2); }
        "
        .replace("observeExitedTarget", &function),
    );
    source.refresh_identity().unwrap();
    let inventory =
        Arc::new(crate::typescript::compile_with_control(&source, &plan.limits, &control).unwrap());
    let host = crate::host::Host::new(plan, json!({}), inventory.assets.clone(), control).unwrap();
    let fault = crate::javascript::run(Arc::clone(&inventory), host.clone()).unwrap_err();
    assert_eq!(fault.category, "TargetExited", "{fault:?}");
    assert!(host.workflow_entered());
    let mut fault = crate::typescript::map_fault(&inventory, fault);
    assert!(
        fault.context["typescript"]["frames"]
            .as_array()
            .unwrap()
            .iter()
            .any(|frame| frame["original"]["module"] == "main.ts")
    );
    assert!(encode_bounded(&fault.context, 16 * 1024).is_err());
    fault.bound_diagnostics();
    assert_eq!(fault.context["exit_reason"], "absent");
    assert_eq!(
        fault.context["diagnostic_truncation"]["context_omitted"],
        true
    );
    encode_bounded(&fault.context, 16 * 1024).unwrap();
    let record = settle_controlled_host("mapped-exit", 1, &host, Some(fault));
    assert_eq!(record["cleanup"]["clean"], true);
    assert_eq!(
        crate::runner::recovery_allowance(&record, 300, 64),
        Some((299, 64))
    );
    let retained = crate::runner::attempt_summary(&record);
    assert_eq!(retained["primary"]["context"]["exit_reason"], "absent");
    assert_eq!(
        retained["primary"]["context"]["diagnostic_truncation"]["context_omitted"],
        true
    );
}

#[test]
fn predecessor_logs_survive_delivery_races_and_successor_startup_failure() {
    for (successor_fails, poll_during_recovery) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let controller = DesktopController::new("unused".into(), "unused".into());
        let (ready, observed) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        controller
            .reserve("run", None, move |run, _, observer, _, _| {
                let first = json!({
                    "run":run,"attempt":1,
                    "observations":{"script_logs":[
                        {"run":run,"attempt":1,"sequence":1,"message":"streamed-before-exit"},
                        {"run":run,"attempt":1,"sequence":2,"message":"retained-at-exit"}
                    ]}
                });
                observer
                    .logs
                    .try_send(first["observations"]["script_logs"][0].clone())
                    .unwrap();
                observer
                    .attempts
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(first);
                ready.send(()).unwrap();
                resume.recv().unwrap();
                if successor_fails {
                    return Err(Fault::new(
                        "EngineUnavailable",
                        "successor refused before startup",
                    ));
                }
                let second = json!({
                    "run":run,"attempt":2,
                    "observations":{"script_logs":[
                        {"run":run,"attempt":2,"sequence":1,"message":"fresh-attempt-sequence"}
                    ]}
                });
                observer
                    .attempts
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(second.clone());
                Ok(second)
            })
            .unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut delivered = Vec::new();
        if poll_during_recovery {
            let active = controller.poll();
            assert_eq!(
                active
                    .logs
                    .iter()
                    .map(|log| log["message"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["streamed-before-exit", "retained-at-exit"]
            );
            delivered.extend(active.logs);
            assert!(controller.poll().logs.is_empty());
        }
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let terminal = loop {
            let mut view = controller.poll();
            delivered.append(&mut view.logs);
            if view.state == "terminal" {
                break view;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        let identities: Vec<_> = delivered
            .iter()
            .map(|log| {
                (
                    log["attempt"].as_u64().unwrap(),
                    log["sequence"].as_u64().unwrap(),
                )
            })
            .collect();
        if successor_fails {
            assert_eq!(terminal.error.unwrap().category, "EngineUnavailable");
            assert_eq!(identities, [(1, 1), (1, 2)]);
        } else {
            assert!(terminal.error.is_none());
            assert_eq!(identities, [(1, 1), (1, 2), (2, 1)]);
        }
        assert_eq!(terminal.dropped_logs, 0);
        assert!(controller.poll().logs.is_empty());
    }
}
