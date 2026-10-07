use super::*;
use crate::model::{Control, NativeBudgets, Plan};
use crate::runner::{StartupLink, StartupReply};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};

fn native_host() -> (Host, Arc<StartupLink>, mpsc::Receiver<u64>) {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    let budgets = NativeBudgets {
        startup_ms: 10_000,
        readiness_ms: 10_000,
        workflow_ms: 10_000,
    };
    plan.lane = "native".into();
    plan.native_budgets = Some(budgets);
    plan.limits.duration_ms = budgets.total_ms().unwrap();
    plan.limits.readiness_ms = budgets.readiness_ms;
    let control = Arc::new(Control::new(&plan.limits));
    control.start_native(budgets, None).unwrap();
    let host = Host::new(plan, json!({}), BTreeMap::new(), control).unwrap();
    let (link, requests) = StartupLink::new();
    host.connect_startup(Arc::clone(&link)).unwrap();
    (host, link, requests)
}

#[test]
fn no_request_never_acquires_or_captures_and_clean_settlement_keeps_fresh_start_possible() {
    for _ in 0..2 {
        let (host, _, requests) = native_host();
        host.begin_readiness().unwrap();
        for _ in 0..3 {
            assert_eq!(
                host.call("target_status", json!({})).unwrap()["status"],
                "not_requested"
            );
        }
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            host.call("observe", json!({})).unwrap_err().category,
            "TargetNotReady"
        );
        assert_eq!(
            host.begin_workflow().unwrap_err().category,
            "ReadinessContract"
        );
        assert_eq!(host.snapshot()["native_initialization_started"], false);
        let cleanup = host.finish();
        assert_eq!(cleanup["clean"], true);
        assert!(
            cleanup.get("engine").is_none(),
            "no invented native release evidence"
        );
        assert_eq!(host.snapshot()["dispatches"], 0);
    }
}

#[test]
fn startup_is_readiness_only_empty_payload_and_one_shot() {
    let (host, _, requests) = native_host();
    for method in ["target_start", "target_status"] {
        assert_eq!(
            host.call(method, json!({})).unwrap_err().category,
            "AdmissionClosed"
        );
    }
    host.begin_readiness().unwrap();
    for method in ["target_start", "target_status"] {
        for payload in [
            Value::Null,
            json!([]),
            json!({"process_id":7}),
            json!({"launch_approved":true}),
        ] {
            assert_eq!(host.call(method, payload).unwrap_err().category, "Argument");
        }
    }
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        host.call("target_start", json!({})).unwrap()["status"],
        "pending"
    );
    assert_eq!(requests.try_recv().unwrap(), 1);
    assert_eq!(
        host.call("target_start", json!({})).unwrap_err().category,
        "NativeStartRefused"
    );
    assert_eq!(
        host.call("observe", json!({})).unwrap_err().category,
        "TargetNotReady"
    );
    assert_eq!(
        host.begin_workflow().unwrap_err().category,
        "ReadinessContract"
    );
    for _ in 0..3 {
        assert_eq!(
            host.call("target_status", json!({})).unwrap()["status"],
            "pending"
        );
    }
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        host.call(
            "submit",
            json!({"observation":{"width":8,"height":8},
        "actions":[{"kind":"click","x":1,"y":1,"button":"left"}]})
        )
        .unwrap_err()
        .category,
        "AdmissionClosed"
    );
    host.control().cancel();
    assert_eq!(
        host.call("target_status", json!({})).unwrap_err().category,
        "Cancelled"
    );
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn pending_responses_require_a_script_poll_and_typed_fault_closes_continuation() {
    let (host, link, requests) = native_host();
    host.begin_readiness().unwrap();
    host.call("target_start", json!({})).unwrap();
    assert_eq!(requests.try_recv().unwrap(), 1);
    let progress = NativeProgress {
        attempt: 1,
        status: NativeTargetStatus::Pending,
        phase: NativePhase::WaitingForProcess,
        launch: LaunchDisposition::Accepted,
    };
    link.receive(
        1,
        StartupReply {
            progress,
            configuration: None,
            fault: None,
        },
    )
    .unwrap();
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let pending = host.call("target_status", json!({})).unwrap();
    assert_eq!(pending["launch"], "accepted");
    assert_eq!(pending["phase"], "waiting_for_process");
    assert_eq!(requests.try_recv().unwrap(), 2);
    link.receive(
        2,
        StartupReply {
            progress,
            configuration: None,
            fault: Some(
                Fault::new("TargetLost", "selected lifetime exited")
                    .with_context(json!({"stage":"process_discovery"})),
            ),
        },
    )
    .unwrap();
    // No status call is necessary to latch the independently reported failure.
    assert_eq!(host.begin_workflow().unwrap_err().category, "TargetLost");
    assert_eq!(
        host.call("target_status", json!({})).unwrap_err().context["stage"],
        "process_discovery"
    );
    assert_eq!(host.snapshot()["native_launch"], "accepted");
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn non_native_and_unlinked_hosts_cannot_gain_startup_authority() {
    for lane in ["controlled", "replay"] {
        let mut host = crate::host::test_support::make_host("success", "template-first");
        // No backend operation is exercised: only the lane authority gate.
        Arc::get_mut(&mut host.inner).unwrap().plan.lane = lane.into();
        lock(&host.inner.state).phase = Phase::Readiness;
        for method in ["target_start", "target_status"] {
            assert_eq!(
                host.call(method, json!({})).unwrap_err().category,
                "Authority"
            );
        }
        assert_eq!(host.finish()["clean"], true);
    }
    let (mut host, _, _) = native_host();
    let _ = Arc::get_mut(&mut host.inner).unwrap().startup_link.take();
    host.begin_readiness().unwrap();
    assert_eq!(
        host.call("target_start", json!({})).unwrap_err().category,
        "Authority"
    );
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn settlement_retains_a_blocked_initializer_and_rejects_its_late_readiness() {
    struct OwnedRelease {
        release: Option<mpsc::Sender<()>>,
        host: Host,
    }
    impl Drop for OwnedRelease {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            let worker = lock(&self.host.inner.startup).worker.take();
            if let Some(worker) = worker {
                let _ = worker.join();
            }
        }
    }

    let (mut host, _, _) = native_host();
    Arc::get_mut(&mut host.inner)
        .unwrap()
        .plan
        .limits
        .cleanup_ms = 20;
    host.begin_readiness().unwrap();
    let (release, resume) = mpsc::channel();
    let (entered, started) = mpsc::channel();
    let (outcome, result) = mpsc::channel();
    let worker_host = host.clone();
    {
        let mut startup = lock(&host.inner.startup);
        startup.progress.status = NativeTargetStatus::Pending;
        startup.progress.phase = NativePhase::NativeInitialization;
        startup.worker = Some(thread::spawn(move || {
            entered.send(()).unwrap();
            let _ = resume.recv();
            outcome
                .send(worker_host.control().native_transition(1, None))
                .unwrap();
        }));
    }
    let owner = OwnedRelease {
        release: Some(release),
        host: host.clone(),
    };
    started
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    let cleanup = host.finish();
    assert_eq!(cleanup["clean"], false);
    assert_eq!(cleanup["remaining"]["in_flight_native"], 1);
    assert!(
        host.startup_active(),
        "logical settlement cannot detach the physical initializer"
    );
    assert!(!host.control().admission.load(Ordering::Acquire));
    drop(owner);
    assert_eq!(result.recv().unwrap().unwrap_err().category, "Cancelled");
    assert_eq!(host.snapshot()["native_status"], "pending");
    assert_eq!(
        host.finish()["clean"],
        true,
        "clean only after physical return"
    );
}

#[test]
fn late_initializer_cleanup_uncertainty_survives_an_earlier_script_fault() {
    let (host, _, _) = native_host();
    host.begin_readiness().unwrap();
    let (release, resume) = mpsc::channel();
    let worker_host = host.clone();
    lock(&host.inner.startup).worker = Some(thread::spawn(move || {
        resume.recv().unwrap();
        worker_host.fail(
            Fault::new("Native", "session acquisition rollback was not verified")
                .with_context(json!({"stage":"session_open","native_cleanup":"unverified"})),
        );
    }));
    host.fail(Fault::new(
        "TargetNotReady",
        "capture was requested too early",
    ));
    release.send(()).unwrap();
    let cleanup = host.finish();
    assert_eq!(host.failure().unwrap().category, "TargetNotReady");
    assert_eq!(cleanup["remaining"]["in_flight_native"], 0);
    assert_eq!(cleanup["clean"], false);
    assert_eq!(host.finish()["clean"], false);
}

#[test]
fn late_initializer_panic_survives_an_earlier_stop_without_replacing_it() {
    let (host, _, _) = native_host();
    host.begin_readiness().unwrap();
    let (release, resume) = mpsc::channel();
    lock(&host.inner.startup).worker = Some(thread::spawn(move || {
        resume.recv().unwrap();
        panic!("initializer panicked after Stop");
    }));
    host.control().cancel();
    host.fail(host.control().check().unwrap_err());
    release.send(()).unwrap();
    let cleanup = host.finish();
    assert_eq!(host.failure().unwrap().category, "Cancelled");
    assert_eq!(cleanup["remaining"]["in_flight_native"], 0);
    assert_eq!(cleanup["clean"], false);
}

#[test]
fn initializer_refused_before_sdk_entry_does_not_claim_sdk_admission() {
    let (host, _, _) = native_host();
    host.begin_readiness().unwrap();
    {
        let mut startup = lock(&host.inner.startup);
        startup.target_bound = true;
        startup.configuration = Some(json!({}));
        startup.progress.status = NativeTargetStatus::Pending;
    }
    host.schedule_target_probe().unwrap();
    let worker = lock(&host.inner.startup).worker.take().unwrap();
    worker.join().unwrap();
    assert_eq!(host.failure().unwrap().category, "Blocked");
    assert_eq!(host.snapshot()["native_initialization_started"], false);
    assert_eq!(host.finish()["clean"], true);
}
