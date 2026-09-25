use super::*;
use crate::host::test_support::{observe, ready_host, submit, wait_until};

#[test]
fn clicks_use_capture_pixels_and_preserve_existing_key_wire_shape() {
    let host = ready_host("success");
    let observation = observe(&host);
    let actions = json!([
        {"kind":"key_down","key":"A"},
        {"kind":"key_up","key":"A"},
        {"kind":"click","x":12.5,"y":20.25,"button":"right"}
    ]);
    let sequence = host
        .call(
            "submit",
            json!({"observation":observation,"actions":actions}),
        )
        .expect("bounded mixed input");
    let receipt = host
        .call("settle", json!({"id":sequence["id"]}))
        .expect("settled");
    assert_eq!(receipt["status"], "Submitted");
    assert_eq!(receipt["submitted"], 3);
    assert_eq!(host.snapshot()["accepted"][0]["actions"], actions);
    assert_eq!(
        host.snapshot()["effects"][2],
        json!({
            "order":1,"kind":"click","x":12.5,"y":20.25,"button":"right"
        })
    );
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn invalid_click_coordinates_buttons_and_mixed_fields_never_enqueue() {
    let host = ready_host("success");
    let observation = observe(&host);
    for action in [
        json!({"kind":"click","x":640,"y":0,"button":"left"}),
        json!({"kind":"click","x":0,"y":480,"button":"left"}),
        json!({"kind":"click","x":-1,"y":0,"button":"left"}),
        json!({"kind":"click","x":null,"y":0,"button":"left"}),
        json!({"kind":"click","x":1,"y":1,"button":"unknown"}),
        json!({"kind":"click","x":1,"y":1,"button":"middle","key":"A"}),
        json!({"kind":"key_down","key":"A","x":1}),
    ] {
        assert_eq!(
            host.call(
                "submit",
                json!({
                    "observation":observation,"actions":[action]
                })
            )
            .expect_err("invalid input is refused")
            .category,
            "Argument"
        );
    }
    assert_eq!(host.snapshot()["accepted"], json!([]));
    assert_eq!(host.snapshot()["effects"], json!([]));
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn click_expansion_consumes_the_attempt_event_budget_even_when_released() {
    let host = ready_host("success");
    let observation = observe(&host);
    let clicks = vec![json!({"kind":"click","x":1,"y":1,"button":"left"}); 10];
    let queued = host
        .call(
            "submit",
            json!({"observation":observation,"actions":clicks}),
        )
        .expect("thirty expanded events fit");
    host.call("release", json!({"id":queued["id"]}))
        .expect("cancel queued input");
    assert_eq!(
        host.call(
            "submit",
            json!({
                "observation":observation,
                "actions":[{"kind":"click","x":1,"y":1,"button":"left"}]
            })
        )
        .expect_err("three more events exceed the thirty-two event budget")
        .category,
        "ActionLimit"
    );
    let keys = submit(&host, &observation, "A").expect("two remaining events fit");
    host.call("settle", json!({"id":keys["id"]}))
        .expect("keys settle");
    assert_eq!(host.snapshot()["receipts"][0]["status"], "Cancelled");
    assert_eq!(
        host.snapshot()["effects"],
        json!([
            {"order":2,"kind":"key_down","key":"A"},{"order":2,"kind":"key_up","key":"A"}
        ])
    );
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn enqueue_order_is_preserved_when_later_sequence_is_awaited_first() {
    let host = ready_host("success");
    let observation = observe(&host);
    let first = submit(&host, &observation, "A").expect("first");
    let second = submit(&host, &observation, "B").expect("second");
    assert_eq!(
        submit(&host, &observation, "C")
            .expect_err("overflow")
            .category,
        "QueueCapacity"
    );
    host.call("settle", json!({"id":second["id"]}))
        .expect("settle accepted order");
    assert_eq!(
        host.snapshot()["effects"],
        json!([
            {"order":1,"kind":"key_down","key":"A"},{"order":1,"kind":"key_up","key":"A"},
            {"order":2,"kind":"key_down","key":"B"},{"order":2,"kind":"key_up","key":"B"}
        ])
    );
    assert_eq!(
        host.call("settle", json!({"id":first["id"]}))
            .expect("retained receipt")["order"],
        1
    );
    assert_eq!(host.snapshot()["queue_high_water"], 2);
    assert_eq!(host.snapshot()["queue_rejections"], 1);
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn queued_generation_route_and_focus_changes_are_refused_at_dispatch() {
    for (event, category) in [
        ("geometry", "StaleIdentity"),
        ("session", "StaleIdentity"),
        ("focus_lost", "FocusRefused"),
        ("route_revoked", "RouteRefused"),
        ("target_exit", "TargetLost"),
    ] {
        let host = ready_host("success");
        let observation = observe(&host);
        let accepted = submit(&host, &observation, "A").expect("accepted before event");
        host.call("fixture", json!({"event":event}))
            .expect("controlled invalidation");
        let receipt = host
            .call("settle", json!({"id":accepted["id"]}))
            .expect("refusal receipt");
        assert_eq!(receipt["status"], "Refused", "{event}");
        assert_eq!(receipt["reason"], category, "{event}");
        assert_eq!(receipt["submitted"], 0);
        assert_eq!(host.snapshot()["effects"], json!([]));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn held_active_sequence_has_one_bounded_pending_queue() {
    let host = ready_host("held-work");
    let observation = observe(&host);
    let first = submit(&host, &observation, "A").expect("active");
    let settling = host.clone();
    let id = first["id"].clone();
    let thread = thread::spawn(move || settling.call("settle", json!({"id":id})));
    wait_until(|| host.snapshot()["in_flight_native"] == 1);
    let second = submit(&host, &observation, "B").expect("pending one");
    let third = submit(&host, &observation, "C").expect("pending two");
    assert_eq!(
        submit(&host, &observation, "D")
            .expect_err("no secondary backlog")
            .category,
        "QueueCapacity"
    );
    assert_eq!(host.snapshot()["queue_depth"], 2);
    host.call("fixture", json!({"event":"release_hold"}))
        .expect("complete held dispatch");
    assert_eq!(
        thread
            .join()
            .expect("settlement thread")
            .expect("first receipt")["status"],
        "Submitted"
    );
    host.call("settle", json!({"id":third["id"]}))
        .expect("serialize remaining sequences");
    assert_eq!(
        host.call("settle", json!({"id":second["id"]}))
            .expect("receipt retained")["order"],
        2
    );
    let orders: Vec<_> = host.snapshot()["effects"]
        .as_array()
        .expect("effects")
        .iter()
        .map(|effect| effect["order"].as_u64().expect("order"))
        .collect();
    assert_eq!(orders, [1, 1, 2, 2, 3, 3]);
    assert_eq!(host.finish()["clean"], true);
}
