use super::*;
use crate::target::{
    AuthoringProcess, ResolvedLocation, TargetConfiguration, TargetInputPolicy, TargetLocation,
    TargetResolution,
};
use mado_runtime_comparison::desktop::{NativeIntent, native_limits};
use mado_runtime_comparison::model::{Plan, identity};
use serde_json::json;

fn inputs() -> (StartRequest, PackageInfo, TargetRecord) {
    let declaration = TargetDeclaration {
        id: "game".into(),
        window_title: Some("Game".into()),
        macos: None,
    };
    let declaration_identity = identity(&declaration).unwrap();
    let package = PackageInfo {
        package_id: "test.native".into(),
        inventory_identity: "inventory".into(),
        schema_identity: "schema".into(),
        runtime: "javascript".into(),
        schema: json!({}),
        profiles: Default::default(),
        effective_defaults: None,
        target: Some(declaration),
        target_identity: Some(declaration_identity.clone()),
    };
    let record = TargetRecord {
        version: 1,
        internal_name: "Main".into(),
        package_id: package.package_id.clone(),
        revision: 3,
        binding: Some(TargetBinding {
            id: "aaaaaaaaaaaaaaaaaaaa".into(),
            package_id: package.package_id.clone(),
            target_id: "game".into(),
            declaration_identity: declaration_identity.clone(),
            configuration: TargetConfiguration {
                platform: "macos".into(),
                game: TargetLocation {
                    kind: "bundle".into(),
                    path: "/Applications/Game.app".into(),
                },
                launcher: None,
                arguments: Vec::new(),
                working_directory: None,
                window_title: "Game".into(),
                input: Some(TargetInputPolicy {
                    route: "process_directed".into(),
                    focus: "preserve".into(),
                    pointer_mode: Some("core_graphics".into()),
                    click_hold_ms: 0,
                }),
            },
            resolution: TargetResolution {
                game: ResolvedLocation {
                    path: "/Applications/Game.app".into(),
                    executable: "/Applications/Game.app/Contents/MacOS/Game".into(),
                },
                launcher: None,
                working_directory: None,
            },
        }),
    };
    let request = StartRequest {
        package_path: "package".into(),
        inventory_identity: package.inventory_identity.clone(),
        package_id: package.package_id.clone(),
        schema_identity: package.schema_identity.clone(),
        profile_id: "draft".into(),
        values: json!({}),
        lane: "native".into(),
        scenario: "workflow".into(),
        replay_descriptor_path: None,
        native_intent: Some(NativeIntent {
            target_revision: record.revision,
            target_binding_id: record.binding.as_ref().unwrap().id.clone(),
            target_declaration_identity: declaration_identity,
            capture_approved: true,
            input_approved: true,
            operation: "Click the declared button once".into(),
            visible_postcondition: "The declared label changes".into(),
            limits: native_limits(),
        }),
    };
    (request, package, record)
}

#[test]
fn stale_review_cannot_select_changed_binding_or_declaration() {
    let (request, package, mut record) = inputs();
    record.revision += 1;
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap_err()
            .category,
        "TargetConflict"
    );

    let (request, package, mut record) = inputs();
    record.binding.as_mut().unwrap().id = "bbbbbbbbbbbbbbbbbbbb".into();
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap_err()
            .category,
        "TargetConflict"
    );

    let (request, mut package, record) = inputs();
    package.target_identity = Some("changed-declaration".into());
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap_err()
            .category,
        "StaleIdentity"
    );

    let (request, package, mut record) = inputs();
    record.binding.as_mut().unwrap().package_id = "another.package".into();
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap_err()
            .category,
        "TargetConflict"
    );
}

#[test]
fn configuration_without_native_scope_cannot_be_projected() {
    let (request, package, mut record) = inputs();
    record.binding.as_mut().unwrap().configuration.game.kind = "executable".into();
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap_err()
            .category,
        "NativeTargetUnsupported"
    );

    let (request, package, mut record) = inputs();
    record.binding.as_mut().unwrap().configuration.input = None;
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap_err()
            .category,
        "NativeTargetUnset"
    );

    let (request, package, mut record) = inputs();
    record
        .binding
        .as_mut()
        .unwrap()
        .configuration
        .window_title
        .clear();
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap_err()
            .category,
        "NativeTargetUnset"
    );
}

fn process(pid: u32) -> AuthoringProcess {
    AuthoringProcess {
        pid,
        lifetime: 0x4123abcdef010203,
        architecture: 0,
        started: (1, 0),
        executable: "/runtime/Game.app/Contents/MacOS/Game".into(),
    }
}

#[test]
fn only_unique_fresh_correspondence_selects_the_runtime_copy() {
    for (processes, category) in [
        (Vec::new(), "NativeTargetMissing"),
        (vec![process(1), process(2)], "NativeTargetAmbiguous"),
    ] {
        let (request, package, record) = inputs();
        let binding = NativeBinding::capture(&request, &package, record).unwrap();
        let proof = AuthoringApplication {
            processes,
            installation: "verified".into(),
        };
        assert_eq!(binding.project(proof).unwrap_err().category, category);
    }
    let (request, package, record) = inputs();
    let binding = NativeBinding::capture(&request, &package, record).unwrap();
    let target = binding
        .project(AuthoringApplication {
            processes: vec![process(7)],
            installation: "verified".into(),
        })
        .unwrap();
    assert_eq!(
        target.executable,
        std::path::Path::new("/runtime/Game.app/Contents/MacOS/Game")
    );
    assert_eq!(target.process_id, 7);
    assert_eq!(target.process_lifetime, "4123abcdef010203");
}

#[test]
fn cancellation_and_deadline_refuse_before_any_os_correspondence() {
    let mut plan: Plan = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tools/runtime-comparison/fixtures/manual-plan.json"
    )))
    .unwrap();
    let control = Control::new(&plan.limits);
    control.cancel();
    let (request, package, record) = inputs();
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap()
            .resolve(&control)
            .unwrap_err()
            .category,
        "Cancelled"
    );

    plan.limits.duration_ms = 0;
    let control = Control::new(&plan.limits);
    let (request, package, record) = inputs();
    assert_eq!(
        NativeBinding::capture(&request, &package, record)
            .unwrap()
            .resolve(&control)
            .unwrap_err()
            .category,
        "Timeout"
    );
}
