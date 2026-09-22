mod lifecycle;
mod loading;

use crate::host::{Host, resolve_options};
use crate::inventory::Inventory;
use crate::model::{Control, Fault, Limits, Plan};
use crate::runner::{RunRecord, run_once, run_once_after_milestone};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub fn plan(candidate: &str, scenario: &str, profile: &str) -> Plan {
    Plan {
        version: 1,
        id: "controlled-check".into(),
        candidate: candidate.into(),
        lane: "controlled".into(),
        scenario: scenario.into(),
        profile: profile.into(),
        limits: Limits {
            duration_ms: 10_000,
            readiness_ms: 2_000,
            wait_ms: 1_000,
            cleanup_ms: 1_000,
            containment_ms: 2_000,
            queue_capacity: 2,
            handles: 32,
            log_records: 8,
            log_bytes: 1024,
            vm_bytes: 16 * 1024 * 1024,
            max_actions: 64,
            snapshot_files: 128,
            snapshot_bytes: 1024 * 1024,
        },
        samples: 1,
        warmups: 0,
        repetitions: 1,
        budgets: [
            ("startup_us", 5_000_000.0),
            ("preflight_us", 8_000_000.0),
            ("host_call_us", 1_000_000.0),
            ("workflow_us", 8_000_000.0),
            ("stop_receipt_us", 500_000.0),
            ("admission_close_us", 500_000.0),
            ("cleanup_us", 1_000_000.0),
            ("containment_us", 2_000_000.0),
            ("cpu_percent", 400.0),
            ("supervisor_rss_bytes", 512_000_000.0),
            ("child_rss_bytes", 512_000_000.0),
            ("vm_bytes", 16_777_216.0),
            ("live_owners", 32.0),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect(),
        native_config: None,
    }
}

fn fixture(candidate: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(if candidate == "rust" {
            "javascript"
        } else {
            candidate
        })
}

fn case(rows: &mut Vec<Value>, name: &str, record: RunRecord, passed: bool, oracle: &str) {
    let mut row = serde_json::to_value(record).expect("result record is JSON-compatible");
    row["id"] = json!(name);
    row["execution_status"] = row["status"].clone();
    row["status"] = json!(if passed { "PASS" } else { "FAIL" });
    row["oracle"] = json!(oracle);
    rows.push(row);
}

fn host_for(inventory: &Inventory, scenario: &str) -> Result<Host, Fault> {
    let plan = plan("rust", scenario, "template-first");
    let profile = inventory
        .profiles
        .get(&plan.profile)
        .ok_or_else(|| Fault::new("Fixture", "missing profile"))?;
    let options = resolve_options(&inventory.schema, profile, &inventory.package_id)?;
    let control = Arc::new(Control::new(&plan.limits));
    let host = Host::new(plan, options, inventory.assets.clone(), control)?;
    host.begin_readiness()?;
    host.begin_workflow()?;
    Ok(host)
}

fn host_case(rows: &mut Vec<Value>, id: &str, host: &Host, passed: bool, oracle: &str) {
    let cleanup = host.finish();
    rows.push(
        json!({"id":id,"status":if passed && cleanup["clean"] == true {"PASS"} else {"FAIL"},
        "candidate":"rust","lane":"controlled","os":std::env::consts::OS,"oracle":oracle,
        "observations":host.snapshot(),"cleanup":cleanup,"build":crate::report::build_identity()}),
    );
}

fn shared_host_cases(rows: &mut Vec<Value>, inventory: &Inventory) -> Result<(), Fault> {
    let host = host_for(inventory, "success")?;
    let observation = host.call("observe", json!({}))?;
    let actions = json!([{"kind":"key_down","key":"A"},{"kind":"key_up","key":"A"}]);
    let first = host.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    )?;
    let second = host.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    )?;
    let overflow = host.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    );
    let receipt = host.call("settle", json!({"id":second["id"]}))?;
    let snapshot = host.snapshot();
    let passed = overflow.is_err()
        && first["order"] == 1
        && second["order"] == 2
        && receipt["status"] == "Submitted"
        && snapshot["effects"].as_array().is_some_and(|v| v.len() == 4)
        && snapshot["queue_high_water"] == 2;
    host_case(
        rows,
        "queue-overflow-and-order",
        &host,
        passed,
        "capacity2 rejects third; settling second dispatches first and second exactly once in order",
    );

    for event in [
        "geometry",
        "session",
        "target_exit",
        "focus_lost",
        "route_revoked",
    ] {
        let host = host_for(inventory, "success")?;
        let observation = host.call("observe", json!({}))?;
        let admitted = host.call(
            "submit",
            json!({"observation":observation,"actions":actions}),
        )?;
        host.call("fixture", json!({"event":event}))?;
        let settled = host.call("settle", json!({"id":admitted["id"]}));
        let refused = settled.as_ref().map_or(true, |value| {
            value["status"] == "Refused" || value["status"] == "Cancelled"
        });
        let empty = host.snapshot()["effects"]
            .as_array()
            .is_some_and(Vec::is_empty);
        host_case(
            rows,
            &format!("queued-{event}"),
            &host,
            refused && empty,
            "changed identity/authority after acceptance must not dispatch",
        );
    }
    let host = host_for(inventory, "success")?;
    let observation = host.call("observe", json!({}))?;
    host.call("release", json!({"id":observation["id"]}))?;
    let reused = host.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    );
    host_case(
        rows,
        "explicit-release-invalidates",
        &host,
        reused.is_err(),
        "released observation cannot authorize input",
    );

    let host = host_for(inventory, "success")?;
    let observation = host.call("observe", json!({}))?;
    let stale = host.call(
        "postcondition",
        json!({"observation":observation,"checkpoint":observation,"expected":"DONE"}),
    );
    host_case(
        rows,
        "postcondition-requires-new-frame",
        &host,
        stale.is_err(),
        "pre-input frame cannot prove postcondition",
    );

    let host = host_for(inventory, "success")?;
    for _ in 0..100 {
        host.call("log", json!({"message":"bounded progress"}))?;
    }
    let observation = host.call("observe", json!({}))?;
    host.control().cancel();
    let denied = host.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    );
    let released = host.call("release", json!({"id":observation["id"]}));
    host_case(
        rows,
        "log-pressure-stop-release",
        &host,
        denied.is_err()
            && released.is_ok()
            && host.snapshot()["dropped_logs"]
                .as_u64()
                .is_some_and(|v| v >= 92),
        "log saturation preserves Stop and explicit cleanup release",
    );

    let host = host_for(inventory, "query-absent")?;
    let observation = host.call("observe", json!({}))?;
    let query = host.call("query",json!({"observation":observation,"kind":"ocr","roi":{"x":0,"y":0,"width":640,"height":480},"expected":"MISSING"}))?;
    let result = host.call("query_wait", json!({"id":query["id"],"timeout_ms":10}));
    host_case(
        rows,
        "finite-query-timeout",
        &host,
        result.as_ref().is_err_and(|e| e.category == "Timeout"),
        "absent visual condition returns Timeout, never mere-frame success",
    );
    Ok(())
}

fn replace_entry(inventory: &Inventory, code: &str) -> Result<Inventory, Fault> {
    let mut inventory = inventory.clone();
    inventory
        .sources
        .insert(inventory.entries.workflow.module.clone(), code.into());
    inventory.refresh_identity()?;
    Ok(inventory)
}

struct FixtureTree(PathBuf);

impl FixtureTree {
    fn new(candidate: &str) -> Result<Self, Fault> {
        fn copy(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
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
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Fault::new("Fixture", error.to_string()))?
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("mado-inventory-{}-{nonce}", std::process::id()));
        copy(&fixture(candidate), &root)
            .map_err(|error| Fault::new("Fixture", error.to_string()))?;
        Ok(Self(root))
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn inventory_cases(rows: &mut Vec<Value>) -> Result<(), Fault> {
    let baseline = plan("javascript", "success", "template-first");
    for name in [
        "missing-asset",
        "case-collision",
        "ambient-tree",
        "file-limit",
        "byte-limit",
        "self-approval",
        "helper-version",
        "sdk-version",
        "manifest-traversal",
        "asset-shape",
        "hardlink",
    ] {
        let tree = FixtureTree::new("javascript")?;
        let mut limits = baseline.limits.clone();
        let path = tree.0.join("package.json");
        let mut manifest: Value = serde_json::from_slice(
            &std::fs::read(&path).map_err(|e| Fault::new("Fixture", e.to_string()))?,
        )
        .map_err(|e| Fault::new("Fixture", e.to_string()))?;
        let mutation = match name {
            "missing-asset" => std::fs::remove_file(tree.0.join("assets/marker.rgba")),
            "case-collision" => {
                manifest["sources"] = json!(["main.js", "decisions.js", "MAIN.js"]);
                std::fs::write(tree.0.join("MAIN.js"), "export const value=1;")
            }
            "ambient-tree" => std::fs::create_dir(tree.0.join("node_modules")),
            "hardlink" => std::fs::hard_link(tree.0.join("main.js"), tree.0.join("alias.js")),
            "file-limit" => {
                limits.snapshot_files = 1;
                Ok(())
            }
            "byte-limit" => {
                limits.snapshot_bytes = 16;
                Ok(())
            }
            "self-approval" => {
                manifest["approved_modules"] = json!(["unapproved"]);
                Ok(())
            }
            "helper-version" => {
                manifest["dependencies"] = json!({"@mado/helper":"999.0.0"});
                Ok(())
            }
            "sdk-version" => {
                manifest["sdk"] = json!("unapproved-sdk");
                Ok(())
            }
            "manifest-traversal" => {
                manifest["sources"] = json!(["../outside.js"]);
                Ok(())
            }
            "asset-shape" => {
                manifest["assets"]["marker"]["width"] = json!(100);
                Ok(())
            }
            _ => unreachable!(),
        };
        mutation.map_err(|error| Fault::new("Fixture", error.to_string()))?;
        std::fs::write(
            &path,
            serde_json::to_vec(&manifest).map_err(|e| Fault::new("Fixture", e.to_string()))?,
        )
        .map_err(|e| Fault::new("Fixture", e.to_string()))?;
        let result = Inventory::capture(&tree.0, &limits);
        rows.push(json!({"id":format!("inventory-{name}"),"candidate":"inventory","lane":"controlled",
            "os":std::env::consts::OS,"status":if result.is_err() {"PASS"} else {"FAIL"},
            "oracle":"invalid package cannot become an executable inventory","observed":result.err()}));
    }
    let tree = FixtureTree::new("javascript")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("main.js", tree.0.join("alias.js"))
        .map_err(|e| Fault::new("Fixture", e.to_string()))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("main.js", tree.0.join("alias.js"))
        .map_err(|e| Fault::new("Fixture", e.to_string()))?;
    let result = Inventory::capture(&tree.0, &baseline.limits);
    rows.push(json!({"id":"inventory-symlink","candidate":"inventory","lane":"controlled","os":std::env::consts::OS,
        "status":if result.is_err() {"PASS"} else {"FAIL"},"oracle":"package links never enter the captured inventory","observed":result.err()}));
    let inventory = Inventory::capture(&fixture("javascript"), &baseline.limits)?;
    for (index, specifier) in [
        "../outside.js",
        "/outside.js",
        "C:\\outside.js",
        "file:///outside.js",
        "https://example.invalid/code.js",
        "node:fs",
        "./MAIN.js",
        "./main.js?query",
        "./main.js#fragment",
        ".\\main.js",
        "@mado/order",
        "unapproved",
    ]
    .iter()
    .enumerate()
    {
        let result = inventory.resolve("main.js", specifier);
        rows.push(json!({"id":format!("resolver-refusal-{index}"),"candidate":"inventory","lane":"controlled",
            "os":std::env::consts::OS,"status":if result.is_err() {"PASS"} else {"FAIL"},
            "oracle":"resolved names cannot widen captured package/catalog authority","specifier":specifier,"observed":result.err()}));
    }
    for candidate in ["javascript", "lua", "typescript"] {
        let tree = FixtureTree::new(candidate)?;
        let scenario = plan(candidate, "success", "template-first");
        let inventory = Inventory::capture(&tree.0, &scenario.limits)?;
        let entry = &inventory.entries.workflow.module;
        std::fs::write(
            tree.0.join(entry),
            if candidate == "lua" {
                "error('authoring mutation must not execute')"
            } else {
                "throw new Error('authoring mutation must not execute');"
            },
        )
        .map_err(|e| Fault::new("Fixture", e.to_string()))?;
        let record = run_once(&scenario, &inventory, None, false, None)?;
        let passed = record.status == "PASS" && record.observations["effects"][0]["key"] == "A";
        case(
            rows,
            &format!("{candidate}-immutable-snapshot"),
            record,
            passed,
            "changed authoring bytes after capture cannot change compilation or module execution",
        );
    }
    Ok(())
}

pub fn run() -> Result<Value, Fault> {
    let mut rows = Vec::new();
    let mut inventories = BTreeMap::new();
    inventory_cases(&mut rows)?;
    for candidate in ["rust", "javascript", "lua", "typescript"] {
        let inventory = Inventory::capture(
            &fixture(candidate),
            &plan(candidate, "success", "template-first").limits,
        )?;
        for (profile, key) in [("template-first", "A"), ("ocr-first", "D")] {
            let record = run_once(
                &plan(candidate, "success", profile),
                &inventory,
                None,
                false,
                None,
            )?;
            let effects = record.observations["effects"].as_array();
            let passed = record.status == "PASS"
                && effects
                    .is_some_and(|v| v.len() == 2 && v[0]["key"] == key && v[1]["key"] == key)
                && record.observations["postconditions"][0]["satisfied"] == true;
            case(
                &mut rows,
                &format!("{candidate}-{profile}"),
                record,
                passed,
                "independent profile oracle: A for template-first; D for OCR-first; two effects and visible DONE on a newer frame",
            );
        }
        for scenario in [
            "no-match",
            "partial",
            "uncertain",
            "postcondition-absent",
            "backend-failure",
        ] {
            let record = run_once(
                &plan(candidate, scenario, "template-first"),
                &inventory,
                None,
                false,
                None,
            )?;
            let receipts = &record.observations["receipts"];
            let passed = match scenario {
                "no-match" => {
                    record.primary.is_none() && receipts.as_array().is_some_and(Vec::is_empty)
                }
                "partial" => {
                    receipts[0]["status"] == "Partial" && record.observations["dispatches"] == 1
                }
                "uncertain" => {
                    receipts[0]["status"] == "Uncertain" && record.observations["dispatches"] == 1
                }
                "postcondition-absent" => {
                    receipts[0]["status"] == "Submitted"
                        && record.observations["postconditions"][0]["satisfied"] == false
                }
                "backend-failure" => {
                    record.primary.is_some() && receipts.as_array().is_some_and(Vec::is_empty)
                }
                _ => false,
            } && record.cleanup["clean"] == true;
            case(
                &mut rows,
                &format!("{candidate}-{scenario}"),
                record,
                passed,
                "declared absence/partial/uncertain/failure is preserved; no automatic input replay",
            );
        }
        inventories.insert(candidate, inventory);
    }
    shared_host_cases(&mut rows, &inventories["rust"])?;
    loading::run(&mut rows, &inventories)?;
    lifecycle::run(&mut rows, &inventories)?;
    for candidate in ["javascript", "lua"] {
        let loop_source = if candidate == "javascript" {
            "export function readiness(){return 'Ready'} export function workflow(){while(true){}}"
        } else {
            "return {readiness=function() return 'Ready' end, workflow=function() while true do end end}"
        };
        let inventory = replace_entry(&inventories[candidate], loop_source)?;
        rows.push(crate::runner::intentional_exit_evidence(
            &plan(candidate, "success", "template-first"),
            &inventory,
        )?);
        let held_source = if candidate == "javascript" {
            "export function readiness(){return 'Ready'} export function workflow(){const o=host.call('observe',{});host.call('query',{observation:o,kind:'ocr',roi:{x:0,y:0,width:640,height:480},expected:'READY'});host.call('wait',{duration_ms:200});}"
        } else {
            "return {readiness=function() return 'Ready' end, workflow=function() local o=host.call('observe',{});host.call('query',{observation=o,kind='ocr',roi={x=0,y=0,width=640,height=480},expected='READY'});host.call('wait',{duration_ms=200});end}"
        };
        let inventory = replace_entry(&inventories[candidate], held_source)?;
        let record = run_once(
            &plan(candidate, "held-work", "template-first"),
            &inventory,
            None,
            false,
            None,
        )?;
        let passed = record.entry_outcome == "Returned"
            && record.primary.is_none()
            && record.forced
            && record.cleanup["clean"] != true
            && record.status == "FAIL";
        case(
            &mut rows,
            &format!("{candidate}-returned-entry-forced-cleanup"),
            record,
            passed,
            "successful entry settlement survives forced containment without becoming clean completion",
        );
    }
    let failed = run_once(
        &plan("javascript", "success", "template-first"),
        &replace_entry(
            &inventories["javascript"],
            "export function readiness(){return 'Ready'} export function workflow(){throw Error('required operation failed');}",
        )?,
        None,
        false,
        None,
    )?;
    let report = crate::report::summarize(&json!({"version":1,"runs":[failed]}))?;
    rows.push(json!({"id":"report-preserves-required-failure","candidate":"javascript","lane":"controlled",
        "os":std::env::consts::OS,"status":if report["counts"]["FAIL"]==1 && report["decision"]["kind"]=="Blocked" {"PASS"}else{"FAIL"},
        "oracle":"an actually failed required execution remains a failure in the report and cannot select a runtime",
        "observed":report["counts"],"decision":report["decision"]}));
    for candidate in ["javascript", "lua"] {
        let base = &inventories[candidate];
        let cases: Vec<(&str, &str, bool)> = if candidate == "javascript" {
            vec![
                (
                    "malformed-readiness",
                    "export function readiness(){return true;} export function workflow(){throw Error('must not enter');}",
                    false,
                ),
                (
                    "noncallable-entry",
                    "export function readiness(){return 'Ready';} export const workflow=42;",
                    false,
                ),
                (
                    "compute-stop",
                    "export function readiness(){return 'Ready';} export function workflow(){while(true){}}",
                    true,
                ),
                (
                    "top-level-stop",
                    "while(true){} export function readiness(){return 'Ready';} export function workflow(){}",
                    true,
                ),
                (
                    "caught-error-stop",
                    "export function readiness(){return 'Ready';} export function workflow(){while(true){try{throw Error('again');}catch(e){}}}",
                    true,
                ),
                (
                    "microtask-stop",
                    "export function readiness(){return 'Ready';} export async function workflow(){await Promise.resolve();while(true){}}",
                    true,
                ),
                (
                    "computed-refusal",
                    "export function readiness(){return 'Ready';} export async function workflow(){let x='node:'+'fs';try{await import(x);}catch(e){} host.call('observe',{});}",
                    false,
                ),
                (
                    "memory-cap",
                    "export function readiness(){return 'Ready';} export function workflow(){let a=[];while(true){a.push(new Array(10000).fill(17));}}",
                    false,
                ),
            ]
        } else {
            vec![
                (
                    "malformed-readiness",
                    "return {readiness=function() return true end, workflow=function() error('must not enter') end}",
                    false,
                ),
                (
                    "noncallable-entry",
                    "return {readiness=function() return 'Ready' end, workflow=42}",
                    false,
                ),
                (
                    "compute-stop",
                    "return {readiness=function() return 'Ready' end, workflow=function() while true do end end}",
                    true,
                ),
                (
                    "top-level-stop",
                    "while true do end return {readiness=function() return 'Ready' end, workflow=function() end}",
                    true,
                ),
                (
                    "caught-error-stop",
                    "return {readiness=function() return 'Ready' end, workflow=function() while true do pcall(function() error('again') end) end end}",
                    true,
                ),
                (
                    "coroutine-stop",
                    "return {readiness=function() return 'Ready' end, workflow=function() local c=coroutine.create(function() while true do end end); coroutine.resume(c) end}",
                    true,
                ),
                (
                    "computed-refusal",
                    "return {readiness=function() return 'Ready' end, workflow=function() local n='forbidden'..'.lua'; pcall(function() require(n) end); host.call('observe',{}) end}",
                    false,
                ),
                (
                    "memory-cap",
                    "return {readiness=function() return 'Ready' end, workflow=function() local a={} while true do a[#a+1]=string.rep('X',10000) end end}",
                    false,
                ),
            ]
        };
        for (name, source, stop) in cases {
            let inventory = replace_entry(base, source)?;
            // Startup and import validation precede the VM hook; keep the common bound.
            let scenario = plan(candidate, "success", "template-first");
            let record = if stop {
                run_once_after_milestone(&scenario, &inventory, "VmHookReached", 100, false)?
            } else {
                run_once(&scenario, &inventory, None, false, None)?
            };
            let interrupted = record
                .primary
                .as_ref()
                .is_some_and(|fault| fault.category == "Cancelled");
            let passed = if stop {
                interrupted
                    && !record.forced
                    && record
                        .milestones
                        .iter()
                        .any(|milestone| milestone["event"] == "VmHookReached")
                    && record.milestones.iter().any(|milestone| {
                        milestone["event"] == "StopRequested" && milestone["reason"] == "Stop"
                    })
                    && record
                        .milestones
                        .iter()
                        .any(|milestone| milestone["event"] == "AdmissionClosed")
                    && record.metrics["stop_receipt_us"].as_u64().is_some()
                    && record.metrics["admission_close_us"].as_u64().is_some()
            } else {
                record.primary.is_some() && !record.forced
            } && record.cleanup["clean"] == true
                && record.observations["effects"]
                    .as_array()
                    .is_some_and(Vec::is_empty);
            case(
                &mut rows,
                &format!("{candidate}-{name}"),
                record,
                passed,
                if stop {
                    "actual VM hook interrupts execution, without forced containment or dispatched input"
                } else {
                    "guarded contract/import/memory refusal before any input; clean host cleanup"
                },
            );
        }
        let loop_source = if candidate == "javascript" {
            "export function readiness(){return 'Ready';} export function workflow(){while(true){}}"
        } else {
            "return {readiness=function() return 'Ready' end,workflow=function() while true do end end}"
        };
        let inventory = replace_entry(base, loop_source)?;
        let record = run_once_after_milestone(
            &plan(candidate, "success", "template-first"),
            &inventory,
            "VmHookReached",
            100,
            true,
        )?;
        let vm_hook = record
            .milestones
            .iter()
            .position(|event| event["event"] == "VmHookReached");
        let control_lost = record.milestones.iter().position(|event| {
            event["event"] == "StopRequested" && event["reason"] == "ControlLost"
        });
        let passed = record
            .primary
            .as_ref()
            .is_some_and(|e| e.category == "Cancelled")
            && !record.forced
            && record.cleanup["clean"] == true
            && record.exit_code == Some(0)
            && matches!((vm_hook, control_lost), (Some(hook), Some(stop)) if hook < stop);
        case(
            &mut rows,
            &format!("{candidate}-control-eof"),
            record,
            passed,
            "control EOF after observed VM entry cancels without another command and completes clean cleanup",
        );
        for name in [
            "readonly",
            "module-cache",
            "module-cycle",
            "computed-import-refusal",
            "top-level-input",
            "helper-error",
            "static-import-refusal",
            "syntax-error",
            "unawaited",
        ] {
            let mut inventory = base.clone();
            crate::scenarios::select(&mut inventory, name)?;
            let record = run_once(
                &plan(candidate, "success", "template-first"),
                &inventory,
                None,
                false,
                None,
            )?;
            let passed = match name {
                "readonly" | "module-cache" => record.status == "PASS",
                "module-cycle" if candidate == "javascript" => record.status == "PASS",
                "computed-import-refusal" => {
                    record
                        .primary
                        .as_ref()
                        .is_some_and(|e| e.category == "ImportRefused")
                        && record.observations["receipts"]
                            .as_array()
                            .is_some_and(|v| v.len() == 1)
                        && record.observations["effects"]
                            .as_array()
                            .is_some_and(|v| v.len() == 2)
                }
                "unawaited" => {
                    record.cleanup["clean"] == true && record.observations["queue_depth"] == 0
                }
                _ => {
                    record.primary.is_some()
                        && record.observations["effects"]
                            .as_array()
                            .is_some_and(Vec::is_empty)
                }
            } && record.cleanup["clean"] == true
                && !record.forced;
            case(
                &mut rows,
                &format!("{candidate}-{name}"),
                record,
                passed,
                "selected source fixture preserves its declared observable result, source cause, and cleanup invariants",
            );
            if name == "module-cache" {
                let record = run_once(
                    &plan(candidate, "success", "template-first"),
                    &inventory,
                    None,
                    false,
                    None,
                )?;
                let passed = record.status == "PASS";
                case(
                    &mut rows,
                    &format!("{candidate}-module-cache-rerun"),
                    record,
                    passed,
                    "the next attempt begins with fresh module state",
                );
            }
        }
    }
    for (name, source, category) in [
        (
            "typescript-helper-diagnostic",
            "import { fail } from './decisions.js'; export function readiness(): MadoReady {return 'Ready'} export function workflow():void {fail();}",
            "Script",
        ),
        (
            "typescript-option-diagnostic",
            "export function readiness(): MadoReady {return 'Ready'} export function workflow():void {host.options.unknown;}",
            "TypeScript",
        ),
        (
            "typescript-type-only-refusal",
            "import type { Unknown } from 'unapproved'; export function readiness(): MadoReady {return 'Ready'} export function workflow():void {}",
            "ImportRefused",
        ),
        (
            "typescript-ambient-type-refusal",
            "/// <reference types=\"node\" />\nexport function readiness(): MadoReady {return 'Ready'} export function workflow():void {}",
            "ImportRefused",
        ),
    ] {
        let inventory = replace_entry(&inventories["typescript"], source)?;
        let record = run_once(
            &plan("typescript", "success", "template-first"),
            &inventory,
            None,
            false,
            None,
        )?;
        let mut passed = record
            .primary
            .as_ref()
            .is_some_and(|fault| fault.category == category)
            && record.cleanup["clean"] == true
            && !record.forced;
        if name == "typescript-helper-diagnostic" {
            passed &= record.primary.as_ref().is_some_and(|fault| {
                fault.context["typescript"]["frames"]
                    .as_array()
                    .is_some_and(|frames| {
                        frames
                            .iter()
                            .any(|frame| frame["original"]["module"] == "decisions.ts")
                    })
            });
        }
        case(
            &mut rows,
            name,
            record,
            passed,
            "compiler and original-source diagnostics preserve the actual failure and prevent ordinary input",
        );
    }
    for (name, source, category) in [
        (
            "javascript-error-prototype-refusal",
            "export function readiness(){return 'Ready'} export function workflow(){Object.defineProperty(Error.prototype,'category',{set(){throw 'swallowed'}});try{host.call('unknown',{})}catch{};const o=host.call('observe',{});host.call('submit',{observation:o,actions:[{kind:'key_down',key:'A'}]});}",
            "Argument",
        ),
        (
            "javascript-rejection-identity",
            "export function readiness(){return 'Ready'} export async function workflow(){Promise.reject(new Error('first-unhandled'));await Promise.reject(new Error('handled-later')).catch(()=>{});}",
            "Script",
        ),
    ] {
        let inventory = replace_entry(&inventories["javascript"], source)?;
        let record = run_once(
            &plan("javascript", "success", "template-first"),
            &inventory,
            None,
            false,
            None,
        )?;
        let passed = record
            .primary
            .as_ref()
            .is_some_and(|fault| fault.category == category)
            && record.cleanup["clean"] == true
            && !record.forced
            && record.observations["effects"]
                .as_array()
                .is_some_and(Vec::is_empty);
        case(
            &mut rows,
            name,
            record,
            passed,
            "error machinery cannot hide a host refusal or erase an earlier unhandled rejection",
        );
    }
    let held_plan = plan("rust", "held-work", "template-first");
    let held = run_once_after_milestone(&held_plan, &inventories["rust"], "WorkHeld", 100, false)?;
    let held_passed = held.forced
        && held.cleanup["clean"] != true
        && held.milestones.iter().any(|row| row["event"] == "WorkHeld");
    case(
        &mut rows,
        "nonreturning-work-containment",
        held,
        held_passed,
        "held physical owner requires child termination and reaping; no clean cleanup claim",
    );
    rows.push(crate::runner::parent_loss_evidence(
        &held_plan,
        &inventories["rust"],
    )?);
    let build = crate::report::build_identity();
    let inventory_identities: BTreeMap<_, _> = inventories
        .iter()
        .map(|(candidate, inventory)| (*candidate, inventory.identity.as_str()))
        .collect();
    let qualification = crate::report::controlled_qualification(&rows, &inventory_identities)?;
    let evidence_nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| Fault::new("Clock", error.to_string()))?
        .as_nanos();
    for row in &mut rows {
        if row.get("build").is_none() {
            row["build"] = build.clone();
        }
        row["qualification"] = qualification.clone();
        if row.get("run").is_none() {
            row["run"] = json!(format!(
                "check-{}-{evidence_nonce}-{}",
                std::process::id(),
                row["id"].as_str().unwrap_or("unknown")
            ));
        }
    }
    let matrix = crate::report::coverage(&rows)?;
    let check_passed = rows.iter().all(|row| row["status"] == "PASS");
    let mut output = json!({"version":1,"check_passed":check_passed,"rows":rows,
        "coverage":matrix,"native_disposition":{"windows":"BLOCKED: this controlled check grants no native target, workload, or capture/input authority",
            "macos":"BLOCKED: this controlled check grants no native target, workload, or capture/input authority"}});
    output["summary"] = crate::report::summarize(&output)?;
    Ok(output)
}
