use super::*;

#[test]
fn saturated_observer_logs_do_not_consume_terminal_delivery() {
    let (progress, events) = mpsc::sync_channel(1);
    let (logs, messages) = mpsc::sync_channel(1);
    let observer = Observer {
        native_preparation: Default::default(),
        attempts: Default::default(),
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
    };
    observer.log(json!({"message":"retained"}));
    observer.log(json!({"message":"overflow"}));
    observer.progress(&json!({"event":"Terminal","run":"owned","attempt":1}));
    assert_eq!(events.try_recv().unwrap()["event"], "Terminal");
    assert_eq!(messages.try_recv().unwrap()["message"], "retained");
    assert_eq!(observer.dropped_logs.load(Ordering::Relaxed), 1);
}

#[test]
fn fresh_attempt_logs_and_probes_are_fenced_by_the_actual_attempt() {
    let (progress, _) = mpsc::sync_channel(1);
    let (logs, messages) = mpsc::sync_channel(4);
    let observer = Observer {
        native_preparation: Default::default(),
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
        attempts: Default::default(),
    };
    let values = [
        json!({"event":"ScriptLog","run":"same-run","attempt":1,"sequence":1}),
        json!({"event":"ScriptLog","run":"same-run","attempt":2,"sequence":1}),
        json!({"event":"TargetProbe","run":"same-run","attempt":2,"sequence":1}),
    ];
    let bytes = values
        .into_iter()
        .map(|value| format!("{value}\n"))
        .collect::<String>();
    let (events, reader) = receive_frames(
        std::io::Cursor::new(bytes),
        Some(&observer),
        "same-run",
        2,
        4,
        MAX_TRANSPORT_BYTES,
    );
    reader.join().unwrap();
    assert_eq!(messages.try_recv().unwrap()["attempt"], 2);
    assert!(messages.try_recv().is_err());
    let retained: Vec<_> = events.try_iter().map(Result::unwrap).collect();
    assert_eq!(retained.len(), 2);
    assert_eq!(
        retained[0]["attempt"], 1,
        "foreign evidence remains a supervisor fault, not an actionable log"
    );
    assert_eq!(retained[1]["event"], "TargetProbe");
}

#[test]
fn current_attempt_progress_survives_saturated_delivery_and_old_callbacks() {
    let (progress, _) = mpsc::sync_channel(1);
    let (logs, _) = mpsc::sync_channel(1);
    let observer = Observer {
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
        attempts: Default::default(),
        native_preparation: Default::default(),
    };
    for (attempt, phase, status, launch) in [
        (1, "workflow", "capture_ready", "accepted"),
        (1, "settling", "capture_ready", "not_requested"),
        (1, "readiness", "pending", "not_requested"),
        (2, "preflight", "not_requested", "not_requested"),
        (1, "workflow", "capture_ready", "accepted"),
    ] {
        observer.progress(&json!({"event":"NativePreparation","attempt":attempt,
            "phase":phase,"status":status,"launch":launch}));
    }
    let retained = observer
        .native_preparation
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .expect("retained progress");
    assert_eq!(retained.attempt, 2);
    assert_eq!(retained.phase, NativePhase::Preflight);
    assert_eq!(retained.status, NativeTargetStatus::NotRequested);
    assert_eq!(retained.launch, LaunchDisposition::NotRequested);
}
