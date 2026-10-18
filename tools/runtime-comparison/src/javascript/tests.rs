use super::*;
use crate::host::resolve_options;
use crate::model::Plan;
use std::path::Path;

fn plan(runtime: &str) -> Plan {
    let mut plan = crate::check::plan(runtime, "success", "template-first");
    // Desktop copies the reviewed expanded-event allowance without enlarging it.
    plan.limits.max_actions = 4;
    plan
}

fn inventory(plan: &Plan, source: &str, helper: &str) -> Arc<Inventory> {
    let mut inventory = Inventory::capture(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(&plan.candidate),
        &plan.limits,
    )
    .unwrap();
    let extension = if plan.candidate == "typescript" {
        "ts"
    } else {
        "js"
    };
    inventory
        .sources
        .insert(format!("main.{extension}"), source.into());
    inventory
        .sources
        .insert(format!("decisions.{extension}"), helper.into());
    inventory.refresh_identity().unwrap();
    if plan.candidate == "typescript" {
        inventory = crate::typescript::compile(&inventory, &plan.limits).unwrap();
    }
    Arc::new(inventory)
}

fn host(plan: Plan, inventory: &Inventory) -> Host {
    let options = resolve_options(
        &inventory.schema,
        &inventory.profiles[&plan.profile],
        &inventory.package_id,
    )
    .unwrap();
    let control = Arc::new(Control::new(&plan.limits));
    Host::new(plan, options, inventory.assets.clone(), control).unwrap()
}

#[test]
fn tiny_input_budget_allows_module_and_async_work_but_refuses_fifth_event() {
    for runtime in ["javascript", "typescript"] {
        for excess in [false, true] {
            let plan = plan(runtime);
            let ready_type = if runtime == "typescript" {
                ": Promise<MadoReady>"
            } else {
                ""
            };
            let extra = if excess {
                "host.call('submit', { observation, actions: [{ kind: 'key_down', key: 'B' }] });"
            } else {
                ""
            };
            let source = format!(
                r#"
import {{ value }} from './decisions.js';
const initialized = await value();
export async function readiness(){ready_type} {{
    if (initialized !== 7 || await value() !== 7) throw new Error('module state');
    return 'Ready';
}}
export async function workflow() {{
    for (let index = 0; index < 16; index++) await value();
    const observation = host.call('observe', {{}});
    for (let index = 0; index < 2; index++) {{
        const queued = host.call('submit', {{ observation, actions: [
            {{ kind: 'key_down', key: 'A' }}, {{ kind: 'key_up', key: 'A' }}
        ] }});
        host.call('settle', {{ id: queued.id }});
    }}
    {extra}
}}
"#
            );
            let inventory = inventory(
                &plan,
                &source,
                "export async function value() { return await Promise.resolve(7); }",
            );
            let host = host(plan, &inventory);
            let result = run(inventory, host.clone());
            let cleanup = host.finish();
            let snapshot = host.snapshot();
            assert_eq!(cleanup["clean"], true, "{cleanup}");
            assert_eq!(host.limits().max_actions, 4);
            assert_eq!(
                snapshot["accepted"].as_array().unwrap().len(),
                2,
                "{snapshot}"
            );
            assert_eq!(
                snapshot["effects"].as_array().unwrap().len(),
                4,
                "{snapshot}"
            );
            assert!(
                snapshot["receipts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|receipt| receipt["status"] == "Submitted")
            );
            if excess {
                assert_eq!(result.unwrap_err().category, "ActionLimit");
            } else {
                let metrics = result.unwrap();
                assert!(metrics.jobs_executed > 4);
                let diagnostic = metrics.source_diagnostic.unwrap();
                assert_eq!(diagnostic["promise_limit"], 16_384);
                assert_eq!(diagnostic["job_limit"], 16_384);
            }
        }
    }
}

#[test]
fn tiny_input_budget_does_not_limit_handled_rejections() {
    let plan = plan("javascript");
    let inventory = inventory(
        &plan,
        r#"
export function readiness() { return 'Ready'; }
export async function workflow() {
    const rejected = Array.from({ length: 8 }, () => Promise.reject(new Error('handled')));
    await Promise.all(rejected.map(promise => promise.catch(() => {})));
    host.call('log', { message: 'handled' });
}
"#,
        "export {};",
    );
    let host = host(plan, &inventory);
    let result = run(inventory, host.clone());
    let cleanup = host.finish();
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(cleanup["clean"], true);
    assert_eq!(host.snapshot()["logs"], json!(["handled"]));
}

#[test]
fn promise_and_microtask_storms_exhaust_vm_work_without_input() {
    for body in [
        "for (;;) new Promise(() => {});",
        "for (;;) await Promise.resolve();",
        "await new Promise(() => { function again() { queueMicrotask(again); } again(); });",
    ] {
        let plan = plan("javascript");
        let source = format!(
            "export function readiness() {{ return 'Ready'; }} export async function workflow() {{ host.call('log', {{ message: 'storm started' }}); {body} }}"
        );
        let inventory = inventory(&plan, &source, "export {};");
        let host = host(plan, &inventory);
        let result = run(inventory, host.clone());
        let cleanup = host.finish();
        assert_eq!(result.unwrap_err().category, "ResourceLimit", "{body}");
        // The creation hook, not a deadline or heap failure, latched exhaustion.
        assert_eq!(host.failure().unwrap().category, "ResourceLimit", "{body}");
        assert_eq!(host.snapshot()["logs"], json!(["storm started"]));
        assert_eq!(host.snapshot()["effects"], json!([]));
        assert_eq!(cleanup["clean"], true);
    }
}

#[test]
fn thenable_job_storm_exhausts_jobs_without_creating_more_promises() {
    let plan = plan("javascript");
    let inventory = inventory(
        &plan,
        r#"
export function readiness() { return 'Ready'; }
export function workflow() {
    host.call('log', { message: 'storm started' });
    const thenable = { then(resolve) { resolve(thenable); } };
    return Promise.resolve(thenable);
}
"#,
        "export {};",
    );
    let host = host(plan, &inventory);
    let result = run(inventory, host.clone());
    let cleanup = host.finish();
    assert_eq!(result.unwrap_err().category, "ResourceLimit");
    assert!(
        host.failure().is_none(),
        "the promise creation hook must not exhaust"
    );
    assert_eq!(host.snapshot()["logs"], json!(["storm started"]));
    assert_eq!(host.snapshot()["effects"], json!([]));
    assert_eq!(cleanup["clean"], true);
}
