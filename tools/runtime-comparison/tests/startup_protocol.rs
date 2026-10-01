//! Exercise the real child without native authority, libraries, capture or input.
use mado_runtime_comparison::{
    inventory::Inventory,
    model::{NativeBudgets, Plan},
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

// The CPU-expiry child must not compete with unrelated startup assertions.
static SERIAL: Mutex<()> = Mutex::new(());

struct OwnedProtocol {
    child: Child,
    input: ChildStdin,
    events: mpsc::Receiver<Value>,
    reader: Option<thread::JoinHandle<()>>,
    _serial: MutexGuard<'static, ()>,
}

impl OwnedProtocol {
    fn start(source: &str, startup_ms: u64) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        let mut plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        plan.candidate = "javascript".into();
        plan.lane = "native".into();
        let budgets = NativeBudgets {
            startup_ms,
            readiness_ms: 1_000,
            workflow_ms: 1_000,
        };
        plan.limits.duration_ms = budgets.total_ms().unwrap();
        plan.limits.readiness_ms = budgets.readiness_ms;
        plan.limits.wait_ms = 100;
        plan.native_budgets = Some(budgets);
        let mut inventory = Inventory::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &plan.limits,
        )
        .unwrap();
        inventory.sources.insert("main.js".into(), source.into());
        inventory.refresh_identity().unwrap();
        let assets = std::mem::take(&mut inventory.assets);
        let lengths = assets
            .iter()
            .map(|(name, bytes)| (name, bytes.len()))
            .collect::<std::collections::BTreeMap<_, _>>();
        // A distant but representable 100-year monotonic horizon is capped by
        // the child's admitted limits. Clock-transfer precision has separate tests.
        const HORIZON_US: u64 = 100 * 365 * 24 * 60 * 60 * 1_000_000;
        let header = json!({"version":1,"invocation":{
            "run":"startup-protocol","attempt":1,"operation":"run","plan":plan,
            "inventory":inventory,"observe_logs":true,"prepared_modules":null,
            "deadline":HORIZON_US,"startup_deadline":HORIZON_US,"prepare_target":true
        },"asset_lengths":lengths});
        let mut child = Command::new(env!("CARGO_BIN_EXE_mado-runtime-comparison"))
            .arg("child")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (send, events) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    return;
                };
                let value = serde_json::from_str(&line).expect("child protocol JSON");
                if send.send(value).is_err() {
                    return;
                }
            }
        });
        let mut owned = Self {
            child,
            input,
            events,
            reader: Some(reader),
            _serial: serial,
        };
        writeln!(owned.input, "{header}").unwrap();
        for bytes in assets.values() {
            owned.input.write_all(bytes).unwrap();
        }
        owned.input.flush().unwrap();
        owned
    }

    fn command(&mut self, command: Value) {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
    }

    fn reply(&mut self, sequence: u64, status: &str, fault: Value) {
        self.command(json!({"command":"TargetPrepared","run":"startup-protocol","attempt":1,
            "sequence":sequence,"reply":{"progress":{"status":status,"phase":"waiting_for_process","launch":"accepted"},
            "configuration":null,"fault":fault}}));
    }

    fn next(&self) -> Value {
        self.events
            .recv_timeout(Duration::from_secs(15))
            .expect("bounded owned child event")
    }

    fn finish(&mut self) -> (Value, Vec<Value>) {
        let mut events = Vec::new();
        loop {
            let value = self.next();
            if value["event"] == "Terminal" {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Some(exit) = self.child.try_wait().unwrap() {
                        assert!(exit.success());
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "terminal child must physically exit"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
                self.reader.take().unwrap().join().unwrap();
                return (value, events);
            }
            events.push(value);
        }
    }
}

impl Drop for OwnedProtocol {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn no_request_and_early_ready_settle_cleanly_without_native_resources_on_repeated_children() {
    for _ in 0..2 {
        let mut child = OwnedProtocol::start(
            r"
            export function readiness() {
                if (host.call('target_status', {}).status !== 'not_requested') throw new Error('unexpected target');
                return 'Ready';
            }
            export function workflow() { throw new Error('must not enter Workflow'); }
        ",
            3_000,
        );
        let (terminal, events) = child.finish();
        assert_eq!(terminal["primary"]["category"], "ReadinessContract");
        assert_eq!(terminal["cleanup"]["clean"], true);
        assert_eq!(
            terminal["observations"]["native_initialization_started"],
            false
        );
        assert_eq!(terminal["observations"]["native_status"], "not_requested");
        assert_eq!(terminal["observations"]["dispatches"], 0);
        assert!(!events.iter().any(|event| event["event"] == "TargetProbe"));
        assert!(terminal["cleanup"].get("engine").is_none());
    }
}

#[test]
fn request_returns_before_resolution_and_only_script_polling_advances_pending() {
    let mut child = OwnedProtocol::start(
        r"
        export function readiness() {
            if (host.call('target_start', {}).status !== 'pending') throw new Error('start did not return pending');
            host.call('log', {message:'request-returned'});
            while (true) { host.call('target_status', {}); host.call('wait', {duration_ms:10}); }
        }
        export function workflow() { throw new Error('must not enter Workflow'); }
    ",
        3_000,
    );
    let mut request = None;
    let mut returned = false;
    while request.is_none() || !returned {
        let event = child.next();
        match event["event"].as_str() {
            Some("TargetProbe") => request = event["sequence"].as_u64(),
            Some("ScriptLog") if event["message"] == "request-returned" => returned = true,
            Some("Terminal") => panic!("startup did not return promptly: {event}"),
            _ => {}
        }
    }
    child.reply(request.unwrap(), "pending", Value::Null);
    loop {
        let event = child.next();
        if event["event"] == "TargetProbe" {
            assert_eq!(event["sequence"], 2);
            child.reply(2, "pending", json!({"category":"TargetLost","message":"selected process exited","context":{"stage":"process_discovery"}}));
            break;
        }
        assert_ne!(event["event"], "Terminal", "{event}");
    }
    let (terminal, events) = child.finish();
    assert_eq!(terminal["primary"]["category"], "TargetLost");
    assert_eq!(terminal["observations"]["native_launch"], "accepted");
    assert_eq!(
        terminal["observations"]["native_initialization_started"],
        false
    );
    assert_eq!(terminal["cleanup"]["clean"], true);
    assert!(!events.iter().any(|event| event["event"] == "TargetProbe"));
}

#[test]
fn startup_failure_interrupts_cpu_work_without_waiting_for_another_status_call() {
    let mut child = OwnedProtocol::start(
        r"
        export function readiness() { host.call('target_start', {}); while (true) {} }
        export function workflow() {}
    ",
        3_000,
    );
    loop {
        let event = child.next();
        if event["event"] == "TargetProbe" {
            child.reply(1, "pending", json!({"category":"PermissionDenied","message":"discovery denied","context":{"stage":"target_discovery"}}));
            break;
        }
        assert_ne!(event["event"], "Terminal", "{event}");
    }
    let (terminal, _) = child.finish();
    assert_eq!(terminal["primary"]["category"], "PermissionDenied");
    assert_eq!(terminal["cleanup"]["clean"], true);
}

#[test]
fn startup_cpu_expiry_never_acquires_a_target_or_borrows_readiness_time() {
    let mut child = OwnedProtocol::start(
        r"
        export function readiness() { while (true) {} }
        export function workflow() {}
    ",
        5_000,
    );
    let (terminal, events) = child.finish();
    assert_eq!(terminal["primary"]["category"], "Timeout");
    assert_eq!(terminal["observations"]["native_status"], "not_requested");
    assert_eq!(terminal["cleanup"]["clean"], true);
    assert!(events.iter().any(|event| event["event"] == "VmHookReached"));
    assert!(!events.iter().any(|event| event["event"] == "TargetProbe"));
}

#[test]
fn stop_and_forged_capture_readiness_cannot_admit_late_native_initialization() {
    for command in ["stop", "capture_ready"] {
        let mut child = OwnedProtocol::start(
            r"
            export function readiness() {
                host.call('target_start', {});
                while (true) { host.call('target_status', {}); host.call('wait', {duration_ms:10}); }
            }
            export function workflow() {}
        ",
            3_000,
        );
        loop {
            let event = child.next();
            if event["event"] == "TargetProbe" {
                break;
            }
            assert_ne!(event["event"], "Terminal", "{event}");
        }
        if command == "stop" {
            child.command(json!({"command":"Stop","run":"startup-protocol","attempt":1}));
        } else {
            child.reply(1, "capture_ready", Value::Null);
        }
        let (terminal, _) = child.finish();
        assert_eq!(terminal["primary"]["category"], "Cancelled", "{command}");
        assert_eq!(
            terminal["observations"]["native_initialization_started"],
            false
        );
        assert_eq!(terminal["observations"]["dispatches"], 0);
        assert_eq!(terminal["cleanup"]["clean"], true);
    }
}
