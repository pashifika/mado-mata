use super::*;

#[test]
fn awaited_helpers_keep_their_own_context_and_cannot_reuse_retired_handles() {
    let plan = plan("javascript");
    let inventory = inventory(
        &plan,
        r#"
export function readiness() { return 'Ready'; }
async function late(scan) {
    await Promise.resolve();
    scan.call('observe', {});
}
export async function workflow() {
    const root = host.call('context', {});
    if (root !== host.call('context', {}) || root.done || root.deadline_ms === null) throw Error('root');
    const search = root.child({ deadline_ms: root.deadline_ms + 1000 });
    if (search.deadline_ms !== root.deadline_ms) throw Error('extended parent');
    const earlier = search.child({ timeout_ms: 1 });
    try {
        await Promise.resolve();
        earlier.call('wait', { duration_ms: 40 });
        throw Error('admitted expired search');
    } catch (error) {
        if (error.category !== 'ScopeDeadline' || earlier.cause !== 'ScopeDeadline') throw error;
    } finally { earlier.close(); }
    const left = search.child();
    const frame = left.call('observe', {});
    const right = root.child();
    search.cancel('search finished');
    search.cancel('replacement');
    search.close();
    left.close();
    search.close();
    if (!left.done || left.cause !== 'search finished' || right.done || root.done) throw Error('cause');
    try { await late(left); throw Error('late callback admitted'); }
    catch (error) { if (error.category !== 'ScopeCancelled') throw error; }
    try { right.call('submit', { observation: frame, actions: [{kind:'key_down', key:'A'}] }); throw Error('stale frame'); }
    catch (error) { if (error.category !== 'ScopeMismatch') throw error; }
    const fresh = right.call('observe', {});
    const sequence = right.call('submit', { observation: fresh, actions: [{kind:'click',x:1,y:1,button:'left'}] });
    if (right.call('settle', {id:sequence.id}).status !== 'Submitted') throw Error('fresh decision failed');
    right.close();
    for (let i = 0; i < 100; i++) root.child().close();
}
"#,
        "export {};",
    );
    let host = host(plan, &inventory);
    run(inventory, host.clone()).unwrap();
    let snapshot = host.snapshot();
    assert_eq!(snapshot["dispatches"], 1);
    assert_eq!(snapshot["live_handles"], 0);
    assert_eq!(snapshot["live_contexts"], 0);
    assert!(host.failure().is_none());
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn a_caught_uncertain_action_cannot_open_another_scope() {
    let mut plan = plan("javascript");
    plan.scenario = "uncertain".into();
    let inventory = inventory(
        &plan,
        r#"
export function readiness() { return 'Ready'; }
export function workflow() {
    const root = host.call('context', {});
    const child = root.child();
    const observation = child.call('observe', {});
    const sequence = child.call('submit', {observation, actions:[{kind:'click',x:1,y:1,button:'left'}]});
    try { child.call('settle', {id:sequence.id}); } catch (_) {}
    root.child().call('observe', {});
}
"#,
        "export {};",
    );
    let host = host(plan, &inventory);
    assert_eq!(
        run(inventory, host.clone()).unwrap_err().category,
        "InputUncertain"
    );
    let snapshot = host.snapshot();
    assert_eq!(snapshot["receipts"][0]["status"], "Uncertain");
    assert_eq!(snapshot["dispatches"], 1);
    assert_eq!(host.finish()["clean"], true);
    assert_eq!(host.terminal_accounting().unwrap()["input_uncertain"], true);
}
