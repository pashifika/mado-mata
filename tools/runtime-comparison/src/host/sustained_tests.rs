use super::*;
use crate::inventory::Inventory;
use crate::model::{NativeBudgets, NativeWorkflow};
use std::path::Path;

// Each completed drag advances the fixture clock and supplies the next scene. The final
// READY screen appears only after 301 actions; the Script must read it to choose its drop.
#[test]
fn one_workflow_guards_a_fresh_effect_past_all_finite_limits() {
    let plan = crate::check::plan("typescript", "success", "template-first");
    let mut inventory = Inventory::capture(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/typescript"),
        &plan.limits,
    )
    .unwrap();
    inventory.sources.insert("main.ts".into(), r#"
export function readiness(): MadoReady { return 'Ready'; }

async function decide(ctx: MadoWorkflowContext, observation: MadoObservation) {
    for (let n = 0; n < 64; ++n) await Promise.resolve();
    const match = ctx.call('recognize', { kind: 'ocr', observation,
        roi: { x: 100, y: 80, width: 40, height: 20 } });
    if (!match) throw new Error('missing guard');
    try { return match.text; }
    finally { ctx.call('release', {id: match.id}); }
}

export async function workflow() {
    const ctx = host.call('context', {});
    let before = ctx.call('observe', {});
    try {
        for (let i = 0; i < 302; ++i) {
            const text = await decide(ctx, before);
            if (text !== (i === 0 || i === 301 ? 'READY' : 'DONE')) throw new Error('wrong scene');
            const x = text === 'READY' ? 20 : 10;
            const {id} = ctx.call('submit', {observation: before, actions: [{kind:'drag', button:'left',
                path: [{x:1,y:1}, {x,y:1}]}]});
            try {
                if (ctx.call('settle', {id}).status !== 'Submitted') throw new Error('incomplete drag');
            } finally { ctx.call('release', {id}); }
            const after = ctx.call('observe', {});
            try {
                const effect = ctx.call('postcondition', {checkpoint:before, observation:after,
                    expected: i === 300 ? 'READY' : 'DONE'});
                if (!effect.satisfied || effect.frame <= effect.checkpoint) throw new Error('no fresh effect');
            } finally { ctx.call('release', {id:before.id}); before = after; }
        }
    } finally { ctx.call('release', {id: before.id}); }
}
"#.into());
    inventory.refresh_identity().unwrap();
    let inventory = Arc::new(crate::typescript::compile(&inventory, &plan.limits).unwrap());
    let options = resolve_options(
        &inventory.schema,
        &inventory.profiles[&plan.profile],
        &inventory.package_id,
    )
    .unwrap();
    let vm_budget = plan.limits.vm_bytes;
    let control = Arc::new(Control::reserved(None).unwrap());
    control
        .start_native(
            NativeBudgets {
                workflow: NativeWorkflow::Continuous { overall_ms: None },
                ..NativeBudgets::DEFAULT
            },
            None,
        )
        .unwrap();
    let modules =
        crate::typescript::PreparedModules::capture(&inventory, &plan.limits, &control).unwrap();
    control.native_transition(1, None).unwrap();
    control.native_transition(2, None).unwrap();
    let host = Host::new(plan, options, inventory.assets.clone(), control.clone()).unwrap();
    let inner = Arc::downgrade(&host.inner);
    let completed = AtomicUsize::new(0);
    let samples = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::clone(&samples);
    *lock(&host.inner.drag_hook) = Some(Arc::new(move |events, waiting| {
        if events != 4 || waiting {
            return;
        }
        let n = completed.fetch_add(1, Ordering::Relaxed) + 1;
        if matches!(n, 1 | 151 | 302) {
            let inner = inner.upgrade().unwrap();
            let state = lock(&inner.state);
            lock(&active).push((
                state.handles.len(),
                state.queue.len(),
                state
                    .receipts
                    .values()
                    .filter(|r| r.permit.is_some())
                    .count(),
                inner.physical.load(Ordering::Acquire),
            ));
        }
        if n == 301 {
            lock(&inner.upgrade().unwrap().state).visible = "READY".into();
        }
        control.set_test_time(control.now() + Duration::from_secs(3));
    }));
    let result = crate::javascript::run_prepared(inventory, host.clone(), &modules);
    let cleaned = host.finish();
    let metrics = result.expect("one continuous Workflow on the controlled sink");
    assert_eq!(cleaned["clean"], true);
    // At the last ordinary event the source frame is retained; the sequence is
    // active, not queued, and no earlier receipt or physical worker is owed.
    assert_eq!(*lock(&samples), [(1, 0, 0, 0); 3]);
    assert!(host.control().elapsed_us() > 900_000_000);
    assert!(metrics.jobs_executed > 16_384);
    assert!(
        metrics.source_diagnostic.unwrap()["promises_created"]
            .as_u64()
            .unwrap()
            > 16_384
    );
    assert!((1..=vm_budget).contains(&metrics.vm_bytes.unwrap()));
    let facts = host.snapshot();
    assert_eq!(facts["dispatches"], 302);
    assert!(facts["observations"].as_u64().unwrap() > 300);
    assert_eq!(facts["pointer_position"], json!({"x":20.0,"y":1.0}));
    assert_eq!(facts["attempt_owners"], 0);
    assert_eq!(facts["live_handles"], 0);
    assert_eq!(facts["in_flight_native"], 0);
    assert_eq!(
        host.terminal_accounting().unwrap()["expanded_input_events"],
        302 * 4
    );
}
