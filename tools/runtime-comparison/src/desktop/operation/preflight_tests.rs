use super::*;
#[cfg(target_os = "macos")]
use crate::desktop::test_support::request;
use crate::desktop::test_support::settled;
#[cfg(target_os = "macos")]
use crate::desktop::{NativeIntent, native_limits};
use crate::inventory::PackageDraft;
use std::fs;
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicBool;

struct Package {
    root: PathBuf,
}

impl Package {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mado-native-preflight-{}-{}",
            std::process::id(),
            NEXT_RUN.fetch_add(1, Ordering::Relaxed),
        ));
        let draft = PackageDraft::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &manual_plan().unwrap().limits,
        )
        .unwrap();
        for (path, bytes) in draft.files() {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        Self { root }
    }

    #[cfg(target_os = "macos")]
    fn request(&self) -> StartRequest {
        let inventory = Inventory::capture(&self.root, &manual_plan().unwrap().limits).unwrap();
        let mut request = request(&inventory);
        request.package_path = self.root.to_str().unwrap().into();
        request.lane = "native".into();
        request.native_intent = Some(NativeIntent {
            target_revision: 1,
            target_binding_id: "saved-target".into(),
            target_declaration_identity: "reviewed-declaration".into(),
            capture_approved: true,
            input_approved: true,
            launch_approved: true,
            operation: "One reviewed workflow".into(),
            visible_postcondition: "Reviewed result".into(),
            limits: native_limits(),
        });
        request
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(target_os = "macos")]
fn refused_before_resolve(
    request: StartRequest,
    engine: PathBuf,
    environment: Option<OcrEnvironment>,
) -> Fault {
    let controller = DesktopController::new("unused-controlled".into(), engine);
    let resolved = Arc::new(AtomicBool::new(false));
    let called = resolved.clone();
    controller
        .start_with_preparation(
            request,
            move |_, _| {
                Ok(StartPreparation {
                    environment,
                    native: (),
                })
            },
            move |(), _, _, _| {
                called.store(true, Ordering::Release);
                Err(Fault::new(
                    "UnexpectedResolution",
                    "preflight admitted target resolution",
                ))
            },
        )
        .unwrap();
    let view = settled(&controller);
    assert!(!resolved.load(Ordering::Acquire));
    assert_eq!(
        view.native_preparation,
        Some(NativeProgress {
            phase: NativePhase::Preflight,
            launch: LaunchDisposition::NotRequested,
        })
    );
    let fault = view.error.unwrap();
    assert_eq!(
        fault.context["cleanup"],
        json!({"clean":true,"child_started":false})
    );
    fault
}

#[cfg(target_os = "macos")]
#[test]
fn invalid_native_inputs_never_reach_target_resolution() {
    for invalid in [
        "manifest",
        "stale",
        "profile",
        "schema",
        "image",
        "import",
        "syntax",
        "engine",
        "top-level",
        "resources",
    ] {
        let package = Package::new();
        let mut request = package.request();
        let mut engine = std::env::current_exe().unwrap();
        let expected_stage = match invalid {
            "manifest" => {
                fs::write(package.root.join("package.json"), b"{}").unwrap();
                "package_validation"
            }
            "stale" => {
                fs::write(package.root.join("main.js"), b"export function readiness() { return 'Ready'; }\nexport function workflow() {}\n").unwrap();
                "profile_validation"
            }
            "profile" => {
                request.values["postcondition"] = json!("INVALID");
                "profile_validation"
            }
            "schema" => {
                request.schema_identity = "stale-schema".into();
                "profile_validation"
            }
            "image" => {
                fs::write(package.root.join("assets/marker.rgba"), [0; 3]).unwrap();
                "package_validation"
            }
            "import" | "syntax" => {
                let source = if invalid == "import" {
                    "export { missing } from './missing.js';\nexport function readiness() { return 'Ready'; }\nexport function workflow() {}\n"
                } else {
                    "export function readiness() { const invalid = ; }\nexport function workflow() {}\n"
                };
                fs::write(package.root.join("main.js"), source).unwrap();
                request = package.request();
                "static_preflight"
            }
            "engine" => {
                engine = package.root.join("missing-engine");
                "engine_availability"
            }
            "top-level" => {
                fs::write(package.root.join("main.js"),
                    "export function readiness() { return 'Ready'; }\nexport function workflow() {}\nthrow new Error('must not evaluate during preflight');\n").unwrap();
                request = package.request();
                engine = package.root.join("missing-engine");
                "engine_availability"
            }
            "resources" => "environment_validation",
            _ => unreachable!(),
        };
        let environment = (invalid == "resources").then(|| OcrEnvironment {
            model: crate::environment::G004_PROFILE.into(),
            profile: crate::environment::G004_PROFILE.into(),
            language: crate::environment::LANGUAGE.into(),
            provider: crate::environment::PROVIDER.into(),
            runtime_profile: crate::environment::RUNTIME_PROFILE.into(),
            model_root: package.root.join("missing-models").to_str().unwrap().into(),
            runtime_path: package
                .root
                .join("missing-runtime")
                .to_str()
                .unwrap()
                .into(),
            native_library_paths: vec![
                package
                    .root
                    .join("missing-library")
                    .to_str()
                    .unwrap()
                    .into(),
            ],
        });
        let fault = refused_before_resolve(request, engine, environment);
        let stage = fault
            .context
            .get("operation_stage")
            .unwrap_or(&fault.context["stage"]);
        assert_eq!(stage, expected_stage, "{invalid}: {fault:?}");
        if invalid == "stale" {
            assert_eq!(fault.category, "StaleIdentity");
        }
    }
}

#[test]
fn changed_engine_artifact_is_refused_at_both_consistency_boundaries() {
    let package = Package::new();
    let engine = package.root.join("engine");
    fs::write(&engine, b"immutable engine artifact").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let control = Control::new(&manual_plan().unwrap().limits);
    let artifact = engine_available(&engine, &control).unwrap();
    verify_resources(None, Some(&artifact), &control).unwrap();
    fs::write(&engine, b"different engine artifact").unwrap();
    let failure = verify_resources(None, Some(&artifact), &control).unwrap_err();
    assert_eq!(failure.context["reason"], "configuration_changed");
    let replacement = engine_available(&engine, &control).unwrap();
    fs::remove_file(&engine).unwrap();
    assert!(verify_resources(None, Some(&replacement), &control).is_err());
}

#[test]
fn prepared_modules_refuse_a_different_inventory_and_keep_typescript_source_mapping() {
    let plan = manual_plan().unwrap();
    let limits = plan.limits.clone();
    let control = Arc::new(Control::new(&limits));
    let mut source = crate::desktop::test_support::fixture();
    source.sources.insert("main.ts".into(),
        "export function readiness() { throw new Error('prepared source'); }\nexport function workflow() {}\n".into());
    source.refresh_identity().unwrap();
    let compiled = crate::typescript::compile_with_control(&source, &limits, &control).unwrap();
    let inventory = Arc::new(compiled);
    let prepared =
        crate::typescript::PreparedModules::capture(&inventory, &limits, &control).unwrap();
    let options = crate::host::resolve_options(
        &inventory.schema,
        &inventory.profiles[&plan.profile],
        &inventory.package_id,
    )
    .unwrap();
    let host = crate::host::Host::new(plan, options, inventory.assets.clone(), control).unwrap();
    let fault =
        crate::javascript::run_prepared(inventory.clone(), host.clone(), &prepared).unwrap_err();
    let fault = crate::typescript::map_fault(&inventory, fault);
    assert_eq!(fault.category, "Script");
    assert!(fault.context["typescript"]["frames"].as_array().unwrap().iter()
        .any(|frame| frame["original"]["module"] == "main.ts" && frame["original"]["line"] == 1));
    assert_eq!(host.finish()["clean"], true);
    let mut changed = (*inventory).clone();
    changed
        .sources
        .get_mut("main.js")
        .unwrap()
        .push_str("\nthrow new Error('changed');\n");
    changed.refresh_identity().unwrap();
    assert_eq!(
        prepared.parser(&changed).unwrap_err().category,
        "StaleIdentity"
    );
}

#[test]
fn late_launch_settlement_keeps_owner_and_disposition_despite_stop_and_full_progress() {
    let controller = DesktopController::new("unused-controlled".into(), "unused-engine".into());
    let (entered, started) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let run = controller
        .reserve(
            "run",
            Some(30_000),
            move |owner, control, observer, _, _| {
                let evidence = Evidence::new("run", owner, None, None).unwrap();
                control.admit_launch()?;
                entered.send(()).unwrap();
                wait.recv().unwrap();
                for _ in 0..PROGRESS_CAPACITY {
                    let _ = observer
                        .progress
                        .try_send(json!({"event":"PreparationStage"}));
                }
                evidence.native_progress(
                    NativeProgress {
                        phase: NativePhase::LaunchSubmission,
                        launch: LaunchDisposition::Accepted,
                    },
                    observer,
                );
                control
                    .check()
                    .map_err(|fault| evidence.fault(fault, true))?;
                panic!("Stop must prevent target-dependent initialization");
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
    let terminal = settled(&controller);
    let accepted = NativeProgress {
        phase: NativePhase::LaunchSubmission,
        launch: LaunchDisposition::Accepted,
    };
    assert_eq!(terminal.native_preparation, Some(accepted));
    let fault = terminal.error.unwrap();
    assert_eq!(fault.category, "Cancelled");
    assert_eq!(fault.context["native_preparation"], json!(accepted));
    assert_eq!(
        fault.context["cleanup"],
        json!({"clean":true,"child_started":false})
    );
    let next = controller.check_environment(None, None, None).unwrap();
    let mut state = controller.state();
    state.progress(
        json!({"event":"NativePreparation","app_run":run,"phase":"workflow","launch":"accepted"}),
    );
    assert_eq!(state.run.as_deref(), Some(next.as_str()));
    assert_eq!(state.native_preparation, None);
}

#[test]
fn phase_only_child_progress_preserves_the_accepted_launch() {
    let mut state = State::new();
    state.run = Some("owner".into());
    state.native_preparation = Some(NativeProgress {
        phase: NativePhase::WaitingForWindow,
        launch: LaunchDisposition::Accepted,
    });
    for phase in [
        NativePhase::NativeInitialization,
        NativePhase::Readiness,
        NativePhase::Workflow,
    ] {
        state.progress(json!({"event":"NativePreparation","run":"child","phase":phase}));
        assert_eq!(
            state.native_preparation,
            Some(NativeProgress {
                phase,
                launch: LaunchDisposition::Accepted
            })
        );
        let event = state.progress.last().unwrap();
        assert_eq!(event["run"], "owner");
        assert_eq!(event["child_run"], "child");
        assert_eq!(event["launch"], "accepted");
    }
}

#[test]
fn spent_preparation_budget_cannot_be_restarted_by_the_supervisor() {
    let plan = manual_plan().unwrap();
    let control = Control::with_deadline(&plan.limits, Instant::now() - Duration::from_millis(1));
    let (progress, _) = mpsc::sync_channel(PROGRESS_CAPACITY);
    let (logs, _) = mpsc::sync_channel(LOG_CAPACITY);
    let observer = Observer {
        progress,
        logs,
        dropped_logs: Arc::new(AtomicU64::new(0)),
    };
    let fault = run_prepared_with_executable(
        Path::new("must-not-spawn"),
        &plan,
        &crate::desktop::test_support::fixture(),
        crate::runner::PreparedExecution {
            control: &control,
            modules: None,
            images: None,
        },
        &observer,
    )
    .unwrap_err();
    assert_eq!(fault.category, "Timeout");
    assert_eq!(
        fault.context["cleanup"],
        json!({"clean":true,"child_started":false})
    );
}
