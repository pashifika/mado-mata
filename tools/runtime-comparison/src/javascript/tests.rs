use super::*;
use crate::host::resolve_options;
use crate::model::Plan;
use std::path::Path;

#[path = "context_tests.rs"]
mod contexts;

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

fn continuous_host(
    plan: Plan,
    inventory: &Arc<Inventory>,
) -> (Host, crate::typescript::PreparedModules) {
    let options = resolve_options(
        &inventory.schema,
        &inventory.profiles[&plan.profile],
        &inventory.package_id,
    )
    .unwrap();
    // Exercise the real continuous Control with the existing non-native sink.
    // This is not an externally admissible controlled/continuous Plan.
    let control = Arc::new(Control::reserved(None).unwrap());
    control
        .start_native(
            crate::model::NativeBudgets {
                startup_ms: 60_000,
                readiness_ms: 30_000,
                workflow: crate::model::NativeWorkflow::Continuous { overall_ms: None },
            },
            None,
        )
        .unwrap();
    let modules =
        crate::typescript::PreparedModules::capture(inventory, &plan.limits, &control).unwrap();
    control.native_transition(1, None).unwrap();
    control.native_transition(2, None).unwrap();
    let host = Host::new(plan, options, inventory.assets.clone(), control).unwrap();
    (host, modules)
}

#[test]
fn continuous_helpers_cross_both_finite_work_caps_and_reclaim_handled_promises() {
    for runtime in ["javascript", "typescript"] {
        let plan = plan(runtime);
        let ready_type = if runtime == "typescript" {
            ": MadoReady"
        } else {
            ""
        };
        let source = format!(
            r#"
import {{ synchronous, asynchronous }} from './decisions.js';
export function readiness(){ready_type} {{ return 'Ready'; }}
export async function workflow() {{
    const total = synchronous() + await asynchronous();
    host.call('log', {{ message: String(total) }});
}}
"#
        );
        let inventory = inventory(
            &plan,
            &source,
            r#"
export function synchronous() {
    let sum = 0;
    for (let index = 0; index < 20000; index++) {
        Promise.resolve(index);
        sum += index;
    }
    return sum;
}
export async function asynchronous() {
    let sum = 0;
    for (let index = 0; index < 20000; index++) {
        // The handler is attached before rejection. QuickJS emits no Resolve
        // hook for these promises; completed work must still release capacity.
        sum += await Promise.resolve().then(() => { throw index; }).catch(() => index);
    }
    return sum;
}
"#,
        );
        let (host, modules) = continuous_host(plan, &inventory);
        let metrics = run_prepared(inventory, host.clone(), &modules).unwrap();
        let diagnostic = metrics.source_diagnostic.unwrap();
        assert_eq!(host.snapshot()["logs"], json!(["399980000"]));
        assert!(metrics.jobs_executed > 16_384, "{diagnostic}");
        assert!(diagnostic["promises_created"].as_u64().unwrap() > 16_384);
        assert!(diagnostic["work_slices"].as_u64().unwrap() > 1);
        assert_eq!(diagnostic["job_slice_limit"], 256);
        assert_eq!(diagnostic["promise_slice_limit"], 16_384);
        assert_eq!(diagnostic["finite_initialization_work_limit"], 16_384);
        assert_eq!(diagnostic["outstanding_promise_limit"], 1024);
        assert!(diagnostic["outstanding_promises_peak"].as_u64().unwrap() <= 1024);
        assert_eq!(diagnostic["outstanding_promises_at_entry_settlement"], 0);
        assert!(metrics.vm_bytes.unwrap() <= host.limits().vm_bytes);
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn continuous_pressure_limits_pending_work_and_unhandled_rejection_records() {
    for (body, message) in [
        (
            "for (;;) new Promise(() => {});",
            "JavaScript outstanding promise limit exceeded",
        ),
        (
            "const reason = new Error('pressure'); for (;;) Promise.reject(reason);",
            "unhandled rejection capacity exceeded",
        ),
    ] {
        let plan = plan("javascript");
        let source = format!(
            "export function readiness() {{ return 'Ready'; }} export function workflow() {{ {body} }}"
        );
        let inventory = inventory(&plan, &source, "export {};");
        let (host, modules) = continuous_host(plan, &inventory);
        let fault = run_prepared(inventory, host.clone(), &modules).unwrap_err();
        assert_eq!(fault.category, "ResourceLimit", "{body}: {fault:?}");
        assert_eq!(fault.message, message);
        assert_eq!(host.failure().unwrap().category, "ResourceLimit");
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert_eq!(host.snapshot()["effects"], json!([]));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn continuous_stop_interrupts_busy_js_jobs_and_thenables_without_context_checks() {
    for body in [
        "for (;;) {}",
        "for (;;) await Promise.resolve();",
        "const thenable = { then(resolve) { resolve(thenable); } }; await Promise.resolve(thenable);",
        "await new Promise(() => { function again() { queueMicrotask(again); } again(); });",
    ] {
        let plan = plan("javascript");
        let source = format!(
            "export function readiness() {{ return 'Ready'; }} export async function workflow() {{ host.call('log', {{ message: 'busy' }}); {body} }}"
        );
        let inventory = inventory(&plan, &source, "export {};");
        let (host, modules) = continuous_host(plan, &inventory);
        let stopper = host.clone();
        let fault = std::thread::scope(|scope| {
            scope.spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline && stopper.snapshot()["logs"] != json!(["busy"]) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                stopper.control().cancel();
            });
            run_prepared(inventory, host.clone(), &modules).unwrap_err()
        });
        assert_eq!(fault.category, "Cancelled", "{body}: {fault:?}");
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert!(host.control().stop_us.load(Ordering::Acquire) > 0);
        assert_eq!(host.snapshot()["logs"], json!(["busy"]));
        assert_eq!(host.snapshot()["effects"], json!([]));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn continuous_entry_settlement_discards_detached_jobs_and_bounds_diagnostics() {
    for (body, expected) in [
        (
            "queueMicrotask(() => host.call('log', { message: 'detached' }));",
            None,
        ),
        (
            "Promise.reject(new Error('é'.repeat(10000)));",
            Some("Script"),
        ),
    ] {
        let plan = plan("javascript");
        let source = format!(
            "export function readiness() {{ return 'Ready'; }} export function workflow() {{ {body} }}"
        );
        let inventory = inventory(&plan, &source, "export {};");
        let (host, modules) = continuous_host(plan, &inventory);
        let result = run_prepared(inventory, host.clone(), &modules);
        if let Some(category) = expected {
            let fault = result.unwrap_err();
            assert_eq!(fault.category, category);
            let truncation = &fault.context["diagnostic_truncation"];
            assert_eq!(
                fault.message.len(),
                truncation["field_byte_limit"].as_u64().unwrap() as usize
            );
            assert_eq!(
                truncation["message_bytes_dropped"],
                20_000 - fault.message.len()
            );
        } else {
            let diagnostic = result.unwrap().source_diagnostic.unwrap();
            assert_eq!(diagnostic["outstanding_promises_at_entry_settlement"], 1);
        }
        assert_eq!(host.snapshot()["logs"], json!([]));
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn continuous_preentry_pending_promises_share_workflow_capacity() {
    let source = r#"
const retained = [];
for (let index = 0; index < 600; index++) retained.push(new Promise(() => {}));
export function readiness() {
    for (let index = 0; index < 400; index++) retained.push(new Promise(() => {}));
    return 'Ready';
}
export function workflow() {
    host.call('log', { message: 'workflow' });
    for (let index = 0; index < 32; index++) retained.push(new Promise(() => {}));
}
"#;
    for continuous in [false, true] {
        let plan = plan("javascript");
        let inventory = inventory(&plan, source, "export {};");
        let (host, modules) = if continuous {
            continuous_host(plan, &inventory)
        } else {
            let host = host(plan, &inventory);
            let modules = crate::typescript::PreparedModules::capture(
                &inventory,
                &host.limits(),
                &host.control(),
            )
            .unwrap();
            (host, modules)
        };
        let result = run_prepared(inventory, host.clone(), &modules);
        assert_eq!(host.snapshot()["logs"], json!(["workflow"]));
        if continuous {
            let fault = result.unwrap_err();
            assert_eq!(fault.category, "ResourceLimit");
            assert_eq!(
                fault.message,
                "JavaScript outstanding promise limit exceeded"
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
        }
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn continuous_preentry_rejections_obey_the_same_retained_capacity() {
    for (phase, continuous) in [
        ("module", false),
        ("module", true),
        ("readiness", false),
        ("readiness", true),
    ] {
        let body = "for (let index = 0; index < 65; index++) Promise.reject(new Error('queued'));";
        let module = if phase == "module" { body } else { "" };
        let readiness = if phase == "readiness" { body } else { "" };
        let source = format!(
            "{module} export function readiness() {{ {readiness} return 'Ready'; }} export function workflow() {{ host.call('log', {{ message: 'workflow' }}); }}"
        );
        let plan = plan("javascript");
        let inventory = inventory(&plan, &source, "export {};");
        let (host, modules) = if continuous {
            continuous_host(plan, &inventory)
        } else {
            let host = host(plan, &inventory);
            let modules = crate::typescript::PreparedModules::capture(
                &inventory,
                &host.limits(),
                &host.control(),
            )
            .unwrap();
            (host, modules)
        };
        let fault = run_prepared(inventory, host.clone(), &modules).unwrap_err();
        if continuous {
            assert_eq!(fault.category, "ResourceLimit", "{phase}: {fault:?}");
            assert_eq!(fault.message, "unhandled rejection capacity exceeded");
            assert_eq!(host.snapshot()["logs"], json!([]));
        } else {
            assert_eq!(fault.category, "Script", "{phase}: {fault:?}");
            assert_eq!(fault.message, "queued");
            assert_eq!(host.snapshot()["logs"], json!(["workflow"]));
        }
        assert_eq!(host.workflow_entered(), !continuous);
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn continuous_initialization_still_uses_cumulative_vm_work_limits() {
    for phase in ["module", "readiness"] {
        let body = "for (let index = 0; index < 20000; index++) await Promise.resolve();";
        let module = if phase == "module" { body } else { "" };
        let readiness = if phase == "readiness" { body } else { "" };
        let source = format!(
            "{module} export async function readiness() {{ {readiness} return 'Ready'; }} export function workflow() {{ host.call('log', {{ message: 'forbidden' }}); }}"
        );
        let plan = plan("javascript");
        let inventory = inventory(&plan, &source, "export {};");
        let (host, modules) = continuous_host(plan, &inventory);
        let fault = run_prepared(inventory, host.clone(), &modules).unwrap_err();
        assert_eq!(fault.category, "ResourceLimit", "{phase}: {fault:?}");
        assert_eq!(fault.message, "JavaScript promise creation limit exceeded");
        assert_eq!(host.snapshot()["logs"], json!([]));
        assert!(!host.workflow_entered());
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert_eq!(host.finish()["clean"], true);
    }
}
