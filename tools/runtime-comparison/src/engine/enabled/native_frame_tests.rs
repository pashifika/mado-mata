use super::*;
use mado_pilot_capture::{CaptureProvider, CaptureSession, SessionDescription, StreamState};
use mado_pilot_runtime::{EngineWiring, Matcher, PackageLoader};
use mado_pilot_testkit::controlled_input::{Behavior, Cleanup};
use mado_pilot_testkit::{ControlledInput, ControlledMatcher, ControlledProducer};

const PROVIDER: mp::ProviderId = mp::ProviderId::new("controlled");

#[derive(Debug)]
struct Capture {
    description: SessionDescription,
    stream: StreamState,
    producer: ControlledProducer,
    lose_on_commit: AtomicBool,
}

impl CaptureSession for Capture {
    fn description(&self) -> SessionDescription {
        self.description.clone()
    }

    fn frame(
        &self,
        request: &mp::FrameRequest,
        operation: &mp::OperationContext,
    ) -> mp::Result<mp::Frame> {
        self.stream.frame(request, operation)
    }

    fn commit_frame(&self, frame: &mp::Frame, operation: &mp::OperationContext) -> mp::Result<()> {
        if self.lose_on_commit.swap(false, Ordering::AcqRel) {
            self.stream.terminate(mp::CaptureFault::TargetLost);
        }
        self.stream.commit_frame(frame, operation)
    }

    fn close(&self, operation: &mp::OperationContext) -> mp::Result<()> {
        self.stream.drain(operation)
    }

    fn lifecycle(&self) -> mp::Lifecycle {
        self.stream.lifecycle()
    }
}

#[derive(Debug)]
struct Provider(Arc<Capture>);

impl CaptureProvider for Provider {
    fn provider(&self) -> mp::ProviderId {
        PROVIDER
    }

    fn discover(&self, _: &mp::OperationContext) -> mp::Result<Vec<mp::TargetDescription>> {
        Ok(vec![mp::TargetDescription::new(
            self.0.description.target(),
            "controlled window",
            mp::PixelExtent::new(8, 8),
            mp::PixelFormat::Rgba8,
            mp::CoordinateSupport::with_target_placement(),
        )])
    }

    fn open(
        &self,
        target: mp::TargetId,
        _: &mp::OpenRequest,
        _: &mp::OperationContext,
    ) -> mp::Result<Arc<dyn CaptureSession>> {
        if target != self.0.description.target() {
            return Err(mp::CaptureFault::TargetLost.into());
        }
        Ok(self.0.clone())
    }
}

fn routes() -> &'static [mp::InputDelivery] {
    if cfg!(windows) {
        &[mp::InputDelivery::System, mp::InputDelivery::WindowMessage]
    } else {
        &[mp::InputDelivery::System]
    }
}

fn fixture(route: mp::InputDelivery) -> (Engine, Arc<Capture>, Arc<ControlledInput>) {
    fixture_with_input(route, Behavior::Complete, Cleanup::Complete)
}

fn fixture_with_input(
    route: mp::InputDelivery,
    behavior: Behavior,
    cleanup: Cleanup,
) -> (Engine, Arc<Capture>, Arc<ControlledInput>) {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.lane = "replay".into();
    let mut engine = Engine::replay_for_test(
        &plan,
        Arc::new(Control::new(&plan.limits)),
        "native-frame-test",
        1,
        Arc::new(HandleBudget::new(plan.limits.handles)),
    );
    let issuer = mado_pilot_core::IdentityIssuer::new();
    let target = issuer.issue_target(PROVIDER).unwrap();
    let stream = issuer.issue_stream().unwrap();
    let capture = Arc::new(Capture {
        description: SessionDescription::new(
            target,
            stream,
            mp::PixelExtent::new(8, 8),
            mp::PixelFormat::Rgba8,
            mp::CoordinateSupport::with_target_placement(),
        ),
        stream: StreamState::new(stream),
        producer: ControlledProducer::new(mp::PixelExtent::new(8, 8), mp::PixelFormat::Rgba8, 1, 2)
            .unwrap(),
        lose_on_commit: AtomicBool::new(false),
    });
    let evidence = if route == mp::InputDelivery::System {
        mp::SubmissionEvidence::SystemInputAdmission
    } else {
        mp::SubmissionEvidence::TargetQueueAdmission
    };
    let capability = mp::InputCapability::none()
        .with_pair(
            mp::InputOperationKind::Pointer,
            route,
            mp::CapabilitySupport::Supported,
            evidence,
        )
        .with_pair(
            mp::InputOperationKind::Keyboard,
            route,
            mp::CapabilitySupport::Supported,
            evidence,
        )
        .with_pointer_space(route, mp::CoordinateSpace::CapturePixels);
    let input = Arc::new(ControlledInput::with_capability(target, capability));
    input.set_behavior(behavior);
    input.set_cleanup(cleanup);
    let facade = mp::Engine::new(EngineWiring {
        engine: issuer.engine(),
        capture: Arc::new(Provider(capture.clone())),
        matcher: Matcher::new(Arc::new(ControlledMatcher::new(mp::PixelFormat::Rgba8))),
        loader: PackageLoader::new(),
        ocr: None,
        input: Some(input.clone()),
        permission: None,
    })
    .unwrap();
    let operation = mp::OperationContext::new();
    let session = facade
        .open_session(
            target,
            &mp::SessionRequest::new()
                .capturing(mp::OpenRequest::new())
                .requesting_input(
                    mp::InputOpenRequest::new()
                        .with_requirement(mp::InputRequirement::Required)
                        .requiring(mp::InputOperationKind::Pointer, route)
                        .requiring(mp::InputOperationKind::Keyboard, route),
                ),
            &operation,
        )
        .unwrap();
    {
        let mut state = engine.lock().unwrap();
        state.session.take().unwrap().close(&operation).unwrap();
        state.session = Some(session);
    }
    Arc::get_mut(&mut engine.resources).unwrap().engine = facade;
    engine.native = Some(serde_json::from_value(json!({
        "executable_or_bundle":"unused","process_id":17,"process_lifetime":"0000000000000001",
        "window_rule":"controlled window","operating_system":std::env::consts::OS,
        "hardware":"test","permission_executable":"unused",
        "capture":{"approved":true,"duration_ms":10000,"max_frames":8,"wait_ms":1000,"interval_ms":1},
        "input":{"approved":true,"duration_ms":10000,"max_actions":32,"route":route.as_str(),
            "focus":if route == mp::InputDelivery::System { "require_focused" } else { "preserve" },
            "click_hold_ms":1,"package_approved":true},
        "geometry":null,"recognition_language":crate::environment::LANGUAGE,
        "review":{"kind":"script"},"cleanup_ms":500,"containment_ms":2000
    })).unwrap());
    engine.control.admission.store(true, Ordering::Release);
    publish(&capture, mp::Continuity::Continuous);
    (engine, capture, input)
}

fn publish(capture: &Capture, continuity: mp::Continuity) {
    capture
        .stream
        .publish_storage(
            capture
                .producer
                .placed_publication(
                    0,
                    mp::TargetPlacement::new(
                        (-20.0, 10.0),
                        (4.0, 4.0),
                        mp::Scale::new(2.0, 2.0).unwrap(),
                    )
                    .unwrap(),
                    continuity,
                )
                .unwrap(),
        )
        .unwrap();
}

fn click(observation: &Value) -> Value {
    json!({"observation":observation,"actions":[{"kind":"click","x":1,"y":2,"button":"left"}]})
}

#[test]
fn package_routes_use_the_committed_frame_and_exact_expanded_events() {
    for &route in routes() {
        let (engine, capture, input) = fixture(route);
        let observation = engine.call("observe", json!({})).unwrap();
        let stamp = engine
            .lock()
            .unwrap()
            .native_frame
            .as_ref()
            .unwrap()
            .stamp();
        let receipt = engine
            .call(
                "dispatch",
                json!({"observation":observation,"actions":[
                    {"kind":"click","x":1,"y":2,"button":"left"},
                    {"kind":"drag","button":"left","path":[
                        {"x":1,"y":2,"hold_ms":1},{"x":3,"y":4,"hold_ms":1},{"x":1,"y":2}]},
                    {"kind":"key_down","key":"A"},{"kind":"key_up","key":"A"}
                ]}),
            )
            .unwrap();
        assert_eq!(receipt["status"], "Submitted");
        assert_eq!(receipt["submitted"], 4);
        assert_eq!(receipt["submitted_events"], 13);
        assert_eq!(receipt["total_events"], 13);
        assert_eq!(receipt["application_effect_confirmed"], false);
        let admitted = input.admitted();
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].routes, [route]);
        assert_eq!(admitted[0].selected_route, route);
        assert_eq!(
            admitted[0].geometry,
            mp::PointerGeometry::require_unchanged_since(stamp)
        );
        assert_eq!(
            admitted[0].focus,
            if route == mp::InputDelivery::System {
                mp::FocusPolicy::RequireFocused
            } else {
                mp::FocusPolicy::Preserve
            }
        );
        let events = input.submitted_events();
        assert_eq!(events.len(), 13);
        assert!(events.iter().all(|event| event.route == route));
        assert_eq!(
            events[0].event,
            mp::InputEvent::PointerMove(
                mp::Point::new(mp::CoordinateSpace::CapturePixels, 1.0, 2.0).unwrap()
            )
        );
        assert_eq!(
            events[1].event,
            mp::InputEvent::PointerPress(mp::PointerButton::Primary)
        );
        assert_eq!(
            events[2].event,
            mp::InputEvent::Delay(Duration::from_millis(1))
        );
        assert_eq!(
            events[3].event,
            mp::InputEvent::PointerRelease(mp::PointerButton::Primary)
        );
        engine
            .call("release", json!({"id":observation["id"]}))
            .unwrap();
        assert_eq!(
            engine
                .lock()
                .unwrap()
                .native_frame
                .as_ref()
                .unwrap()
                .stamp(),
            stamp
        );
        assert_eq!(engine.finish()["clean"], true);
        assert!(engine.lock().unwrap().native_frame.is_none());
        assert_eq!(
            capture.producer.detached_slots_free(),
            capture.producer.detached_budget()
        );
        assert_eq!(
            engine.terminal_accounting().unwrap()["expanded_input_events"],
            13
        );
    }
}

#[test]
fn a_native_frame_losing_terminal_commit_never_establishes_input_authority() {
    let (engine, capture, input) = fixture(mp::InputDelivery::System);
    capture.lose_on_commit.store(true, Ordering::Release);
    let fault = engine.call("observe", json!({})).unwrap_err();
    assert_eq!(fault.category, "TargetLost");
    assert_eq!(fault.context["stage"], "observation_publication");
    let state = engine.lock().unwrap();
    assert!(state.native_frame.is_none());
    assert!(state.placement.is_none());
    assert!(state.latest.is_none());
    assert!(state.observations.is_empty());
    drop(state);
    assert!(input.admitted().is_empty());
    assert!(engine.handle_budget.reserve(engine.limits.handles).is_ok());
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn terminal_stopped_discontinuous_and_foreign_observations_never_dispatch() {
    for failure in ["terminal", "stop", "epoch", "owner"] {
        let (engine, capture, input) = fixture(mp::InputDelivery::System);
        let mut observation = engine.call("observe", json!({})).unwrap();
        let expected = match failure {
            "terminal" => {
                capture.stream.terminate(mp::CaptureFault::TargetLost);
                "TargetLost"
            }
            "stop" => {
                engine.control.cancel();
                "Cancelled"
            }
            "epoch" => {
                publish(&capture, mp::Continuity::Discontinuous);
                engine.call("observe", json!({})).unwrap();
                "StaleIdentity"
            }
            "owner" => {
                observation["attempt"] = json!(2);
                "StaleIdentity"
            }
            _ => unreachable!(),
        };
        assert_eq!(
            engine
                .call("dispatch", click(&observation))
                .unwrap_err()
                .category,
            expected
        );
        assert!(input.admitted().is_empty());
        assert!(input.submitted_events().is_empty());
        assert_eq!(engine.finish()["clean"], true);
    }
}

#[test]
fn route_refusal_never_falls_back_and_incomplete_release_closes_input_admission() {
    for &route in routes() {
        for incomplete in [false, true] {
            let behavior = if incomplete {
                Behavior::FailAfter {
                    submitted: 2,
                    fault: mp::InputFault::SubmissionFailed,
                }
            } else {
                Behavior::Refuse {
                    route,
                    fault: mp::InputFault::FocusRequired,
                }
            };
            let cleanup = if incomplete {
                Cleanup::Fails
            } else {
                Cleanup::Complete
            };
            let (engine, _, input) = fixture_with_input(route, behavior, cleanup);
            let observation = engine.call("observe", json!({})).unwrap();
            let receipt = engine.call("dispatch", click(&observation)).unwrap();
            assert_eq!(
                receipt["status"],
                if incomplete { "Partial" } else { "Refused" }
            );
            assert_eq!(receipt["used_fallback"], false);
            assert_eq!(receipt["attempts"].as_array().unwrap().len(), 1);
            assert_eq!(receipt["cleanup"]["may_leave_state_held"], incomplete);
            assert_eq!(receipt["application_effect_confirmed"], false);
            if incomplete {
                assert_eq!(
                    engine
                        .call("dispatch", click(&observation))
                        .unwrap_err()
                        .category,
                    "IncompleteCleanup"
                );
                assert_eq!(input.admitted().len(), 1);
                assert_eq!(input.submitted_events().len(), 2);
            } else {
                assert!(input.admitted().is_empty());
                assert!(input.submitted_events().is_empty());
            }
            let cleanup = engine.finish();
            assert_eq!(cleanup["clean"], !incomplete);
            assert_eq!(cleanup["session_closed"], true);
        }
    }
}

#[test]
fn released_native_frame_keeps_terminal_precedence_without_another_capture() {
    let (mut engine, capture, input) = fixture(mp::InputDelivery::System);
    engine.native.as_mut().unwrap().capture.max_frames = 1;
    let observation = engine.call("observe", json!({})).unwrap();
    engine
        .call("release", json!({"id":observation["id"]}))
        .unwrap();
    capture.stream.terminate(mp::CaptureFault::TargetLost);
    let fault = engine.call("observe", json!({})).unwrap_err();
    assert_eq!(fault.category, "TargetLost");
    assert_eq!(fault.context["stage"], "capture_terminal");
    assert_eq!(engine.lock().unwrap().captured_frames, 1);
    assert!(input.admitted().is_empty());
    assert_eq!(engine.finish()["clean"], true);
}

#[test]
fn a_settled_attempt_cannot_supply_frame_authority_to_a_fresh_attempt() {
    let (old, _, _) = fixture(mp::InputDelivery::System);
    let stale = old.call("observe", json!({})).unwrap();
    assert_eq!(old.finish()["clean"], true);
    let (fresh, _, input) = fixture(mp::InputDelivery::System);
    let current = fresh.call("observe", json!({})).unwrap();
    assert_ne!(stale["session"], current["session"]);
    assert!(fresh.call("dispatch", click(&stale)).is_err());
    assert!(input.admitted().is_empty());
    assert!(input.submitted_events().is_empty());
    assert_eq!(
        fresh.call("dispatch", click(&current)).unwrap()["status"],
        "Submitted"
    );
    assert_eq!(input.admitted().len(), 1);
    assert_eq!(fresh.finish()["clean"], true);
}
