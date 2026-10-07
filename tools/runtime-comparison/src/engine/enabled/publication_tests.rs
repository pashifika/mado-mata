use super::*;
use mado_pilot_runtime::{EngineWiring, Matcher, OcrRecognizer, PackageLoader};
use mado_pilot_testkit::{
    CompletionGate, ControlledCapture, ControlledMatcher, ControlledOcr, ScriptedOcrCall,
    ScriptedOcrCandidate,
};

fn plan() -> Plan {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.lane = "replay".into();
    plan.limits.wait_ms = 5_000;
    plan
}

fn candidates() -> Vec<ScriptedOcrCandidate> {
    vec![ScriptedOcrCandidate::new(
        Arc::<[u8]>::from(b"visible".as_slice()),
        [(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)],
        0.9,
        0,
    )]
}

fn replay(backend: Arc<ControlledOcr>) -> Engine {
    let plan = plan();
    Engine::replay_with_ocr_for_test(
        &plan,
        Arc::new(Control::new(&plan.limits)),
        "publication-test",
        1,
        Arc::new(HandleBudget::new(plan.limits.handles)),
        Some(backend),
    )
}

// Public deterministic capture supplies terminal ordering that finite replay
// cannot inject. It owns no native capture, permissions, models or input.
fn controlled(backend: Arc<ControlledOcr>) -> (Engine, Arc<ControlledCapture>) {
    let mut engine = replay(backend.clone());
    let issuer = Arc::new(mado_pilot_core::IdentityIssuer::new());
    let capture = Arc::new(
        ControlledCapture::new(
            issuer.clone(),
            mp::PixelExtent::new(8, 8),
            mp::PixelFormat::Rgba8,
        )
        .unwrap(),
    );
    let facade = mp::Engine::new(EngineWiring {
        engine: issuer.engine(),
        capture: capture.clone(),
        matcher: Matcher::new(Arc::new(ControlledMatcher::new(mp::PixelFormat::Rgba8))),
        loader: PackageLoader::new(),
        ocr: Some(OcrRecognizer::new(backend)),
        input: None,
        permission: None,
    })
    .unwrap();
    let operation = mp::OperationContext::new();
    let session = facade
        .open(capture.target(), &mp::OpenRequest::new(), &operation)
        .unwrap();
    {
        let mut state = engine.lock().unwrap();
        state.session.take().unwrap().close(&operation).unwrap();
        state.session = Some(session);
    }
    Arc::get_mut(&mut engine.resources).unwrap().engine = facade;
    capture.publish(0, mp::Continuity::Continuous).unwrap();
    (engine, capture)
}

#[test]
fn terminal_accounting_requires_settlement_and_retains_consumed_frames() {
    let (engine, capture) = controlled(Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8)));
    assert_eq!(
        engine.terminal_accounting().unwrap_err().category,
        "IncompleteCleanup"
    );
    let first = engine.call("observe", json!({})).unwrap();
    engine.call("release", json!({"id":first["id"]})).unwrap();
    capture.publish(1, mp::Continuity::Continuous).unwrap();
    let second = engine.call("observe", json!({})).unwrap();
    engine.call("release", json!({"id":second["id"]})).unwrap();
    assert_eq!(engine.finish()["clean"], true);
    assert_eq!(
        engine.terminal_accounting().unwrap(),
        json!({"frames":2,"expanded_input_events":0,"input_uncertain":false})
    );
}

fn recognition(observation: &Value) -> Value {
    json!({"observation":observation,"kind":"ocr","roi":{"x":0,"y":0,"width":8,"height":8}})
}

fn grouped(observation: &Value) -> Value {
    json!({"observation":observation,
        "basis":{"frame_width":8,"frame_height":8,"content":{"x":0,"y":0,"width":8,"height":8}},
        "zones":[{"id":"label","region":{"u0":0.0,"v0":0.0,"u1":1.0,"v1":1.0}}]})
}

#[test]
fn acquired_observation_losing_commit_is_not_published_and_releases_its_permit() {
    let (engine, capture) = controlled(Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8)));
    let operation = engine.operation(1_000).unwrap();
    let mut state = engine.lock().unwrap();
    let frame = state
        .session
        .as_ref()
        .unwrap()
        .acquire_frame(&mp::FrameRequest::latest(), &operation)
        .unwrap();
    capture.lose(capture.target());
    // A later successful close must not replace the original terminal cause.
    state
        .session
        .as_ref()
        .unwrap()
        .close(&mp::OperationContext::new())
        .unwrap();
    let permit = engine.handle_budget.reserve(1).unwrap();
    let fault = engine
        .retain_observation(&mut state, frame, permit, &operation)
        .unwrap_err();
    assert_eq!(fault.category, "TargetLost");
    assert_eq!(fault.context["stage"], "observation_publication");
    assert!(state.observations.is_empty());
    assert!(state.latest.is_none());
    drop(state);
    assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn terminal_before_ocr_grouped_query_and_postcondition_commit_discards_even_empty_results() {
    for method in ["recognize", "scan_ocr_zones", "query_wait", "postcondition"] {
        for present in [false, true] {
            let gate = Arc::new(CompletionGate::new());
            let backend = Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8).with_calls(vec![
                ScriptedOcrCall::new(if present { candidates() } else { vec![] })
                    .with_completion_gate(gate.clone()),
            ]));
            let (engine, capture) = controlled(backend);
            let observation = engine.call("observe", json!({})).unwrap();
            let args = match method {
                "recognize" => recognition(&observation),
                "scan_ocr_zones" => grouped(&observation),
                "query_wait" => {
                    let query = engine.call("query", recognition(&observation)).unwrap();
                    json!({"id":query["id"],"timeout_ms":5_000})
                }
                "postcondition" => {
                    capture.publish(1, mp::Continuity::Continuous).unwrap();
                    let newer = engine.call("observe", json!({})).unwrap();
                    json!({"observation":newer,"checkpoint":observation,"expected":"visible"})
                }
                _ => unreachable!(),
            };
            let result = thread::scope(|scope| {
                let worker = scope.spawn(|| engine.call(method, args));
                let _release = gate.release_guard();
                assert!(gate.wait_until_entered(Duration::from_secs(5)), "{method}");
                capture.lose(capture.target());
                gate.release();
                worker.join().unwrap()
            });
            let fault = result.unwrap_err();
            assert_eq!(fault.category, "TargetLost", "{method}/{present}");
            let state = engine.lock().unwrap();
            assert!(state.results.is_empty());
            assert_eq!(state.active_queries, 0);
            drop(state);
            assert_eq!(engine.finish()["clean"], true);
            assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
        }
    }
}

#[test]
fn replay_keeps_no_match_distinct_from_postcondition_freshness_and_effect() {
    let backend = Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8).with_calls(vec![
        ScriptedOcrCall::new(vec![]),
        ScriptedOcrCall::new(candidates()),
        ScriptedOcrCall::new(vec![]),
    ]));
    let engine = replay(backend);
    let before = engine.call("observe", json!({})).unwrap();
    assert!(
        engine
            .call("recognize", recognition(&before))
            .unwrap()
            .is_null()
    );
    assert_eq!(engine.snapshot()["script_handles"], 1);
    assert_eq!(
        engine
            .call(
                "postcondition",
                json!({
                    "observation":before,"checkpoint":before,"expected":"visible",
                })
            )
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    let after = engine.call("observe", json!({})).unwrap();
    let request = json!({"observation":after,"checkpoint":before,"expected":"visible"});
    let effect = engine.call("postcondition", request.clone()).unwrap();
    assert_eq!(effect["satisfied"], true);
    assert_eq!(effect["causation_claimed"], false);
    assert!(effect["frame"].as_u64().unwrap() > effect["checkpoint"].as_u64().unwrap());
    assert_eq!(
        engine.call("postcondition", request).unwrap()["satisfied"],
        false
    );
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn retained_query_cannot_republish_a_released_source_observation() {
    let engine = replay(Arc::new(
        ControlledOcr::new(mp::PixelFormat::Rgba8).with_candidates(candidates()),
    ));
    let source = engine.call("observe", json!({})).unwrap();
    let query = engine.call("query", recognition(&source)).unwrap();
    let wait = json!({"id":query["id"],"timeout_ms":1_000});
    let result = engine.call("query_wait", wait.clone()).unwrap();
    assert_eq!(result["text"], "visible");
    engine
        .call("release", json!({"id":result["observation"]["id"]}))
        .unwrap();
    assert_eq!(
        engine.call("query_wait", wait).unwrap_err().category,
        "InvalidHandle"
    );
    assert_eq!(engine.finish()["clean"], true);
    assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
}

#[test]
fn template_no_match_is_historical_but_new_recognition_preserves_capture_terminal_cause() {
    let (mut engine, capture) = controlled(Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8)));
    let source = mp::TemplateSource::new(mp::TemplateSourceRequest {
        id: mp::TemplateId::new("reviewed").unwrap(),
        encoding: mp::TemplateEncoding::Png,
        extent: mp::PixelExtent::new(1, 1),
        space: mp::CoordinateSpace::CapturePixels,
        defaults: mp::MatchDefaults::new(0.9, 1).unwrap(),
        // The controlled matcher accepts bytes; production PNG policy is tested separately.
        content: Arc::from([1u8]),
    })
    .unwrap();
    let template = engine
        .resources
        .engine
        .prepare_template(&source, &mp::OperationContext::new())
        .unwrap();
    Arc::get_mut(&mut engine.resources)
        .unwrap()
        .templates
        .insert("reviewed".into(), template);
    let observation = engine.call("observe", json!({})).unwrap();
    let request = json!({"observation":observation,"kind":"template","asset":"reviewed",
        "roi":{"x":0,"y":0,"width":8,"height":8}});
    let historical = engine.call("recognize", request.clone()).unwrap();
    assert!(historical.is_null());
    capture.lose(capture.target());
    engine
        .lock()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .close(&mp::OperationContext::new())
        .unwrap();
    assert_eq!(
        engine.call("recognize", request).unwrap_err().category,
        "TargetLost"
    );
    assert!(historical.is_null());
    assert_eq!(engine.finish()["clean"], true);
}

fn native_policy() -> NativeConfig {
    serde_json::from_value(json!({
        "executable_or_bundle":"/unused-runtime-executable","process_id":17,
        "process_lifetime":"0123456789abcdef","window_rule":"reviewed",
        "operating_system":"macos","hardware":"test","permission_executable":"/unused-engine",
        "capture":{"approved":true,"duration_ms":10000,"max_frames":3,"wait_ms":1000,"interval_ms":1},
        "input":{"approved":true,"duration_ms":10000,"max_actions":4,"route":"system",
            "focus":"preserve","click_hold_ms":50,"reviewed_operation":"Click once"},
        "geometry":null,"recognition_language":crate::environment::LANGUAGE,
        "visible_postcondition":"visible","cleanup_ms":500,"containment_ms":2000
    })).unwrap()
}

#[cfg(target_os = "macos")]
fn bind_scripted_process(engine: &mut Engine) {
    use super::super::lifetime::{classify_metadata, require_current};

    let pid = engine.native.as_ref().unwrap().process_id;
    let current = classify_metadata(pid, pid, libc::SRUN, (100, 20)).unwrap();
    engine.lock().unwrap().native_process_started = Some(require_current(None, current).unwrap());
    script_process(engine, Ok(current));
}

#[cfg(target_os = "macos")]
fn script_process(
    engine: &mut Engine,
    current: Result<super::super::lifetime::ProcessState, Fault>,
) {
    let pid = engine.native.as_ref().unwrap().process_id;
    engine.process_probe = Some(Box::new(move |requested| {
        assert_eq!(requested, i32::try_from(pid).unwrap());
        current.clone()
    }));
}

#[cfg(target_os = "macos")]
fn controlled_native() -> (Engine, Arc<ControlledCapture>, Value) {
    let (mut engine, capture) = controlled(Arc::new(
        ControlledOcr::new(mp::PixelFormat::Rgba8).with_candidates(candidates()),
    ));
    let observation = engine.call("observe", json!({})).unwrap();
    engine.native = Some(native_policy());
    bind_scripted_process(&mut engine);
    // The controlled provider has no native placement. Seed the lifetime gate
    // with its actual public-facade frame, not a fabricated successful response.
    // Native observation/retention itself is covered by placed_replay below.
    {
        let mut state = engine.lock().unwrap();
        state.native_frame = Some(
            engine
                .observation(&state, &observation)
                .unwrap()
                .frame
                .clone(),
        );
    }
    (engine, capture, observation)
}

fn placed_replay() -> Engine {
    let mut engine = replay(Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8)));
    let descriptor =
        mp::FrameDescriptor::packed(mp::PixelExtent::new(8, 8), mp::PixelFormat::Rgba8).unwrap();
    let frames = [10.0, 11.0]
        .into_iter()
        .enumerate()
        .map(|(index, x)| {
            mp::replay::ReplayFrame::new(
                descriptor,
                mp::MonotonicInstant::from_origin(Duration::from_millis(index as u64)),
                mp::Continuity::Continuous,
                Some(
                    mp::TargetPlacement::new(
                        (x, 20.0),
                        (8.0, 8.0),
                        mp::Scale::new(1.0, 1.0).unwrap(),
                    )
                    .unwrap(),
                ),
                vec![0; descriptor.byte_len()].into_boxed_slice(),
            )
            .unwrap()
        })
        .collect();
    let source = mp::replay::ReplaySource::from_targets(vec![
        mp::replay::ReplayTarget::new("placed", frames).unwrap(),
    ])
    .unwrap();
    let facade = mp::replay_engine(source).unwrap();
    let operation = mp::OperationContext::new();
    let target = facade.discover(&operation).unwrap()[0].id();
    let session = facade
        .open(target, &mp::OpenRequest::new(), &operation)
        .unwrap();
    {
        let mut state = engine.lock().unwrap();
        state.session.take().unwrap().close(&operation).unwrap();
        state.session = Some(session);
    }
    Arc::get_mut(&mut engine.resources).unwrap().engine = facade;
    engine.native = Some(native_policy());
    #[cfg(target_os = "macos")]
    bind_scripted_process(&mut engine);
    engine
}

#[test]
fn native_run_latches_first_authoritative_geometry_and_refuses_later_movement() {
    let engine = placed_replay();
    let first = engine.call("observe", json!({})).unwrap();
    assert_eq!(first["width"], 8);
    let placement = engine.lock().unwrap().placement.unwrap();
    assert_eq!(placement.desktop_origin(), (10.0, 20.0));
    assert_eq!(
        engine.call("observe", json!({})).unwrap_err().category,
        "StaleIdentity"
    );
    assert_eq!(engine.lock().unwrap().placement, Some(placement));
    assert_eq!(engine.snapshot()["script_handles"], 1);
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn native_capture_budget_and_expanded_held_click_budget_apply_without_dispatch() {
    for workflow_ms in [30_000, 900_000] {
        let mut limits = crate::desktop::native_limits();
        limits.workflow_ms = workflow_ms;
        let mut engine = placed_replay();
        let native = engine.native.as_mut().unwrap();
        native.capture.duration_ms = limits.budgets().total_ms().unwrap();
        native.input.duration_ms = native.capture.duration_ms;
        native.capture.max_frames = limits.max_frames;
        native.input.max_actions = limits.max_actions;
        // Seed only consumed accounting; the last acquisition uses the real facade.
        engine.lock().unwrap().captured_frames = limits.max_frames - 1;
        let observation = engine.call("observe", json!({})).unwrap();
        assert_eq!(
            engine.call("observe", json!({})).unwrap_err().category,
            "CaptureLimit"
        );
        engine.control.admission.store(true, Ordering::Release);
        let request = DispatchRequest {
            observation,
            actions: vec![Action::Click {
                x: 1.0,
                y: 2.0,
                button: PointerButton::Left,
            }],
        };
        let mut state = engine.lock().unwrap();
        state.input_events = limits.max_actions - 4;
        let input = engine.input_request(&state, &request).unwrap();
        assert_eq!(input.sequence().len(), 4);
        for spent in [limits.max_actions - 3, limits.max_actions] {
            state.input_events = spent;
            assert_eq!(
                engine.input_request(&state, &request).unwrap_err().category,
                "ActionLimit"
            );
        }
        state.input_cleanup_incomplete = true;
        assert_eq!(
            engine.input_request(&state, &request).unwrap_err().category,
            "IncompleteCleanup"
        );
        drop(state);
        engine
            .call("release", json!({"id":request.observation["id"]}))
            .unwrap();
        assert_eq!(
            engine.call("observe", json!({})).unwrap_err().category,
            "CaptureLimit"
        );
        assert_eq!(engine.lock().unwrap().captured_frames, 300);
        assert_eq!(engine.lock().unwrap().input_events, 64);
        engine.control.check().unwrap();
        assert_eq!(engine.cleanup_limit_ms(), 500);
        let cleanup = engine.finish();
        assert_eq!(cleanup["clean"], false);
        assert_eq!(cleanup["session_closed"], true);
        assert_eq!(cleanup["native_input_release"], "incomplete");
    }
}

#[test]
fn cached_query_cannot_cross_a_new_geometry_generation() {
    let (engine, capture) = controlled(Arc::new(
        ControlledOcr::new(mp::PixelFormat::Rgba8).with_candidates(candidates()),
    ));
    let source = engine.call("observe", json!({})).unwrap();
    let query = engine.call("query", recognition(&source)).unwrap();
    let wait = json!({"id":query["id"],"timeout_ms":1_000});
    let result = engine.call("query_wait", wait.clone()).unwrap();
    assert_eq!(result["text"], "visible");
    capture
        .publish_reshaped(mp::PixelExtent::new(4, 4), 0)
        .unwrap();
    engine.call("observe", json!({})).unwrap();
    assert_eq!(
        engine.call("query_wait", wait).unwrap_err().category,
        "StaleIdentity"
    );
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn missing_authoritative_placement_cannot_establish_a_native_baseline() {
    let mut engine = replay(Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8)));
    engine.native = Some(native_policy());
    #[cfg(target_os = "macos")]
    bind_scripted_process(&mut engine);
    assert_eq!(
        engine.call("observe", json!({})).unwrap_err().category,
        "StaleIdentity"
    );
    let state = engine.lock().unwrap();
    assert!(state.placement.is_none());
    assert!(state.latest.is_none());
    assert!(state.observations.is_empty());
    drop(state);
    assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn historically_committed_query_survives_loss_but_cannot_admit_new_input() {
    let (engine, capture) = controlled(Arc::new(
        ControlledOcr::new(mp::PixelFormat::Rgba8).with_candidates(candidates()),
    ));
    let source = engine.call("observe", json!({})).unwrap();
    let query = engine.call("query", recognition(&source)).unwrap();
    let wait = json!({"id":query["id"],"timeout_ms":1_000});
    let historical = engine.call("query_wait", wait.clone()).unwrap();
    capture.lose(capture.target());
    assert_eq!(engine.call("query_wait", wait).unwrap(), historical);
    let fault = engine
        .call(
            "validate_observation",
            json!({
                "observation":historical["observation"],
            }),
        )
        .unwrap_err();
    assert_eq!(fault.category, "TargetLost");
    assert_eq!(fault.context["stage"], "input_admission");
    assert_eq!(engine.finish()["clean"], true);
}

#[cfg(target_os = "macos")]
#[test]
fn provider_terminal_before_process_exit_preserves_the_consumer_fault() {
    use super::super::lifetime::ProcessState;
    use mado_pilot_capture::CaptureFault;

    for terminal in [
        CaptureFault::AccessDenied,
        CaptureFault::StreamEnded,
        CaptureFault::WindowGeometryChanged,
    ] {
        for method in [
            "observe",
            "recognize",
            "validate_observation",
            "validate_input",
            "dispatch",
            "query",
            "query_wait",
            "scan_ocr_zones",
        ] {
            let (mut engine, capture, observation) = controlled_native();
            let args = match method {
                "observe" => {
                    engine
                        .call("release", json!({"id":observation["id"]}))
                        .unwrap();
                    assert!(engine.lock().unwrap().observations.is_empty());
                    json!({})
                }
                "recognize" | "query" => recognition(&observation),
                "query_wait" => {
                    let query = engine.call("query", recognition(&observation)).unwrap();
                    let wait = json!({"id":query["id"],"timeout_ms":1_000});
                    engine.call("query_wait", wait.clone()).unwrap();
                    wait
                }
                "scan_ocr_zones" => grouped(&observation),
                "validate_observation" => json!({"observation":observation}),
                _ => json!({"observation":observation,"actions":[]}),
            };
            capture.terminate(terminal);
            script_process(&mut engine, Ok(ProcessState::Absent));
            let expected = engine_error("target_lifetime", terminal.into());
            let fault = engine.call(method, args).unwrap_err();
            assert_eq!(fault.category, expected.category, "{terminal:?}/{method}");
            assert_eq!(fault.message, expected.message, "{terminal:?}/{method}");
            assert_eq!(fault.context["cause"], expected.context["cause"]);
            let state = engine.lock().unwrap();
            assert_eq!(state.captured_frames, 1);
            assert_eq!(state.input_events, 0);
            assert!(!state.input_uncertain);
            drop(state);
            assert_eq!(engine.finish()["clean"], true);
            assert!(engine.lock().unwrap().native_frame.is_none());
            assert_eq!(
                engine.terminal_accounting().unwrap(),
                json!({"frames":1,"expanded_input_events":0,"input_uncertain":false})
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn terminal_before_the_first_frame_is_not_masked_by_process_exit() {
    use super::super::lifetime::ProcessState;
    use mado_pilot_capture::CaptureFault;

    for terminal in [CaptureFault::AccessDenied, CaptureFault::StreamEnded] {
        let (mut engine, capture) =
            controlled(Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8)));
        engine.native = Some(native_policy());
        bind_scripted_process(&mut engine);
        capture.terminate(terminal);
        script_process(&mut engine, Ok(ProcessState::Absent));
        let fault = engine.call("observe", json!({})).unwrap_err();
        let expected = engine_error("capture", terminal.into());
        assert_eq!(fault.category, expected.category);
        assert_eq!(fault.context, expected.context);
        assert_eq!(engine.lock().unwrap().captured_frames, 0);
        assert_eq!(engine.finish()["clean"], true);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn provider_failure_during_process_probe_wins_before_exit_publication() {
    use super::super::lifetime::ProcessState;
    use mado_pilot_capture::CaptureFault;

    let (mut engine, capture, observation) = controlled_native();
    engine
        .call("release", json!({"id":observation["id"]}))
        .unwrap();
    engine.process_probe = Some(Box::new(move |_| {
        capture.terminate(CaptureFault::AccessDenied);
        Ok(ProcessState::Absent)
    }));
    let fault = engine.call("observe", json!({})).unwrap_err();
    let expected = engine_error("target_lifetime", CaptureFault::AccessDenied.into());
    assert_eq!(fault.category, expected.category);
    assert_eq!(fault.context, expected.context);
    assert_eq!(engine.lock().unwrap().captured_frames, 1);
    assert_eq!(engine.finish()["clean"], true);
}

#[cfg(target_os = "macos")]
#[test]
fn confirmed_exit_after_script_release_needs_no_additional_capture_credit() {
    use super::super::lifetime::{ProcessState, classify_metadata};

    for (current, reason) in [
        (ProcessState::Absent, "absent"),
        (
            classify_metadata(17, 17, libc::SZOMB, (100, 20)).unwrap(),
            "zombie",
        ),
        (
            classify_metadata(17, 17, libc::SRUN, (101, 20)).unwrap(),
            "reused_pid",
        ),
    ] {
        let mut engine = placed_replay();
        engine.native.as_mut().unwrap().capture.max_frames = 1;
        let observation = engine.call("observe", json!({})).unwrap();
        engine
            .call("release", json!({"id":observation["id"]}))
            .unwrap();
        assert!(engine.lock().unwrap().observations.is_empty());
        script_process(&mut engine, Ok(current));
        let fault = engine.call("observe", json!({})).unwrap_err();
        assert_eq!(fault.category, "TargetExited");
        assert_eq!(fault.context["exit_reason"], reason);
        assert_eq!(engine.lock().unwrap().captured_frames, 1);
        assert_eq!(engine.finish()["clean"], true);
        assert!(engine.lock().unwrap().native_frame.is_none());
        assert_eq!(
            engine.terminal_accounting().unwrap(),
            json!({"frames":1,"expanded_input_events":0,"input_uncertain":false})
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn only_positive_process_proof_refines_a_facade_target_loss() {
    use super::super::lifetime::{ProcessState, classify_metadata};

    for (current, category) in [
        (Ok(ProcessState::Absent), "TargetExited"),
        (
            classify_metadata(17, 17, libc::SRUN, (100, 20)),
            "TargetLost",
        ),
        (classify_metadata(17, 17, libc::SRUN, (0, 20)), "TargetLost"),
        (
            Err(blocked(
                "target_identity_unavailable",
                "scripted lookup failure",
            )),
            "TargetLost",
        ),
    ] {
        let (mut engine, capture, observation) = controlled_native();
        capture.lose(capture.target());
        script_process(&mut engine, current);
        let fault = engine
            .call("recognize", recognition(&observation))
            .unwrap_err();
        assert_eq!(fault.category, category);
        if category == "TargetExited" {
            assert_eq!(fault.context["exit_reason"], "absent");
        } else {
            assert!(fault.context["exit_reason"].is_null());
        }
        assert_eq!(engine.finish()["clean"], true);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_admission_without_a_bound_lifetime_fails_before_capture_or_probe() {
    let mut engine = placed_replay();
    engine.lock().unwrap().native_process_started = None;
    engine.process_probe = Some(Box::new(|_| {
        panic!("an unbound process must not be probed")
    }));
    let fault = engine.call("observe", json!({})).unwrap_err();
    assert_eq!(fault.category, "Blocked");
    assert_eq!(fault.context["stage"], "target_identity_unavailable");
    let state = engine.lock().unwrap();
    assert_eq!(state.captured_frames, 0);
    assert!(state.observations.is_empty());
    assert!(state.native_frame.is_none());
    drop(state);
    assert_eq!(engine.finish()["clean"], true);
}

#[cfg(target_os = "macos")]
#[test]
fn in_flight_recognition_keeps_provider_fault_when_the_process_also_exits() {
    use super::super::lifetime::{ProcessState, classify_metadata};
    use mado_pilot_capture::CaptureFault;

    for terminal in [CaptureFault::AccessDenied, CaptureFault::StreamEnded] {
        let gate = Arc::new(CompletionGate::new());
        let backend = Arc::new(ControlledOcr::new(mp::PixelFormat::Rgba8).with_calls(vec![
            ScriptedOcrCall::new(candidates()).with_completion_gate(gate.clone()),
        ]));
        let (mut engine, capture) = controlled(backend);
        let observation = engine.call("observe", json!({})).unwrap();
        engine.native = Some(native_policy());
        bind_scripted_process(&mut engine);
        {
            let mut state = engine.lock().unwrap();
            state.native_frame = Some(
                engine
                    .observation(&state, &observation)
                    .unwrap()
                    .frame
                    .clone(),
            );
        }
        let exited = Arc::new(AtomicBool::new(false));
        let probe = exited.clone();
        let alive = classify_metadata(17, 17, libc::SRUN, (100, 20)).unwrap();
        engine.process_probe = Some(Box::new(move |pid| {
            assert_eq!(pid, 17);
            Ok(if probe.load(Ordering::Acquire) {
                ProcessState::Absent
            } else {
                alive
            })
        }));
        let result = thread::scope(|scope| {
            let worker = scope.spawn(|| engine.call("recognize", recognition(&observation)));
            let _release = gate.release_guard();
            assert!(gate.wait_until_entered(Duration::from_secs(5)));
            capture.terminate(terminal);
            exited.store(true, Ordering::Release);
            gate.release();
            worker.join().unwrap()
        });
        let fault = result.unwrap_err();
        let expected = engine_error("ocr_recognition", terminal.into());
        assert_eq!(fault.category, expected.category);
        assert_eq!(fault.context, expected.context);
        let state = engine.lock().unwrap();
        assert!(state.results.is_empty());
        assert_eq!(state.captured_frames, 1);
        assert_eq!(state.input_events, 0);
        assert!(!state.input_uncertain);
        drop(state);
        assert_eq!(engine.finish()["clean"], true);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn first_frame_orders_a_confirmed_exit_without_publishing_an_observation() {
    use super::super::lifetime::ProcessState;

    let mut engine = placed_replay();
    script_process(&mut engine, Ok(ProcessState::Absent));
    let fault = engine.call("observe", json!({})).unwrap_err();
    assert_eq!(fault.category, "TargetExited");
    assert_eq!(fault.context["exit_reason"], "absent");
    let state = engine.lock().unwrap();
    assert_eq!(state.captured_frames, 1);
    assert!(state.observations.is_empty());
    assert!(state.placement.is_none());
    assert!(state.latest.is_none());
    assert!(state.native_frame.is_none());
    drop(state);
    assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
    assert_eq!(engine.finish()["clean"], true);
    assert_eq!(
        engine.terminal_accounting().unwrap(),
        json!({"frames":1,"expanded_input_events":0,"input_uncertain":false})
    );
}
