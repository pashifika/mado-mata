use super::*;
use crate::model::{NativeBudgets, NativeWorkflow};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

struct ProtocolChild(PathBuf);

impl Drop for ProtocolChild {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn protocol_child(body: &str) -> (ProtocolChild, PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = ProtocolChild(std::env::temp_dir().join(format!(
        "mado-settlement-protocol-{}-{nonce}",
        std::process::id()
    )));
    fs::create_dir(&fixture.0).unwrap();
    let executable = fixture.0.join("child.py");
    // No engine, native target, capture or input.
    let preamble = r#"#!/usr/bin/env python3
import json, os, pathlib, sys, time
root = pathlib.Path(__file__).parent
header = json.loads(sys.stdin.buffer.readline())
remaining = sum(header['asset_lengths'].values())
while remaining:
    chunk = sys.stdin.buffer.read(remaining)
    if not chunk:
        sys.exit(2)
    remaining -= len(chunk)
invocation = header['invocation']
def emit(event, **fields):
    print(json.dumps(dict(event=event, run=invocation['run'], attempt=invocation['attempt'], **fields)), flush=True)
def wait(name):
    deadline = time.monotonic() + 5
    while not (root / name).exists():
        if time.monotonic() >= deadline:
            sys.exit(3)
        time.sleep(0.002)
"#;
    fs::write(&executable, [preamble, body].concat()).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    (fixture, executable)
}

#[test]
fn authenticated_entry_settlement_stops_stage_polling_before_child_reaping() {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.candidate = "javascript".into();
    plan.limits.duration_ms = 30_000;
    let inventory = Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &plan.limits,
    )
    .unwrap();
    let (fixture, executable) = protocol_child(
        r#"
emit('ChildStarted', pid=os.getpid(), build={})
wait('enter')
emit('EntrySettled', primary=None, entry_outcome='Returned')
wait('finish')
emit('Terminal', primary=None, entry_outcome='Returned', cleanup={'clean': True}, observations={})
"#,
    );
    let owner = Arc::new(Control::new(&plan.limits));
    let control = Arc::new(Control::for_attempt(Arc::clone(&owner), true));
    control
        .start_native(
            NativeBudgets {
                startup_ms: 5_000,
                readiness_ms: 5_000,
                workflow: crate::model::NativeWorkflow::Finite { workflow_ms: 150 },
            },
            None,
        )
        .unwrap();
    let original_deadline = owner.outer_deadline();
    let (progress, events) = mpsc::sync_channel(32);
    let (logs, _) = mpsc::sync_channel(1);
    let observer = Observer {
        progress,
        logs,
        dropped_logs: Default::default(),
        attempts: Default::default(),
        native_preparation: Default::default(),
    };
    let record = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run_prepared_with_executable(
                &executable,
                &plan,
                &inventory,
                super::super::PreparedExecution {
                    control: &control,
                    modules: None,
                    images: None,
                    startup: None,
                    identity: Some(("settlement-protocol", 1)),
                },
                &observer,
            )
        });
        let started = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(started["event"], "ChildStarted");
        control.native_transition(1, None).unwrap();
        let workflow = control.native_transition(2, None).unwrap().unwrap();
        fs::write(fixture.0.join("enter"), b"").unwrap();
        let settled = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(settled["event"], "EntrySettled");
        thread::sleep(
            workflow.saturating_duration_since(Instant::now()) + Duration::from_millis(10),
        );
        assert!(control.check().is_ok());
        assert_eq!(control.outer_deadline(), original_deadline);
        assert_eq!(control.deadline_opt(), original_deadline);
        fs::write(fixture.0.join("finish"), b"").unwrap();
        worker.join().unwrap().unwrap()
    });
    assert_eq!(record.status, "PASS");
    assert_eq!(record.cleanup["clean"], true);
    assert_eq!(record.observations["terminal_accounted"], true);
    assert_eq!(record.observations["child_reaped"], true);
    assert!(!record.forced);
    assert_eq!(record.exit_code, Some(0));
    assert!(owner.check().is_ok());
}

#[test]
fn continuous_stop_primary_follows_authority_time_not_settlement_delivery() {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.candidate = "javascript".into();
    plan.lane = "native".into();
    let inventory = Arc::new(
        Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &plan.limits,
        )
        .unwrap(),
    );
    let modules = crate::typescript::PreparedModules::capture(
        &inventory,
        &plan.limits,
        &Arc::new(Control::new(&plan.limits)),
    )
    .unwrap();
    let (fixture, executable) = protocol_child(
        r#"
emit('ChildStarted', pid=os.getpid(), build={})
(root / 'ready').touch()

# Hold both transitions and settlement until the supervisor has sent Stop.
command = json.loads(sys.stdin.buffer.readline())
assert command['command'] in ('Stop', 'Timeout')
settlement = json.loads((root / 'settlement.json').read_text())
emit('NativeTransition', phase=1, deadline=invocation['startup_deadline'])
phase = settlement['second_phase']
emit('NativeTransition', phase=phase,
     deadline=None if phase == 2 else invocation['startup_deadline'])
finished = settlement['finished']
if command['command'] == 'Timeout':
    finished = invocation['deadline'] + settlement['deadline_offset']
entry = dict(entry_outcome='Returned', primary=settlement['primary'],
             workflow_finished_clock_us=finished)
emit('EntrySettled', **entry)
wait('reap')
if not settlement['terminal']:
    sys.exit(124)
emit('Terminal', **entry, cleanup={'clean': True})
"#,
    );
    // The supervisor only configures this loader path; the protocol fixture
    // never opens a library or issues a preparation request.
    plan.native_config = Some(json!({
        "version":1, "replay":null, "native":null, "native_libraries":[],
        "ocr":{"model":"","profile":"","language":"","provider":"","runtime_profile":"",
            "model_root":fixture.0,
            "runtime":{"path":fixture.0.join("unused"),"sha256":"","bytes":0}}
    }));

    for (cause, returned_first, terminal, second_phase, child_fault, expected_primary) in [
        (StopReason::Cancelled, true, true, 2, None, None),
        (
            StopReason::Cancelled,
            false,
            true,
            2,
            None,
            Some("Cancelled"),
        ),
        (StopReason::Timeout, true, true, 2, None, None),
        (StopReason::Timeout, false, true, 2, None, Some("Timeout")),
        (
            StopReason::Cancelled,
            false,
            true,
            2,
            Some("TargetExited"),
            Some("TargetExited"),
        ),
        (StopReason::Timeout, true, false, 2, None, Some("Timeout")),
        (
            StopReason::Cancelled,
            true,
            true,
            1,
            None,
            Some("Transport"),
        ),
    ] {
        let overall_ms = (cause == StopReason::Timeout).then_some(90_000);
        let budgets = NativeBudgets {
            workflow: NativeWorkflow::Continuous { overall_ms },
            ..NativeBudgets::DEFAULT
        };
        plan.native_budgets = Some(budgets);
        plan.limits.duration_ms = budgets.preparation_ms().unwrap();
        plan.limits.readiness_ms = budgets.readiness_ms;
        let owner = Arc::new(Control::reserved(overall_ms).unwrap());
        let control = Arc::new(Control::for_attempt(Arc::clone(&owner), true));
        control.start_native(budgets, None).unwrap();
        let startup = super::super::NativePreparation::new(Arc::clone(&control), 1, |_, _| {
            panic!("protocol fixture must not request target preparation")
        });
        let ready = fixture.0.join("ready");
        let mut requested = false;
        let mut poll = || {
            if !requested && ready.exists() {
                fs::remove_file(&ready).unwrap();
                let finished = if let Some(deadline) = owner.outer_deadline() {
                    // Advance admission without sleeping. The fixture brackets
                    // the exact shared deadline it receives in the header.
                    owner.set_test_time(deadline);
                    None
                } else if returned_first {
                    let finished = super::super::clock::now_us().unwrap();
                    // Separate the boundaries without depending on scheduling
                    // or sub-microsecond conversion precision.
                    owner.set_test_time(Instant::now() + Duration::from_secs(1));
                    owner.cancel();
                    Some(finished)
                } else {
                    // Stop is accepted by the reservation, not the attempt.
                    // Entry completes before the attempt polls or sends Stop.
                    owner.cancel();
                    Some(super::super::clock::now_us().unwrap())
                };
                fs::write(
                    fixture.0.join("settlement.json"),
                    json!({"finished":finished,"deadline_offset":if returned_first {-1} else {1},
                        "terminal":terminal,"second_phase":second_phase,
                        "primary":child_fault.map(|category| Fault::new(category, "fixture"))})
                    .to_string(),
                )
                .unwrap();
                requested = true;
            }
            control.stop_reason()
        };
        let (progress, events) = mpsc::sync_channel(32);
        let (logs, _) = mpsc::sync_channel(1);
        let observer = Observer {
            progress,
            logs,
            dropped_logs: Default::default(),
            attempts: Default::default(),
            native_preparation: Default::default(),
        };
        let reap = fixture.0.join("reap");
        let record = thread::scope(|scope| {
            let worker = scope.spawn(|| {
                supervise(
                    &plan,
                    &inventory,
                    None,
                    false,
                    Some(&mut poll),
                    None,
                    &executable,
                    Some(&observer),
                    Operation::Run,
                    Some(&super::super::PreparedExecution {
                        control: &control,
                        modules: Some(&modules),
                        images: None,
                        startup: Some(&startup),
                        identity: None,
                    }),
                )
            });
            // Keep the child alive until the running supervisor has handled
            // both transitions and EntrySettled, not just from the final drain.
            while events.recv_timeout(Duration::from_secs(5)).unwrap()["event"] != "EntrySettled" {}
            fs::write(&reap, b"").unwrap();
            worker.join().unwrap().unwrap()
        });
        fs::remove_file(&reap).unwrap();
        let case = (cause, returned_first, terminal, second_phase, child_fault);
        assert_eq!(
            record.observations["supervisor_stop"],
            json!(cause),
            "{case:?}"
        );
        assert_eq!(
            record.metrics["protocol_fault"]["category"].as_str(),
            (second_phase != 2).then_some("Transport"),
            "{case:?}"
        );
        assert_eq!(record.observations["terminal_accounted"], false, "{case:?}");
        assert_eq!(record.entry_outcome, "Returned", "{case:?}");
        assert_eq!(record.cleanup["clean"], terminal, "{case:?}");
        assert_eq!(record.forced, !terminal, "{case:?}");
        assert_eq!(
            record.primary.as_ref().map(|fault| fault.category.as_str()),
            expected_primary,
            "{case:?}"
        );
        assert_eq!(
            record.status,
            if expected_primary.is_none() {
                "PASS"
            } else {
                "FAIL"
            },
            "{case:?}"
        );
    }
}
