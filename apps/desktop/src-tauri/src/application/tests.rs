use super::test_support::*;
use super::*;
use crate::storage::PackageSource;
use crate::target::TargetExpectation;
use mado_runtime_comparison::desktop::StartRequest;
use mado_runtime_comparison::model::Plan;
use std::fs;

#[cfg(unix)]
#[test]
fn target_owner_conflicts_reinspection_and_native_refusal_preserve_configuration() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = fixture.package_at("target-package");
    declare_target(&path, Some("metadata-fixture"));
    let first = inspect_named(application, "First", &path).unwrap();
    let second = inspect_named(application, "Second", &path).unwrap();
    let first_ref = workspace_ref(&first);
    let second_ref = workspace_ref(&second);
    let initial = application.read_target(&first_ref).unwrap();
    let expected = target_expectation(&initial);
    let configuration = target_configuration();
    let checked = application
        .check_target(&first_ref, &expected, &configuration)
        .unwrap();
    assert_eq!(checked.context.workspace, first_ref);
    assert_eq!(
        application.read_target(&first_ref).unwrap().record.revision,
        0
    );
    let saved = application
        .save_target(&first_ref, &expected, configuration.clone(), None)
        .unwrap();
    assert_eq!(saved.view.context.workspace, first_ref);
    assert!(saved.view.compatible);
    assert_eq!(
        saved
            .view
            .record
            .binding
            .as_ref()
            .unwrap()
            .configuration
            .arguments,
        configuration.arguments
    );
    let file = fixture
        .root
        .join("tabs/First")
        .join(&first.package.package_id)
        .join("target.config");
    let bytes = fs::read(&file).unwrap();
    assert!(
        application
            .save_target(&first_ref, &expected, configuration.clone(), None)
            .is_err()
    );
    assert!(application.remove_target(&first_ref, &expected).is_err());
    assert_eq!(fs::read(&file).unwrap(), bytes);
    let other = application.read_target(&second_ref).unwrap();
    assert!(other.record.binding.is_none());
    assert_eq!(other.record.revision, 0);

    let mut native = request(&first);
    native.lane = "native".into();
    application.start(&first_ref, native).unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.error.unwrap().category, "NativeRefused");
    assert_eq!(fs::read(&file).unwrap(), bytes);

    declare_target(&path, Some("changed-target"));
    let changed = application
        .inspect(&path, &first_ref)
        .unwrap()
        .workspace
        .selection
        .unwrap();
    let changed_ref = workspace_ref(&changed);
    assert_eq!(
        application.read_target(&first_ref).unwrap_err().category,
        "StaleIdentity"
    );
    let incompatible = application.read_target(&changed_ref).unwrap();
    assert!(!incompatible.compatible);
    assert_eq!(fs::read(&file).unwrap(), bytes);
    declare_target(&path, None);
    let targetless = application
        .inspect(&path, &changed_ref)
        .unwrap()
        .workspace
        .selection
        .unwrap();
    let targetless_ref = workspace_ref(&targetless);
    let preserved = application.read_target(&targetless_ref).unwrap();
    assert!(!preserved.compatible);
    assert!(preserved.record.binding.is_some());
    let removed = application
        .remove_target(&targetless_ref, &target_expectation(&preserved))
        .unwrap();
    assert!(removed.record.binding.is_none());
    assert_eq!(removed.record.revision, preserved.record.revision + 1);
    assert!(
        application
            .profiles(&targetless_ref)
            .unwrap()
            .profiles_error
            .is_none()
    );
    assert!(
        application
            .read_target(&second_ref)
            .unwrap()
            .record
            .binding
            .is_none()
    );
}

#[test]
fn target_storage_fault_does_not_gate_controlled_admission_and_reload_repairs() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let selection = inspect_named(application, "Main", &package_path()).unwrap();
    let workspace = workspace_ref(&selection);
    application
        .save_profile(&workspace, None, "Keep", request(&selection).values)
        .unwrap();
    let file = fixture
        .root
        .join("tabs/Main")
        .join(&selection.package.package_id)
        .join("target.config");
    let damaged = b"{broken target";
    fs::write(&file, damaged).unwrap();
    let pending = file.with_extension("pending");
    fs::write(&pending, b"preserve unfinished target write").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(application.read_target(&workspace).is_err());
    assert_eq!(application.profiles(&workspace).unwrap().profiles.len(), 1);
    let mut saved_request = request(&selection);
    saved_request.profile_id = application.profiles(&workspace).unwrap().profiles[0]
        .id
        .clone();
    application.start(&workspace, saved_request).unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.error.unwrap().category, "ChildStartup");
    assert_eq!(fs::read(&file).unwrap(), damaged);
    assert_eq!(
        fs::read(&pending).unwrap(),
        b"preserve unfinished target write"
    );
    fs::remove_file(&pending).unwrap();
    fs::remove_file(&file).unwrap();
    let repaired = application.read_target(&workspace).unwrap();
    assert!(repaired.record.binding.is_none());
    assert_eq!(repaired.record.revision, 0);
}

#[test]
fn target_read_during_preparation_keeps_polling_and_stop_independent() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let selection = inspect_named(application, "Main", &package_path()).unwrap();
    let workspace = workspace_ref(&selection);
    let store = lock(&application.store);
    let starting = application.clone();
    let starting_ref = workspace.clone();
    let start_request = request(&selection);
    let starter = std::thread::spawn(move || starting.start(&starting_ref, start_request));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let run = loop {
        let poll = application.poll();
        if poll.controller["state"] == "preparing" {
            break poll.controller["run"].as_str().unwrap().to_owned();
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    };
    application.command_admitted.store(false, Ordering::Release);
    let reading = application.clone();
    let read_ref = workspace.clone();
    let reader = std::thread::spawn(move || reading.read_target(&read_ref));
    while !application.command_admitted.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        application.poll().controller["workspace_id"],
        workspace.workspace_id
    );
    application.stop(&run).unwrap();
    assert!(matches!(
        application.poll().controller["state"].as_str(),
        Some("stopping" | "terminal")
    ));
    drop(store);
    assert_eq!(starter.join().unwrap().unwrap(), run);
    let target = reader.join().unwrap().unwrap();
    assert_eq!(target.context.workspace, workspace);
    assert_eq!(target.record.revision, 0);
    assert!(target.record.binding.is_none());
    let terminal = settled(application);
    assert_eq!(terminal.error.unwrap().category, "Cancelled");
}

#[test]
fn unbound_commands_refuse_without_borrowing_another_tabs_inventory() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let bound = inspect_named(application, "Bound", &package_path()).unwrap();
    let empty = application.create_workspace("Empty", "No package").unwrap();
    let workspace = view_ref(&empty);
    assert_eq!(workspace.revision, 0);
    assert!(empty.selection.is_none());
    assert!(empty.source_error.is_none());
    let errors = [
        application.validate(&workspace, json!({})).unwrap_err(),
        application.profiles(&workspace).unwrap_err(),
        application
            .save_profile(&workspace, None, "No owner", json!({}))
            .unwrap_err(),
        application
            .rename_profile(&workspace, "missing", "No owner")
            .unwrap_err(),
        application
            .delete_profile(&workspace, "missing")
            .unwrap_err(),
        application.start(&workspace, request(&bound)).unwrap_err(),
        application
            .check_environment(Some(&workspace), None)
            .unwrap_err(),
        application
            .import_legacy_profiles(&workspace)
            .err()
            .unwrap(),
    ];
    for error in errors {
        assert_eq!(error.category, "WorkspaceUnbound");
    }
    assert!(application.runner.poll().run.is_none());
    assert!(
        application
            .inspect(&fixture.root.join("absent"), &workspace)
            .is_err()
    );
    let catalog = application.workspace_catalog().unwrap();
    let unchanged = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Empty")
        .unwrap();
    assert_eq!(view_ref(unchanged), workspace);
    assert!(unchanged.selection.is_none());
    let run = application.check_environment(None, None).unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
    assert_eq!(terminal.error.unwrap().category, "EnvironmentUnset");
    assert!(application.poll().controller["workspace_id"].is_null());
}

#[test]
fn restart_revalidates_saved_open_sources_and_scopes_faults_without_rewriting_them() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let good = inspect_named(application, "Good", &package_path()).unwrap();
    let old = workspace_ref(&good);
    let saved = application
        .save_profile(&old, None, "Durable", json!({"priorities":["ocr"]}))
        .unwrap();
    let missing_path = fixture.package_at("disappearing-source");
    inspect_named(application, "Missing", &missing_path).unwrap();
    inspect_named(application, "Archive", &package_path()).unwrap();
    inspect_named(application, "Changed", &package_path()).unwrap();
    application.create_workspace("Empty", "No package").unwrap();
    let closed = application
        .create_workspace("Closed", "Retained closed")
        .unwrap();
    application.close_workspace(&view_ref(&closed)).unwrap();
    application.create_workspace("Broken", "Malformed").unwrap();
    application.start(&old, request(&good)).unwrap();
    settled(application);
    application.shutdown().unwrap();
    fs::rename(&missing_path, fixture.root.join("relocated-source")).unwrap();
    let archive_path = fixture.root.join("tabs/Archive/tab.config");
    let mut archive = lock(&application.store).tab("Archive").unwrap();
    archive.packages[0].source = PackageSource::CustomArchive {
        path: fixture
            .root
            .join("unsupported.czip")
            .to_string_lossy()
            .into_owned(),
    };
    let archive_bytes = serde_json::to_vec(&archive).unwrap();
    fs::write(&archive_path, &archive_bytes).unwrap();
    let changed_path = fixture.root.join("tabs/Changed/tab.config");
    let mut changed = lock(&application.store).tab("Changed").unwrap();
    changed.packages[0].package_id = "another-package".into();
    changed.selected_package_id = Some("another-package".into());
    let changed_bytes = serde_json::to_vec(&changed).unwrap();
    fs::write(&changed_path, &changed_bytes).unwrap();
    let broken_path = fixture.root.join("tabs/Broken/tab.config");
    fs::write(&broken_path, b"malformed retained Tab").unwrap();
    let capture = application.capture_configuration().unwrap();
    assert_eq!(
        capture.files["tabs/Broken/tab.config"],
        b"malformed retained Tab"
    );
    assert!(capture.files.contains_key("tabs/Closed/tab.config"));
    assert!(!capture.files.keys().any(|path| path.starts_with("logs/")));
    let restarted = Application::new(
        fixture.root.clone(),
        fixture.root.join("absent-runner"),
        fixture.root.join("absent-engine"),
    )
    .unwrap();
    let catalog = restarted.workspace_catalog().unwrap();
    let restored = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Good")
        .unwrap();
    assert_ne!(restored.workspace_id, old.workspace_id);
    assert_eq!(
        restored.selection.as_ref().unwrap().profiles[0].id,
        saved.id
    );
    assert_eq!(
        restarted.validate(&old, json!({})).unwrap_err().category,
        "StaleIdentity"
    );
    let missing = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Missing")
        .unwrap();
    assert!(missing.selection.is_none());
    assert_eq!(
        missing.source_error.as_ref().unwrap().context["internal_name"],
        "Missing"
    );
    assert_eq!(
        restarted.profiles(&view_ref(missing)).unwrap_err().category,
        "WorkspaceUnbound"
    );
    let unsupported = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Archive")
        .unwrap();
    assert!(unsupported.selection.is_none());
    assert_eq!(
        unsupported.source_error.as_ref().unwrap().category,
        "UnsupportedPackageSource"
    );
    let changed = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Changed")
        .unwrap();
    assert!(changed.selection.is_none());
    assert_eq!(
        changed.source_error.as_ref().unwrap().category,
        "PackageIdentity"
    );
    assert_eq!(fs::read(changed_path).unwrap(), changed_bytes);
    let empty = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Empty")
        .unwrap();
    assert!(empty.selection.is_none() && empty.source_error.is_none());
    assert_eq!(catalog.closed[0].internal_name, "Closed");
    assert_eq!(catalog.faults.len(), 1);
    assert_eq!(fs::read(archive_path).unwrap(), archive_bytes);
    assert_eq!(fs::read(broken_path).unwrap(), b"malformed retained Tab");
    let poll = restarted.poll();
    assert!(poll.workspace_results.is_empty());
    assert!(poll.last_check.is_none());
    assert!(poll.controller["run"].is_null());
    restarted.shutdown().unwrap();
}

#[test]
fn create_reopen_and_import_report_through_command_records() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    application.poll();
    let created = application.create_workspace("Alpha", "Alpha").unwrap();
    assert!(application.create_workspace("alpha", "Alias").is_err());
    application.close_workspace(&view_ref(&created)).unwrap();
    let reopened = application.reopen_workspace("Alpha").unwrap();
    assert!(application.reopen_workspace("Alpha").is_err());
    assert_eq!(
        application
            .import_legacy_profiles(&view_ref(&reopened))
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    let entries = application.poll().logs.entries;
    let opened: Vec<_> = entries
        .iter()
        .filter(|entry| entry.code == "workspace.opened")
        .map(|entry| (entry.fields["action"].clone(), entry.workspace_id.clone()))
        .collect();
    assert_eq!(
        opened,
        [
            (
                json!("create_workspace"),
                Some(created.workspace_id.clone())
            ),
            (
                json!("reopen_workspace"),
                Some(reopened.workspace_id.clone())
            ),
        ]
    );
    let failed: Vec<_> = entries
        .iter()
        .filter(|entry| entry.code == "command.failed")
        .map(|entry| {
            (
                entry.fields["action"].clone(),
                entry.fields["category"].clone(),
                entry.workspace_id.clone(),
            )
        })
        .collect();
    assert_eq!(
        failed,
        [
            (json!("create_workspace"), json!("StorageAlias"), None),
            (json!("reopen_workspace"), json!("WorkspaceConflict"), None),
            (
                json!("import_legacy_profiles"),
                json!("WorkspaceUnbound"),
                Some(reopened.workspace_id.clone()),
            ),
        ]
    );
}

#[test]
fn reconstruction_requires_settled_commands_and_closes_all_future_admission() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application.create_workspace("Empty", "Empty").unwrap();
    let store = lock(&application.store);
    application.command_admitted.store(false, Ordering::Release);
    let saving = application.clone();
    let writer = std::thread::spawn(move || saving.save_settings(preferences()));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !application.command_admitted.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let refusal = application.prepare_reconstruction().unwrap_err();
    assert_eq!(refusal.category, "WorkspaceBusy");
    assert_ne!(refusal.context["application_retired"], true);
    assert!(!application.closing.load(Ordering::Acquire));
    drop(store);
    writer.join().unwrap().unwrap();
    application.prepare_reconstruction().unwrap();
    assert_eq!(
        application
            .create_workspace("Late", "Late")
            .unwrap_err()
            .category,
        "Closing"
    );
    assert_eq!(
        application
            .close_workspace(&view_ref(&workspace))
            .unwrap_err()
            .category,
        "Closing"
    );
    assert_eq!(
        application
            .save_settings(preferences())
            .unwrap_err()
            .category,
        "Closing"
    );
    assert_eq!(
        application.prepare_reconstruction().unwrap_err().context["application_retired"],
        true
    );
    assert!(lock(&application.store).tab("Empty").unwrap().open);
    assert!(application.poll().controller["run"].is_null());
}

#[test]
fn invalid_settings_precede_worker_creation() {
    let fixture = Fixture::new();
    fixture.application.shutdown().unwrap();
    let settings_path = fixture.root.join("settings.json");
    let bytes = fs::read(&settings_path).unwrap();
    for invalid in [None, Some(b"invalid settings".as_slice())] {
        match invalid {
            Some(bytes) => {
                Store::new(fixture.root.clone())
                    .unwrap()
                    .initialize(preferences())
                    .unwrap();
                fs::write(&settings_path, bytes).unwrap();
            }
            None => {
                fs::remove_file(&settings_path).unwrap();
            }
        }
        let result = Application::build(
            fixture.root.clone(),
            fixture.root.join("runner"),
            fixture.root.join("engine"),
            Arc::default(),
            |_| panic!("invalid settings must be rejected before starting workers"),
            None,
        );
        assert!(result.is_err());
    }
    fs::write(settings_path, bytes).unwrap();
}

#[test]
fn writer_start_failure_waits_for_bridge_termination_before_retry() {
    let fixture = Fixture::new();
    fixture.application.shutdown().unwrap();
    let root = fixture.root.clone();
    let (exiting, exit) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let (returned, result) = mpsc::sync_channel(1);
    let constructor = std::thread::spawn(move || {
        let failure = Application::build(
            root.clone(),
            root.join("runner"),
            root.join("engine"),
            Arc::default(),
            |_| {
                Err(Fault::new(
                    "LoggingInitialization",
                    "Injected writer creation failure",
                ))
            },
            Some((exiting, released)),
        );
        let _ = returned.send(failure);
    });
    // The actual bridge has observed failed publication, but cannot exit
    // until released. Construction must still own and await that thread.
    exit.recv_timeout(Duration::from_secs(2)).unwrap();
    let premature = result.recv_timeout(Duration::from_millis(100));
    release.send(()).unwrap();
    assert!(matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)));
    let failure = result.recv_timeout(Duration::from_secs(2)).unwrap();
    constructor.join().unwrap();
    assert_eq!(failure.err().unwrap().category, "LoggingInitialization");
    let retried = Application::new(
        fixture.root.clone(),
        fixture.root.join("runner"),
        fixture.root.join("engine"),
    )
    .unwrap();
    assert!(retried.workspace_catalog().unwrap().open.is_empty());
    retried.shutdown().unwrap();
}

#[test]
fn stale_saved_values_are_refused_before_runner_startup() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = package_path();
    let selection = inspect_named(application, "Main", &path).unwrap();
    let workspace = workspace_ref(&selection);
    let mut plan: Plan = serde_json::from_str(include_str!(
        "../../../../../tools/runtime-comparison/fixtures/manual-plan.json"
    ))
    .unwrap();
    plan.limits.snapshot_bytes = mado_runtime_comparison::images::PACKAGE_BYTES;
    let inventory = Inventory::capture(&path, &plan.limits).unwrap();
    let values = inventory.profiles["template-first"]["options"].clone();
    let saved = application
        .save_profile(&workspace, None, "Saved choice", values.clone())
        .unwrap();
    let mut replacement = values.clone();
    replacement["priorities"] = json!(["ocr", "template"]);
    application
        .save_profile(
            &workspace,
            Some(&saved.id),
            "Saved choice",
            replacement.clone(),
        )
        .unwrap();
    let stored_path = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&stored_path).unwrap();
    let run = application
        .start(
            &workspace,
            StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: inventory.identity,
                package_id: saved.package_id.clone(),
                schema_identity: saved.schema_identity.clone(),
                profile_id: saved.id.clone(),
                values,
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            },
        )
        .unwrap();
    let controller = settled(application);
    let fault = controller.error.unwrap();
    assert_eq!(fault.category, "ProfileIdentity");
    assert_eq!(fault.context["profile_id"], saved.id);
    assert_eq!(controller.run.as_deref(), Some(run.as_str()));
    assert_eq!(
        fault.context["cleanup"],
        json!({"clean":true,"child_started":false})
    );
    assert_eq!(fs::read(&stored_path).unwrap(), before);
    let profiles = application.profiles(&workspace).unwrap();
    assert_eq!(profiles.profiles[0].values, replacement);
}

#[test]
fn admission_reserves_before_store_io_and_owns_the_profile_snapshot() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = package_path();
    let selection = inspect_named(application, "Main", &path).unwrap();
    let workspace = workspace_ref(&selection);
    let values = selection.package.profiles["template-first"]["options"].clone();
    let saved = application
        .save_profile(&workspace, None, "Before", values.clone())
        .unwrap();
    let request = StartRequest {
        package_path: path.to_string_lossy().into_owned(),
        inventory_identity: selection.package.inventory_identity.clone(),
        package_id: saved.package_id.clone(),
        schema_identity: saved.schema_identity.clone(),
        profile_id: saved.id.clone(),
        values: values.clone(),
        lane: "controlled".into(),
        scenario: "workflow".into(),
        replay_descriptor_path: None,
    };
    let store = lock(&application.store);
    let (admitted, admission) = mpsc::sync_channel(1);
    let starting = application.clone();
    let starting_workspace = workspace.clone();
    let starter = std::thread::spawn(move || {
        let _ = admitted.send(starting.start(&starting_workspace, request));
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while application.runner.poll().state != "preparing" {
        assert!(
            std::time::Instant::now() < deadline,
            "Start did not reserve"
        );
        std::thread::yield_now();
    }
    let published = application.poll();
    assert_eq!(published.controller["workspace_id"], workspace.workspace_id);
    assert_eq!(
        published.controller["workspace_revision"],
        workspace.revision
    );
    let (checked, check) = mpsc::sync_channel(1);
    let checking = application.clone();
    let competitor = std::thread::spawn(move || {
        let _ = checked.send(checking.check_environment(None, None));
    });
    let refusal = check.recv_timeout(Duration::from_secs(2));
    let premature = admission.recv_timeout(Duration::from_millis(100));
    drop(store);
    competitor.join().unwrap();
    starter.join().unwrap();
    assert_eq!(refusal.unwrap().unwrap_err().category, "RunActive");
    assert!(
        matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)),
        "Start must acquire snapshot ownership before returning admission"
    );
    let run = admission
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    let mut next_values = values;
    next_values["priorities"] = json!(["ocr", "template"]);
    application
        .save_profile(&workspace, Some(&saved.id), "After", next_values)
        .unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
    // The absent fixture executable is the expected boundary, not a changed-profile refusal.
    assert_eq!(terminal.error.unwrap().category, "ChildStartup");
}

#[test]
fn inspection_preserves_unrelated_app_preferences_and_legacy_hint() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let settings_path = fixture.root.join("settings.json");
    let mut legacy = application.settings().unwrap();
    legacy.package_path = Some("previous-package".into());
    fs::write(&settings_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let before = fs::read(&settings_path).unwrap();
    let selection = inspect_named(application, "Main", &package_path()).unwrap();
    let workspace = workspace_ref(&selection);
    assert_eq!(
        application
            .validate(&workspace, json!({"priorities":["ocr"]}))
            .unwrap()["priorities"],
        json!(["ocr"])
    );
    assert_eq!(fs::read(settings_path).unwrap(), before);
    assert_eq!(
        application.settings().unwrap().package_path.as_deref(),
        Some("previous-package")
    );
    let tab = lock(&application.store).tab("Main").unwrap();
    assert_eq!(
        tab.selected_package_id.as_deref(),
        Some(selection.package.package_id.as_str())
    );
}

#[test]
fn unsafe_stored_numbers_remain_preserved_and_unavailable_to_commands() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = fixture.numeric_package();
    let selection = inspect_named(application, "Main", &path).unwrap();
    let workspace = workspace_ref(&selection);
    let inventory = lock(&application.workspaces)
        .resolve(&workspace)
        .unwrap()
        .inventory
        .as_ref()
        .clone();
    let saved = lock(&application.store)
        .profile_store(&selection.internal_name, &inventory.package_id)
        .unwrap()
        .save(
            &inventory,
            None,
            "Exact imported number",
            json!({"amount":9_007_199_254_740_993_u64}),
        )
        .unwrap();
    let stored_path = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&stored_path).unwrap();
    let reopened = application.profiles(&workspace).unwrap();
    assert!(reopened.profiles.is_empty());
    let error = reopened.profiles_error.unwrap();
    assert_eq!(
        error.context["rejected"][0]["context"]["profile_id"],
        saved.id
    );
    assert_eq!(
        error.context["rejected"][0]["context"]["cause"]["context"]["path"],
        "$.amount"
    );
    assert_eq!(
        error.context["rejected"][0]["context"]["cause"]["context"]["value"],
        "9007199254740993"
    );
    let catalog = application.profiles(&workspace).unwrap();
    assert!(catalog.profiles.is_empty());
    let warning = catalog.profiles_error.unwrap();
    assert_eq!(warning.category, "ProfileRejected");
    assert_eq!(
        warning.context["rejected"][0]["context"]["profile_id"],
        saved.id
    );
    assert_eq!(
        application
            .save_profile(
                &workspace,
                Some(&saved.id),
                "Rounded replacement",
                json!({"amount":1})
            )
            .unwrap_err()
            .category,
        "ProfileRejected",
    );
    assert_eq!(
        application
            .rename_profile(&workspace, &saved.id, "Renamed")
            .unwrap_err()
            .category,
        "ProfileRejected"
    );
    application
        .start(
            &workspace,
            StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: inventory.identity,
                package_id: saved.package_id,
                schema_identity: saved.schema_identity,
                profile_id: saved.id,
                values: json!({"amount":9_007_199_254_740_992_u64}),
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            },
        )
        .unwrap();
    let controller = settled(application);
    assert_eq!(controller.error.unwrap().category, "ProfileRejected");
    assert_eq!(fs::read(stored_path).unwrap(), before);
}

#[test]
fn floating_profiles_survive_webview_normalization_save_reopen_and_start() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = fixture.numeric_package();
    let selection = inspect_named(application, "Main", &path).unwrap();
    let workspace = workspace_ref(&selection);
    let inventory = lock(&application.workspaces)
        .resolve(&workspace)
        .unwrap()
        .inventory
        .as_ref()
        .clone();
    let source: Value = serde_json::from_str(
        r#"{"amount":1000000000000000100.0,"numbers":[1.0,1e18,1e100,0.1,9007199254740991.0]}"#,
    )
    .unwrap();
    let stored = lock(&application.store)
        .profile_store(&selection.internal_name, &inventory.package_id)
        .unwrap()
        .save(&inventory, None, "Floating values", source.clone())
        .unwrap();
    let selection = application.profiles(&workspace).unwrap();
    assert_eq!(selection.profiles[0].values, source);
    // These are the integer spellings emitted by WebView JSON.stringify.
    let submitted: Value = serde_json::from_str(
        r#"{"amount":1000000000000000100,"numbers":[1,1000000000000000000,1e100,0.1,9007199254740991]}"#
    ).unwrap();
    let effective = application.validate(&workspace, submitted.clone()).unwrap();
    assert_eq!(effective["amount"], source["amount"]);
    let saved = application
        .save_profile(
            &workspace,
            Some(&stored.id),
            "Floating values",
            submitted.clone(),
        )
        .unwrap();
    assert_eq!(saved.values, source);
    let reopened = application.profiles(&workspace).unwrap();
    assert!(reopened.profiles_error.is_none());
    assert_eq!(reopened.profiles[0].values, source);
    let stored_path = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&stored_path).unwrap();
    let run = application
        .start(
            &workspace,
            StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: inventory.identity,
                package_id: saved.package_id,
                schema_identity: saved.schema_identity,
                profile_id: saved.id.clone(),
                values: submitted,
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            },
        )
        .unwrap();
    assert_eq!(application.runner.poll().run.as_deref(), Some(run.as_str()));
    let terminal = settled(application);
    assert_eq!(terminal.error.as_ref().unwrap().category, "ChildStartup");
    assert_eq!(
        terminal.error.unwrap().context["operation_stage"],
        "execution"
    );
    assert_eq!(fs::read(stored_path).unwrap(), before);
}

#[test]
fn same_id_source_replacement_checks_profiles_before_durable_binding() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let original = inspect_named(application, "Main", &package_path()).unwrap();
    let workspace = workspace_ref(&original);
    let saved = application
        .save_profile(
            &workspace,
            None,
            "Retained profile",
            json!({"priorities":["ocr"]}),
        )
        .unwrap();
    let tab_path = fixture.root.join("tabs/Main/tab.config");
    let profile_path = profile_path(&fixture, "Main", &saved);
    let original_tab = fs::read(&tab_path).unwrap();
    let original_profile = fs::read(&profile_path).unwrap();
    let incompatible = fixture.package_at("incompatible-relocation");
    let schema_path = incompatible.join("schema.json");
    let mut schema: Value = serde_json::from_slice(&fs::read(&schema_path).unwrap()).unwrap();
    schema["properties"]["priorities"]["minItems"] = json!(2);
    fs::write(schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
    let outcome = application.inspect(&incompatible, &workspace).unwrap();
    assert_eq!(outcome.kind, InspectionKind::RecoveryRequired);
    assert_eq!(
        outcome.binding_error.unwrap().category,
        "PackageCompatibility"
    );
    assert_eq!(fs::read(&tab_path).unwrap(), original_tab);
    assert_eq!(fs::read(&profile_path).unwrap(), original_profile);
    let current = outcome.workspace;
    assert_eq!(current.revision, workspace.revision + 1);
    assert!(current.selection.is_none());
    assert_eq!(
        current.recovery.as_ref().unwrap().profiles[0].profile.id,
        saved.id
    );
    let relocated = fixture.package_at("compatible-relocation");
    let replacement = application
        .inspect(&relocated, &view_ref(&current))
        .unwrap()
        .workspace
        .selection
        .unwrap();
    assert_eq!(replacement.workspace_id, workspace.workspace_id);
    assert_eq!(replacement.revision, current.revision + 1);
    assert_eq!(replacement.profiles[0].id, saved.id);
    assert_eq!(replacement.profiles[0].values, saved.values);
    assert_eq!(fs::read(profile_path).unwrap(), original_profile);
    let tab = lock(&application.store).tab("Main").unwrap();
    assert_eq!(tab.packages.len(), 1);
    assert_eq!(
        tab.packages[0].source,
        PackageSource::Directory {
            path: relocated
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        }
    );
}

#[test]
fn same_source_tabs_keep_independent_profiles_and_fresh_reopened_sessions() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let first = inspect_named(application, "Main", &package_path()).unwrap();
    let second = inspect_named(application, "Other", &package_path().join(".")).unwrap();
    let original = workspace_ref(&first);
    let other = workspace_ref(&second);
    assert_ne!(original.workspace_id, other.workspace_id);
    let saved = application
        .save_profile(&original, None, "First", json!({"priorities":["ocr"]}))
        .unwrap();
    assert!(application.profiles(&other).unwrap().profiles.is_empty());
    assert_eq!(
        application
            .rename_profile(&other, &saved.id, "Foreign")
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert_eq!(
        application
            .delete_profile(&other, &saved.id)
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    let mut foreign_request = request(&second);
    foreign_request.profile_id = saved.id.clone();
    foreign_request.values = saved.values.clone();
    application.start(&other, foreign_request).unwrap();
    assert_eq!(
        settled(application).error.unwrap().category,
        "ProfileNotFound"
    );
    let independent = application
        .save_profile(&other, None, "Second", json!({"priorities":["template"]}))
        .unwrap();
    application
        .rename_profile(&original, &saved.id, "Renamed")
        .unwrap();
    assert_eq!(
        application.profiles(&other).unwrap().profiles[0].name,
        independent.name
    );
    assert_eq!(
        application.profiles(&other).unwrap().profiles[0].values["priorities"],
        json!(["template"])
    );
    let stored_path = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&stored_path).unwrap();
    fs::write(&stored_path, b"malformed owner profile").unwrap();
    assert!(application.profiles(&original).is_err());
    assert_eq!(
        application.profiles(&other).unwrap().profiles[0].id,
        independent.id
    );
    assert_eq!(fs::read(&stored_path).unwrap(), b"malformed owner profile");
    fs::write(stored_path, before).unwrap();
    application.close_workspace(&original).unwrap();
    let reopened = application.reopen_workspace("Main").unwrap();
    assert_ne!(reopened.workspace_id, original.workspace_id);
    assert_eq!(
        application
            .validate(&original, json!({}))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(reopened.selection.unwrap().profiles[0].id, saved.id);
    for name in ["A", "B", "C", "D", "E", "F"] {
        application.create_workspace(name, name).unwrap();
    }
    assert_eq!(
        application
            .create_workspace("Ninth", "Ninth")
            .unwrap_err()
            .category,
        "WorkspaceLimit"
    );
    application.close_workspace(&other).unwrap();
    let last = application.create_workspace("Ninth", "Ninth").unwrap();
    assert!(last.selection.is_none());
    assert_eq!(
        application.reopen_workspace("Other").unwrap_err().category,
        "WorkspaceLimit"
    );
    assert!(!lock(&application.store).tab("Other").unwrap().open);
}

#[test]
fn stale_revisions_and_foreign_profiles_cannot_mutate_saved_values() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let first = inspect_named(application, "Main", &package_path()).unwrap();
    let original = workspace_ref(&first);
    let second = inspect_named(application, "Other", &fixture.numeric_package()).unwrap();
    let foreign = workspace_ref(&second);
    let values = first.package.profiles["template-first"]["options"].clone();
    let profile = application
        .save_profile(&original, None, "Original", values.clone())
        .unwrap();
    let path = profile_path(&fixture, "Main", &profile);
    let before = fs::read(&path).unwrap();
    assert_eq!(
        application
            .save_profile(&foreign, Some(&profile.id), "Foreign", values.clone())
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert_eq!(
        application
            .rename_profile(&foreign, &profile.id, "Foreign")
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert_eq!(
        application
            .delete_profile(&foreign, &profile.id)
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    let replacement = application
        .inspect(&package_path(), &original)
        .unwrap()
        .workspace
        .selection
        .unwrap();
    let current = workspace_ref(&replacement);
    assert_eq!(current.revision, original.revision + 1);
    assert_eq!(
        application
            .validate(&original, values.clone())
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(
        application.profiles(&original).unwrap_err().category,
        "StaleIdentity"
    );
    assert_eq!(
        application
            .save_profile(&original, Some(&profile.id), "Stale", values)
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(
        application
            .rename_profile(&original, &profile.id, "Stale")
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(
        application
            .delete_profile(&original, &profile.id)
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(
        application
            .start(&original, request(&first))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(
        application
            .check_environment(Some(&original), None)
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    application.delete_profile(&current, &profile.id).unwrap();
    assert!(!path.exists());
}

#[test]
fn busy_owner_refuses_close_reinspection_and_competitors_without_blocking_stop() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let first = inspect_named(application, "Main", &package_path()).unwrap();
    let origin = workspace_ref(&first);
    let other = inspect_named(application, "Other", &fixture.numeric_package()).unwrap();
    let other_ref = workspace_ref(&other);
    let store = lock(&application.store);
    let starting = application.clone();
    let starting_ref = origin.clone();
    let start_request = request(&first);
    let starter = std::thread::spawn(move || starting.start(&starting_ref, start_request));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let preparing = loop {
        let poll = application.poll();
        if poll.controller["state"] == "preparing" {
            break poll;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    };
    let run = preparing.controller["run"].as_str().unwrap().to_owned();
    let damaged_settings = b"settings became invalid after operation admission";
    fs::write(fixture.root.join("settings.json"), damaged_settings).unwrap();
    let close = application.close_workspace(&origin);
    let inspect = application.inspect(&package_path(), &origin);
    let competing_start = application.start(&other_ref, request(&other));
    let competing_check = application.check_environment(Some(&other_ref), None);
    let target_expected = TargetExpectation {
        revision: 0,
        binding_id: None,
    };
    let target_configuration = target_configuration();
    let target_check =
        application.check_target(&other_ref, &target_expected, &target_configuration);
    let target_save =
        application.save_target(&other_ref, &target_expected, target_configuration, None);
    let target_remove = application.remove_target(&other_ref, &target_expected);
    let reconstruct = application.prepare_reconstruction();
    assert!(!application.closing.load(Ordering::Acquire));
    let capturing = application.clone();
    let (entered, entry) = mpsc::sync_channel(1);
    let snapshot = std::thread::spawn(move || {
        entered.send(()).unwrap();
        capturing.capture_configuration()
    });
    entry.recv_timeout(Duration::from_secs(2)).unwrap();
    let stopped = application.stop(&run);
    let stopped_state = application.poll();
    drop(store);
    starter.join().unwrap().unwrap();
    let captured = snapshot.join().unwrap().unwrap();
    assert_eq!(captured.files["settings.json"], damaged_settings);
    assert!(application.settings().is_err());
    assert_eq!(reconstruct.unwrap_err().category, "RunActive");
    assert_eq!(close.unwrap_err().category, "RunActive");
    assert_eq!(inspect.unwrap_err().category, "RunActive");
    assert_eq!(competing_start.unwrap_err().category, "RunActive");
    assert_eq!(competing_check.unwrap_err().category, "RunActive");
    assert_eq!(target_check.unwrap_err().category, "RunActive");
    assert_eq!(target_save.unwrap_err().category, "RunActive");
    assert_eq!(target_remove.unwrap_err().category, "RunActive");
    stopped.unwrap();
    assert_eq!(stopped_state.controller["state"], "stopping");
    assert_eq!(
        stopped_state.controller["workspace_id"],
        origin.workspace_id
    );
    let terminal = settled(application);
    assert_eq!(terminal.error.unwrap().category, "Cancelled");
    application.close_workspace(&origin).unwrap();
    let closed = application.poll();
    assert_eq!(closed.controller["workspace_id"], origin.workspace_id);
    assert!(
        closed
            .workspace_results
            .iter()
            .all(|value| value.workspace != origin)
    );
}

#[test]
fn successor_retains_unpolled_terminal_and_independent_check_association() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    // Disable only automatic collection, not the real controller or workers.
    application.closing.store(true, Ordering::Release);
    lock(&application.bridge).take().unwrap().join().unwrap();
    application.closing.store(false, Ordering::Release);
    let first = inspect_named(application, "Main", &package_path()).unwrap();
    let origin = workspace_ref(&first);
    let second = inspect_named(application, "Other", &fixture.numeric_package()).unwrap();
    let next = workspace_ref(&second);
    let environment = OcrEnvironment {
        model: mado_runtime_comparison::environment::G004_PROFILE.into(),
        profile: mado_runtime_comparison::environment::G004_PROFILE.into(),
        language: mado_runtime_comparison::environment::LANGUAGE.into(),
        provider: mado_runtime_comparison::environment::PROVIDER.into(),
        runtime_profile: mado_runtime_comparison::environment::RUNTIME_PROFILE.into(),
        model_root: fixture
            .root
            .join("absent-model")
            .to_string_lossy()
            .into_owned(),
        runtime_path: fixture
            .root
            .join("absent-runtime")
            .to_string_lossy()
            .into_owned(),
        native_library_paths: vec![
            fixture
                .root
                .join("absent-library")
                .to_string_lossy()
                .into_owned(),
        ],
    };
    let settings = application.settings().unwrap();
    application
        .save_settings(EditableSettings {
            locale: settings.locale,
            backup_directory: settings.backup_directory,
            packages_root: settings.packages_root,
            gui_log_limit: settings.gui_log_limit,
            ocr_environment: Some(environment.clone()),
            notifications: settings.notifications,
        })
        .unwrap();
    let descriptor = "private-recorded-corpus.json".to_owned();
    let store = lock(&application.store);
    let checking = application.clone();
    let check_origin = origin.clone();
    let check_descriptor = descriptor.clone();
    let checker = std::thread::spawn(move || {
        checking.check_environment(Some(&check_origin), Some(check_descriptor))
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while application.poll().controller["state"] != "preparing" {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(store);
    let checked_run = checker.join().unwrap().unwrap();
    let check_terminal = settled(application);
    assert_eq!(check_terminal.run.as_deref(), Some(checked_run.as_str()));
    let settings = application.settings().unwrap();
    application
        .save_settings(EditableSettings {
            locale: settings.locale,
            backup_directory: settings.backup_directory,
            packages_root: settings.packages_root,
            gui_log_limit: settings.gui_log_limit,
            ocr_environment: None,
            notifications: settings.notifications,
        })
        .unwrap();
    let mut next_request = request(&second);
    next_request.package_path = fixture
        .root
        .join("foreign-missing-package")
        .to_string_lossy()
        .into_owned();
    let next_run = application.start(&next, next_request).unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.run.as_deref(), Some(next_run.as_str()));
    assert_eq!(terminal.error.unwrap().category, "ChildStartup");
    let poll = application.poll();
    let retained = poll
        .workspace_results
        .iter()
        .find(|result| result.workspace == origin)
        .unwrap();
    assert_eq!(retained.controller["run"], checked_run);
    assert_eq!(retained.controller["state"], "terminal");
    let check = poll.last_check.unwrap();
    assert_eq!(check.workspace.as_ref(), Some(&origin));
    assert_eq!(check.environment.as_ref(), Some(&environment));
    assert_eq!(check.descriptor_path.as_deref(), Some(descriptor.as_str()));
    assert_eq!(
        check.package_inventory_identity.as_deref(),
        Some(first.package.inventory_identity.as_str())
    );
    assert_eq!(check.controller["workspace_revision"], origin.revision);
    assert_eq!(poll.controller["workspace_id"], next.workspace_id);
    application.close_workspace(&origin).unwrap();
    let closed = application.poll();
    assert_eq!(closed.last_check.unwrap().controller["run"], checked_run);
    assert!(
        closed
            .workspace_results
            .iter()
            .all(|result| result.workspace != origin)
    );
    let terminal_events: Vec<_> = poll
        .logs
        .entries
        .iter()
        .filter(|entry| entry.code == "run.terminal")
        .collect();
    assert_eq!(terminal_events.len(), 2);
    assert!(terminal_events.iter().all(|entry| entry.level == "error"));
    assert!(
        terminal_events
            .iter()
            .any(
                |entry| entry.workspace_id.as_deref() == Some(origin.workspace_id.as_str())
                    && entry.run.as_deref() == Some(checked_run.as_str())
            )
    );
    assert!(
        terminal_events
            .iter()
            .any(
                |entry| entry.workspace_id.as_deref() == Some(next.workspace_id.as_str())
                    && entry.run.as_deref() == Some(next_run.as_str())
            )
    );
    assert!(
        terminal_events
            .iter()
            .all(|entry| !entry.message.contains(&descriptor))
    );
    assert!(
        application
            .poll()
            .logs
            .entries
            .iter()
            .all(|entry| entry.code != "run.terminal")
    );
}

#[test]
fn close_refuses_inflight_profile_save_without_deleting_saved_data() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let selection = inspect_named(application, "Main", &package_path()).unwrap();
    let workspace = workspace_ref(&selection);
    let values = selection.package.profiles["template-first"]["options"].clone();
    let store = lock(&application.store);
    // Observing admission must not contend for the nonblocking command mutex.
    application.command_admitted.store(false, Ordering::Release);
    let saving = application.clone();
    let save_workspace = workspace.clone();
    let saver = std::thread::spawn(move || {
        saving.save_profile(&save_workspace, None, "Keep on close", values)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !application.command_admitted.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let close = application.close_workspace(&workspace);
    drop(store);
    let profile = saver.join().unwrap().unwrap();
    assert_eq!(close.unwrap_err().category, "WorkspaceBusy");
    application.close_workspace(&workspace).unwrap();
    let reopened = application
        .reopen_workspace("Main")
        .unwrap()
        .selection
        .unwrap();
    assert_ne!(reopened.workspace_id, workspace.workspace_id);
    assert_eq!(reopened.profiles[0].id, profile.id);
    assert_eq!(reopened.profiles[0].values, profile.values);
}
