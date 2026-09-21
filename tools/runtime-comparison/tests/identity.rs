use mado_runtime_comparison::{inventory::Inventory, model::Plan, runner};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, atomic::AtomicU64, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};

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
