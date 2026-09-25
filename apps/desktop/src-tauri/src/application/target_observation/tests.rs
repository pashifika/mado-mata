use super::*;
use crate::application::test_support::*;
use serde_json::json;

fn observed() -> ApplicationObservation {
    ApplicationObservation {
        observed_at_ms: 1,
        status: "matched".into(),
        evidence: Some("signed_application".into()),
        diagnostics: json!({"private": "not logged"}),
    }
}

fn pending(
    application: &Application,
) -> (
    WorkspaceRef,
    PendingObservation,
    Arc<AtomicBool>,
    mpsc::SyncSender<Result<ApplicationObservation, Fault>>,
) {
    let selection = inspect_named(application, "Observed", &package_path()).unwrap();
    let owner = workspace_ref(&selection);
    let selected = lock(&application.workspaces)
        .resolve(&owner)
        .unwrap()
        .clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let (send, receive) = mpsc::sync_channel(1);
    lock(&application.target_observation).active = Some(ObservationWork {
        workspace: owner.clone(),
        request_id: "request-1".into(),
        cancelled: cancelled.clone(),
        finished: finished.clone(),
        result: Some(send.clone()),
    });
    let pending = PendingObservation {
        selected,
        expected: TargetExpectation {
            revision: 0,
            binding_id: None,
        },
        request_id: "request-1".into(),
        cancelled,
        deadline: Instant::now() + OBSERVATION_TIMEOUT,
        result: receive,
    };
    (owner, pending, finished, send)
}

#[test]
fn cancellation_discards_queued_success_but_retains_the_physical_worker_slot() {
    let fixture = Fixture::new();
    let (owner, pending, finished, send) = pending(&fixture.application);
    let (release, gate) = mpsc::sync_channel::<()>(0);
    let (queued, ready) = mpsc::sync_channel(0);
    let worker = std::thread::spawn(move || {
        let _finished = WorkerFinished(finished);
        send.send(Ok(observed())).unwrap();
        queued.send(()).unwrap();
        gate.recv().unwrap();
    });
    ready.recv().unwrap();
    assert!(
        !fixture
            .application
            .cancel_running_application(&owner, "different-request")
    );
    assert!(
        fixture
            .application
            .cancel_running_application(&owner, "request-1")
    );
    assert_eq!(
        fixture
            .application
            .finish_running_application(pending)
            .unwrap_err()
            .category,
        "TargetObservationCancelled"
    );
    assert_eq!(
        lock(&fixture.application.target_observation)
            .available()
            .unwrap_err()
            .category,
        "TargetObservationBusy"
    );
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(
        lock(&fixture.application.target_observation)
            .available()
            .is_ok()
    );
    assert!(fixture.application.runner.poll().run.is_none());
}

#[test]
fn a_result_queued_at_the_publication_deadline_is_not_success() {
    let fixture = Fixture::new();
    let (_, mut pending, _, send) = pending(&fixture.application);
    pending.deadline = Instant::now();
    send.send(Ok(observed())).unwrap();
    assert_eq!(
        fixture
            .application
            .finish_running_application(pending)
            .unwrap_err()
            .category,
        "TargetObservationTimeout"
    );
    let slot = lock(&fixture.application.target_observation);
    assert!(
        slot.active
            .as_ref()
            .unwrap()
            .cancelled
            .load(Ordering::Acquire)
    );
    assert_eq!(
        slot.available().unwrap_err().category,
        "TargetObservationBusy"
    );
}

#[test]
fn reinspection_invalidates_an_outstanding_observation_without_starting_a_run() {
    let fixture = Fixture::new();
    let (owner, pending, _, send) = pending(&fixture.application);
    let replacement = fixture
        .application
        .inspect(&package_path(), &owner)
        .unwrap();
    assert_ne!(replacement.workspace.revision, owner.revision);
    let _ = send.try_send(Ok(observed()));
    assert_eq!(
        fixture
            .application
            .finish_running_application(pending)
            .unwrap_err()
            .category,
        "TargetObservationCancelled"
    );
    assert!(fixture.application.runner.poll().run.is_none());
}

#[test]
fn closure_and_shutdown_invalidate_without_waiting_for_the_os_worker() {
    for shutdown in [false, true] {
        let fixture = Fixture::new();
        let (owner, pending, finished, _) = pending(&fixture.application);
        if shutdown {
            fixture.application.shutdown().unwrap();
        } else {
            fixture.application.close_workspace(&owner).unwrap();
        }
        assert!(!finished.load(Ordering::Acquire));
        assert_eq!(
            fixture
                .application
                .finish_running_application(pending)
                .unwrap_err()
                .category,
            "TargetObservationCancelled"
        );
        assert_eq!(
            lock(&fixture.application.target_observation)
                .available()
                .unwrap_err()
                .category,
            "TargetObservationBusy"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn picker_admission_excludes_run_and_ocr_until_guard_release() {
    let fixture = Fixture::new();
    let selected = inspect_named(&fixture.application, "Picker", &package_path()).unwrap();
    let owner = workspace_ref(&selected);
    let guard = fixture.application.begin_target_picker(&owner).unwrap();
    assert_eq!(
        fixture
            .application
            .start(&owner, request(&selected))
            .unwrap_err()
            .category,
        "TargetPickerBusy"
    );
    assert_eq!(
        fixture
            .application
            .check_environment(None, None)
            .unwrap_err()
            .category,
        "TargetPickerBusy"
    );
    assert!(fixture.application.runner.poll().run.is_none());
    drop(guard);
    assert!(lock(&fixture.application.workspaces).idle().is_ok());
}

#[test]
fn publication_does_not_wait_on_a_later_operations_store_lock() {
    let fixture = Fixture::new();
    let (_, pending, _, send) = pending(&fixture.application);
    send.send(Ok(observed())).unwrap();
    let store = lock(&fixture.application.store);
    let application = fixture.application.clone();
    let (reported, result) = mpsc::sync_channel(1);
    let waiter = std::thread::spawn(move || {
        reported
            .send(application.finish_running_application(pending))
            .unwrap();
    });
    let received = result.recv_timeout(Duration::from_secs(1));
    drop(store);
    waiter.join().unwrap();
    assert_eq!(
        received.unwrap().unwrap_err().category,
        "TargetObservationBusy"
    );
    assert!(fixture.application.runner.poll().run.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn cancelled_or_superseded_reservations_cannot_start_an_os_read() {
    let fixture = Fixture::new();
    let selected = inspect_named(&fixture.application, "Reservation", &package_path()).unwrap();
    let owner = workspace_ref(&selected);
    let expected = TargetExpectation {
        revision: 0,
        binding_id: None,
    };
    assert_eq!(
        fixture
            .application
            .check_running_application(&owner, &expected, "unreserved")
            .unwrap_err()
            .category,
        "TargetObservationCancelled"
    );
    fixture
        .application
        .reserve_running_application(&owner, "cancelled")
        .unwrap();
    assert!(
        fixture
            .application
            .cancel_running_application(&owner, "cancelled")
    );
    assert_eq!(
        fixture
            .application
            .check_running_application(&owner, &expected, "cancelled")
            .unwrap_err()
            .category,
        "TargetObservationCancelled"
    );
    fixture
        .application
        .reserve_running_application(&owner, "old")
        .unwrap();
    fixture
        .application
        .reserve_running_application(&owner, "new")
        .unwrap();
    assert_eq!(
        fixture
            .application
            .check_running_application(&owner, &expected, "old")
            .unwrap_err()
            .category,
        "TargetObservationCancelled"
    );
    assert!(
        fixture
            .application
            .cancel_running_application(&owner, "new")
    );
    let slot = lock(&fixture.application.target_observation);
    let reservation = slot.active.as_ref().unwrap();
    assert!(reservation.finished.load(Ordering::Acquire));
    assert!(reservation.result.is_none());
    assert!(fixture.application.runner.poll().run.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn reconstruction_retains_occupancy_until_the_previous_worker_returns() {
    let fixture = Fixture::new();
    fixture.application.shutdown().unwrap();
    let bootstrap = crate::bootstrap::Bootstrap::new(
        Ok(fixture.root.clone()),
        None,
        fixture.root.join("runner"),
        fixture.root.join("engine"),
    );
    bootstrap.ensure_started().unwrap();
    let previous = bootstrap.application().unwrap();
    let (_, _pending, finished, _) = pending(&previous);
    let (release, gate) = mpsc::sync_channel::<()>(0);
    let physical = finished.clone();
    let worker = std::thread::spawn(move || {
        let _finished = WorkerFinished(physical);
        let _ = gate.recv();
    });
    bootstrap.retry(true).unwrap();
    let current = bootstrap.application().unwrap();
    let selected = inspect_named(&current, "Replacement", &package_path()).unwrap();
    let owner = workspace_ref(&selected);
    let refused = current.reserve_running_application(&owner, "next");
    let premature_release = finished.load(Ordering::Acquire);
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(!premature_release);
    assert_eq!(refused.unwrap_err().category, "TargetObservationBusy");
    current.reserve_running_application(&owner, "next").unwrap();
    assert!(current.cancel_running_application(&owner, "next"));
    current.shutdown().unwrap();
}

#[test]
fn application_observation_logs_outcomes_without_private_evidence() {
    for (status, stage, category) in [
        ("matched", "publication", None),
        ("refused", "observation", Some("TargetObservationEvidence")),
        (
            "cancelled",
            "observation",
            Some("TargetObservationCancelled"),
        ),
        ("timeout", "observation", Some("TargetObservationTimeout")),
    ] {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let (owner, mut pending, _, send) = pending(application);
        if status == "refused" {
            send.send(Err(Fault::new(
                "TargetObservationEvidence",
                "/private/runtime/path",
            )
            .with_context(json!({"stage": "/private/runtime/path", "pid": 4321, "signature": "private-signature"}))))
                .unwrap();
        } else {
            send.send(Ok(observed())).unwrap();
        }
        if status == "cancelled" {
            assert!(application.cancel_running_application(&owner, "request-1"));
        } else if status == "timeout" {
            pending.deadline = Instant::now();
        }
        let result = application.finish_running_application(pending);
        assert_eq!(
            result.as_ref().err().map(|error| error.category.as_str()),
            category
        );
        let entries = application.poll().logs.entries;
        let record = entries
            .iter()
            .find(|entry| entry.fields["action"] == "check_running_application")
            .expect("application observation must leave a routine outcome");
        assert_eq!(
            record.workspace_id.as_deref(),
            Some(owner.workspace_id.as_str())
        );
        assert_eq!(
            record.fields,
            json!({
                "action": "check_running_application",
                "status": status,
                "stage": stage,
                "category": category,
            })
        );
        assert!(
            !serde_json::to_string(record)
                .unwrap()
                .contains("/private/runtime/path")
        );
        assert!(
            !serde_json::to_string(record)
                .unwrap()
                .contains("private-signature")
        );
    }
}

#[test]
fn application_observation_admission_refusals_are_attributed() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let selected = inspect_named(application, "Admission", &package_path()).unwrap();
    let owner = workspace_ref(&selected);
    assert_eq!(
        application
            .reserve_running_application(&owner, "invalid/request")
            .unwrap_err()
            .category,
        "TargetObservationRequest",
    );
    assert_eq!(
        application
            .check_running_application(
                &owner,
                &TargetExpectation {
                    revision: 0,
                    binding_id: None
                },
                "unreserved",
            )
            .unwrap_err()
            .category,
        "TargetObservationCancelled",
    );
    let entries = application.poll().logs.entries;
    let records: Vec<_> = entries
        .iter()
        .filter(|entry| entry.fields["action"] == "check_running_application")
        .collect();
    assert_eq!(records.len(), 2);
    for (record, status, category) in [
        (records[0], "refused", "TargetObservationRequest"),
        (records[1], "cancelled", "TargetObservationCancelled"),
    ] {
        assert_eq!(
            record.workspace_id.as_deref(),
            Some(owner.workspace_id.as_str())
        );
        assert_eq!(
            record.fields,
            json!({
                "action": "check_running_application",
                "status": status,
                "stage": "admission",
                "category": category,
            })
        );
    }
    assert!(application.runner.poll().run.is_none());
}
