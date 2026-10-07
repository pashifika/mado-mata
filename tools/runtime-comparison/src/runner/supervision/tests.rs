use super::*;
use crate::model::NativeBudgets;
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
    // A protocol-only child: no engine, native target, capture or input. File
    // barriers let the real supervisor receive settlement before time advances.
    fs::write(
        &executable,
        r#"#!/usr/bin/env python3
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
emit('ChildStarted', pid=os.getpid(), build={})
wait('enter')
emit('EntrySettled', primary=None, entry_outcome='Returned')
wait('finish')
emit('Terminal', primary=None, entry_outcome='Returned', cleanup={'clean': True}, observations={})
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let owner = Arc::new(Control::new(&plan.limits));
    let control = Arc::new(Control::for_attempt(Arc::clone(&owner), &plan.limits, true));
    control
        .start_native(
            NativeBudgets {
                startup_ms: 5_000,
                readiness_ms: 5_000,
                workflow_ms: 150,
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
        let workflow = control.native_transition(2, None).unwrap();
        fs::write(fixture.0.join("enter"), b"").unwrap();
        let settled = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(settled["event"], "EntrySettled");
        thread::sleep(
            workflow.saturating_duration_since(Instant::now()) + Duration::from_millis(10),
        );
        assert!(control.check().is_ok());
        assert_eq!(control.outer_deadline(), original_deadline);
        assert_eq!(control.deadline(), original_deadline);
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
