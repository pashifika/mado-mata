use super::*;

fn loader_plan(candidate: &str, selected: &Path) -> Plan {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.candidate = candidate.into();
    plan.lane = "replay".into();
    // Only exercise loader admission: these descriptors are never opened,
    // and the consumer below uses the controlled, non-native host.
    plan.native_config = Some(json!({
        "version": 1,
        "ocr": {
            "model": crate::environment::BOUNDED_PROFILE,
            "profile": crate::environment::BOUNDED_PROFILE,
            "language": crate::environment::LANGUAGE,
            "provider": crate::environment::PROVIDER,
            "runtime_profile": crate::environment::RUNTIME_PROFILE,
            "model_root": selected,
            "runtime": {"path": selected.join("onnxruntime.dll"), "bytes": 0, "sha256": ""}
        },
        "native_libraries": [], "native": null,
        "replay": {"corpus_id": "loader-boundary", "frames": [], "package_entries": {}, "templates": {}}
    }));
    plan
}

#[test]
fn script_candidates_keep_owned_node_outside_the_native_library_path() {
    let node = crate::typescript::node_from_path().unwrap();
    let original_path = std::env::var_os("PATH");
    let selected = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    assert!(!selected.join("node.exe").exists());
    for candidate in ["javascript", "typescript", "lua", "rust"] {
        let plan = loader_plan(candidate, &selected);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.env("MADO_COMPILER_NODE", "untrusted-node.exe");
        let loader = child_loader_environment(&mut command, &plan, Operation::Run).unwrap();
        assert_eq!(loader["search_variable"], "PATH");
        assert_eq!(loader["directories"], json!([selected]));
        let path = command
            .get_envs()
            .find(|(name, _)| {
                name.to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("PATH"))
            })
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(
            std::env::split_paths(path).collect::<Vec<_>>(),
            [selected.clone()]
        );
        let compiler = command
            .get_envs()
            .find(|(name, _)| *name == "MADO_COMPILER_NODE")
            .expect("ambient compiler override must be explicitly replaced or removed")
            .1;
        if matches!(candidate, "javascript" | "typescript") {
            assert_eq!(compiler, Some(node.as_os_str()));
            // No engine-linked test executable is launched with synthetic DLL
            // descriptors. The default build exercises the real parser and VM.
            #[cfg(not(feature = "engine"))]
            run_parser_consumer(command, candidate);
        } else {
            assert_eq!(compiler, None);
        }
    }
    assert_eq!(std::env::var_os("PATH"), original_path);
}

#[cfg(not(feature = "engine"))]
fn run_parser_consumer(mut command: Command, candidate: &str) {
    command
        .args([
            "--exact",
            "runner::supervision::windows_tests::parser_process_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MADO_PARSER_PROCESS_FIXTURE", candidate);
    let mut child =
        OwnedChild::spawn(&mut command, ChildStdio::Protocol, Environment::Inherited).unwrap();
    let stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(16 * 1024).read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(35);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(5));
    };
    if status.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    let output = reader.join().unwrap();
    assert!(
        status.is_some_and(|status| status.success()),
        "{candidate} parser consumer failed: {}",
        String::from_utf8_lossy(&output)
    );
}

#[cfg(not(feature = "engine"))]
#[test]
fn parser_process_fixture() {
    let Ok(candidate) = std::env::var("MADO_PARSER_PROCESS_FIXTURE") else {
        return;
    };
    assert!(matches!(candidate.as_str(), "javascript" | "typescript"));
    assert_eq!(
        crate::typescript::node_from_path().unwrap_err().category,
        "Blocked"
    );
    // This process has no Node on PATH. Backend-only admission must still work,
    // remove the ambient compiler override, and retain DLL-path isolation.
    let selected = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let replay = loader_plan(&candidate, &selected);
    Operation::EnvironmentCheck.validate(&replay).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.env("MADO_COMPILER_NODE", "untrusted-node.exe");
    let loader =
        child_loader_environment(&mut command, &replay, Operation::EnvironmentCheck).unwrap();
    assert_eq!(loader["search_variable"], "PATH");
    assert_eq!(loader["directories"], json!([selected]));
    assert_eq!(
        command
            .get_envs()
            .find(|(name, _)| *name == "MADO_COMPILER_NODE")
            .unwrap()
            .1,
        None
    );
    assert_eq!(
        child_loader_environment(&mut command, &replay, Operation::Run)
            .unwrap_err()
            .category,
        "Blocked"
    );
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.candidate = candidate.clone();
    plan.limits.duration_ms = 30_000;
    let mut inventory = Inventory::capture(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(candidate),
        &plan.limits,
    )
    .unwrap();
    let control = std::sync::Arc::new(Control::new(&plan.limits));
    if plan.candidate == "typescript" {
        inventory =
            crate::typescript::compile_with_control(&inventory, &plan.limits, &control).unwrap();
    }
    let options = crate::host::resolve_options(
        &inventory.schema,
        &inventory.profiles[&plan.profile],
        &inventory.package_id,
    )
    .unwrap();
    let host = crate::host::Host::new(plan, options, inventory.assets.clone(), control).unwrap();
    let result = crate::javascript::run(std::sync::Arc::new(inventory), host.clone());
    let facts = host.snapshot();
    let cleanup = host.finish();
    let metrics = result.unwrap();
    assert!(host.failure().is_none());
    assert_eq!(cleanup["clean"], true);
    assert_eq!(facts["dispatches"], 1);
    assert_eq!(facts["postconditions"][0]["satisfied"], true);
    assert_eq!(host.snapshot()["live_handles"], 0);
    assert_eq!(
        metrics.source_diagnostic.unwrap()["import_parser"]["identity"]["node"],
        "24.18.0"
    );
}
