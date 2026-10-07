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
        Self::start_attempt(source, startup_ms, 1, true)
    }

    fn start_attempt(source: &str, startup_ms: u64, attempt: u64, native: bool) -> Self {
        Self::start_case(source, startup_ms, attempt, native, "success", None)
    }

    fn start_case(
        source: &str,
        startup_ms: u64,
        attempt: u64,
        native: bool,
        scenario: &str,
        actions: Option<usize>,
    ) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        let mut plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        plan.candidate = "javascript".into();
        plan.lane = if native { "native" } else { "controlled" }.into();
        plan.scenario = scenario.into();
        if let Some(actions) = actions {
            plan.limits.max_actions = actions;
        }
        let budgets = NativeBudgets {
            startup_ms,
            readiness_ms: 1_000,
            workflow_ms: 1_000,
        };
        plan.limits.duration_ms = budgets.total_ms().unwrap();
        plan.limits.readiness_ms = budgets.readiness_ms;
        plan.limits.wait_ms = 100;
        plan.native_budgets = native.then_some(budgets);
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
            "run":"startup-protocol","attempt":attempt,"operation":"run","plan":plan,
            "inventory":inventory,"observe_logs":true,"prepared_modules":null,
            "deadline":HORIZON_US,"startup_deadline":HORIZON_US,"prepare_target":native
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
            "sequence":sequence,"reply":{"progress":{"attempt":1,"status":status,"phase":"waiting_for_process","launch":"accepted"},
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

#[test]
fn settled_real_children_keep_attempt_identity_and_fresh_vm_handles_and_accounts() {
    let source = r"
        export function readiness() {
            if (Object.keys(host.state).length !== 0) throw new Error('inherited state');
            host.state.entered = true;
            return 'Ready';
        }
        export function workflow() {
            const observation = host.call('observe', {});
            host.call('log', {message: JSON.stringify(observation)});
            const sequence = host.call('submit', {observation, actions:[{kind:'key_down',key:'ENTER'},{kind:'key_up',key:'ENTER'}]});
            const receipt = host.call('settle', {id:sequence.id});
            if (receipt.status !== 'Submitted') throw new Error('receipt');
            host.call('release', {id:sequence.id});
            host.call('release', {id:observation.id});
            if (observation.attempt === 1) {
                host.call('fixture', {event:'confirmed_exit'});
                host.call('observe', {});
            }
        }
    ";
    let mut observations = Vec::new();
    let mut totals = (0, 0);
    for attempt in [1, 2] {
        let mut child = OwnedProtocol::start_attempt(source, 3_000, attempt, false);
        let (terminal, _) = child.finish();
        assert_eq!(terminal["attempt"], attempt);
        assert_eq!(terminal["cleanup"]["clean"], true);
        assert_eq!(terminal["observations"]["workflow_entered"], true);
        let account = &terminal["observations"]["accounting"];
        assert_eq!(account["complete"], true);
        assert_eq!(account["input_uncertain"], false);
        totals.0 += account["frames"].as_u64().unwrap();
        totals.1 += account["expanded_input_events"].as_u64().unwrap();
        let logs = terminal["observations"]["script_logs"].as_array().unwrap();
        let observation: Value =
            serde_json::from_str(logs[0]["message"].as_str().unwrap()).unwrap();
        assert_eq!(observation["attempt"], attempt);
        observations.push(observation);
        if attempt == 1 {
            assert_eq!(terminal["primary"]["category"], "TargetExited");
            assert_eq!(terminal["primary"]["context"]["exit_reason"], "absent");
        } else {
            assert!(terminal["primary"].is_null());
            assert_eq!(terminal["entry_outcome"], "Returned");
        }
    }
    assert_ne!(observations[0]["id"], observations[1]["id"]);
    assert_eq!(observations[0]["run"], observations[1]["run"]);
    assert_ne!(
        observations[0]["process_lifetime"],
        observations[1]["process_lifetime"]
    );
    assert_eq!(totals, (4, 4));
}

#[test]
fn real_child_pre_workflow_exit_and_stale_stop_do_not_continue() {
    let mut child = OwnedProtocol::start_attempt(
        r"
        export function readiness() {
            host.call('fixture', {event:'confirmed_exit'});
            host.call('observe', {});
            return 'Ready';
        }
        export function workflow() { throw new Error('must not enter'); }
    ",
        3_000,
        1,
        false,
    );
    let (terminal, _) = child.finish();
    assert_eq!(terminal["primary"]["category"], "TargetExited");
    assert_eq!(terminal["observations"]["workflow_entered"], false);
    assert_eq!(terminal["observations"]["accounting"]["complete"], true);
    drop(child);

    let mut child = OwnedProtocol::start_attempt(
        r"
        export function readiness() { return 'Ready'; }
        export function workflow() {
            const observation = host.call('observe', {});
            host.call('log', {message:'ready-for-stop'});
            while (true) { host.call('wait', {duration_ms:10}); }
        }
    ",
        3_000,
        2,
        false,
    );
    loop {
        let event = child.next();
        if event["event"] == "ScriptLog" {
            break;
        }
        assert_ne!(event["event"], "Terminal");
    }
    child.command(json!({"command":"Stop","run":"startup-protocol","attempt":1}));
    let (terminal, _) = child.finish();
    assert_eq!(terminal["attempt"], 2);
    assert_eq!(terminal["primary"]["category"], "Cancelled");
    assert_eq!(terminal["cleanup"]["clean"], true);
}

#[test]
fn old_attempt_startup_reply_cannot_initialize_a_fresh_child() {
    let mut child = OwnedProtocol::start_attempt(
        r"
        export function readiness() {
            host.call('target_start', {});
            while (true) { host.call('target_status', {}); host.call('wait', {duration_ms:10}); }
        }
        export function workflow() { throw new Error('must not enter'); }
    ",
        3_000,
        2,
        true,
    );
    loop {
        let event = child.next();
        if event["event"] == "TargetProbe" {
            assert_eq!(event["attempt"], 2);
            break;
        }
        assert_ne!(event["event"], "Terminal");
    }
    child.reply(1, "pending", Value::Null);
    let (terminal, _) = child.finish();
    assert_eq!(terminal["attempt"], 2);
    assert_eq!(terminal["primary"]["category"], "Cancelled");
    assert_eq!(
        terminal["observations"]["native_initialization_started"],
        false
    );
    assert_eq!(terminal["cleanup"]["clean"], true);
}

#[test]
fn real_child_partial_and_uncertain_input_never_report_refundable_authority() {
    for scenario in ["partial", "uncertain"] {
        let mut child = OwnedProtocol::start_case(
            r"
            export function readiness() { return 'Ready'; }
            export function workflow() {
                const observation = host.call('observe', {});
                const sequence = host.call('submit', {observation, actions:[{kind:'key_down',key:'ENTER'},{kind:'key_up',key:'ENTER'}]});
                host.call('settle', {id:sequence.id});
                host.call('release', {id:sequence.id});
                host.call('release', {id:observation.id});
                host.call('fixture', {event:'confirmed_exit'});
                host.call('observe', {});
            }
        ",
            3_000,
            1,
            false,
            scenario,
            None,
        );
        let (terminal, _) = child.finish();
        assert_eq!(terminal["primary"]["category"], "TargetExited");
        assert_eq!(terminal["cleanup"]["clean"], true);
        assert_eq!(terminal["observations"]["accounting"]["complete"], true);
        assert_eq!(
            terminal["observations"]["accounting"]["expanded_input_events"],
            2
        );
        assert_eq!(
            terminal["observations"]["accounting"]["input_uncertain"],
            true
        );
    }
}

#[test]
fn fresh_child_cannot_spend_beyond_transferred_expanded_event_allowance() {
    let mut child = OwnedProtocol::start_case(
        r"
        export function readiness() { return 'Ready'; }
        export function workflow() {
            const observation = host.call('observe', {});
            for (let i=0;i<2;i++) {
                const actions = Array.from({length:20}, (_, i) => ({kind:i % 2 ? 'key_up' : 'key_down',key:'ENTER'}));
                const sequence = host.call('submit', {observation, actions});
                host.call('settle', {id:sequence.id});
                host.call('release', {id:sequence.id});
            }
        }
    ",
        3_000,
        2,
        false,
        "success",
        Some(30),
    );
    let (terminal, _) = child.finish();
    assert_eq!(terminal["attempt"], 2);
    assert_eq!(terminal["primary"]["category"], "ActionLimit");
    assert_eq!(terminal["cleanup"]["clean"], true);
    assert_eq!(
        terminal["observations"]["accounting"]["expanded_input_events"],
        20
    );
}

#[test]
fn desktop_default_off_retains_one_real_child_result_under_its_reservation() {
    use mado_runtime_comparison::desktop::{DesktopController, StartRequest};
    use mado_runtime_comparison::inventory::PackageDraft;
    use mado_runtime_comparison::model::identity;
    struct Package(std::path::PathBuf);
    impl Drop for Package {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let plan: Plan = serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let package = Package(
        std::env::temp_dir().join(format!("mado-exit-default-{}-{nonce}", std::process::id())),
    );
    let draft = PackageDraft::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &plan.limits,
    )
    .unwrap();
    for (path, bytes) in draft.files() {
        let destination = package.0.join(path);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, bytes).unwrap();
    }
    std::fs::write(
        package.0.join("main.js"),
        r"
        export function readiness() { return 'Ready'; }
        export function workflow() {
            host.call('wait', {duration_ms:1000});
            host.call('fixture', {event:'confirmed_exit'});
            host.call('observe', {});
        }
    ",
    )
    .unwrap();
    let executable = std::path::PathBuf::from(env!("CARGO_BIN_EXE_mado-runtime-comparison"));
    let controller = DesktopController::new(executable.clone(), executable);
    let inventory = controller.inspect(&package.0).unwrap();
    let request = StartRequest {
        package_path: package.0.to_str().unwrap().into(),
        inventory_identity: inventory.inventory_identity.clone(),
        package_id: inventory.package_id.clone(),
        schema_identity: identity(&inventory.schema).unwrap(),
        profile_id: "saved-choice".into(),
        values: inventory.profiles["ocr-first"]["options"].clone(),
        lane: "controlled".into(),
        scenario: "workflow".into(),
        replay_descriptor_path: None,
        native_intent: None,
    };
    let run = controller.start(request.clone(), None).unwrap();
    assert_eq!(
        controller.start(request, None).unwrap_err().category,
        "RunActive"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let terminal = loop {
        let view = controller.poll();
        if view.state == "terminal" {
            break view;
        }
        assert_eq!(view.run.as_deref(), Some(run.as_str()));
        assert!(Instant::now() < deadline, "owned child did not settle");
        thread::sleep(Duration::from_millis(5));
    };
    assert!(terminal.error.is_none(), "{:?}", terminal.error);
    assert_eq!(terminal.attempts.len(), 1);
    let result = terminal.result.unwrap();
    assert_eq!(result["run"], run);
    assert_eq!(result["recovery_count"], 0);
    assert_eq!(result["primary"]["category"], "TargetExited");
    assert_eq!(result["cleanup"]["clean"], true);
    assert_eq!(result["observations"]["terminal_accounted"], true);
    assert_eq!(result["observations"]["workflow_entered"], true);
}

#[test]
fn real_child_large_exit_attribution_preserves_the_typed_cause() {
    let function = format!("observe_exit_{}", "x".repeat(6_000));
    let source = format!(
        "export function readiness() {{ return 'Ready'; }}\n\
         function {function}(depth) {{\n\
             if (depth > 0) {{ {function}(depth - 1); return depth; }}\n\
             host.call('fixture', {{event:'confirmed_exit'}});\n\
             host.call('observe', {{}});\n\
         }}\n\
         export function workflow() {{ {function}(2); }}"
    );
    let mut child = OwnedProtocol::start_attempt(&source, 3_000, 1, false);
    let (terminal, _) = child.finish();
    assert_eq!(
        terminal["primary"]["category"], "TargetExited",
        "{terminal}"
    );
    assert_eq!(terminal["primary"]["context"]["exit_reason"], "absent");
    assert_eq!(
        terminal["primary"]["context"]["diagnostic_truncation"]["context_omitted"],
        true
    );
    assert_eq!(terminal["observations"]["workflow_entered"], true);
    assert_eq!(terminal["observations"]["accounting"]["complete"], true);
    assert_eq!(terminal["cleanup"]["clean"], true);
}
