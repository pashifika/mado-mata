use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct FixtureTree(PathBuf);

impl FixtureTree {
    fn new(code: &str) -> Self {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        fn copy(source: &Path, destination: &Path) -> std::io::Result<()> {
            std::fs::create_dir(destination)?;
            for entry in std::fs::read_dir(source)? {
                let entry = entry?;
                let target = destination.join(entry.file_name());
                if entry.file_type()?.is_dir() {
                    copy(&entry.path(), &target)?;
                } else {
                    std::fs::copy(entry.path(), target)?;
                }
            }
            Ok(())
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mado-terminal-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create private fixture root");
        let tree = Self(root);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/javascript");
        copy(&fixture, &tree.0.join("package")).expect("copy controlled package");
        std::fs::write(tree.0.join("package/main.js"), code).expect("write regression workflow");
        tree
    }

    fn run(&self, cleanup_ms: u64) -> Value {
        let mut plan: Value =
            serde_json::from_str(include_str!("../fixtures/controlled-plan.json"))
                .expect("controlled plan");
        plan["id"] = json!("terminal-regression");
        plan["samples"] = json!(1);
        plan["warmups"] = json!(0);
        plan["limits"]["cleanup_ms"] = json!(cleanup_ms);
        plan["limits"]["vm_bytes"] = json!(64 * 1024 * 1024);
        let plan_path = self.0.join("plan.json");
        std::fs::write(&plan_path, serde_json::to_vec(&plan).expect("encode plan"))
            .expect("write plan");
        // This is the real CLI supervisor and its owned child, never native input.
        let output = Command::new(env!("CARGO_BIN_EXE_mado-runtime-comparison"))
            .arg("run")
            .arg(plan_path)
            .arg(self.0.join("package"))
            .output()
            .expect("run controlled supervisor");
        assert_eq!(
            output.status.code(),
            Some(1),
            "script failure must fail the CLI: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "retained supervisor result: {error}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        result["runs"][0].take()
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry_settled(record: &Value) -> &Value {
    record["milestones"]
        .as_array()
        .expect("supervisor milestones")
        .iter()
        .find(|row| row["event"] == "EntrySettled")
        .expect("entry evidence precedes cleanup")
}

const SUBMITTED_INPUT: &str = r"
export function readiness() { return 'Ready'; }
export function workflow() {
    const observation = host.call('observe', {});
    const sequence = host.call('submit', {
        observation, actions: [{kind:'key_down', key:'A'}]
    });
    const receipt = host.call('settle', {id:sequence.id});
    if (receipt.status !== 'Submitted') throw new Error('input precondition failed');
";

#[test]
fn oversized_script_exception_keeps_primary_receipt_and_key_cleanup() {
    let tree = FixtureTree::new(&format!(
        "{SUBMITTED_INPUT} throw 'x'.repeat(9 * 1024 * 1024); }}"
    ));
    let record = tree.run(1000);
    assert_eq!(record["primary"]["category"], "Script");
    assert_eq!(record["entry_outcome"], "FailedOrNotStarted");
    let retained = record["primary"]["message"]
        .as_str()
        .expect("bounded diagnostic");
    assert!(retained.len() < 9 * 1024 * 1024);
    assert_eq!(
        record["primary"]["context"]["diagnostic_truncation"]["message_bytes_dropped"],
        9 * 1024 * 1024 - retained.len()
    );
    assert_eq!(record["cleanup"]["clean"], true);
    assert_eq!(record["forced"], false);
    assert_eq!(record["exit_code"], 0);
    assert_eq!(record["observations"]["receipts"][0]["status"], "Submitted");
    assert_eq!(record["observations"]["receipts"][0]["submitted"], 1);
    assert_eq!(record["observations"]["held_keys"], json!([]));
    assert_eq!(record["cleanup"]["release_outcomes"][0]["key"], "A");
    assert_eq!(record["cleanup"]["release_outcomes"][0]["released"], true);
    let entry = entry_settled(&record);
    assert_eq!(entry["primary"]["category"], "Script");
    assert_eq!(entry["observations"]["snapshot_stage"], "pre_cleanup");
    assert_eq!(entry["observations"]["receipts"][0]["status"], "Submitted");
    assert_eq!(entry["observations"]["held_keys"], json!(["A"]));
}

#[test]
fn cleanup_watchdog_retains_already_submitted_receipts() {
    let tree = FixtureTree::new(&format!(
        "{SUBMITTED_INPUT}
        host.call('fixture', {{event:'hold'}});
        host.call('query', {{observation, kind:'ocr',
            roi:{{x:0,y:0,width:640,height:480}}, expected:'READY'}});
        host.call('wait', {{duration_ms:20}});
        throw new Error('failure after input and retained work');
        }}"
    ));
    let record = tree.run(1);
    assert_eq!(record["primary"]["category"], "Script");
    assert_eq!(record["cleanup"]["clean"], false);
    assert_eq!(record["cleanup"]["status"], "IncompleteCleanup");
    assert_eq!(record["forced"], true);
    assert_eq!(record["exit_code"], 124);
    assert_eq!(record["observations"]["receipts"][0]["status"], "Submitted");
    assert_eq!(record["observations"]["receipts"][0]["submitted"], 1);
    let entry = entry_settled(&record);
    assert_eq!(entry["observations"]["snapshot_stage"], "pre_cleanup");
    assert_eq!(
        entry["observations"]["receipts"][0]["cleanup_required"],
        json!(["A"])
    );
    assert_eq!(entry["observations"]["held_keys"], json!(["A"]));
    assert!(
        entry["observations"]["in_flight_native"]
            .as_u64()
            .expect("retained work")
            > 0
    );
    assert!(
        entry["observations"]["attempt_owners"]
            .as_u64()
            .expect("owner snapshot")
            > 0
    );
    // Whether the watchdog wins before or after Terminal, the same known input
    // receipt must survive. The pre-cleanup snapshot is not a clean acknowledgement.
    assert!(entry.get("cleanup").is_none());
}
