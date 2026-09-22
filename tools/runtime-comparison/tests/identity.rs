use mado_runtime_comparison::{
    desktop::{DesktopController, StartRequest},
    inventory::Inventory,
    model::Plan,
    runner,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, atomic::AtomicU64, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct PlanFile(PathBuf);

impl PlanFile {
    fn new(plan: &Plan) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("mado-identity-{}-{nonce}.json", std::process::id()));
        let file = File::create_new(&path).expect("create private controlled plan");
        let owner = Self(path);
        serde_json::to_writer(file, plan).expect("write controlled plan");
        owner
    }
}

impl Drop for PlanFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn executable_sha256(path: &Path) -> String {
    let mut file = File::open(path).expect("open executable");
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).expect("read executable");
        if count == 0 {
            return format!("{:x}", hash.finalize());
        }
        hash.update(&buffer[..count]);
    }
}

fn run_separately(
    plan: &Plan,
    inventory: &Inventory,
    stop_event: Option<&str>,
) -> runner::RunRecord {
    let (progress, events) = mpsc::sync_channel::<Value>(16);
    let (logs, _messages) = mpsc::sync_channel(plan.limits.log_records);
    let observer = runner::Observer {
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
    };
    let mut stop = || {
        events.try_iter().any(|event| {
            stop_event.is_some_and(|expected| event["event"].as_str() == Some(expected))
        })
    };
    runner::run_once_with_executable(
        Path::new(env!("CARGO_BIN_EXE_mado-runtime-comparison")),
        plan,
        inventory,
        &mut stop,
        &observer,
    )
    .expect("supervise the real owned runtime executable")
}

#[test]
fn separate_supervisor_records_runtime_identity_through_cleanup() {
    let executable = Path::new(env!("CARGO_BIN_EXE_mado-runtime-comparison"));
    let runtime_sha256 = executable_sha256(executable);
    let supervisor_sha256 = executable_sha256(&std::env::current_exe().unwrap());
    // The integration-test process is genuinely a different executable from
    // the runtime it supervises, unlike a CLI launching another copy of itself.
    assert_ne!(runtime_sha256, supervisor_sha256);
    let mut plan: Plan =
        serde_json::from_str(include_str!("../fixtures/controlled-plan.json")).unwrap();
    plan.id = "identity-regression".into();
    plan.candidate = "rust".into();
    plan.samples = 1;
    plan.warmups = 0;
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/javascript");
    let inventory = Inventory::capture(&package, &plan.limits).unwrap();
    let record = run_separately(&plan, &inventory, None);
    assert_eq!(record.status, "PASS", "{}", record.reason);
    assert_eq!(record.build["executable_sha256"], runtime_sha256);
    assert_ne!(record.build["executable_sha256"], supervisor_sha256);

    // The same-executable CLI remains an independent consumer of the protocol.
    // Compare the complete provenance, not a patched hash plus parent fields.
    let plan_file = PlanFile::new(&plan);
    let output = Command::new(executable)
        .arg("run")
        .arg(&plan_file.0)
        .arg(&package)
        .output()
        .expect("run the real CLI supervisor");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI result");
    assert_eq!(cli["runs"][0]["status"], "PASS");
    assert_eq!(cli["runs"][0]["build"], record.build);

    // Stop after the real controlled worker retains an owner. Provenance must
    // survive forced containment without turning incomplete cleanup into PASS.
    plan.scenario = "held-work".into();
    plan.limits.cleanup_ms = 100;
    let stopped = run_separately(&plan, &inventory, Some("WorkHeld"));
    assert_eq!(stopped.status, "FAIL");
    assert!(stopped.forced);
    assert_eq!(stopped.cleanup["clean"], false);
    assert_eq!(
        stopped
            .primary
            .as_ref()
            .map(|fault| fault.category.as_str()),
        Some("Cancelled")
    );
    assert_eq!(stopped.build, record.build);
}

#[cfg(unix)]
#[test]
fn child_exit_before_rust_startup_is_not_a_successful_check() {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../fixtures/controlled-plan.json")).unwrap();
    plan.lane = "replay".into();
    // Only loader locations are consumed before this process exits. No SDK or
    // recognition result is substituted for the absent Rust startup protocol.
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    plan.native_config = Some(serde_json::json!({
        "version":1,
        "ocr":{
            "model":"phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2",
            "profile":"phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2",
            "language":"horizontal-ja-basic-latin-ascii-digits-ui-symbols-v1",
            "provider":"cpu","runtime_profile":"onnxruntime-1.29.0-api17-cpu",
            "model_root":executable.parent().unwrap(),
            "runtime":{"path":executable,"sha256":"0".repeat(64),"bytes":1},
        },
        "native_libraries":[],
        "native":null,
        "replay":{"corpus_id":"startup-boundary","frames":[],"package_entries":{},"templates":{}},
    }));
    let inventory = Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &plan.limits,
    )
    .unwrap();
    let (progress, _events) = mpsc::sync_channel(16);
    let (logs, _messages) = mpsc::sync_channel(1);
    let observer = runner::Observer {
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
    };
    let record = runner::run_environment_check_with_executable(
        Path::new("/usr/bin/true"),
        &plan,
        &inventory,
        &mut || false,
        &observer,
    )
    .unwrap();
    assert_eq!(record.exit_code, Some(0));
    assert_eq!(record.status, "FAIL");
    let primary = record.primary.unwrap();
    assert_eq!(primary.category, "ChildStartup");
    assert_eq!(primary.context["boundary"], "before_child_started");
    assert_eq!(record.entry_outcome, "NotExecuted");
    assert_eq!(record.observations["operation"], "environment_check");
    assert_eq!(record.cleanup["clean"], false);
    assert!(record.metrics["runtime"].is_null());
    assert!(record.metrics["vm_bytes"].is_null());
}

#[test]
fn inspected_package_starts_controlled_without_requiring_an_engine() {
    let controller = DesktopController::new(
        PathBuf::from(env!("CARGO_BIN_EXE_mado-runtime-comparison")),
        PathBuf::from("engine-not-used-for-controlled"),
    );
    let package = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/typescript"));
    let inspected = controller.inspect(package).unwrap();
    let run = controller
        .start(
            StartRequest {
                package_path: package.to_str().unwrap().into(),
                inventory_identity: inspected.inventory_identity,
                package_id: inspected.package_id,
                schema_identity: inspected.schema_identity,
                profile_id: "draft".into(),
                values: inspected.profiles["template-first"]["options"].clone(),
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            },
            None,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(14);
    loop {
        let view = controller.poll();
        assert_eq!(view.run.as_deref(), Some(run.as_str()));
        if view.state == "terminal" {
            assert!(view.error.is_none(), "{:?}", view.error);
            let result = view.result.expect("terminal execution result");
            assert_eq!(result["status"], "PASS", "{result}");
            assert_eq!(result["cleanup"]["clean"], true);
            break;
        }
        assert!(Instant::now() < deadline, "controller did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn missing_replay_environment_retains_a_blocked_cli_record_without_a_child() {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../fixtures/controlled-plan.json")).unwrap();
    plan.lane = "replay".into();
    plan.samples = 1;
    plan.warmups = 0;
    plan.native_config = None;
    let plan_file = PlanFile::new(&plan);
    let output = Command::new(env!("CARGO_BIN_EXE_mado-runtime-comparison"))
        .arg("run")
        .arg(&plan_file.0)
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output: Value =
        serde_json::from_slice(&output.stdout).expect("structured prerequisite result");
    let record = &output["runs"][0];
    assert_eq!(record["status"], "BLOCKED");
    assert_eq!(record["primary"]["category"], "Blocked");
    assert_eq!(record["entry_outcome"], "NotExecuted");
    assert_eq!(record["cleanup"]["child_started"], false);
    assert_eq!(record["cleanup"]["clean"], true);
    assert!(record["build"].is_null());
    assert!(record["metrics"]["runtime"].is_null());
    assert!(record["metrics"]["workflow_us"].is_null());
}
