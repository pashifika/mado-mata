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
            launch_approved: false,
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
            .resolve(&control, &|_| {}, &|| Ok(()))
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
            .resolve(&control, &|_| {}, &|| Ok(()))
            .unwrap_err()
            .category,
        "Timeout"
    );
}

struct ScriptedPreparation {
    discoveries: std::collections::VecDeque<Result<NativeDiscovery, Fault>>,
    recipes: usize,
    launches: usize,
    waits: usize,
    recipe_fault: Option<Fault>,
    prepare_action: Option<Box<dyn FnOnce(&Control) -> Result<(), Fault> + Send>>,
    discovery_after_prepare: Option<Result<NativeDiscovery, Fault>>,
    launch_fault: Option<LaunchFailure>,
    cancel_on_wait: bool,
    callback_gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

impl ScriptedPreparation {
    fn new(discoveries: impl IntoIterator<Item = Result<NativeDiscovery, Fault>>) -> Self {
        Self {
            discoveries: discoveries.into_iter().collect(),
            recipes: 0,
            launches: 0,
            waits: 0,
            recipe_fault: None,
            prepare_action: None,
            discovery_after_prepare: None,
            launch_fault: None,
            cancel_on_wait: false,
            callback_gate: None,
        }
    }
}

impl PreparationPlatform for ScriptedPreparation {
    type Recipe = ();
    type Prepared = ();

    fn discover(
        &mut self,
        _binding: &NativeBinding,
        _control: &Control,
    ) -> Result<NativeDiscovery, Fault> {
        self.discoveries.pop_front().expect("unexpected discovery")
    }

    fn recipe(&mut self, _binding: &NativeBinding) -> Result<(), Fault> {
        self.recipes += 1;
        self.recipe_fault.take().map_or(Ok(()), Err)
    }

    fn prepare(&mut self, (): (), control: &Control) -> Result<(), Fault> {
        if let Some(action) = self.prepare_action.take() {
            action(control)?;
        }
        if let Some(discovery) = self.discovery_after_prepare.take() {
            self.discoveries.push_front(discovery);
        }
        Ok(())
    }

    fn submit(&mut self, (): ()) -> Result<LaunchDisposition, LaunchFailure> {
        self.launches += 1;
        if let Some((admitted, complete)) = self.callback_gate.take() {
            admitted.send(()).unwrap();
            complete.recv().unwrap();
        }
        self.launch_fault
            .take()
            .map_or(Ok(LaunchDisposition::Accepted), Err)
    }

    fn wait(&mut self, control: &Control) -> Result<(), Fault> {
        self.waits += 1;
        if self.cancel_on_wait {
            control.cancel();
        }
        control.check()
    }
}

fn control() -> Control {
    let plan: Plan = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tools/runtime-comparison/fixtures/manual-plan.json"
    )))
    .unwrap();
    Control::new(&plan.limits)
}

fn binding(launch_approved: bool) -> NativeBinding {
    let (mut request, package, record) = inputs();
    request.native_intent.as_mut().unwrap().launch_approved = launch_approved;
    NativeBinding::capture(&request, &package, record).unwrap()
}

fn unique(pid: u32) -> Result<NativeDiscovery, Fault> {
    Ok(NativeDiscovery::Unique(AuthoringApplication {
        processes: vec![process(pid)],
        installation: "verified".into(),
    }))
}

#[test]
fn an_existing_game_or_external_launch_never_submits_the_recipe() {
    for (discoveries, expected_recipes) in [
        (vec![unique(7)], 0),
        (vec![Ok(NativeDiscovery::Absent), unique(7)], 1),
    ] {
        let mut platform = ScriptedPreparation::new(discoveries);
        let target = binding(true)
            .resolve_with(&control(), &|_| {}, &|| Ok(()), &mut platform)
            .unwrap();
        assert_eq!(target.process_id, 7);
        assert_eq!(target.process_lifetime, "4123abcdef010203");
        assert_eq!(platform.recipes, expected_recipes);
        assert_eq!(platform.launches, 0);
        assert_eq!(platform.waits, 0);
    }
}

#[test]
fn game_appearing_during_recipe_preparation_attaches_without_admission() {
    let mut platform =
        ScriptedPreparation::new([Ok(NativeDiscovery::Absent), Ok(NativeDiscovery::Absent)]);
    platform.discovery_after_prepare = Some(unique(17));
    let control = control();
    let target = binding(true)
        .resolve_with(&control, &|_| {}, &|| Ok(()), &mut platform)
        .unwrap();
    assert_eq!(target.process_id, 17);
    assert_eq!(platform.launches, 0);
    assert_eq!(platform.waits, 0);
    control
        .admit_launch()
        .expect("attachment must not spend launch admission");
}

#[test]
fn resource_changed_during_recipe_preparation_refuses_without_admission() {
    let fixture = crate::application::test_support::Fixture::new();
    let resource = fixture.root.join("captured-engine");
    std::fs::write(&resource, b"captured engine artifact").unwrap();
    let captured_identity = identity(&std::fs::read(&resource).unwrap()).unwrap();
    let mut platform =
        ScriptedPreparation::new([Ok(NativeDiscovery::Absent), Ok(NativeDiscovery::Absent)]);
    let changed_resource = resource.clone();
    platform.prepare_action = Some(Box::new(move |_| {
        std::fs::write(changed_resource, b"replacement engine artifact").unwrap();
        Ok(())
    }));
    let verify_resources = || {
        if identity(&std::fs::read(&resource).unwrap())? != captured_identity {
            return Err(Fault::new(
                "EnginePrerequisite",
                "Captured resource changed",
            ));
        }
        Ok(())
    };
    let control = control();
    let fault = binding(true)
        .resolve_with(&control, &|_| {}, &verify_resources, &mut platform)
        .unwrap_err();
    assert_eq!(fault.category, "EnginePrerequisite");
    assert_eq!(
        fault.context["native_preparation"]["launch"],
        "not_requested"
    );
    assert_eq!(platform.launches, 0);
    assert_eq!(platform.waits, 0);
    control
        .admit_launch()
        .expect("resource refusal must precede admission");
}

#[test]
fn recipe_preparation_failures_remain_not_requested() {
    for category in ["NativeLaunchPreparation", "NativeLaunchRecipeChanged"] {
        let mut platform = ScriptedPreparation::new([Ok(NativeDiscovery::Absent)]);
        platform.prepare_action = Some(Box::new(move |_| Err(Fault::new(category, "refused"))));
        let control = control();
        let fault = binding(true)
            .resolve_with(&control, &|_| {}, &|| Ok(()), &mut platform)
            .unwrap_err();
        assert_eq!(fault.category, category);
        assert_eq!(
            fault.context["native_preparation"]["launch"],
            "not_requested"
        );
        assert_eq!(platform.launches, 0);
        control
            .admit_launch()
            .expect("preparation refusal must precede admission");
    }
}

#[test]
fn stop_after_recipe_preparation_or_at_admission_prevents_submission() {
    for at_admission in [false, true] {
        let mut platform =
            ScriptedPreparation::new([Ok(NativeDiscovery::Absent), Ok(NativeDiscovery::Absent)]);
        if !at_admission {
            platform.prepare_action = Some(Box::new(|control| {
                control.cancel();
                Ok(())
            }));
        }
        let control = control();
        let fault = binding(true)
            .resolve_with(
                &control,
                &|progress| {
                    if at_admission && progress.phase == NativePhase::LaunchSubmission {
                        control.cancel();
                    }
                },
                &|| control.check(),
                &mut platform,
            )
            .unwrap_err();
        assert_eq!(fault.category, "Cancelled");
        assert_eq!(
            fault.context["native_preparation"]["launch"],
            "not_requested"
        );
        assert_eq!(platform.launches, 0);
        assert_eq!(platform.waits, 0);
    }
}

#[test]
fn accepted_launch_waits_for_the_actual_game_without_resubmission() {
    let mut platform = ScriptedPreparation::new([
        Ok(NativeDiscovery::Absent),
        Ok(NativeDiscovery::Absent),
        Ok(NativeDiscovery::Absent),
        unique(9),
    ]);
    let progress = std::cell::RefCell::new(Vec::new());
    let target = binding(true)
        .resolve_with(
            &control(),
            &|value| progress.borrow_mut().push(value),
            &|| Ok(()),
            &mut platform,
        )
        .unwrap();
    assert_eq!(target.process_id, 9);
    assert_eq!(platform.launches, 1);
    assert_eq!(platform.waits, 1);
    let progress = progress.into_inner();
    assert_eq!(
        progress.last().unwrap().phase,
        NativePhase::WaitingForWindow
    );
    assert_eq!(progress.last().unwrap().launch, LaunchDisposition::Accepted);
}

#[test]
fn absence_without_approval_and_untrustworthy_discovery_never_launch() {
    for (approved, discovery, category) in [
        (false, Ok(NativeDiscovery::Absent), "NativeTargetMissing"),
        (
            true,
            Ok(NativeDiscovery::Ambiguous),
            "NativeTargetAmbiguous",
        ),
        (
            true,
            Ok(NativeDiscovery::Unverifiable),
            "NativeTargetUnverifiable",
        ),
        (
            true,
            Err(Fault::new("LookupFailed", "OS discovery failed")),
            "LookupFailed",
        ),
    ] {
        let mut platform = ScriptedPreparation::new([discovery]);
        let fault = binding(approved)
            .resolve_with(&control(), &|_| {}, &|| Ok(()), &mut platform)
            .unwrap_err();
        assert_eq!(fault.category, category);
        assert_eq!(platform.recipes, 0);
        assert_eq!(platform.launches, 0);
        assert_eq!(
            fault.context["native_preparation"]["launch"],
            "not_requested"
        );
    }
}

#[test]
fn final_discovery_and_recipe_failures_refuse_before_launch() {
    for discovery in [NativeDiscovery::Ambiguous, NativeDiscovery::Unverifiable] {
        let mut platform = ScriptedPreparation::new([Ok(NativeDiscovery::Absent), Ok(discovery)]);
        let fault = binding(true)
            .resolve_with(&control(), &|_| {}, &|| Ok(()), &mut platform)
            .unwrap_err();
        assert!(matches!(
            fault.category.as_str(),
            "NativeTargetAmbiguous" | "NativeTargetUnverifiable"
        ));
        assert_eq!(platform.launches, 0);
    }
    let mut platform = ScriptedPreparation::new([Ok(NativeDiscovery::Absent)]);
    platform.recipe_fault = Some(Fault::new(
        "TargetResolutionChanged",
        "saved recipe changed",
    ));
    let fault = binding(true)
        .resolve_with(&control(), &|_| {}, &|| Ok(()), &mut platform)
        .unwrap_err();
    assert_eq!(fault.category, "TargetResolutionChanged");
    assert_eq!(platform.launches, 0);
}

#[test]
fn rejected_or_uncertain_launch_preserves_disposition_without_retry() {
    for (disposition, serialized) in [
        (LaunchDisposition::Rejected, "rejected"),
        (LaunchDisposition::Uncertain, "uncertain"),
    ] {
        let mut platform =
            ScriptedPreparation::new([Ok(NativeDiscovery::Absent), Ok(NativeDiscovery::Absent)]);
        platform.launch_fault = Some(LaunchFailure {
            disposition,
            fault: Fault::new("NativeLaunch", "OS request failed"),
        });
        let fault = binding(true)
            .resolve_with(&control(), &|_| {}, &|| Ok(()), &mut platform)
            .unwrap_err();
        assert_eq!(fault.category, "NativeLaunch");
        assert_eq!(fault.context["native_preparation"]["launch"], serialized);
        assert_eq!(
            fault.context["native_preparation"]["phase"],
            "launch_submission"
        );
        assert_eq!(platform.launches, 1);
        assert_eq!(platform.waits, 0);
    }
}

#[test]
fn stop_during_process_wait_retains_accepted_launch_and_admits_no_target() {
    let mut platform = ScriptedPreparation::new([
        Ok(NativeDiscovery::Absent),
        Ok(NativeDiscovery::Absent),
        Ok(NativeDiscovery::Absent),
    ]);
    platform.cancel_on_wait = true;
    let fault = binding(true)
        .resolve_with(&control(), &|_| {}, &|| Ok(()), &mut platform)
        .unwrap_err();
    assert_eq!(fault.category, "Cancelled");
    assert_eq!(
        fault.context["native_preparation"]["phase"],
        "waiting_for_process"
    );
    assert_eq!(fault.context["native_preparation"]["launch"], "accepted");
    assert_eq!(platform.launches, 1);
}

#[test]
fn a_callback_settling_after_stop_cannot_admit_native_execution() {
    use std::sync::{Arc, mpsc};
    let (admitted, pending) = mpsc::channel();
    let (complete, completion) = mpsc::channel();
    let mut platform =
        ScriptedPreparation::new([Ok(NativeDiscovery::Absent), Ok(NativeDiscovery::Absent)]);
    platform.callback_gate = Some((admitted, completion));
    let control = Arc::new(control());
    let worker_control = Arc::clone(&control);
    let worker = std::thread::spawn(move || {
        binding(true).resolve_with(&worker_control, &|_| {}, &|| Ok(()), &mut platform)
    });
    pending.recv_timeout(Duration::from_secs(5)).unwrap();
    control.cancel();
    assert!(
        !worker.is_finished(),
        "the admitted callback is still physically owned"
    );
    complete.send(()).unwrap();
    let fault = worker.join().unwrap().unwrap_err();
    assert_eq!(fault.category, "Cancelled");
    assert_eq!(fault.context["native_preparation"]["launch"], "accepted");
    assert_eq!(
        fault.context["native_preparation"]["phase"],
        "launch_submission"
    );
}
