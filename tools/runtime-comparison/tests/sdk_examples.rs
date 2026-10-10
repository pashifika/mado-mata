//! SDK discovery examples, read through the public discovery API, compile with the
//! pinned TypeScript compiler under representative option schemas and keep their
//! documented behavior on the controlled host. Declared stage/lane availability is
//! checked against actual controlled admission. No native target, capture, OCR model
//! or native input sink is used.
use mado_runtime_comparison::check::plan;
use mado_runtime_comparison::host::{Host, resolve_options};
use mado_runtime_comparison::images::PayloadBytes;
use mado_runtime_comparison::inventory::{Inventory, PackageDraft};
use mado_runtime_comparison::model::{Control, Fault, Limits};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MARKER: &[u8] = include_bytes!("../fixtures/typescript/assets/marker.rgba");
const RICH_SCHEMA: &str = include_str!("../fixtures/typescript/schema.json");
const EMPTY_SCHEMA: &str =
    r#"{"version":1,"type":"object","additionalProperties":false,"required":[],"properties":{}}"#;
const MINIMAL: &str = "export function readiness(): MadoReady { return \"Ready\"; }\nexport function workflow(): void {}\n";
const DISCOVERY: &str = r#"
import { pathToFileURL } from "node:url";
const sdk = await import(pathToFileURL(process.argv[1]).href);
const pages = [];
let cursor;
do {
  const response = sdk.searchSdk("", cursor === undefined ? { limit: 5 } : { cursor, limit: 5 });
  if (!response.ok) throw new Error(response.error.code);
  pages.push(response.result);
  cursor = response.result.next ?? undefined;
} while (cursor !== undefined && pages.length < 64);
const details = pages.flatMap(page => page.matches).map(match => sdk.sdkDetail(match.name));
process.stdout.write(JSON.stringify({ identity: sdk.sdk, pages, details }));
"#;

struct Example {
    source: String,
    methods: BTreeSet<String>,
}

struct Discovered {
    availability: BTreeMap<String, Value>,
    examples: BTreeMap<String, Example>,
}

impl Discovered {
    fn admits(&self, method: &str, field: &str, value: &str) -> bool {
        self.availability[method][field]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == value))
    }

    /// The fault the controlled host must return for a call at `stage`, or None.
    fn expected_refusal(&self, method: &str, stage: &str) -> Option<&'static str> {
        if !self.admits(method, "stages", stage) {
            Some("AdmissionClosed")
        } else if !self.admits(method, "lanes", "controlled") {
            Some("Authority")
        } else {
            None
        }
    }
}

struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        bytes
    })
}

/// Methods, availability and examples, read once through the public discovery API.
static DISCOVERED: LazyLock<Discovered> = LazyLock::new(discover);

fn discover() -> Discovered {
    let mut command = Command::new("node");
    command.env_clear();
    for name in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .args(["--input-type=module", "--eval", DISCOVERY, "--"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/compiler/sdk.mjs"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = Owned(command.spawn().expect("Node 24 runs SDK discovery"));
    let stdout = drain(child.0.stdout.take().unwrap());
    let stderr = drain(child.0.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "SDK discovery exceeded its finite wait"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let (stdout, stderr) = (stdout.join().unwrap(), stderr.join().unwrap());
    assert!(
        status.success(),
        "SDK discovery failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    let value: Value = serde_json::from_slice(&stdout).expect("SDK discovery emits JSON");
    let identity = &value["identity"];
    let pages = value["pages"].as_array().unwrap();
    let (last, earlier) = pages.split_last().unwrap();
    assert!(last["complete"] == true && earlier.iter().all(|page| page["complete"] == false));
    let names: Vec<&str> = pages
        .iter()
        .flat_map(|page| page["matches"].as_array().unwrap())
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(Some(names.len() as u64), identity["methods"].as_u64());
    assert_eq!(names.iter().collect::<BTreeSet<_>>().len(), names.len());
    let details = value["details"].as_array().unwrap();
    assert_eq!(details.len(), names.len());
    let mut availability = BTreeMap::new();
    let mut examples = BTreeMap::<String, Example>::new();
    for (name, response) in names.iter().zip(details) {
        assert_eq!(response["ok"], true, "{name}: {response}");
        let result = &response["result"];
        assert_eq!(result["noMatch"], false, "{name}");
        assert_eq!(&result["sdk"], identity);
        let method = &result["method"];
        assert_eq!(method["name"], *name);
        availability.insert((*name).to_owned(), method["availability"].clone());
        for example in method["examples"].as_array().unwrap() {
            let source = example["source"].as_str().unwrap();
            let entry = examples
                .entry(example["id"].as_str().unwrap().to_owned())
                .or_insert_with(|| Example {
                    source: source.to_owned(),
                    methods: BTreeSet::new(),
                });
            assert_eq!(entry.source, source, "one example id has one source");
            entry.methods.insert((*name).to_owned());
        }
    }
    Discovered {
        availability,
        examples,
    }
}

fn limits() -> Limits {
    plan("typescript", "success", "default").limits
}

/// Captures an owned package and compiles it with the pinned application compiler.
fn compiled(schema: &str, source: &str) -> Arc<Inventory> {
    let manifest = json!({
        "version":1,"package_id":"sdk-example","runtime":"typescript","sdk":"mado-host-v1",
        "entry_contract":"ready-string-v1",
        "entries":{"readiness":{"module":"main.ts","function":"readiness"},
            "workflow":{"module":"main.ts","function":"workflow"}},
        "sources":["main.ts"],"schema":"schema.json","profiles":{"default":"profiles/default.json"},
        "assets":{"marker":{"path":"assets/marker.rgba","format":"raw-rgba8","width":2,"height":2}},
        "source_maps":{},"dependencies":{}
    });
    let profile = json!({"package_id":"sdk-example","schema_version":1,"options":{}});
    let bytes = |value: Vec<u8>| PayloadBytes::new(value).unwrap();
    let files = BTreeMap::from([
        (
            "package.json".to_owned(),
            bytes(serde_json::to_vec(&manifest).unwrap()),
        ),
        ("schema.json".to_owned(), bytes(schema.as_bytes().to_vec())),
        (
            "profiles/default.json".to_owned(),
            bytes(serde_json::to_vec(&profile).unwrap()),
        ),
        ("main.ts".to_owned(), bytes(source.as_bytes().to_vec())),
        ("assets/marker.rgba".to_owned(), bytes(MARKER.to_vec())),
    ]);
    let inventory = PackageDraft::from_files(files, &limits())
        .and_then(|draft| draft.validate())
        .unwrap_or_else(|fault| panic!("owned package is invalid: {fault:?}"));
    Arc::new(
        mado_runtime_comparison::typescript::compile(&inventory, &limits())
            .unwrap_or_else(|fault| panic!("pinned compiler refused:\n{source}\n{fault:?}")),
    )
}

struct Run {
    scenario: String,
    outcome: Result<(), Fault>,
    during: Value,
}

impl Run {
    fn count(&self, method: &str) -> u64 {
        self.during["operation_metrics"][method]["count"]
            .as_u64()
            .unwrap()
    }

    fn failures(&self, method: &str) -> u64 {
        self.during["operation_metrics"][method]["failures"]
            .as_u64()
            .unwrap()
    }

    fn logs(&self) -> Vec<&str> {
        self.during["logs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect()
    }

    fn fault(&self) -> Option<&str> {
        self.outcome
            .as_ref()
            .err()
            .map(|fault| fault.category.as_str())
    }

    /// Normal exits release every handle themselves, before host teardown.
    fn returned_released(&self) -> &Self {
        assert!(
            self.outcome.is_ok(),
            "{}: {:?}",
            self.scenario,
            self.outcome
        );
        assert_eq!(self.during["live_handles"], 0, "{}", self.scenario);
        self
    }

    fn effects(&self) -> usize {
        self.during["effects"].as_array().unwrap().len()
    }
}

/// Runs the compiled package on the controlled host and always settles host teardown.
fn run(compiled: &Arc<Inventory>, scenario: &str) -> Run {
    let plan = plan("typescript", scenario, "default");
    let options = resolve_options(
        &compiled.schema,
        &compiled.profiles["default"],
        &compiled.package_id,
    )
    .unwrap();
    let control = Arc::new(Control::new(&plan.limits));
    let host = Host::new(plan, options, compiled.assets.clone(), control).unwrap();
    let outcome =
        mado_runtime_comparison::javascript::run(Arc::clone(compiled), host.clone()).map(|_| ());
    let during = host.snapshot();
    let cleanup = host.finish();
    assert_eq!(cleanup["clean"], true, "{scenario}: {cleanup}");
    assert_eq!(host.snapshot()["live_handles"], 0, "{scenario}");
    Run {
        scenario: scenario.to_owned(),
        outcome,
        during,
    }
}

/// Every scenario run for one example, with its behavioral oracle.
fn exercise(id: &str, compiled: &Arc<Inventory>, baseline: &Run) -> Vec<Run> {
    let runs: Vec<Run> = match id {
        "observation-lifetime" => vec![run(compiled, "success")],
        "grouped-ocr" | "template-match" | "bounded-poll" => vec![
            run(compiled, "success"),
            run(compiled, "no-match"),
            run(compiled, "backend-failure"),
        ],
        "visual-query" => vec![run(compiled, "success"), run(compiled, "query-absent")],
        "input-receipt" => vec![
            run(compiled, "success"),
            run(compiled, "postcondition-absent"),
            run(compiled, "partial"),
            run(compiled, "uncertain"),
        ],
        "native-target-startup" => vec![run(compiled, "success")],
        _ => panic!("SDK example {id} has no behavioral oracle"),
    };
    let scenario = |name: &str| runs.iter().find(|run| run.scenario == name).unwrap();
    match id {
        "observation-lifetime" => {
            let success = scenario("success").returned_released();
            assert_eq!(success.count("observe"), baseline.count("observe") + 1);
            assert_eq!(success.count("release"), baseline.count("release") + 1);
            assert_eq!(success.logs().len(), 1);
        }
        "grouped-ocr" => {
            let success = scenario("success").returned_released();
            assert_eq!(success.count("scan_ocr_zones"), 1);
            assert_eq!(success.logs(), ["title: 1 region(s)", "status: no_match"]);
            let absent = scenario("no-match").returned_released();
            assert_eq!(absent.logs(), ["title: no_match", "status: no_match"]);
            let failed = scenario("backend-failure");
            assert_eq!(failed.fault(), Some("Backend"));
            assert_eq!(failed.failures("scan_ocr_zones"), 1);
            assert!(failed.logs().is_empty());
        }
        "template-match" => {
            let success = scenario("success").returned_released();
            assert_eq!((success.count("asset"), success.count("recognize")), (1, 1));
            assert_eq!(success.logs(), ["marker: score 0.95"]);
            let absent = scenario("no-match").returned_released();
            assert_eq!(absent.logs(), ["marker: no_match"]);
            let failed = scenario("backend-failure");
            assert_eq!(failed.fault(), Some("Backend"));
            assert!(failed.logs().is_empty());
        }
        "visual-query" => {
            let success = scenario("success").returned_released();
            assert_eq!(
                (success.count("query"), success.count("query_wait")),
                (1, 1)
            );
            assert_eq!(success.logs().len(), 1);
            let absent = scenario("query-absent");
            assert_eq!(absent.fault(), Some("Timeout"));
            assert_eq!(absent.failures("query_wait"), 1);
            assert!(
                absent.logs().is_empty(),
                "a timeout is never reported as a match"
            );
            let waited = absent.during["operation_metrics"]["query_wait"]["total_us"]
                .as_u64()
                .unwrap();
            assert!(
                waited >= 500_000 && waited < limits().duration_ms * 1000,
                "{waited}"
            );
        }
        "input-receipt" => {
            let success = scenario("success").returned_released();
            assert_eq!(success.logs(), ["effect: confirmed"]);
            assert_eq!(success.during["receipts"][0]["status"], "Submitted");
            assert_eq!(
                success.during["receipts"][0]["sink"],
                "controlled-non-native"
            );
            assert_eq!(success.effects(), 2);
            let absent = scenario("postcondition-absent").returned_released();
            assert_eq!(absent.logs(), ["effect: absent"]);
            assert_eq!(absent.during["postconditions"][0]["satisfied"], false);
            for (name, status) in [("partial", "Partial"), ("uncertain", "Uncertain")] {
                let incomplete = scenario(name).returned_released();
                assert_eq!(incomplete.logs(), [format!("input: {status}")]);
                assert_eq!(
                    incomplete.count("submit"),
                    1,
                    "{name} input is not replayed"
                );
                assert_eq!(
                    incomplete.count("postcondition"),
                    0,
                    "{name} claims no effect"
                );
            }
        }
        "bounded-poll" => {
            let success = scenario("success").returned_released();
            assert_eq!((success.count("recognize"), success.count("wait")), (1, 0));
            let absent = scenario("no-match");
            assert_eq!(absent.fault(), Some("Script"), "{:?}", absent.outcome);
            assert_eq!((absent.count("recognize"), absent.count("wait")), (5, 4));
            assert_eq!(
                absent.during["live_handles"], 0,
                "probes release before giving up"
            );
            assert_eq!(scenario("backend-failure").fault(), Some("Backend"));
        }
        "native-target-startup" => {
            let refused = scenario("success");
            assert_eq!(refused.fault(), Some("Authority"), "{:?}", refused.outcome);
            assert_eq!(
                (
                    refused.count("target_start"),
                    refused.failures("target_start")
                ),
                (1, 1)
            );
            assert_eq!(refused.count("target_status"), 0);
            assert_eq!(refused.count("observe"), baseline.count("observe"));
        }
        _ => unreachable!(),
    }
    runs
}

#[test]
fn discovered_examples_compile_and_keep_their_controlled_contracts() {
    let discovered = &*DISCOVERED;
    let baseline = run(&compiled(EMPTY_SCHEMA, MINIMAL), "success");
    baseline.returned_released();
    for (id, example) in &discovered.examples {
        let rich = compiled(RICH_SCHEMA, &example.source);
        let empty = compiled(EMPTY_SCHEMA, &example.source);
        let runs = exercise(id, &rich, &baseline);
        let success = runs.iter().find(|run| run.scenario == "success").unwrap();
        let portable = run(&empty, "success");
        assert_eq!(
            (portable.fault(), portable.logs()),
            (success.fault(), success.logs()),
            "{id}: option schemas do not change example behavior"
        );
        let native_only = example
            .methods
            .iter()
            .any(|method| !discovered.admits(method, "lanes", "controlled"));
        assert_eq!(
            success.fault() == Some("Authority"),
            native_only,
            "{id}: static support differs from controlled availability"
        );
        for method in &example.methods {
            if discovered.admits(method, "lanes", "controlled") {
                assert!(
                    runs.iter()
                        .any(|run| run.count(method) > baseline.count(method)),
                    "{id} does not exercise {method}"
                );
            }
        }
        if !example.methods.contains("submit") {
            assert!(
                runs.iter().all(|run| run.effects() == 0),
                "{id} submitted input"
            );
        }
    }
}

#[test]
fn declared_stages_and_lanes_match_controlled_admission() {
    let discovered = &*DISCOVERED;
    for (method, args) in [
        ("observe", "{}"),
        ("asset", r#"{ id: "marker" }"#),
        ("wait", "{ duration_ms: 1 }"),
        ("log", r#"{ message: "probe" }"#),
        ("target_status", "{}"),
    ] {
        let source = format!("host.call(\"{method}\", {args});\n{MINIMAL}");
        let probe = run(&compiled(EMPTY_SCHEMA, &source), "success");
        assert_eq!(
            probe.fault(),
            discovered.expected_refusal(method, "module"),
            "{method} during module evaluation: {:?}",
            probe.outcome
        );
    }
    for (method, body) in [
        (
            "postcondition",
            "const a = host.call(\"observe\", {});\n  const b = host.call(\"observe\", {});\n  host.call(\"postcondition\", { observation: b, checkpoint: a, expected: \"READY\" });\n  host.call(\"release\", { id: b.id });\n  host.call(\"release\", { id: a.id });",
        ),
        (
            "submit",
            "const a = host.call(\"observe\", {});\n  host.call(\"submit\", { observation: a, actions: [{ kind: \"key_down\", key: \"A\" }] });\n  host.call(\"release\", { id: a.id });",
        ),
        ("wait", "host.call(\"wait\", { duration_ms: 1 });"),
        ("target_status", "host.call(\"target_status\", {});"),
    ] {
        let source = format!(
            "export function readiness(): MadoReady {{\n  {body}\n  return \"Ready\";\n}}\nexport function workflow(): void {{}}\n"
        );
        let probe = run(&compiled(EMPTY_SCHEMA, &source), "success");
        assert_eq!(
            probe.fault(),
            discovered.expected_refusal(method, "readiness"),
            "{method} during readiness: {:?}",
            probe.outcome
        );
        assert_eq!(
            probe.effects(),
            0,
            "{method}: readiness never dispatches input"
        );
    }
}
