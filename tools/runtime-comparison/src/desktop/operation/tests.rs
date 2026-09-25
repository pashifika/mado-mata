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

#[test]
fn reserved_preparation_excludes_check_and_stop_prevents_launch() {
    let controller = DesktopController::new("missing-controlled".into(), "missing-engine".into());
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
