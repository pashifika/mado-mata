use super::super::clock::SharedDeadline;
use super::*;
use crate::model::{Limits, Plan};
use std::io::Cursor;
use std::sync::mpsc;

fn command(reason: &str) -> Vec<u8> {
    format!("{{\"command\":\"{reason}\",\"run\":\"owner\",\"attempt\":1}}\n").into_bytes()
}

fn limits() -> Limits {
    serde_json::from_str::<Plan>(include_str!("../../../fixtures/manual-plan.json"))
        .unwrap()
        .limits
}

fn adopt(control: &Control, wire: Vec<u8>, run: &str) -> &'static str {
    adopt_stop(control, frame(&mut Cursor::new(wire), 1024), run, 1)
}

/// A child whose payload transfer and validation outlasted the shared deadline:
/// the supervisor's absolute bound is already elapsed when the Control exists.
fn late_child_control() -> Control {
    let elapsed = Instant::now() - Duration::from_millis(1);
    let wire = SharedDeadline::from_instant(elapsed).unwrap();
    let control = Control::with_deadline(&limits(), wire.instant().unwrap());
    assert_eq!(control.check().unwrap_err().category, "Timeout");
    control
}

/// A child whose first deadline reading passed and whose deadline then elapsed
/// during initialization, as during build identity collection or `Host::new`.
/// Returns the Timeout that initialization captured from that reading.
fn control_expiring_after_first_check() -> (Control, Fault) {
    let deadline = Instant::now() + Duration::from_millis(200);
    let control = Control::with_deadline(&limits(), deadline);
    assert!(
        control.check().is_ok(),
        "first reading precedes the deadline"
    );
    // Clock progress, not thread ordering: the deadline has provably passed
    // before the next reading, whatever the scheduler did meanwhile.
    thread::sleep(deadline.saturating_duration_since(Instant::now()));
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
    let provisional = control.check().unwrap_err();
    assert_eq!(provisional.category, "Timeout");
    (control, provisional)
}

#[test]
fn provisional_timeout_after_a_passing_first_check_publishes_the_earlier_accepted_stop() {
    let (control, provisional) = control_expiring_after_first_check();
    let control = Arc::new(control);
    let closed_at = control.stop_us.load(Ordering::Acquire);
    let verdict = Arc::new(OnceLock::new());
    let (entered, publication) = mpsc::sync_channel(1);
    let publisher = {
        let control = control.clone();
        let verdict = verdict.clone();
        thread::spawn(move || {
            entered.send(()).unwrap();
            settled_primary(&control, &verdict, provisional)
        })
    };
    // The supervisor accepted Stop before the deadline; its frame was queued
    // behind the payload and is adopted only after publication has started.
    publication.recv().unwrap();
    assert_eq!(adopt(&control, command("Stop"), "owner"), "Stop");
    verdict.set(()).unwrap();
    let published = publisher.join().unwrap();
    assert_eq!(published.category, "Cancelled");
    assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
    assert_eq!(control.admit_launch().unwrap_err().category, "Cancelled");
    // Reattribution never reopens admission or moves the first closure time.
    assert_eq!(control.stop_us.load(Ordering::Acquire), closed_at);
    assert!(control.cancelled.load(Ordering::Acquire));
    assert!(!control.admission.load(Ordering::Acquire));
}

#[test]
fn provisional_timeout_keeps_its_stage_context_when_reattributed() {
    let control = late_child_control();
    let staged = control
        .check()
        .unwrap_err()
        .with_context(json!({"stage":"waiting_for_window"}));
    adopt(&control, command("Stop"), "owner");
    let verdict = OnceLock::from(());
    let published = settled_primary(&control, &verdict, staged);
    assert_eq!(published.category, "Cancelled");
    assert_eq!(published.context["stage"], "waiting_for_window");
}

#[test]
fn elapsed_deadline_without_an_accepted_stop_publishes_the_timeout() {
    let (control, provisional) = control_expiring_after_first_check();
    assert_eq!(adopt(&control, command("Timeout"), "owner"), "Timeout");
    let published = settled_primary(&control, &OnceLock::from(()), provisional.clone());
    assert_eq!(published.category, "Timeout");
    assert_eq!(published.message, provisional.message);
    assert!(published.context.is_null());
    assert_eq!(control.admit_launch().unwrap_err().category, "Timeout");
    // A lost or foreign control channel asserts nothing on the supervisor's behalf.
    for (wire, run, label) in [
        (Vec::new(), "owner", "ControlLost"),
        (command("Stop"), "other-run", "InvalidControl"),
    ] {
        let control = late_child_control();
        let provisional = control.check().unwrap_err();
        assert_eq!(adopt(&control, wire, run), label);
        assert_eq!(control.stop_reason(), Some(StopReason::Timeout));
        let published = settled_primary(&control, &OnceLock::from(()), provisional);
        assert_eq!(published.category, "Timeout");
        assert!(published.context.is_null());
    }
}

#[test]
fn observed_faults_publish_without_waiting_or_reattribution() {
    // A resource fault is evidence in its own right; the verdict is not awaited.
    let (control, _) = control_expiring_after_first_check();
    let resource = Fault::new("Profile", "selected profile is not in the inventory");
    let published = settled_primary(&control, &OnceLock::new(), resource.clone());
    assert_eq!(published.category, resource.category);
    assert_eq!(published.message, resource.message);
    adopt(&control, command("Stop"), "owner");
    let published = settled_primary(&control, &OnceLock::from(()), resource.clone());
    assert_eq!(published.category, "Profile");
    // A stage Timeout is already earned evidence, even if the operation deadline
    // expires and the supervisor's Stop supersedes that separate derived cause.
    let readiness = Fault::new("Timeout", "readiness deadline expired")
        .with_context(json!({"stage":"readiness"}));
    let control = late_child_control();
    assert_eq!(adopt(&control, command("Stop"), "owner"), "Stop");
    assert_eq!(control.superseding_verdict(), Some(StopReason::Cancelled));
    let published = settled_primary(&control, &OnceLock::new(), readiness);
    assert_eq!(published.category, "Timeout");
    assert_eq!(published.context["stage"], "readiness");
}

#[test]
fn supervisor_verdict_is_first_cause_for_a_child_that_initialized_in_time() {
    let limits = limits();
    let control = Control::with_deadline(
        &limits,
        SharedDeadline::from_instant(Instant::now() + Duration::from_secs(5))
            .unwrap()
            .instant()
            .unwrap(),
    );
    assert!(control.check().is_ok());
    adopt(&control, command("Stop"), "owner");
    assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
    let lost = Control::with_deadline(&limits, Instant::now() + Duration::from_secs(5));
    adopt(&lost, Vec::new(), "owner");
    assert_eq!(lost.stop_reason(), Some(StopReason::Cancelled));
}

#[test]
fn preflight_unverified_native_cleanup_is_independent_of_cache_release() {
    let primary = Fault::new("NativeSession", "session opening failed")
        .with_context(json!({"native_cleanup":"unverified"}));
    let released = preflight_cleanup(&primary, Ok(()));
    assert_eq!(released["clean"], false);
    assert_eq!(released["status"], "IncompleteCleanup");
    assert_eq!(released["native_cleanup"], "unverified");
    let retained = preflight_cleanup(
        &primary,
        Err(Fault::new("RunnerResources", "retained owner")),
    );
    assert_eq!(retained["clean"], false);
    assert_eq!(retained["runner_release"]["category"], "RunnerResources");
    assert_eq!(retained["native_cleanup"], "unverified");
}

#[test]
fn preflight_cleanup_never_parses_human_diagnostics() {
    let primary = Fault::new("Blocked", "native_cleanup unverified; rollback incomplete");
    assert_eq!(preflight_cleanup(&primary, Ok(()))["clean"], true);
    assert_eq!(
        preflight_cleanup(
            &primary,
            Err(Fault::new("RunnerResources", "still retained"))
        )["clean"],
        false
    );
}
