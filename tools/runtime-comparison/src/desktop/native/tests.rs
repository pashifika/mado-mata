use super::*;
use crate::desktop::StartPreparation;
use crate::desktop::test_support::{fixture, request, settled};
use crate::desktop::{DesktopController, NativeInputPolicy, NativeLimits};
use crate::images::{DecodedImage, PayloadBytes, encode_crop};
use crate::model::Control;
#[cfg(any(target_os = "macos", windows))]
use crate::model::identity;
use crate::recognition::{self, RecognitionDocument};

fn intent() -> NativeIntent {
    NativeIntent {
        target_revision: 1,
        target_binding_id: "saved-target".into(),
        target_declaration_identity: "reviewed-declaration".into(),
        capture_approved: true,
        input_approved: true,
        launch_approved: false,
        max_exit_recoveries: 0,
        review: NativeReview::Operation {
            operation: "Submit the reviewed package action once".into(),
            visible_postcondition: "The selected result is visible".into(),
        },
        limits: native_limits(),
    }
}

#[test]
fn native_review_wire_requires_one_explicit_mode_without_legacy_prose() {
    let mut raw = serde_json::to_value(intent()).unwrap();
    assert_eq!(raw["review"]["kind"], "operation");
    assert!(raw.get("operation").is_none());
    assert!(raw.get("visible_postcondition").is_none());
    raw["review"] = json!({"kind":"script"});
    let selected: NativeIntent = serde_json::from_value(raw.clone()).unwrap();
    assert!(matches!(selected.review, NativeReview::Script {}));
    validate_intent(&selected).unwrap();
    assert_eq!(serde_json::to_value(selected).unwrap(), raw);
    for invalid in [
        Value::Null,
        json!({}),
        json!({"kind":"unknown"}),
        json!({"kind":"operation","operation":"Click once"}),
        json!({"kind":"script","operation":"Click once"}),
        json!({"kind":"script","visible_postcondition":"A label changes"}),
    ] {
        let mut changed = raw.clone();
        changed["review"] = invalid;
        assert!(serde_json::from_value::<NativeIntent>(changed).is_err());
    }
    raw.as_object_mut().unwrap().remove("review");
    assert!(serde_json::from_value::<NativeIntent>(raw.clone()).is_err());
    raw["operation"] = json!("Click once");
    raw["visible_postcondition"] = json!("A label changes");
    assert!(serde_json::from_value::<NativeIntent>(raw).is_err());
}

#[test]
fn saved_script_review_does_not_replace_target_permissions_or_finite_limits() {
    let mut selected = intent();
    selected.review = NativeReview::Script {};
    validate_intent(&selected).unwrap();
    for (pointer, invalid) in [
        ("/target_revision", json!(0)),
        ("/target_binding_id", json!("")),
        ("/target_declaration_identity", json!("")),
        ("/capture_approved", json!(false)),
        ("/input_approved", json!(false)),
        ("/limits/workflow_ms", json!(0)),
        ("/limits/max_actions", json!(0)),
    ] {
        let mut raw = serde_json::to_value(&selected).unwrap();
        *raw.pointer_mut(pointer).unwrap() = invalid;
        let invalid = serde_json::from_value(raw).unwrap();
        assert_eq!(
            validate_intent(&invalid).unwrap_err().category,
            "NativeRefused"
        );
    }
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn four_event_native_projection_bootstraps_typescript_without_widening_input() {
    let inventory = fixture();
    let mut request = request(&inventory);
    request.lane = "native".into();
    let mut selected = intent();
    selected.limits.max_actions = 4;
    request.native_intent = Some(selected);
    let mut plan = crate::desktop::operation::requested_plan(&request).unwrap();
    assert_eq!(plan.limits.max_actions, 4);
    assert_eq!(
        plan.limits.vm_bytes,
        crate::desktop::manual_plan().unwrap().limits.vm_bytes
    );
    // Exercise the projected limits on real QuickJS, never a native input sink.
    plan.lane = "controlled".into();
    plan.native_budgets = None;
    plan.profile = "template-first".into();
    let compiled = crate::typescript::compile(&inventory, &plan.limits).unwrap();
    let options = crate::host::resolve_options(
        &compiled.schema,
        &compiled.profiles[&plan.profile],
        &compiled.package_id,
    )
    .unwrap();
    let control = std::sync::Arc::new(Control::new(&plan.limits));
    let host = crate::host::Host::new(plan, options, compiled.assets.clone(), control).unwrap();
    let result = crate::javascript::run(std::sync::Arc::new(compiled), host.clone());
    let cleanup = host.finish();
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(host.limits().max_actions, 4);
    assert_eq!(host.snapshot()["effects"].as_array().unwrap().len(), 2);
    assert_eq!(cleanup["clean"], true);
}

#[test]
fn recovery_is_default_zero_and_requires_separate_launch_approval() {
    let mut raw = serde_json::to_value(intent()).unwrap();
    raw.as_object_mut().unwrap().remove("max_exit_recoveries");
    let legacy: NativeIntent = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(legacy.max_exit_recoveries, 0);
    validate_intent(&legacy).unwrap();
    for recoveries in [1, 2] {
        raw["max_exit_recoveries"] = json!(recoveries);
        let denied: NativeIntent = serde_json::from_value(raw.clone()).unwrap();
        assert!(validate_intent(&denied).is_err());
    }
    raw["max_exit_recoveries"] = json!(1);
    raw["launch_approved"] = json!(true);
    let approved = validate_intent(&serde_json::from_value(raw.clone()).unwrap());
    if cfg!(windows) {
        assert_eq!(approved.unwrap_err().category, "NativeRefused");
    } else {
        approved.unwrap();
    }
    raw["max_exit_recoveries"] = json!(2);
    assert!(validate_intent(&serde_json::from_value(raw.clone()).unwrap()).is_err());
    for invalid in [json!(-1), json!(256), json!(0.5), json!(true)] {
        raw["max_exit_recoveries"] = invalid;
        assert!(serde_json::from_value::<NativeIntent>(raw.clone()).is_err());
    }
}

fn target() -> NativeTarget {
    NativeTarget {
        executable: std::env::current_exe().unwrap().canonicalize().unwrap(),
        process: Some((17, "0123456789abcdef".into())),
        window_title: "Exact reviewed window".into(),
        input: NativeInputPolicy {
            route: if cfg!(windows) {
                "window_message"
            } else {
                "process_directed"
            }
            .into(),
            focus: "preserve".into(),
            pointer_mode: (!cfg!(windows)).then(|| "appkit_background".into()),
            click_hold_ms: 50,
        },
    }
}

#[test]
fn native_review_rejects_missing_approval_identity_and_unbounded_intent() {
    let controller = DesktopController::new(
        "must-not-launch-controlled".into(),
        "must-not-launch-engine".into(),
    );
    let inventory = fixture();
    let refused_before_effects = |intent| {
        let mut request = request(&inventory);
        request.lane = "native".into();
        request.native_intent = Some(intent);
        let fault = controller
            .start_with_preparation(
                request,
                |_, _| -> Result<StartPreparation, Fault> {
                    panic!("invalid review reached host preparation")
                },
                |_, _| {
                    |_: &Control,
                     _: &dyn Fn(crate::desktop::NativeProgress),
                     _: &dyn Fn() -> Result<(), Fault>|
                     -> Result<Option<NativeTarget>, Fault> {
                        panic!("invalid review reached target resolution")
                    }
                },
            )
            .unwrap_err();
        assert_eq!(fault.category, "NativeRefused");
        assert!(controller.poll().run.is_none());
    };
    for (pointer, invalid) in [
        ("/target_revision", json!(0)),
        ("/target_binding_id", json!("")),
        ("/target_declaration_identity", json!("\n")),
        ("/capture_approved", json!(false)),
        ("/input_approved", json!(false)),
        ("/review/operation", json!("  ")),
        ("/review/operation", json!("x".repeat(4097))),
        ("/review/visible_postcondition", json!("line\nbreak")),
    ] {
        let mut raw = serde_json::to_value(intent()).unwrap();
        *raw.pointer_mut(pointer).unwrap() = invalid;
        let invalid = serde_json::from_value(raw).unwrap();
        refused_before_effects(invalid);
    }
    for field in [
        "startup_ms",
        "readiness_ms",
        "workflow_ms",
        "max_frames",
        "wait_ms",
        "interval_ms",
        "max_actions",
        "cleanup_ms",
        "containment_ms",
    ] {
        let maximum = if field == "workflow_ms" {
            crate::desktop::native_capability().max_workflow_ms
        } else {
            serde_json::to_value(native_limits()).unwrap()[field]
                .as_u64()
                .unwrap()
        };
        for value in [0, maximum + 1, u64::MAX] {
            let mut raw = serde_json::to_value(intent()).unwrap();
            raw["limits"][field] = json!(value);
            refused_before_effects(serde_json::from_value(raw).unwrap());
        }
    }
    for limits in [
        NativeLimits {
            wait_ms: 99,
            ..native_limits()
        },
        NativeLimits {
            startup_ms: 999,
            ..native_limits()
        },
        NativeLimits {
            readiness_ms: 999,
            ..native_limits()
        },
        NativeLimits {
            workflow_ms: 999,
            ..native_limits()
        },
        NativeLimits {
            containment_ms: 1999,
            ..native_limits()
        },
    ] {
        refused_before_effects(NativeIntent { limits, ..intent() });
    }
    for invalid in [json!(-1), json!(1.5), json!("900000"), Value::Null] {
        let mut raw = serde_json::to_value(intent()).unwrap();
        raw["limits"]["workflow_ms"] = invalid;
        assert!(serde_json::from_value::<NativeIntent>(raw).is_err());
    }
}

#[test]
fn native_ipc_rejects_authority_injection_at_every_typed_level() {
    let mut request = request(&fixture());
    request.native_intent = Some(intent());
    for parent in [
        "",
        "/native_intent",
        "/native_intent/review",
        "/native_intent/limits",
    ] {
        for field in [
            "executable",
            "process_id",
            "process_lifetime",
            "native_config",
            "environment",
            "plan",
            "geometry",
        ] {
            let mut raw = serde_json::to_value(&request).unwrap();
            raw.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.into(), json!(42));
            assert!(
                serde_json::from_value::<StartRequest>(raw).is_err(),
                "{parent}/{field}"
            );
        }
    }
}

#[test]
fn native_approval_cannot_enter_other_lanes_or_bypass_host_correspondence() {
    let controller = DesktopController::new(
        "must-not-launch-controlled".into(),
        "must-not-launch-engine".into(),
    );
    for lane in ["controlled", "replay", "native"] {
        let mut request = request(&fixture());
        request.lane = lane.into();
        request.native_intent = Some(intent());
        controller.start(request, None).unwrap();
        let terminal = settled(&controller);
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "NativeRefused", "{lane}");
        assert_eq!(
            fault.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
    }
}

#[test]
fn native_target_policy_has_no_route_focus_or_pointer_fallback() {
    for (route, focus, pointer, hold) in [
        ("unknown", "preserve", None, 0),
        ("system", "activate", None, 0),
        ("system", "preserve", Some("appkit_background"), 0),
        (
            "process_directed",
            "require_focused",
            Some("appkit_background"),
            0,
        ),
        ("process_directed", "preserve", Some("unknown"), 0),
        ("system", "preserve", None, 1001),
    ] {
        let mut target = target();
        target.input = NativeInputPolicy {
            route: route.into(),
            focus: focus.into(),
            pointer_mode: pointer.map(str::to_owned),
            click_hold_ms: hold,
        };
        assert_eq!(
            validate_target(Some(&target)).err().unwrap().category,
            "NativeRefused"
        );
    }
    if cfg!(windows) {
        for (route, focus, pointer) in [
            ("system", "preserve", None),
            ("window_message", "require_focused", None),
            ("process_directed", "preserve", None),
            ("system", "require_focused", Some("core_graphics")),
        ] {
            let mut selected = target();
            selected.input.route = route.into();
            selected.input.focus = focus.into();
            selected.input.pointer_mode = pointer.map(str::to_owned);
            assert!(validate_target(Some(&selected)).is_err());
        }
        let mut selected = target();
        selected.input.route = "system".into();
        selected.input.focus = "require_focused".into();
        validate_target(Some(&selected)).unwrap();
        selected.process = None;
        assert!(validate_target(Some(&selected)).is_err());
    }
    if cfg!(target_os = "macos") {
        let mut selected = target();
        selected.input.route = "window_message".into();
        selected.input.pointer_mode = None;
        assert!(validate_target(Some(&selected)).is_err());
    }
    for lifetime in ["", "0123456789abcdeF", "0123456789abcdef0"] {
        let mut target = target();
        target.process.as_mut().unwrap().1 = lifetime.into();
        assert_eq!(
            validate_target(Some(&target)).err().unwrap().category,
            "NativeRefused"
        );
    }
}

fn authored_inventory() -> Inventory {
    let mut inventory = fixture();
    let image = DecodedImage::from_rgba(2, 2, vec![128; 16]).unwrap();
    let (bytes, reservation) = encode_crop(&image, [0, 0, 2, 2]).unwrap().into_parts();
    let png = PayloadBytes::from_reserved(bytes, reservation).unwrap();
    let crop = recognition::SavedCrop::from_png("reviewed_crop".into(), &png).unwrap();
    let document: RecognitionDocument = serde_json::from_value(json!({
        "version":1,"rounding":1,
        "basis":{"frame_width":8,"frame_height":8,"content":{"x":0,"y":0,"width":8,"height":8}},
        "definitions":[{"id":"button","name":"Button","revision":1,"kind":"template",
            "region":{"u0":0.0,"v0":0.0,"u1":0.25,"v1":0.25},
            "template":{"search_region":{"u0":0.0,"v0":0.0,"u1":1.0,"v1":1.0},"threshold":0.9,"max_results":8},"saved":crop}],
        "template_rights":{"license":"CC0-1.0","created_by":"regression test","created_for":null,"reviewed":true}
    })).unwrap();
    let (maps, manifest) = recognition::build_template_assets([&document], &inventory.package_id)
        .unwrap()
        .unwrap();
    for (id, path, bytes, format, size) in [
        (
            recognition::AUTHORING_ASSET,
            "recognition/authoring.json",
            PayloadBytes::new(document.to_bytes().unwrap()).unwrap(),
            "json",
            0,
        ),
        (
            recognition::TEMPLATE_MAPS_ASSET,
            "recognition/maps.json",
            PayloadBytes::new(serde_json::to_vec(&maps).unwrap()).unwrap(),
            "json",
            0,
        ),
        (
            recognition::ENGINE_MANIFEST_ASSET,
            "recognition/engine.json",
            PayloadBytes::new(manifest).unwrap(),
            "json",
            0,
        ),
        ("reviewed_crop", "recognition/crop.png", png, "png", 2),
    ] {
        inventory.metadata["manifest"]["assets"][id] =
            json!({"path":path,"format":format,"width":size,"height":size});
        inventory.assets.insert(id.into(), bytes);
    }
    inventory.refresh_identity().unwrap();
    inventory.validate().unwrap();
    inventory
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn native_projection_freezes_exact_host_target_review_and_declared_template_assets() {
    let capability = crate::desktop::native_capability();
    let published = serde_json::to_value(&capability).unwrap();
    assert_eq!(
        published,
        json!({
            "default_limits":{
                "startup_ms":60000,"readiness_ms":30000,"workflow_ms":30000,
                "max_frames":300,"wait_ms":1000,"interval_ms":100,"max_actions":64,
                "cleanup_ms":1000,"containment_ms":2000
            },
            "max_workflow_ms":900000
        })
    );
    let inventory = authored_inventory();
    for (workflow_ms, total_ms, review) in [
        (30_000, 120_000, intent().review),
        (900_000, 990_000, intent().review),
        (30_000, 120_000, NativeReview::Script {}),
        (900_000, 990_000, NativeReview::Script {}),
    ] {
        let mut selected = intent();
        selected.review = review;
        selected.limits = capability.default_limits.clone();
        selected.limits.workflow_ms = workflow_ms;
        let mut request = request(&inventory);
        request.lane = "native".into();
        request.native_intent = Some(selected);
        let mut target = target();
        let mut plan = crate::desktop::operation::requested_plan(&request).unwrap();
        assert_eq!(plan.limits.duration_ms, total_ms);
        assert_eq!(plan.native_budgets.unwrap().workflow_ms, workflow_ms);
        let environment = json!({"version":1,"ocr":{"language":crate::environment::LANGUAGE},"native_libraries":[],"replay":null,"native":null});
        let templates = prepare_templates(&inventory, &plan.limits).unwrap();
        project(
            &mut plan,
            &request,
            &target,
            &target.executable,
            &templates,
            environment,
        )
        .unwrap();
        let frozen = identity(&plan).unwrap();
        let native = &plan.native_config.as_ref().unwrap()["native"];
        assert_eq!(native["capture"]["duration_ms"], total_ms);
        assert_eq!(native["input"]["duration_ms"], total_ms);
        assert_eq!(native["capture"]["max_frames"], 300);
        assert_eq!(native["input"]["max_actions"], 64);
        assert_eq!(native["cleanup_ms"], 1_000);
        assert_eq!(native["containment_ms"], 2_000);
        assert_eq!(native["executable_or_bundle"], json!(target.executable));
        assert_eq!(
            native["process_lifetime"],
            target.process.as_ref().unwrap().1
        );
        assert_eq!(native["operating_system"], std::env::consts::OS);
        assert_eq!(native["permission_executable"], json!(target.executable));
        assert_eq!(native["input"]["route"], target.input.route);
        if cfg!(windows) {
            assert!(native["input"].get("macos_process_pointer_mode").is_none());
        } else {
            assert_eq!(
                native["input"]["macos_process_pointer_mode"],
                "appkit_background"
            );
        }
        assert_eq!(native["input"]["click_hold_ms"], 50);
        assert_eq!(
            native["review"],
            serde_json::to_value(&request.native_intent.as_ref().unwrap().review).unwrap()
        );
        assert_eq!(native["input"]["package_approved"], true);
        assert!(native["input"].get("reviewed_operation").is_none());
        assert!(native.get("visible_postcondition").is_none());
        assert!(native["input"].get("representative_actions").is_none());
        assert!(native["geometry"].is_null());
        assert_eq!(
            native["templates"]["reviewed_crop"],
            "recognition.reviewed_crop"
        );
        assert_eq!(
            native["package_entries"]["templates/reviewed_crop.png"],
            "reviewed_crop"
        );
        target.window_title = "replacement".into();
        let review = request.native_intent.as_mut().unwrap();
        review.input_approved = false;
        review.limits.workflow_ms = 1_000;
        assert_eq!(identity(&plan).unwrap(), frozen);
    }
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn native_admission_reserves_selected_duration_before_preparation() {
    use std::time::{Duration, Instant};

    for (workflow_ms, recoveries, operation_ms) in [
        (30_000, 0, 120_000),
        (30_000, 1, 243_000),
        (900_000, 0, 990_000),
        (900_000, 1, 1_983_000),
    ] {
        if cfg!(windows) && recoveries != 0 {
            continue;
        }
        let controller = DesktopController::new(
            "must-not-launch-controlled".into(),
            "must-not-launch-engine".into(),
        );
        let mut request = request(&fixture());
        request.lane = "native".into();
        let mut selected = intent();
        selected.review = NativeReview::Script {};
        selected.limits.workflow_ms = workflow_ms;
        selected.max_exit_recoveries = recoveries;
        selected.launch_approved = recoveries == 1;
        request.native_intent = Some(selected);
        let before = Instant::now();
        controller
            .start_with_preparation(
                request,
                move |_, control| -> Result<StartPreparation, Fault> {
                    let now = Instant::now();
                    let duration = Duration::from_millis(operation_ms);
                    assert!(control.outer_deadline() >= before + duration);
                    assert!(control.outer_deadline() <= now + duration);
                    assert!(control.deadline() >= before + Duration::from_millis(60_000));
                    assert!(control.deadline() <= now + Duration::from_millis(60_000));
                    // End at the public preparation boundary, before any native work.
                    Err(Fault::new("NativeFreeBoundary", "reservation observed"))
                },
                |_, _| {
                    |_: &Control,
                     _: &dyn Fn(crate::desktop::NativeProgress),
                     _: &dyn Fn() -> Result<(), Fault>|
                     -> Result<Option<NativeTarget>, Fault> {
                        panic!("native-free admission must not resolve a target")
                    }
                },
            )
            .unwrap();
        let fault = settled(&controller).error.unwrap();
        assert_eq!(fault.category, "NativeFreeBoundary");
        assert_eq!(
            fault.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
    }
}

#[test]
fn native_projection_rejects_invalid_saved_maps_before_child_activity() {
    let mut inventory = authored_inventory();
    let maps = &inventory.assets[recognition::TEMPLATE_MAPS_ASSET];
    let mut raw: Value = serde_json::from_slice(maps).unwrap();
    raw["templates"]["reviewed_crop"] = json!("undeclared-template");
    inventory.assets.insert(
        recognition::TEMPLATE_MAPS_ASSET.into(),
        PayloadBytes::new(serde_json::to_vec(&raw).unwrap()).unwrap(),
    );
    let plan = crate::desktop::manual_plan().unwrap();
    let error = prepare_templates(&inventory, &plan.limits).err().unwrap();
    assert_eq!(error.category, "RecognitionMetadata");
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn native_stop_during_host_preparation_retains_the_slot_and_prevents_child_launch() {
    use std::sync::mpsc;
    use std::time::Duration;
    let controller = DesktopController::new("unused-controlled".into(), "unused-engine".into());
    let mut request = request(&fixture());
    request.lane = "native".into();
    request.native_intent = Some(intent());
    let (entered, started) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let run = controller
        .start_with_preparation(
            request,
            move |_, control: &Control| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                assert!(control.check().is_err());
                Ok(StartPreparation {
                    environment: None,
                    native: (),
                    inventory: None,
                })
            },
            |_, _| {
                |_: &Control,
                 _: &dyn Fn(crate::desktop::NativeProgress),
                 _: &dyn Fn() -> Result<(), Fault>| {
                    panic!("cancelled capture must not reach target resolution")
                }
            },
        )
        .unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    controller.stop(&run).unwrap();
    assert_eq!(
        controller
            .check_environment(None, None, None)
            .unwrap_err()
            .category,
        "RunActive"
    );
    release.send(()).unwrap();
    let fault = settled(&controller).error.unwrap();
    assert_eq!(fault.category, "Cancelled");
    assert_eq!(
        fault.context["cleanup"],
        json!({"clean":true,"child_started":false})
    );
}

#[test]
fn native_scenarios_and_replay_descriptors_cannot_change_the_admitted_workflow() {
    let mut request = request(&fixture());
    request.lane = "native".into();
    request.native_intent = Some(intent());
    let normal = crate::desktop::operation::requested_plan(&request);
    if cfg!(any(target_os = "macos", windows)) {
        let plan = normal.unwrap();
        assert_eq!(plan.lane, "native");
        request.scenario = "partial".into();
        assert_eq!(
            crate::desktop::operation::requested_plan(&request)
                .unwrap_err()
                .category,
            "InvalidPlan"
        );
        request.scenario = "workflow".into();
        request.replay_descriptor_path = Some("unapproved.json".into());
        assert_eq!(
            crate::desktop::operation::requested_plan(&request)
                .unwrap_err()
                .category,
            "NativeRefused"
        );
    } else {
        assert_eq!(normal.unwrap_err().category, "NativeRefused");
    }
}

#[test]
fn function_requests_reject_launch_recovery_and_controlled_scenarios() {
    let mut request = request(&fixture());
    request.function = Some(crate::typescript::FunctionInvocation {
        module: "main.ts".into(),
        function: "workflow".into(),
        arguments: Vec::new(),
    });
    request.lane = "native".into();
    for (launch_approved, max_exit_recoveries) in [(true, 0), (true, 1), (false, 1)] {
        let mut review = intent();
        review.review = NativeReview::Script {};
        review.launch_approved = launch_approved;
        review.max_exit_recoveries = max_exit_recoveries;
        request.native_intent = Some(review);
        assert_eq!(
            crate::desktop::operation::requested_plan(&request)
                .unwrap_err()
                .category,
            "NativeRefused"
        );
    }
    request.lane = "controlled".into();
    request.native_intent = None;
    request.scenario = "partial".into();
    assert_eq!(
        crate::desktop::operation::requested_plan(&request)
            .unwrap_err()
            .category,
        "FunctionEntry"
    );
    request.scenario = "workflow".into();
    let controller = DesktopController::new("must-not-start".into(), "must-not-start".into());
    assert_eq!(
        controller.start(request, None).unwrap_err().category,
        "Authority"
    );
    assert!(controller.poll().run.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn only_script_projection_defers_process_selection_to_the_sdk() {
    let inventory = fixture();
    let engine = std::env::current_exe().unwrap().canonicalize().unwrap();
    for recoveries in [0, 1] {
        let mut request = request(&inventory);
        request.lane = "native".into();
        let mut selected = intent();
        selected.launch_approved = recoveries != 0;
        selected.max_exit_recoveries = recoveries;
        request.native_intent = Some(selected);
        let mut target = target();
        target.process = None;
        target.executable = engine.clone();
        let mut plan = crate::desktop::manual_plan().unwrap();
        let budgets = request.native_intent.as_ref().unwrap().limits.budgets();
        plan.lane = "native".into();
        plan.native_budgets = Some(budgets);
        plan.limits.duration_ms = budgets.total_ms().unwrap();
        plan.limits.readiness_ms = budgets.readiness_ms;
        let templates = prepare_templates(&inventory, &plan.limits).unwrap();
        let environment = json!({"version":1,"ocr":{"language":crate::environment::LANGUAGE},"native_libraries":[],"replay":null,"native":null});
        assert_eq!(
            project(
                &mut plan,
                &request,
                &target,
                &engine,
                &templates,
                environment.clone()
            )
            .unwrap_err()
            .category,
            "NativeRefused"
        );
        request.native_intent.as_mut().unwrap().review = NativeReview::Script {};
        project(
            &mut plan,
            &request,
            &target,
            &engine,
            &templates,
            environment,
        )
        .unwrap();
        let native = &plan.native_config.as_ref().unwrap()["native"];
        assert_eq!(native["executable_or_bundle"], json!(target.executable));
        assert!(native["process_id"].is_null());
        assert!(native["process_lifetime"].is_null());
        assert_eq!(native["confirm_process_exit"], recoveries != 0);
    }
}

#[cfg(windows)]
#[test]
fn windows_launch_and_recovery_refuse_before_reservation_or_preparation() {
    for (launch_approved, max_exit_recoveries) in [(true, 0), (true, 1), (false, 1)] {
        for review in [intent().review, NativeReview::Script {}] {
            let controller =
                DesktopController::new("unused-controlled".into(), "unused-engine".into());
            let mut request = request(&fixture());
            request.lane = "native".into();
            let mut selected = intent();
            selected.launch_approved = launch_approved;
            selected.max_exit_recoveries = max_exit_recoveries;
            selected.review = review;
            request.native_intent = Some(selected);
            let fault = controller
                .start_with_preparation(
                    request,
                    |_, _| -> Result<StartPreparation, Fault> {
                        panic!("unsupported Windows intent reached preparation")
                    },
                    |_, _| {
                        |_: &Control,
                         _: &dyn Fn(crate::desktop::NativeProgress),
                         _: &dyn Fn() -> Result<(), Fault>|
                         -> Result<Option<NativeTarget>, Fault> {
                            panic!("unsupported Windows intent reached target resolution")
                        }
                    },
                )
                .unwrap_err();
            assert_eq!(fault.category, "NativeRefused");
            assert!(controller.poll().run.is_none());
        }
    }
}
