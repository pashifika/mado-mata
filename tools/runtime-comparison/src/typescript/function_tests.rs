use super::*;
use crate::host::{Host, resolve_options};
use crate::model::{Plan, RuntimeMetrics};
use std::path::Path;
use std::sync::atomic::Ordering;

fn plan(runtime: &str) -> Plan {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../fixtures/manual-plan.json")).unwrap();
    plan.candidate = runtime.into();
    plan.limits.duration_ms = 30_000;
    plan
}

fn fixture(runtime: &str, source: &str) -> Inventory {
    let plan = plan(runtime);
    let mut inventory = Inventory::capture(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(runtime),
        &plan.limits,
    )
    .unwrap();
    let extension = if runtime == "typescript" { "ts" } else { "js" };
    inventory
        .sources
        .insert(format!("decisions.{extension}"), source.into());
    inventory.sources.insert(format!("main.{extension}"),
        "export function readiness() { throw new Error('readiness must not run'); }\nexport function workflow() { throw new Error('workflow must not run'); }\nthrow new Error('unrelated module must not instantiate');\n".into());
    inventory.refresh_identity().unwrap();
    inventory
}

fn invocation(module: &str, function: &str, arguments: Vec<Value>) -> FunctionInvocation {
    FunctionInvocation {
        module: module.into(),
        function: function.into(),
        arguments,
    }
}

fn execute(inventory: &Inventory, prepared: &PreparedModules) -> Result<RuntimeMetrics, Fault> {
    let plan = plan("javascript");
    let control = Arc::new(Control::new(&plan.limits));
    let options = resolve_options(
        &inventory.schema,
        &inventory.profiles[&plan.profile],
        &inventory.package_id,
    )
    .unwrap();
    let host = Host::new(plan, options, inventory.assets.clone(), control.clone()).unwrap();
    let result =
        crate::javascript::run_prepared(Arc::new(inventory.clone()), host.clone(), prepared);
    assert!(!control.admission.load(Ordering::Acquire));
    assert_eq!(host.finish()["clean"], true);
    result
}

fn prepare(
    inventory: &Inventory,
    invocation: &FunctionInvocation,
) -> Result<(Inventory, PreparedModules), Fault> {
    let plan = plan(inventory.metadata["runtime"].as_str().unwrap());
    PreparedModules::capture_function(
        inventory,
        invocation,
        &plan.limits,
        &Arc::new(Control::new(&plan.limits)),
    )
}

#[test]
fn discovery_is_nonexecuting_and_lists_concrete_file_level_implementations_once() {
    let mut inventory = fixture(
        "typescript",
        r#"
throw new Error('discovery must not evaluate');
function local(a: number = 1, b: number, c?: string, ...rest: number[]) { return b; }
const arrow = async (value: number) => value;
const expression = function (value: number) { return value; };
function overloaded(value: string): string;
function overloaded(value: number): number;
function overloaded(value: string | number) { return value; }
function outer() { function nested() { return 1; } return nested(); }
function* generator() { yield 1; }
function receiver(this: { value: number }) { return this.value; }
const object = { method() { return 1; } };
export { local as renamed };
"#,
    );
    inventory.sources.insert(
        "main.ts".into(),
        "export { renamed as readiness, arrow as workflow } from './decisions.js';".into(),
    );
    inventory.refresh_identity().unwrap();
    let control = Control::new(&plan("typescript").limits);
    let candidates = function_candidates(&inventory, &control).unwrap();
    let names: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.function.as_str())
        .collect();
    assert_eq!(
        names,
        ["local", "arrow", "expression", "overloaded", "outer"]
    );
    let local = &candidates[0];
    assert_eq!(local.module, "decisions.ts");
    assert_eq!(local.required_arguments, 2);
    assert_eq!(local.line, 3);
    assert!(local.signature.contains("c?: string"));
    control.cancel();
    assert_eq!(
        function_candidates(&inventory, &control)
            .unwrap_err()
            .category,
        "Cancelled"
    );
}

#[test]
fn local_async_bridge_preserves_arguments_fresh_state_and_snapshot_isolation() {
    let mut inventory = fixture(
        "typescript",
        r#"
let count = 0;
const __mado_function = 100;
async function battle(value: number, optional = 2, ...rest: number[]) {
    count += 1;
    await Promise.resolve();
    return count + value + optional + rest.reduce((sum, item) => sum + item, 0);
}
"#,
    );
    let original = inventory.sources.clone();
    let call = invocation("decisions.ts", "battle", vec![json!(4), json!(3), json!(5)]);
    let (compiled, prepared) = prepare(&inventory, &call).unwrap();
    assert_eq!(inventory.sources, original);
    for _ in 0..2 {
        let result = execute(&compiled, &prepared).unwrap();
        assert_eq!(
            result.return_summary.unwrap(),
            json!({"kind":"number","value":13.0})
        );
    }
    inventory.sources.insert(
        "decisions.ts".into(),
        "async function battle(value: number) { return value + 20; }".into(),
    );
    inventory.refresh_identity().unwrap();
    assert_eq!(
        execute(&compiled, &prepared)
            .unwrap()
            .return_summary
            .unwrap()["value"],
        13.0
    );
    let (new_compiled, new_prepared) = prepare(&inventory, &call).unwrap();
    assert_eq!(
        execute(&new_compiled, &new_prepared)
            .unwrap()
            .return_summary
            .unwrap()["value"],
        24.0
    );
    assert_eq!(
        prepared.parser(&new_compiled).unwrap_err().category,
        "StaleIdentity"
    );
    inventory.sources.insert(
        "decisions.ts".into(),
        "function renamed(value: number) { return value; }".into(),
    );
    inventory.refresh_identity().unwrap();
    assert_eq!(
        prepare(&inventory, &call).unwrap_err().category,
        "FunctionEntry"
    );
}

#[test]
fn exported_async_exceptions_keep_original_function_coordinates() {
    let inventory = fixture(
        "typescript",
        "export async function battle() {\n    await Promise.resolve();\n    throw new Error('original battle error');\n}\n",
    );
    let (compiled, prepared) =
        prepare(&inventory, &invocation("decisions.ts", "battle", vec![])).unwrap();
    let fault = map_fault(&compiled, execute(&compiled, &prepared).unwrap_err());
    assert_eq!(fault.category, "Script");
    assert_eq!(fault.context["selected_function"]["function"], "battle");
    assert!(
        fault.context["typescript"]["frames"]
            .as_array()
            .unwrap()
            .iter()
            .any(|frame| frame["original"]["module"] == "decisions.ts"
                && frame["original"]["line"] == 3),
        "{fault:?}"
    );
}

#[test]
fn function_arguments_are_strict_bounded_and_missing_positions_are_refused() {
    let valid = json!({"module":"decisions.ts", "function":"battle"});
    assert!(
        serde_json::from_value::<FunctionInvocation>(valid.clone())
            .unwrap()
            .arguments
            .is_empty()
    );
    for value in [
        json!(["decisions.ts", "battle", []]),
        json!({"module":"decisions.ts", "function":"battle", "arguments":{}}),
        json!({"module":"decisions.ts", "function":"battle", "arguments":[], "extra":true}),
        json!({"module":"decisions.ts", "function":"battle", "arguments":vec![Value::Null; 33]}),
        json!({"module":"decisions.ts", "function":"battle", "arguments":["x".repeat(65_536)]}),
    ] {
        assert!(serde_json::from_value::<FunctionInvocation>(value).is_err());
    }
    let exact = invocation("decisions.ts", "battle", vec![json!("x".repeat(65_532))]);
    exact.validate().unwrap();
    serde_json::from_str::<FunctionInvocation>(&serde_json::to_string(&exact).unwrap()).unwrap();
    let mut nested = Value::Null;
    for _ in 0..31 {
        nested = json!([nested]);
    }
    invocation("decisions.ts", "battle", vec![nested.clone()])
        .validate()
        .unwrap();
    let too_deep = invocation("decisions.ts", "battle", vec![json!([nested])]);
    assert_eq!(
        too_deep.validate().unwrap_err().category,
        "FunctionArguments"
    );
    assert!(
        serde_json::from_str::<FunctionInvocation>(&serde_json::to_string(&too_deep).unwrap())
            .is_err()
    );
    let inventory = fixture(
        "typescript",
        "function battle(first = 2, second: number) { return first + second; }",
    );
    assert_eq!(
        prepare(
            &inventory,
            &invocation("decisions.ts", "battle", vec![json!(1)])
        )
        .unwrap_err()
        .category,
        "FunctionArguments"
    );
    let optional = fixture(
        "typescript",
        "const battle = (first = 2, second?: number) => first + (second ?? 3);",
    );
    let (compiled, prepared) =
        prepare(&optional, &invocation("decisions.ts", "battle", vec![])).unwrap();
    assert_eq!(
        execute(&compiled, &prepared)
            .unwrap()
            .return_summary
            .unwrap()["value"],
        5.0
    );
}

#[test]
fn function_loader_refusal_and_pending_promises_remain_finite() {
    for (source, category) in [
        (
            "export async function battle() { const name = 'missing'; try { await import(name); } catch {} return 1; }",
            "ImportRefused",
        ),
        ("const battle = () => new Promise(() => {});", "PendingWork"),
        (
            "host.call('observe', {}); function battle() { return 1; }",
            "AdmissionClosed",
        ),
    ] {
        let inventory = fixture("javascript", source);
        let (compiled, prepared) =
            prepare(&inventory, &invocation("decisions.js", "battle", vec![])).unwrap();
        assert_eq!(
            execute(&compiled, &prepared).unwrap_err().category,
            category
        );
    }
}

#[test]
fn return_projection_is_opaque_bounded_and_never_calls_user_conversion() {
    let inventory = fixture(
        "javascript",
        r#"
String.prototype.slice = () => { throw new Error('replaced slice'); };
function battle(kind) {
    if (kind === 'string') return '\u0000あ'.repeat(40000);
    if (kind === 'high-surrogate') return '\uD800';
    if (kind === 'low-surrogate') return '\uDC00';
    if (kind === 'supplementary') return '\u{1F600}';
    if (kind === 'split-surrogate') return 'a'.repeat(65535) + '\u{1F600}';
    if (kind === 'undefined') return;
    if (kind === 'number') return -0;
    if (kind === 'nan') return NaN;
    if (kind === 'infinity') return Infinity;
    if (kind === 'null') return null;
    if (kind === 'boolean') return true;
    if (kind === 'proxy') return new Proxy({}, { get() { throw new Error('proxy inspected'); } });
    const value = { get field() { throw new Error('accessor inspected'); }, toJSON() { throw new Error('conversion'); } };
    value.self = value;
    return value;
}
"#,
    );
    for (argument, expected) in [
        (
            "object",
            json!({"kind":"unavailable","reason":"nonprimitive return"}),
        ),
        (
            "proxy",
            json!({"kind":"unavailable","reason":"nonprimitive return"}),
        ),
        ("undefined", json!({"kind":"undefined"})),
        ("number", json!({"kind":"number","value":"-0"})),
        ("nan", json!({"kind":"number","value":"NaN"})),
        ("infinity", json!({"kind":"number","value":"Infinity"})),
        ("null", json!({"kind":"null","value":null})),
        ("boolean", json!({"kind":"boolean","value":true})),
        (
            "high-surrogate",
            json!({"kind":"unavailable","reason":"string projection failed"}),
        ),
        (
            "low-surrogate",
            json!({"kind":"unavailable","reason":"string projection failed"}),
        ),
        (
            "supplementary",
            json!({"kind":"string","value":"\u{1F600}","truncated":false}),
        ),
    ] {
        let (compiled, prepared) = prepare(
            &inventory,
            &invocation("decisions.js", "battle", vec![json!(argument)]),
        )
        .unwrap();
        assert_eq!(
            execute(&compiled, &prepared)
                .unwrap()
                .return_summary
                .unwrap(),
            expected
        );
    }
    let (compiled, prepared) = prepare(
        &inventory,
        &invocation("decisions.js", "battle", vec![json!("string")]),
    )
    .unwrap();
    let summary = execute(&compiled, &prepared)
        .unwrap()
        .return_summary
        .unwrap();
    assert_eq!(summary["kind"], "string");
    assert_eq!(summary["truncated"], true);
    assert!(serde_json::to_vec(&summary).unwrap().len() <= 65_536);
    let (compiled, prepared) = prepare(
        &inventory,
        &invocation("decisions.js", "battle", vec![json!("split-surrogate")]),
    )
    .unwrap();
    let summary = execute(&compiled, &prepared)
        .unwrap()
        .return_summary
        .unwrap();
    assert_eq!(summary["kind"], "string");
    assert_eq!(summary["value"], "a".repeat(65_536 - 128));
    assert_eq!(summary["truncated"], true);
}

#[test]
fn whole_package_still_invokes_readiness_before_workflow() {
    let mut inventory = fixture("typescript", "export const unused = 1;");
    inventory.sources.insert("main.ts".into(), "let ready = false;\nexport function readiness() { ready = true; return 'Ready'; }\nexport function workflow() { if (!ready) throw new Error('readiness skipped'); }".into());
    inventory.refresh_identity().unwrap();
    let plan = plan("typescript");
    let control = Arc::new(Control::new(&plan.limits));
    let compiled = Arc::new(compile_with_control(&inventory, &plan.limits, &control).unwrap());
    let prepared = PreparedModules::capture(&compiled, &plan.limits, &control).unwrap();
    assert!(
        execute(&compiled, &prepared)
            .unwrap()
            .return_summary
            .is_none()
    );
}

#[test]
fn function_preflight_checks_declared_entries_without_instantiating_them() {
    let mut inventory = fixture("javascript", "function battle() { return 1; }");
    inventory.sources.insert(
        "main.js".into(),
        "export function readiness() { return 'Ready'; }\nthrow new Error('must not instantiate');"
            .into(),
    );
    inventory.refresh_identity().unwrap();
    assert_eq!(
        prepare(&inventory, &invocation("decisions.js", "battle", vec![]))
            .unwrap_err()
            .category,
        "Entry"
    );
}

#[test]
fn selected_function_keeps_live_import_cycles_and_does_not_call_package_entries() {
    let mut inventory = fixture(
        "typescript",
        "import { increment } from './main.js';\nexport let value = 1;\nfunction battle() { return increment(); }",
    );
    inventory.sources.insert("main.ts".into(), "import { value } from './decisions.js';\nexport function increment() { return value + 1; }\nexport function readiness() { throw new Error('readiness invoked'); }\nexport function workflow() { throw new Error('workflow invoked'); }".into());
    inventory.refresh_identity().unwrap();
    let (compiled, prepared) =
        prepare(&inventory, &invocation("decisions.ts", "battle", vec![])).unwrap();
    assert_eq!(
        execute(&compiled, &prepared)
            .unwrap()
            .return_summary
            .unwrap()["value"],
        2.0
    );
}

#[test]
fn selected_cpu_loop_is_interrupted_by_the_operation_deadline() {
    let inventory = fixture("javascript", "function battle() { while (true) {} }");
    let (compiled, prepared) =
        prepare(&inventory, &invocation("decisions.js", "battle", vec![])).unwrap();
    let mut plan = plan("javascript");
    plan.limits.duration_ms = 100;
    plan.limits.readiness_ms = 100;
    plan.limits.wait_ms = 100;
    let control = Arc::new(Control::new(&plan.limits));
    let options = resolve_options(
        &compiled.schema,
        &compiled.profiles[&plan.profile],
        &compiled.package_id,
    )
    .unwrap();
    let host = Host::new(plan, options, compiled.assets.clone(), control.clone()).unwrap();
    let fault =
        crate::javascript::run_prepared(Arc::new(compiled), host.clone(), &prepared).unwrap_err();
    assert_eq!(fault.category, "Timeout");
    assert!(!control.admission.load(Ordering::Acquire));
    assert_eq!(host.finish()["clean"], true);
}
