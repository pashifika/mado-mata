use super::*;
use crate::host::test_support::make_host;
use crate::host::test_support::{observe, ready_host, submit, wait_until};
use crate::model::{Control, StopReason};
use std::sync::{Arc, Mutex, mpsc};

fn drag(path: Value) -> Value {
    json!({"kind":"drag","button":"left","path":path})
}

fn submit_actions(host: &Host, observation: &Value, actions: Value) -> Result<Value, Fault> {
    host.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    )
}

fn configured_host(max_actions: usize, wait_ms: u64, duration_ms: u64) -> Host {
    let mut host = make_host("success", "template-first");
    let inner = Arc::get_mut(&mut host.inner).expect("unshared host");
    inner.plan.limits.max_actions = max_actions;
    inner.plan.limits.wait_ms = wait_ms;
    inner.plan.limits.duration_ms = duration_ms;
    inner.control = Arc::new(Control::new(&inner.plan.limits));
    host.begin_readiness().expect("readiness");
    host.begin_workflow().expect("workflow");
    host
}

fn settle(host: &Host, sequence: &Value) -> Value {
    host.call("settle", json!({"id":sequence["id"]}))
        .expect("receipt")
}

// The event boundary is acknowledged without holding the input/state gates.
fn pause_drag(host: &Host, events: usize, waiting: bool) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    *lock(&host.inner.drag_hook) = Some(Arc::new(move |count, is_waiting| {
        if count == events && is_waiting == waiting {
            arrived_tx.send(()).expect("boundary observer");
            lock(&resume_rx)
                .recv_timeout(Duration::from_secs(2))
                .expect("resume boundary");
        }
    }));
    (arrived_rx, resume_tx)
}

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

#[test]
fn drag_nested_shape_errors_refuse_the_entire_batch_without_debit() {
    let host = configured_host(8, 5000, 10_000);
    let observation = observe(&host);
    let valid = drag(json!([{"x":0,"y":0},{"x":639.5,"y":479.75}]));
    let too_many = vec![json!({"x":1,"y":1}); 65];
    let invalid = [
        drag(json!([])),
        drag(json!([{"x":1,"y":1}])),
        drag(json!(too_many)),
        drag(json!(null)),
        drag(json!([{"x":0,"y":0},{"x":640,"y":0}])),
        drag(json!([{"x":0,"y":0},{"x":0,"y":480}])),
        drag(json!([{"x":0,"y":0},{"x":-0.25,"y":0}])),
        drag(json!([{"x":0,"y":0},{"x":null,"y":0}])),
        drag(json!([{"x":0,"y":0},{"x":1}])),
        drag(json!([{"x":0,"y":0},{"x":1,"y":1,"extra":1}])),
        drag(json!([{"x":0,"y":0,"hold_ms":-1},{"x":1,"y":1}])),
        drag(json!([{"x":0,"y":0,"hold_ms":0.5},{"x":1,"y":1}])),
        drag(json!([{"x":0,"y":0,"hold_ms":"1"},{"x":1,"y":1}])),
        drag(json!([{"x":0,"y":0,"hold_ms":1001},{"x":1,"y":1}])),
        drag(json!([{"x":0,"y":0,"hold_ms":null},{"x":1,"y":1}])),
        drag(json!(vec![json!({"x":0,"y":0,"hold_ms":1000}); 6])),
        json!({"kind":"drag","button":"other","path":[{"x":0,"y":0},{"x":1,"y":1}]}),
        json!({"kind":"drag","path":[{"x":0,"y":0},{"x":1,"y":1}]}),
        json!({"kind":"drag","button":"left","path":[{"x":0,"y":0},{"x":1,"y":1}],"x":0}),
    ];
    for action in invalid {
        let error = submit_actions(&host, &observation, json!([valid, action]))
            .expect_err("later invalid action must reject every earlier event");
        assert_eq!(error.category, "Argument", "{action}");
        assert!(lock(&host.inner.state).queue.is_empty());
        assert_eq!(lock(&host.inner.state).admitted_actions, 0);
    }
    assert_eq!(host.snapshot()["effects"], json!([]));
    let sequence = submit_actions(&host, &observation, json!([valid, valid]))
        .expect("refusals left all eight events available");
    assert_eq!(sequence["order"], 1);
    assert_eq!(settle(&host, &sequence)["submitted_events"], 8);
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn drag_validation_preserves_fractional_edges_and_bounded_maximums() {
    let mut action = Action::Drag {
        button: PointerButton::Middle,
        path: vec![
            DragPoint {
                x: 639.75,
                y: 479.5,
                hold_ms: 0
            };
            64
        ],
    };
    if let Action::Drag { path, .. } = &mut action {
        for point in path.iter_mut().take(5) {
            point.hold_ms = 1000;
        }
    }
    action
        .validate(Some((640.0, 480.0)))
        .expect("inclusive cardinality and hold limits");
    assert_eq!(action.event_count(), 71);
    assert_eq!(action.scheduled_wait_ms(), 5000);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let action = Action::Drag {
            button: PointerButton::Left,
            path: vec![
                DragPoint {
                    x: bad,
                    y: 0.0,
                    hold_ms: 0
                };
                2
            ],
        };
        assert_eq!(
            action
                .validate(None)
                .expect_err("representative validates finite shape")
                .category,
            "Argument"
        );
    }
    let host = configured_host(71, 5000, 10_000);
    let observation = observe(&host);
    let accepted = submit_actions(&host, &observation, json!([action])).expect("maximum admitted");
    host.call("release", json!({"id":accepted["id"]}))
        .expect("cancel without five-second dwell");
    assert_eq!(host.snapshot()["receipts"][0]["total_events"], 71);
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn mixed_drag_exact_budget_and_configured_wait_refusals_do_not_debit() {
    let host = configured_host(10, 2, 5000);
    let observation = observe(&host);
    let valid = drag(json!([{"x":0,"y":0,"hold_ms":1},{"x":4,"y":5}]));
    let over_wait = drag(json!([{"x":0,"y":0,"hold_ms":3},{"x":4,"y":5}]));
    assert_eq!(
        submit_actions(&host, &observation, json!([over_wait]))
            .expect_err("configured wait")
            .category,
        "Argument"
    );
    let actions = json!([
        {"kind":"key_down","key":"A"}, valid,
        {"kind":"click","button":"right","x":5,"y":5},
        {"kind":"key_up","key":"A"}
    ]);
    let mut too_large = actions.as_array().expect("actions").clone();
    too_large.push(json!({"kind":"key_down","key":"B"}));
    assert_eq!(
        submit_actions(&host, &observation, json!(too_large))
            .expect_err("event budget")
            .category,
        "ActionLimit"
    );
    let sequence = submit_actions(&host, &observation, actions).expect("exactly ten events");
    assert_eq!(sequence["order"], 1);
    let receipt = settle(&host, &sequence);
    assert_eq!(receipt["submitted"], 4);
    assert_eq!(receipt["submitted_events"], 10);
    assert_eq!(receipt["last_submitted_event"], 9);
    assert_eq!(receipt["total_events"], 10);
    assert_eq!(host.snapshot()["effects"][0]["kind"], "key_down");
    assert_eq!(host.snapshot()["effects"][6]["kind"], "click");
    assert_eq!(host.snapshot()["effects"][7]["kind"], "key_up");
    assert_eq!(
        submit_actions(&host, &observation, json!([valid]))
            .expect_err("remaining attempt exhausted")
            .category,
        "ActionLimit"
    );
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn drag_batch_ceiling_does_not_tighten_key_only_batches() {
    let host = configured_host(600, 500, 5000);
    let observation = observe(&host);
    let gesture = drag(json!([{"x":0,"y":0},{"x":0,"y":0}]));
    assert_eq!(
        submit_actions(&host, &observation, json!(vec![gesture.clone(); 65]))
            .expect_err("260 expanded events")
            .category,
        "ActionLimit"
    );
    let sequence =
        submit_actions(&host, &observation, json!(vec![gesture; 64])).expect("exactly 256 events");
    assert_eq!(settle(&host, &sequence)["submitted"], 64);
    let keys = submit_actions(
        &host,
        &observation,
        json!(vec![json!({"kind":"key_up","key":"A"}); 257]),
    )
    .expect("key-only ceiling unchanged");
    assert_eq!(settle(&host, &keys)["submitted"], 257);
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn drag_return_path_and_holds_preserve_every_event_in_order() {
    let host = ready_host("success");
    let observation = observe(&host);
    let sequence = submit_actions(
        &host,
        &observation,
        json!([drag(json!([
            {"x":1.5,"y":2.25,"hold_ms":1},
            {"x":8,"y":9,"hold_ms":1},
            {"x":8,"y":9,"hold_ms":0},
            {"x":1.5,"y":2.25,"hold_ms":1}
        ]))]),
    )
    .expect("return gesture");
    let receipt = settle(&host, &sequence);
    assert_eq!(receipt["submitted"], 1);
    assert_eq!(receipt["submitted_events"], 9);
    assert_eq!(
        host.snapshot()["effects"],
        json!([
            {"order":1,"kind":"pointer_move","x":1.5,"y":2.25,"held_buttons":[]},
            {"order":1,"kind":"button_down","button":"left"},
            {"order":1,"kind":"delay","ms":1},
            {"order":1,"kind":"pointer_move","x":8.0,"y":9.0,"held_buttons":["left"]},
            {"order":1,"kind":"delay","ms":1},
            {"order":1,"kind":"pointer_move","x":8.0,"y":9.0,"held_buttons":["left"]},
            {"order":1,"kind":"pointer_move","x":1.5,"y":2.25,"held_buttons":["left"]},
            {"order":1,"kind":"delay","ms":1},
            {"order":1,"kind":"button_up","button":"left"}
        ])
    );
    assert_eq!(host.snapshot()["held_buttons"], json!([]));
    assert_eq!(
        host.snapshot()["pointer_position"],
        json!({"x":1.5,"y":2.25})
    );
    assert_eq!(host.snapshot()["release_outcomes"], json!([]));
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn drag_waits_must_fit_remaining_time_at_admission_and_dispatch() {
    let host = configured_host(32, 500, 100);
    let observation = observe(&host);
    let action = drag(json!([{"x":0,"y":0,"hold_ms":500},{"x":1,"y":1}]));
    assert_eq!(
        submit_actions(&host, &observation, json!([action]))
            .expect_err("scheduled time exceeds attempt duration")
            .category,
        "Timeout"
    );
    assert_eq!(lock(&host.inner.state).admitted_actions, 0);
    assert_eq!(host.snapshot()["accepted"], json!([]));
    assert_eq!(host.finish()["clean"], true);

    let host = configured_host(32, 1000, 1000);
    let observation = observe(&host);
    let action = drag(json!([{"x":0,"y":0,"hold_ms":800},{"x":1,"y":1}]));
    let accepted =
        submit_actions(&host, &observation, json!([action])).expect("time initially fits");
    // Observe the deadline itself, not guessed input timing.
    wait_until(|| {
        host.control()
            .deadline()
            .saturating_duration_since(Instant::now())
            < Duration::from_millis(800)
    });
    let receipt = settle(&host, &accepted);
    assert_eq!(receipt["status"], "Refused");
    assert_eq!(receipt["reason"], "Timeout");
    assert_eq!(receipt["submitted_events"], 0);
    assert_eq!(receipt["last_submitted_event"], Value::Null);
    assert_eq!(host.snapshot()["effects"], json!([]));
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn drag_stage_retention_and_queued_identity_refusals_have_no_events() {
    let host = make_host("success", "template-first");
    let action = drag(json!([{"x":0,"y":0},{"x":1,"y":1}]));
    assert_eq!(
        submit_actions(&host, &json!({}), json!([action]))
            .expect_err("module evaluation cannot submit")
            .category,
        "AdmissionClosed"
    );
    host.begin_readiness().expect("readiness");
    let observation = observe(&host);
    assert_eq!(
        submit_actions(&host, &observation, json!([action]))
            .expect_err("readiness cannot submit")
            .category,
        "AdmissionClosed"
    );
    host.begin_workflow().expect("workflow");
    host.call("release", json!({"id":observation["id"]}))
        .expect("release observation");
    assert_eq!(
        submit_actions(&host, &observation, json!([action]))
            .expect_err("released observation cannot submit")
            .category,
        "InvalidHandle"
    );
    assert_eq!(host.snapshot()["accepted"], json!([]));
    assert_eq!(host.finish()["clean"], true);

    for (event, category) in [
        ("geometry", "StaleIdentity"),
        ("session", "StaleIdentity"),
        ("focus_lost", "FocusRefused"),
        ("route_revoked", "RouteRefused"),
        ("target_exit", "TargetLost"),
        ("pid_reuse", "TargetLost"),
        ("release_observation", "InvalidHandle"),
    ] {
        let host = ready_host("success");
        let observation = observe(&host);
        let accepted = submit_actions(&host, &observation, json!([action])).expect("queued drag");
        if event == "release_observation" {
            host.call("release", json!({"id":observation["id"]}))
                .expect("release source");
        } else {
            host.call("fixture", json!({"event":event}))
                .expect("invalidate target");
        }
        let receipt = settle(&host, &accepted);
        assert_eq!(receipt["status"], "Refused", "{event}");
        assert_eq!(receipt["reason"], category, "{event}");
        assert_eq!(receipt["submitted_events"], 0);
        assert_eq!(receipt["total_events"], 4);
        assert_eq!(host.snapshot()["effects"], json!([]));
        assert_eq!(host.finish()["clean"], true);
    }
}

#[test]
fn stop_at_drag_boundaries_preserves_prefix_and_cleanup_never_completes_it() {
    for (events, waiting, position) in [
        (2, false, json!({"x":1.0,"y":2.0})),
        (2, true, json!({"x":1.0,"y":2.0})),
        (4, false, json!({"x":10.0,"y":20.0})),
        (7, false, json!({"x":1.0,"y":2.0})),
    ] {
        let host = ready_host("success");
        let observation = observe(&host);
        let sequence = submit_actions(
            &host,
            &observation,
            json!([drag(json!([
                {"x":1,"y":2,"hold_ms":1},
                {"x":10,"y":20,"hold_ms":1},
                {"x":1,"y":2,"hold_ms":1}
            ]))]),
        )
        .expect("gesture");
        let (arrived, resume) = pause_drag(&host, events, waiting);
        let worker = host.clone();
        let dispatch = thread::spawn(move || settle(&worker, &sequence));
        arrived
            .recv_timeout(Duration::from_secs(1))
            .expect("event boundary reached");
        let before = host.snapshot();
        assert_eq!(before["held_buttons"], json!(["left"]));
        assert_eq!(before["pointer_waiting"], waiting);
        assert_eq!(before["pointer_position"], position);
        assert_eq!(before["attempt_owners"], 3);
        host.control().cancel();
        assert!(!host.control().admission.load(Ordering::Acquire));
        resume.send(()).expect("resume after acknowledged Stop");
        let receipt = dispatch.join().expect("settlement");
        assert_eq!(receipt["status"], "Partial");
        assert_eq!(receipt["reason"], "Cancelled");
        assert_eq!(receipt["submitted"], 0);
        assert_eq!(receipt["submitted_events"], events);
        assert_eq!(receipt["last_submitted_event"], events - 1);
        assert_eq!(receipt["total_events"], 8);
        assert_eq!(receipt["cleanup"]["released"], 1);
        assert_eq!(receipt["cleanup"]["owed"], 0);
        assert_eq!(host.snapshot()["effects"], before["effects"]);
        assert_eq!(host.snapshot()["pointer_position"], position);
        assert_eq!(host.snapshot()["held_buttons"], json!([]));
        assert_eq!(
            host.snapshot()["release_outcomes"],
            json!([
                {"button":"left","order":1,"released":true,"sink":"controlled-non-native"}
            ])
        );
        assert_eq!(host.finish()["remaining"]["held_buttons"], 0);
    }
}

#[test]
fn interrupted_drag_releases_only_its_button_not_an_earlier_sequences_key() {
    let host = ready_host("success");
    let observation = observe(&host);
    let key = submit_actions(&host, &observation, json!([{"kind":"key_down","key":"A"}]))
        .expect("earlier sequence owns key");
    settle(&host, &key);
    let sequence = submit_actions(
        &host,
        &observation,
        json!([drag(json!([{"x":0,"y":0},{"x":1,"y":1}]))]),
    )
    .expect("drag");
    let control = host.control();
    *lock(&host.inner.drag_hook) = Some(Arc::new(move |events, _| {
        if events == 2 {
            control.cancel();
        }
    }));
    let receipt = settle(&host, &sequence);
    assert_eq!(receipt["submitted"], 0);
    assert_eq!(receipt["cleanup"]["released"], 1);
    assert_eq!(host.snapshot()["held_keys"], json!(["A"]));
    assert_eq!(
        host.snapshot()["release_outcomes"],
        json!([
            {"button":"left","order":2,"released":true,"sink":"controlled-non-native"}
        ])
    );
    let cleanup = host.finish();
    assert_eq!(cleanup["clean"], true);
    assert_eq!(
        cleanup["release_outcomes"][1],
        json!({
            "key":"A","order":1,"released":true,"sink":"controlled-non-native"
        })
    );
}

#[test]
fn drag_faults_stop_before_later_moves_and_preserve_exact_partial_progress() {
    for category in [
        "Timeout",
        "Script",
        "Host",
        "ControlLost",
        "StaleIdentity",
        "TargetLost",
        "FocusRefused",
        "RouteRefused",
    ] {
        let host = ready_host("success");
        let observation = observe(&host);
        let sequence = submit_actions(
            &host,
            &observation,
            json!([drag(json!([
                {"x":0,"y":0},{"x":10,"y":10},{"x":20,"y":20}
            ]))]),
        )
        .expect("gesture");
        let weak = Arc::downgrade(&host.inner);
        *lock(&host.inner.drag_hook) = Some(Arc::new(move |events, _| {
            if events != 3 {
                return;
            }
            let host = Host {
                inner: weak.upgrade().expect("host retained"),
            };
            match category {
                "Timeout" => host.control().stop(StopReason::Timeout),
                "StaleIdentity" => {
                    host.call("fixture", json!({"event":"geometry"})).unwrap();
                }
                "TargetLost" => {
                    host.call("fixture", json!({"event":"pid_reuse"})).unwrap();
                }
                "FocusRefused" => {
                    host.call("fixture", json!({"event":"focus_lost"})).unwrap();
                }
                "RouteRefused" => {
                    host.call("fixture", json!({"event":"route_revoked"}))
                        .unwrap();
                }
                _ => host.fail(Fault::new(category, "injected owner failure")),
            }
        }));
        let receipt = settle(&host, &sequence);
        assert_eq!(receipt["status"], "Partial", "{category}");
        assert_eq!(receipt["reason"], category);
        assert_eq!(receipt["submitted"], 0);
        assert_eq!(receipt["submitted_events"], 3);
        assert_eq!(receipt["cleanup"]["released"], 1);
        assert_eq!(host.snapshot()["effects"].as_array().unwrap().len(), 3);
        assert_eq!(
            host.snapshot()["pointer_position"],
            json!({"x":10.0,"y":10.0})
        );
        assert_eq!(host.finish()["clean"], true);
    }
}

// Mid-drag fixture injection through the event hook, outside the input gates.
fn inject_at(host: &Host, events: usize, event: &'static str) {
    let weak = Arc::downgrade(&host.inner);
    *lock(&host.inner.drag_hook) = Some(Arc::new(move |count, _| {
        if count == events {
            Host {
                inner: weak.upgrade().expect("host retained"),
            }
            .call("fixture", json!({"event":event}))
            .unwrap();
        }
    }));
}

fn later_input_is_refused(host: &Host, observation: &Value) {
    let gesture = drag(json!([{"x":0,"y":0},{"x":1,"y":1}]));
    for actions in [
        json!([{"kind":"key_down","key":"A"},{"kind":"key_up","key":"A"}]),
        json!([{"kind":"click","x":1,"y":1,"button":"left"}]),
        json!([gesture]),
    ] {
        assert_eq!(
            submit_actions(host, observation, actions)
                .expect_err("no input of any kind after an interrupted drag")
                .category,
            "AdmissionClosed"
        );
    }
}

#[test]
fn interrupted_drag_keeps_its_receipt_readable_and_closes_every_later_input() {
    let gesture = drag(json!([{"x":0,"y":0},{"x":1,"y":1}]));
    let click = json!({"kind":"click","x":1,"y":1,"button":"left"});
    for (scenario, status) in [("partial", "Partial"), ("uncertain", "Uncertain")] {
        let host = ready_host(scenario);
        let observation = observe(&host);
        let first = submit_actions(&host, &observation, json!([gesture])).expect("gesture");
        let queued = submit_actions(&host, &observation, json!([click]))
            .expect("queued before the interruption");
        let receipt = settle(&host, &first);
        assert_eq!(receipt["status"], status, "{scenario}");
        assert_eq!(receipt["submitted"], 0);
        assert_eq!(receipt["submitted_events"], 1);
        assert_eq!(receipt["total_events"], 4);
        assert_eq!(host.snapshot()["failure"], Value::Null, "{scenario}");
        // The Script reports and releases under ordinary admission; nothing is replayed.
        host.call("log", json!({"message":"reported"}))
            .expect("ordinary work continues after a receipt outcome");
        host.call("release", json!({"id":first["id"]}))
            .expect("receipt release");
        let refused = settle(&host, &queued);
        assert_eq!(refused["status"], "Refused", "{scenario}");
        assert_eq!(refused["reason"], "AdmissionClosed");
        assert_eq!(refused["submitted"], 0);
        later_input_is_refused(&host, &observation);
        assert_eq!(host.snapshot()["effects"].as_array().unwrap().len(), 1);
        assert_eq!(host.finish()["clean"], true);
    }
    // A real interruption after the press, with its cleanup released, closes input
    // even against a fresh observation while the attempt itself continues.
    let host = ready_host("success");
    let observation = observe(&host);
    let sequence = submit_actions(&host, &observation, json!([gesture])).expect("gesture");
    inject_at(&host, 2, "geometry");
    let receipt = settle(&host, &sequence);
    assert_eq!(receipt["status"], "Partial");
    assert_eq!(receipt["reason"], "StaleIdentity");
    assert_eq!(receipt["cleanup"]["released"], 1);
    assert_eq!(host.snapshot()["failure"], Value::Null);
    let fresh = observe(&host);
    later_input_is_refused(&host, &fresh);
    assert_eq!(host.snapshot()["held_buttons"], json!([]));
    assert_eq!(host.finish()["clean"], true);
    // Target loss keeps latching the attempt as before.
    let host = ready_host("success");
    let observation = observe(&host);
    let sequence = submit_actions(&host, &observation, json!([gesture])).expect("gesture");
    inject_at(&host, 2, "pid_reuse");
    let receipt = settle(&host, &sequence);
    assert_eq!(receipt["reason"], "TargetLost");
    assert_eq!(receipt["cleanup"]["released"], 1);
    assert_eq!(host.snapshot()["failure"]["category"], "TargetLost");
    assert_eq!(
        host.call("log", json!({"message":"late"}))
            .expect_err("target loss ends the attempt")
            .category,
        "TargetLost"
    );
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn input_closed_during_submit_precheck_refuses_enqueue_without_debit() {
    let host = ready_host("partial");
    let observation = observe(&host);
    let first = submit_actions(
        &host,
        &observation,
        json!([drag(json!([{"x":0,"y":0},{"x":1,"y":1}]))]),
    )
    .expect("gesture");
    let accepted = host.snapshot()["accepted"].clone();
    let admitted = lock(&host.inner.state).admitted_actions;
    let settling = host.clone();
    *lock(&host.inner.before_input_enqueue) = Some(Box::new(move || {
        assert_eq!(settle(&settling, &first)["status"], "Partial");
    }));
    assert_eq!(
        submit_actions(
            &host,
            &observation,
            json!([{"kind":"click","x":1,"y":1,"button":"left"}]),
        )
        .expect_err("the earlier precheck cannot admit input after closure")
        .category,
        "AdmissionClosed"
    );
    assert_eq!(host.snapshot()["accepted"], accepted);
    assert_eq!(host.snapshot()["queue_depth"], 0);
    assert_eq!(lock(&host.inner.state).admitted_actions, admitted);
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn unresolved_button_cleanup_blocks_queued_conflicts_and_clean_reuse_without_retry() {
    let host = ready_host("success");
    let observation = observe(&host);
    let action = drag(json!([{"x":0,"y":0},{"x":1,"y":1}]));
    let first = submit_actions(&host, &observation, json!([action])).expect("first");
    let second = submit_actions(&host, &observation, json!([action])).expect("queued conflict");
    host.inner
        .refuse_button_cleanup
        .store(true, Ordering::Release);
    let control = host.control();
    *lock(&host.inner.drag_hook) = Some(Arc::new(move |events, _| {
        if events == 2 {
            control.cancel();
        }
    }));
    let receipt = settle(&host, &first);
    assert_eq!(receipt["submitted"], 0);
    assert_eq!(receipt["cleanup"]["released"], 0);
    assert_eq!(receipt["cleanup"]["owed"], 1);
    assert_eq!(receipt["cleanup"]["may_leave_state_held"], true);
    assert_eq!(
        receipt["cleanup_required"],
        json!(["controlled_button_release_unconfirmed"])
    );
    assert_eq!(host.snapshot()["held_buttons"], json!(["left"]));
    assert_eq!(settle(&host, &second)["submitted_events"], 0);
    assert_eq!(host.snapshot()["effects"].as_array().unwrap().len(), 2);
    host.inner
        .refuse_button_cleanup
        .store(false, Ordering::Release);
    let cleanup = host.finish();
    assert_eq!(cleanup["clean"], false);
    assert_eq!(cleanup["remaining"]["held_buttons"], 1);
    assert_eq!(cleanup["release_outcomes"].as_array().unwrap().len(), 1);
    assert!(host.terminal_accounting().is_err());
    assert!(host.schedule_fresh().is_err());
    assert_eq!(
        host.finish()["release_outcomes"],
        cleanup["release_outcomes"]
    );
}

#[test]
fn finish_during_drag_retains_active_ownership_until_event_loop_settles() {
    let host = ready_host("success");
    let observation = observe(&host);
    let sequence = submit_actions(
        &host,
        &observation,
        json!([drag(json!([{"x":0,"y":0,"hold_ms":1},{"x":1,"y":1}]))]),
    )
    .expect("drag");
    let (arrived, resume) = pause_drag(&host, 2, true);
    let worker = host.clone();
    let dispatch = thread::spawn(move || settle(&worker, &sequence));
    arrived
        .recv_timeout(Duration::from_secs(1))
        .expect("held dwell");
    let cleanup = host.finish();
    assert_eq!(cleanup["clean"], false);
    assert_eq!(cleanup["remaining"]["active_sequences"], 1);
    assert_eq!(cleanup["remaining"]["held_buttons"], 1);
    assert_eq!(cleanup["release_outcomes"], json!([]));
    resume.send(()).expect("let sequence owner clean up");
    let receipt = dispatch.join().expect("settlement");
    assert_eq!(receipt["submitted"], 0);
    assert_eq!(receipt["submitted_events"], 2);
    assert_eq!(receipt["cleanup"]["released"], 1);
    assert_eq!(host.snapshot()["effects"].as_array().unwrap().len(), 2);
    assert_eq!(host.finish()["clean"], true);
    assert_eq!(host.snapshot()["attempt_owners"], 0);
}

#[test]
fn mixed_receipt_counts_only_ordinary_completed_actions_before_interrupted_drag() {
    let host = ready_host("success");
    let observation = observe(&host);
    let action = drag(json!([{"x":0,"y":0},{"x":1,"y":1}]));
    let sequence = submit_actions(
        &host,
        &observation,
        json!([
            {"kind":"key_down","key":"A"}, {"kind":"key_up","key":"A"},
            {"kind":"click","button":"right","x":1,"y":1}, action, action
        ]),
    )
    .expect("mixed sequence");
    let control = host.control();
    *lock(&host.inner.drag_hook) = Some(Arc::new(move |events, _| {
        if events == 11 {
            control.cancel();
        }
    }));
    let receipt = settle(&host, &sequence);
    assert_eq!(receipt["status"], "Partial");
    assert_eq!(receipt["submitted"], 4);
    assert_eq!(receipt["total"], 5);
    assert_eq!(receipt["submitted_events"], 11);
    assert_eq!(receipt["total_events"], 13);
    assert_eq!(receipt["last_submitted_event"], 10);
    assert_eq!(receipt["cleanup"]["released"], 1);
    let effects = host.snapshot()["effects"].as_array().unwrap().clone();
    assert_eq!(
        effects
            .iter()
            .filter(|effect| effect["kind"] == "button_up")
            .count(),
        1
    );
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn scheduled_wait_authority_applies_to_the_whole_drag_batch() {
    let host = configured_host(32, 2, 5000);
    let observation = observe(&host);
    let action = drag(json!([{"x":0,"y":0,"hold_ms":2},{"x":1,"y":1}]));
    assert_eq!(
        submit_actions(&host, &observation, json!([action, action]))
            .expect_err("individually legal waits exceed the batch allowance")
            .category,
        "Argument"
    );
    assert_eq!(lock(&host.inner.state).admitted_actions, 0);
    let accepted =
        submit_actions(&host, &observation, json!([action])).expect("exact wait allowance");
    assert_eq!(settle(&host, &accepted)["submitted_events"], 5);
    assert_eq!(host.finish()["clean"], true);
}

#[test]
fn owner_stop_during_drag_closes_attempt_input_without_waiting_for_dwell() {
    let mut host = make_host("success", "template-first");
    let inner = Arc::get_mut(&mut host.inner).expect("unshared host");
    let owner = Arc::new(Control::new(&inner.plan.limits));
    inner.control = Arc::new(Control::for_attempt(
        Arc::clone(&owner),
        &inner.plan.limits,
        true,
    ));
    host.begin_readiness().expect("readiness");
    host.begin_workflow().expect("workflow");
    let observation = observe(&host);
    let sequence = submit_actions(
        &host,
        &observation,
        json!([drag(json!([
            {"x":0,"y":0,"hold_ms":500},{"x":1,"y":1}
        ]))]),
    )
    .expect("drag");
    let (arrived, resume) = pause_drag(&host, 2, true);
    let worker = host.clone();
    let dispatch = thread::spawn(move || settle(&worker, &sequence));
    arrived
        .recv_timeout(Duration::from_secs(1))
        .expect("held dwell");
    owner.cancel();
    resume.send(()).expect("resume under owner Stop");
    let receipt = dispatch.join().expect("settlement");
    assert_eq!(receipt["reason"], "Cancelled");
    assert_eq!(receipt["submitted"], 0);
    assert_eq!(receipt["submitted_events"], 2);
    assert_eq!(host.snapshot()["effects"].as_array().unwrap().len(), 2);
    assert_eq!(host.finish()["clean"], true);
}
