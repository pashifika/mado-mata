//! Continuous Workflow authority over the controlled public-facade session:
//! no OS capture, permission, model or input.

use super::native_frame_tests::{Capture, click, fixture_on, publish};
use super::*;
use crate::model::StopReason;
use mado_pilot_testkit::ControlledInput;
use mado_pilot_testkit::controlled_input::{Behavior, Cleanup};

fn continuous(overall_ms: Option<u64>) -> (Engine, Arc<Capture>, Arc<ControlledInput>) {
    continuous_with(overall_ms, Behavior::Complete)
}

// The controlled controller keeps the behavior it was opened with.
fn continuous_with(
    overall_ms: Option<u64>,
    behavior: Behavior,
) -> (Engine, Arc<Capture>, Arc<ControlledInput>) {
    let control = Arc::new(Control::reserved(overall_ms).unwrap());
    let (mut engine, capture, input) = fixture_on(
        control,
        mp::InputDelivery::System,
        behavior,
        Cleanup::Complete,
    );
    let limits = crate::desktop::continuous_limits();
    let native = engine.native.as_mut().unwrap();
    native.capture.duration_ms = None;
    native.capture.max_frames = None;
    native.input.duration_ms = None;
    native.input.max_actions = None;
    native.input.max_batch_events = Some(limits.max_batch_events);
    native.input.event_interval_ms = Some(limits.event_interval_ms);
    (engine, capture, input)
}

fn scope(engine: &Engine, id: u64, deadline: Option<Instant>) -> WorkflowScope {
    WorkflowScope::new(id, 0, Arc::clone(&engine.control), deadline)
}

fn drag(observation: &Value, hold_ms: u64) -> Value {
    json!({"observation":observation,"actions":[{"kind":"drag","button":"left","path":[
        {"x":1,"y":2,"hold_ms":hold_ms},{"x":3,"y":4}]}]})
}

fn wait_in_flight(engine: &Engine) {
    let guard = Instant::now() + Duration::from_secs(5);
    while engine.snapshot()["in_flight"] != 1 {
        assert!(Instant::now() < guard, "the wait never reached the facade");
        thread::yield_now();
    }
}

#[test]
fn one_session_passes_the_former_duration_frame_and_event_totals() {
    let (engine, capture, input) = continuous(None);
    let finite = crate::desktop::native_limits();
    // Already past the longest finite Run, and so past the plan's preparation-only duration.
    let ceiling = crate::model::NativeBudgets::CEILINGS
        .preparation_ms()
        .unwrap();
    let origin = engine.control.started() + Duration::from_millis(ceiling + 1);
    let iterations = finite.max_frames + 1;
    let mut session = None;
    for iteration in 0..iterations {
        // 4 expanded events per 100 ms stay inside the published pace.
        engine
            .control
            .set_test_time(origin + Duration::from_millis(100 * iteration));
        if iteration != 0 {
            publish(&capture, mp::Continuity::Continuous);
        }
        let observation = engine.call("observe", json!({})).unwrap();
        let first = session.get_or_insert_with(|| observation["session"].clone());
        assert_eq!(*first, observation["session"]);
        let receipt = engine.call("dispatch", click(&observation)).unwrap();
        assert_eq!(receipt["status"], "Submitted", "{iteration}");
        engine
            .call("release", json!({"id":observation["id"]}))
            .unwrap();
        assert_eq!(engine.snapshot()["script_handles"], 0);
    }
    let events = 4 * usize::try_from(iterations).unwrap();
    assert!(events > finite.max_actions);
    assert_eq!(input.admitted().len(), events / 4);
    assert_eq!(input.submitted_events().len(), events);
    engine.control.check().unwrap();
    assert_eq!(engine.finish()["clean"], true);
    assert_eq!(
        engine.terminal_accounting().unwrap(),
        json!({"frames":iterations,"expanded_input_events":events,"input_uncertain":false})
    );
}

#[test]
fn the_event_window_is_shared_by_scopes_and_refuses_whole_batches() {
    let (mut engine, _, input) = continuous(None);
    // Two held clicks fill the window: 8 expanded events per interval.
    engine.native.as_mut().unwrap().input.max_batch_events = Some(8);
    let origin = engine.control.started() + Duration::from_secs(1);
    engine.control.set_test_time(origin);
    let (first, sibling) = (scope(&engine, 1, None), scope(&engine, 2, None));
    let observation = engine
        .call_scoped("observe", json!({}), Some(&first))
        .unwrap();
    let action = click(&observation)["actions"][0].clone();
    let batch =
        |clicks: usize| json!({"observation":observation,"actions":vec![action.clone(); clicks]});
    // An oversized batch is refused whole, even into an empty window.
    let fault = engine
        .call_scoped("dispatch", batch(3), Some(&first))
        .unwrap_err();
    assert_eq!(fault.category, "ActionLimit");
    assert!(input.admitted().is_empty());
    let receipt = engine
        .call_scoped("dispatch", batch(2), Some(&first))
        .unwrap();
    assert_eq!(receipt["status"], "Submitted");
    for (elapsed_ms, retry_after_ms) in [(0, 1_000), (999, 1)] {
        engine
            .control
            .set_test_time(origin + Duration::from_millis(elapsed_ms));
        let fault = engine
            .call_scoped("dispatch", batch(1), Some(&sibling))
            .unwrap_err();
        assert_eq!(fault.category, "InputRate");
        assert_eq!(fault.context["stage"], "input_rate");
        assert_eq!(fault.context["retry_after_ms"], retry_after_ms);
    }
    assert_eq!(input.admitted().len(), 1);
    assert_eq!(engine.lock().unwrap().input_events, 8);
    assert!(!engine.lock().unwrap().input_uncertain);
    // The interval, not a scope change, restores room.
    engine
        .control
        .set_test_time(origin + Duration::from_millis(1_000));
    let receipt = engine
        .call_scoped("dispatch", batch(2), Some(&sibling))
        .unwrap();
    assert_eq!(receipt["status"], "Submitted");
    assert_eq!(input.admitted().len(), 2);
    assert_eq!(engine.lock().unwrap().input_events, 16);
}

#[test]
fn a_partial_batch_keeps_its_whole_charge() {
    let (mut engine, _, input) = continuous_with(
        None,
        Behavior::FailAfter {
            submitted: 2,
            fault: mp::InputFault::SubmissionFailed,
        },
    );
    engine.native.as_mut().unwrap().input.max_batch_events = Some(8);
    engine
        .control
        .set_test_time(engine.control.started() + Duration::from_secs(1));
    let observation = engine.call("observe", json!({})).unwrap();
    let action = click(&observation)["actions"][0].clone();
    let partial = engine
        .call(
            "dispatch",
            json!({"observation":observation,"actions":[action.clone(), action]}),
        )
        .unwrap();
    assert_eq!(partial["status"], "Partial");
    assert_eq!(partial["submitted_events"], 2);
    assert_eq!(partial["total_events"], 8);
    assert_eq!(partial["cleanup"]["may_leave_state_held"], false);
    // Two submitted events still cost the whole admitted batch.
    let fault = engine.call("dispatch", click(&observation)).unwrap_err();
    assert_eq!(fault.category, "InputRate");
    assert_eq!(input.admitted().len(), 1);
    let state = engine.lock().unwrap();
    assert_eq!(state.input_events, 8);
    assert!(state.input_uncertain);
}

#[test]
fn an_expired_scope_admits_nothing_while_a_sibling_acts_on_fresh_evidence() {
    let (engine, capture, input) = continuous(None);
    let origin = engine.control.started() + Duration::from_secs(1);
    engine.control.set_test_time(origin);
    let expiry = origin + Duration::from_millis(50);
    let search = scope(&engine, 1, Some(expiry));
    let observation = engine
        .call_scoped("observe", json!({}), Some(&search))
        .unwrap();
    // A hold that cannot finish inside the scope is refused before submission.
    let fault = engine
        .call_scoped("dispatch", drag(&observation, 51), Some(&search))
        .unwrap_err();
    assert_eq!(fault.category, "ScopeBudget");
    engine.control.set_test_time(expiry);
    for (method, args) in [
        ("observe", json!({})),
        ("validate_input", click(&observation)),
        ("dispatch", click(&observation)),
    ] {
        let fault = engine.call_scoped(method, args, Some(&search)).unwrap_err();
        assert_eq!(fault.category, "ScopeDeadline", "{method}");
    }
    assert!(input.admitted().is_empty());
    {
        let state = engine.lock().unwrap();
        assert_eq!(state.observations.len(), 1);
        assert_eq!(state.input_events, 0);
        assert!(!state.input_uncertain);
    }
    engine.control.check().unwrap();
    // The caller releases the child's evidence and decides again under a live scope.
    engine
        .call("release", json!({"id":observation["id"]}))
        .unwrap();
    publish(&capture, mp::Continuity::Continuous);
    let sibling = scope(&engine, 2, None);
    let fresh = engine
        .call_scoped("observe", json!({}), Some(&sibling))
        .unwrap();
    assert_eq!(fresh["session"], observation["session"]);
    assert!(fresh["frame"].as_u64() > observation["frame"].as_u64());
    let receipt = engine
        .call_scoped("dispatch", click(&fresh), Some(&sibling))
        .unwrap();
    assert_eq!(receipt["status"], "Submitted");
    assert_eq!(input.admitted().len(), 1);
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn a_blocked_capture_wait_ends_with_its_scope_not_with_the_run() {
    let (engine, _, _) = continuous(None);
    let consumed = engine.call("observe", json!({})).unwrap();
    let started = Instant::now();
    let search = scope(&engine, 1, Some(started + Duration::from_millis(50)));
    // No newer frame arrives: the scope, not wait_ms, wakes the wait.
    let fault = engine
        .call_scoped("observe", json!({}), Some(&search))
        .unwrap_err();
    assert_eq!(fault.category, "ScopeDeadline");
    assert!(started.elapsed() < Duration::from_millis(engine.limits.wait_ms));
    assert_eq!(engine.snapshot()["script_handles"], 1);
    assert_eq!(engine.snapshot()["in_flight"], 0);
    engine.control.check().unwrap();
    engine
        .call("release", json!({"id":consumed["id"]}))
        .unwrap();
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn stop_and_target_loss_during_a_scoped_wait_are_never_scope_outcomes() {
    // Stop wins while the scope is still live.
    let (engine, _, _) = continuous(None);
    engine.call("observe", json!({})).unwrap();
    let live = scope(&engine, 1, None);
    let fault = thread::scope(|threads| {
        let wait = threads.spawn(|| engine.call_scoped("observe", json!({}), Some(&live)));
        wait_in_flight(&engine);
        engine.control.cancel();
        wait.join().unwrap().unwrap_err()
    });
    assert_eq!(fault.category, "Cancelled");
    assert_eq!(engine.control.stop_reason(), Some(StopReason::Cancelled));

    // Target loss is kept even when the waiting scope has expired meanwhile.
    let (engine, capture, _) = continuous(None);
    engine.call("observe", json!({})).unwrap();
    let origin = Instant::now();
    engine.control.set_test_time(origin);
    let expiry = origin + Duration::from_secs(30);
    let expiring = scope(&engine, 1, Some(expiry));
    let fault = thread::scope(|threads| {
        let wait = threads.spawn(|| engine.call_scoped("observe", json!({}), Some(&expiring)));
        wait_in_flight(&engine);
        engine.control.set_test_time(expiry);
        capture.stream.terminate(mp::CaptureFault::TargetLost);
        wait.join().unwrap().unwrap_err()
    });
    assert_eq!(expiring.check().unwrap_err().category, "ScopeDeadline");
    assert_eq!(fault.category, "TargetLost");
    engine.control.check().unwrap();
}

#[test]
fn a_receipt_outlives_a_scope_that_expires_while_its_input_is_held() {
    let (engine, _, input) = continuous(None);
    let origin = engine.control.started() + Duration::from_secs(1);
    engine.control.set_test_time(origin);
    let expiry = origin + Duration::from_millis(500);
    let holding = scope(&engine, 1, Some(expiry));
    let observation = engine
        .call_scoped("observe", json!({}), Some(&holding))
        .unwrap();
    let held = drag(&observation, 100);
    let receipt = thread::scope(|threads| {
        let dispatch = threads.spawn(|| engine.call_scoped("dispatch", held, Some(&holding)));
        let guard = Instant::now() + Duration::from_secs(5);
        while input.submitted_events().len() < 2 {
            assert!(Instant::now() < guard, "the drag never reached its hold");
            thread::yield_now();
        }
        engine.control.set_test_time(expiry);
        dispatch.join().unwrap().unwrap()
    });
    assert_eq!(holding.check().unwrap_err().category, "ScopeDeadline");
    assert_eq!(receipt["status"], "Submitted");
    assert_eq!(receipt["submitted_events"], receipt["total_events"]);
    // Nothing is re-sent or newly admitted under the expired scope.
    let fault = engine
        .call_scoped("dispatch", click(&observation), Some(&holding))
        .unwrap_err();
    assert_eq!(fault.category, "ScopeDeadline");
    assert_eq!(input.admitted().len(), 1);
    assert!(!engine.lock().unwrap().input_uncertain);
}

#[test]
fn the_reviewed_overall_deadline_ends_the_run_not_a_scope() {
    let (engine, _, input) = continuous(Some(60_000));
    let live = scope(&engine, 1, None);
    let observation = engine
        .call_scoped("observe", json!({}), Some(&live))
        .unwrap();
    engine
        .control
        .set_test_time(engine.control.started() + Duration::from_millis(60_000));
    for (method, args) in [("observe", json!({})), ("dispatch", click(&observation))] {
        let fault = engine.call_scoped(method, args, Some(&live)).unwrap_err();
        assert_eq!(fault.category, "Timeout", "{method}");
    }
    assert!(input.admitted().is_empty());
    assert_eq!(engine.control.stop_reason(), Some(StopReason::Timeout));
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_native_configuration_carries_exactly_its_plan_lifecycle_and_published_pace() {
    let mut finite_plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    finite_plan.lane = "native".into();
    let published = crate::desktop::continuous_limits();
    let mut paced_plan = finite_plan.clone();
    paced_plan.native_budgets = Some(published.budgets());
    paced_plan.limits.max_actions = published.max_batch_events;
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let finite = json!({
        "executable_or_bundle":executable,"process_id":42,
        "process_lifetime":"0123456789abcdef","window_rule":"Selected window",
        "operating_system":std::env::consts::OS,"hardware":std::env::consts::ARCH,
        "permission_executable":executable,
        "capture":{"approved":true,"duration_ms":5000,"max_frames":10,
            "wait_ms":500,"interval_ms":50},
        "input":{"approved":true,"duration_ms":5000,"max_actions":10,
            "route":if cfg!(windows) { "window_message" } else { "process_directed" },
            "focus":"preserve","package_approved":true},
        "geometry":null,"recognition_language":crate::environment::LANGUAGE,
        "review":{"kind":"script"},"cleanup_ms":1000,"containment_ms":2000
    });
    let mut paced = finite.clone();
    paced["capture"] = json!({"approved":true,"wait_ms":500,"interval_ms":published.interval_ms});
    for key in ["duration_ms", "max_actions"] {
        paced["input"].as_object_mut().unwrap().remove(key);
    }
    paced["input"]["max_batch_events"] = json!(published.max_batch_events);
    paced["input"]["event_interval_ms"] = json!(published.event_interval_ms);
    paced["review"] = json!({"kind":"operation","operation":"Click once",
        "visible_postcondition":"The selected label changes"});
    let stage = |raw: &Value, plan: &Plan| {
        let config: NativeConfig = serde_json::from_value(raw.clone()).unwrap();
        validate_native(Some(&config), plan).map_err(|fault| fault.context["stage"].clone())
    };
    assert_eq!(stage(&finite, &finite_plan), Ok(()));
    assert_eq!(stage(&paced, &paced_plan), Ok(()));
    // Finite authority keeps its serialized identity; neither shape gains the other's keys.
    for raw in [&finite, &paced] {
        let config: NativeConfig = serde_json::from_value(raw.clone()).unwrap();
        let written = serde_json::to_value(&config).unwrap();
        assert_eq!(written["capture"], raw["capture"]);
        for key in [
            "duration_ms",
            "max_actions",
            "max_batch_events",
            "event_interval_ms",
        ] {
            assert_eq!(written["input"].get(key), raw["input"].get(key), "{key}");
        }
    }
    assert_eq!(stage(&paced, &finite_plan), Err(json!("capture_authority")));
    for (on_paced, section, key, value) in [
        (true, "capture", "duration_ms", json!(5000)),
        (true, "capture", "max_frames", json!(10)),
        (true, "capture", "interval_ms", json!(1)),
        (true, "input", "duration_ms", json!(5000)),
        (true, "input", "max_actions", json!(10)),
        (true, "input", "max_batch_events", json!(32)),
        (true, "input", "event_interval_ms", json!(999)),
        (true, "input", "package_approved", json!(false)),
        (false, "input", "max_batch_events", json!(64)),
        (false, "input", "event_interval_ms", json!(1000)),
    ] {
        let (base, plan) = if on_paced {
            (&paced, &paced_plan)
        } else {
            (&finite, &finite_plan)
        };
        let mut raw = base.clone();
        raw[section][key] = value;
        assert_eq!(
            stage(&raw, plan),
            Err(json!(format!("{section}_authority"))),
            "{section}/{key}"
        );
    }
    for key in ["max_batch_events", "event_interval_ms"] {
        let mut raw = paced.clone();
        raw["input"].as_object_mut().unwrap().remove(key);
        assert_eq!(
            stage(&raw, &paced_plan),
            Err(json!("input_authority")),
            "{key}"
        );
    }
    // A reviewed batch above the published ceiling is refused even when it matches.
    let mut oversized_plan = paced_plan.clone();
    oversized_plan.limits.max_actions = published.max_batch_events + 1;
    let mut oversized = paced.clone();
    oversized["input"]["max_batch_events"] = json!(published.max_batch_events + 1);
    assert_eq!(
        stage(&oversized, &oversized_plan),
        Err(json!("input_authority"))
    );
    let mut recovering = paced.clone();
    recovering["confirm_process_exit"] = json!(true);
    assert_eq!(
        stage(&recovering, &paced_plan),
        Err(json!("native_authority_validation"))
    );
    let mut scripted = paced.clone();
    scripted["review"] = json!({"kind":"script"});
    assert_eq!(stage(&scripted, &paced_plan), Err(json!("native_review")));
    // An explicit null is not an absent bound.
    for (section, key) in [("capture", "duration_ms"), ("input", "max_batch_events")] {
        let mut raw = paced.clone();
        raw[section][key] = Value::Null;
        assert!(serde_json::from_value::<NativeConfig>(raw).is_err());
    }
}
