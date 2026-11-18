use super::*;
use crate::host::test_support::{make_host, observe, ready_host, wait_until};

fn child(parent: &Host) -> Host {
    let id = parent.child(&json!({})).unwrap()["id"].as_u64().unwrap();
    parent.scoped(id).unwrap()
}

#[test]
fn cancelling_a_parent_interrupts_an_already_polling_descendant_without_closing_root() {
    let root = ready_host("query-absent");
    let parent = child(&root);
    let scan = child(&parent);
    let observation = observe(&scan);
    let query = scan.call("query", json!({"observation":observation,"kind":"ocr","roi":{"x":0,"y":0,"width":640,"height":480}})).unwrap();
    let before = lock(&root.inner.state).observations;
    let reader = scan.clone();
    let task = thread::spawn(move || {
        reader.call("query_wait", json!({"id":query["id"],"timeout_ms":500}))
    });
    wait_until(|| lock(&root.inner.state).observations > before);
    let parent_id = parent.scope.as_ref().unwrap().id();
    root.context_rpc(
        parent_id,
        "cancel",
        "context",
        json!({"cause":"changed scene"}),
    )
    .unwrap();
    let error = task.join().unwrap().unwrap_err();
    assert_eq!(
        (error.category.as_str(), error.message.as_str()),
        ("ScopeCancelled", "changed scene")
    );
    root.context_rpc(parent_id, "close", "context", json!({}))
        .unwrap();
    assert_eq!(root.snapshot()["live_handles"], 0);
    let fresh = observe(&root);
    assert!(fresh["frame"].as_u64() > observation["frame"].as_u64());
    assert_eq!(root.finish()["clean"], true);
}

#[test]
fn sibling_settlement_and_a_fresh_attempt_never_inherit_cancelled_input() {
    let root = ready_host("success");
    let former = child(&root);
    let sibling = child(&root);
    let old = observe(&former);
    let fresh = observe(&sibling);
    let submit = |scope: &Host, frame: &Value| {
        scope.call(
            "submit",
            json!({
        "observation": frame, "actions": [{"kind":"click","x":1,"y":1,"button":"left"}]}),
        )
    };
    submit(&former, &old).unwrap();
    let accepted = submit(&sibling, &fresh).unwrap();
    root.context_rpc(
        former.scope.as_ref().unwrap().id(),
        "cancel",
        "context",
        json!({"cause":"retired"}),
    )
    .unwrap();
    assert_eq!(
        sibling
            .call("settle", json!({"id":accepted["id"]}))
            .unwrap()["status"],
        "Submitted"
    );
    let receipts = root.snapshot()["receipts"].clone();
    assert_eq!(receipts[0]["reason"], "ScopeCancelled");
    assert_eq!(receipts[0]["submitted"], 0);
    assert_eq!(root.finish()["clean"], true);
    let next = root.schedule_fresh().unwrap().admit().unwrap();
    next.begin_readiness().unwrap();
    next.begin_workflow().unwrap();
    assert_eq!(
        submit(&child(&next), &old).unwrap_err().category,
        "ScopeMismatch"
    );
    assert_eq!(next.snapshot()["dispatches"], 0);
    assert_eq!(next.finish()["clean"], true);
}

#[test]
fn settling_a_later_sibling_charges_one_unsplit_rolling_window_per_dispatch() {
    let mut root = make_host("success", "template-first");
    let control = Arc::new(Control::reserved(None).unwrap());
    Arc::get_mut(&mut root.inner).unwrap().control = Arc::clone(&control);
    control
        .start_native(crate::desktop::continuous_limits().budgets(), None)
        .unwrap();
    control.native_transition(1, None).unwrap();
    root.begin_readiness().unwrap();
    control.native_transition(2, None).unwrap();
    root.begin_workflow().unwrap();
    let a = child(&root);
    let b = child(&root);
    let clock = control.now();
    control.set_test_time(clock);
    let frame_a = observe(&a);
    let t0 = clock + Duration::from_millis(100);
    control.set_test_time(t0);
    let frame_b = observe(&b);
    let click = json!({"kind":"click","x":1,"y":1,"button":"left"});
    let submit = |host: &Host, frame: &Value, count: usize| {
        host.call(
            "submit",
            json!({"observation":frame,"actions":vec![&click; count]}),
        )
        .unwrap()
    };
    let settle =
        |host: &Host, sequence: Value| host.call("settle", json!({"id":sequence["id"]})).unwrap();
    let first = submit(&a, &frame_a, 21); // 63 expanded events, not 21.
    let second = submit(&b, &frame_b, 2);
    let refused = settle(&b, second);
    assert_eq!(refused["status"], "Refused");
    assert_eq!(refused["submitted"], 0);
    assert_eq!(refused["reason"], "InputRate");
    assert_eq!(settle(&a, first)["submitted"], 21);
    control.set_test_time(t0 + Duration::from_millis(999));
    assert_eq!(settle(&b, submit(&b, &frame_b, 1))["reason"], "InputRate");
    control.set_test_time(t0 + Duration::from_millis(1000));
    assert_eq!(settle(&b, submit(&b, &frame_b, 2))["submitted"], 2);
    assert_eq!(root.snapshot()["dispatches"], 2);
    assert_eq!(root.finish()["clean"], true);
    assert_eq!(
        root.terminal_accounting().unwrap()["expanded_input_events"],
        69
    );
}
