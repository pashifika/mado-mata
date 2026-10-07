use super::*;
use crate::desktop::StartPreparation;
use crate::desktop::test_support::{fixture, request, settled};
use crate::desktop::{DesktopController, NativeInputPolicy, NativeLimits};
use crate::images::{DecodedImage, PayloadBytes, encode_crop};
use crate::model::Control;
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
        operation: "Submit the reviewed package action once".into(),
        visible_postcondition: "The selected result is visible".into(),
        limits: native_limits(),
    }
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
    validate_intent(&serde_json::from_value(raw.clone()).unwrap()).unwrap();
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
        process_id: 17,
        process_lifetime: "0123456789abcdef".into(),
        window_title: "Exact reviewed window".into(),
        input: NativeInputPolicy {
            route: "process_directed".into(),
            focus: "preserve".into(),
            pointer_mode: Some("appkit_background".into()),
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
    for (field, invalid) in [
        ("target_revision", json!(0)),
        ("target_binding_id", json!("")),
        ("target_declaration_identity", json!("\n")),
        ("capture_approved", json!(false)),
        ("input_approved", json!(false)),
        ("operation", json!("  ")),
        ("operation", json!("x".repeat(4097))),
        ("visible_postcondition", json!("line\nbreak")),
    ] {
        let mut raw = serde_json::to_value(intent()).unwrap();
        raw[field] = invalid;
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
    for parent in ["", "/native_intent", "/native_intent/limits"] {
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
        ("window_message", "preserve", None, 0),
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
    for lifetime in ["", "0123456789abcdeF", "0123456789abcdef0"] {
        let mut target = target();
        target.process_lifetime = lifetime.into();
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
    for (workflow_ms, total_ms) in [(30_000, 120_000), (900_000, 990_000)] {
        let mut selected = intent();
        selected.limits = capability.default_limits.clone();
        selected.limits.workflow_ms = workflow_ms;
        let mut request = request(&inventory);
        request.lane = "native".into();
        request.native_intent = Some(selected);
        let mut target = target();
        let mut plan = if cfg!(target_os = "macos") {
            crate::desktop::operation::requested_plan(&request).unwrap()
        } else {
            // Non-macOS admission remains unavailable; test pure projection only.
            let mut plan = crate::desktop::manual_plan().unwrap();
            let limits = &request.native_intent.as_ref().unwrap().limits;
            plan.lane = "native".into();
            plan.native_budgets = Some(limits.budgets());
            plan.limits.duration_ms = limits.budgets().total_ms().unwrap();
            plan.limits.readiness_ms = limits.readiness_ms;
            plan
        };
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
        assert_eq!(native["process_lifetime"], target.process_lifetime);
        assert_eq!(native["input"]["route"], "process_directed");
        assert_eq!(
            native["input"]["macos_process_pointer_mode"],
            "appkit_background"
        );
        assert_eq!(native["input"]["click_hold_ms"], 50);
        assert_eq!(
            native["input"]["reviewed_operation"],
            request.native_intent.as_ref().unwrap().operation
        );
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

#[cfg(target_os = "macos")]
#[test]
fn native_admission_reserves_selected_duration_before_preparation() {
    use std::time::{Duration, Instant};

    for (workflow_ms, recoveries, operation_ms) in [
        (30_000, 0, 120_000),
        (30_000, 1, 243_000),
        (900_000, 0, 990_000),
        (900_000, 1, 1_983_000),
    ] {
        let controller = DesktopController::new(
            "must-not-launch-controlled".into(),
            "must-not-launch-engine".into(),
        );
        let mut request = request(&fixture());
        request.lane = "native".into();
        let mut selected = intent();
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

#[cfg(target_os = "macos")]
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
    if cfg!(target_os = "macos") {
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
