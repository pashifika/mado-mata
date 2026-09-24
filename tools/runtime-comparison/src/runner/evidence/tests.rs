use super::*;

#[test]
fn saturated_observer_logs_do_not_consume_terminal_delivery() {
    let (progress, events) = mpsc::sync_channel(1);
    let (logs, messages) = mpsc::sync_channel(1);
    let observer = Observer {
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
