use super::*;
use crate::model::{Plan, StopReason};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn control() -> Arc<Control> {
    let plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    Arc::new(Control::new(&plan.limits))
}

fn completed(preparation: &NativePreparation) -> StartupReply {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(reply) = preparation.poll() {
            return reply;
        }
        assert!(Instant::now() < deadline, "probe did not physically return");
        thread::yield_now();
    }
}

#[test]
fn requests_own_one_probe_and_pending_does_not_schedule_a_successor() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let (entered, arrival) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let preparation = NativePreparation::new(control(), 1, move |_, _| {
        let count = observed.fetch_add(1, Ordering::AcqRel);
        if count == 0 {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        Ok((count == 1).then(|| serde_json::json!({"native":"fixed-host-target"})))
    });
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert_eq!(
        preparation.progress().status,
        NativeTargetStatus::NotRequested
    );
    assert!(preparation.poll().is_none());
    preparation.request().unwrap();
    arrival.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(preparation.request().is_err());
    assert!(preparation.poll().is_none());
    release.send(()).unwrap();
    let pending = completed(&preparation);
    assert!(pending.configuration.is_none());
    assert!(pending.fault.is_none());
    assert_eq!(pending.progress.status, NativeTargetStatus::Pending);
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert!(preparation.poll().is_none());
    preparation.request().unwrap();
    assert!(completed(&preparation).configuration.is_some());
    assert!(
        preparation.request().is_err(),
        "an exact target cannot be replaced"
    );
    assert_eq!(calls.load(Ordering::Acquire), 2);
}

#[test]
fn typed_failure_is_terminal_not_another_pending_probe() {
    let preparation = NativePreparation::new(control(), 1, |_, report| {
        report(NativeProgress {
            attempt: 1,
            status: NativeTargetStatus::Pending,
            phase: NativePhase::LaunchSubmission,
            launch: LaunchDisposition::Uncertain,
        });
        Err(Fault::new(
            "NativeLaunchUncertain",
            "submission may have reached the OS",
        ))
    });
    preparation.request().unwrap();
    let reply = completed(&preparation);
    assert_eq!(reply.fault.unwrap().category, "NativeLaunchUncertain");
    assert_eq!(reply.progress.launch, LaunchDisposition::Uncertain);
    assert!(reply.configuration.is_none());
    assert!(preparation.request().is_err());
    assert_eq!(
        preparation.settle().unwrap().category,
        "NativeLaunchUncertain"
    );
}

#[test]
fn stop_retains_physical_callback_and_late_launch_disposition_without_late_target() {
    let control = control();
    let (entered, arrival) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let preparation = Arc::new(NativePreparation::new(
        Arc::clone(&control),
        1,
        move |control, report| {
            control.admit_launch()?;
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            report(NativeProgress {
                attempt: 1,
                status: NativeTargetStatus::Pending,
                phase: NativePhase::LaunchSubmission,
                launch: LaunchDisposition::Accepted,
            });
            Ok(Some(serde_json::json!({"native":"late-target"})))
        },
    ));
    preparation.request().unwrap();
    arrival.recv_timeout(Duration::from_secs(5)).unwrap();
    control.cancel();
    assert!(preparation.poll().is_none());
    assert_eq!(preparation.request().unwrap_err().category, "Cancelled");
    let owned = Arc::clone(&preparation);
    let (settled, result) = mpsc::sync_channel(1);
    let join = thread::spawn(move || settled.send(owned.settle()).unwrap());
    assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release.send(()).unwrap();
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .category,
        "Cancelled"
    );
    join.join().unwrap();
    assert_eq!(preparation.progress().launch, LaunchDisposition::Accepted);
    assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
}

#[test]
fn cancellation_before_the_first_request_never_enters_the_resolver() {
    let control = control();
    let preparation = NativePreparation::new(Arc::clone(&control), 1, |_, _| {
        panic!("no startup authority")
    });
    control.cancel();
    assert_eq!(preparation.request().unwrap_err().category, "Cancelled");
    assert_eq!(
        preparation.progress().status,
        NativeTargetStatus::NotRequested
    );
    assert!(preparation.settle().is_none());
}

#[test]
fn a_consumed_probe_panic_still_prevents_a_clean_preparation_settlement() {
    let preparation = NativePreparation::new(control(), 1, |_, _| panic!("initializer fault"));
    preparation.request().unwrap();
    let fault = completed(&preparation).fault.unwrap();
    assert_eq!(fault.category, "Controller");
    assert_eq!(fault.context["native_cleanup"], "unverified");
    assert!(preparation.request().is_err());
    let retained = preparation.settle().unwrap();
    assert_eq!(retained.context["native_cleanup"], "unverified");
}

#[test]
fn fresh_preparation_owns_its_identity_and_does_not_inherit_one_shot_state() {
    for attempt in [1, 2] {
        let preparation = NativePreparation::new(control(), attempt, |_, report| {
            report(NativeProgress {
                attempt: 1,
                status: NativeTargetStatus::Pending,
                phase: NativePhase::WaitingForProcess,
                launch: LaunchDisposition::NotRequested,
            });
            Ok(Some(serde_json::json!({"captured":"unchanged"})))
        });
        assert_eq!(preparation.progress().attempt, attempt);
        assert_eq!(
            preparation.progress().status,
            NativeTargetStatus::NotRequested
        );
        preparation.request().unwrap();
        let reply = completed(&preparation);
        assert_eq!(reply.progress.attempt, attempt);
        assert_eq!(reply.configuration.unwrap()["captured"], "unchanged");
        assert!(preparation.request().is_err());
        assert!(preparation.settle().is_none());
    }
}
