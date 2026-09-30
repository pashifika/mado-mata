use super::*;
use crate::application::test_support::*;
use std::fs;

#[test]
fn environment_check_refuses_missing_or_stale_package_selection() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let unknown = WorkspaceRef {
        workspace_id: "unknown".into(),
        revision: 1,
    };
    let error = application
        .check_environment(Some(&unknown), None)
        .unwrap_err();
    assert_eq!(error.category, "StaleIdentity");
    let selection = inspect_named(application, "Main", &package_path()).unwrap();
    let workspace = workspace_ref(&selection);
    let replacement = application
        .inspect(&fixture.numeric_package(), &workspace)
        .unwrap();
    assert_eq!(replacement.workspace.workspace_id, workspace.workspace_id);
    let error = application
        .check_environment(Some(&workspace), None)
        .unwrap_err();
    assert_eq!(error.category, "StaleIdentity");
    assert!(application.runner.poll().run.is_none());
}

#[test]
fn environment_check_without_package_does_not_inherit_cached_selection() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let selection = inspect_named(application, "Main", &package_path()).unwrap();
    let run = application.check_environment(None, None).unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
    let fault = terminal.error.unwrap();
    assert_eq!(fault.category, "EnvironmentUnset");
    assert!(fault.context["package_inventory_identity"].is_null());
    let poll = serde_json::to_value(application.poll()).unwrap();
    assert!(poll["controller"]["workspace_id"].is_null());
    assert!(poll["controller"]["workspace_revision"].is_null());
    assert!(poll["last_check"]["workspace"].is_null());
    assert_eq!(poll["last_check"]["controller"]["run"], run);
    assert_eq!(poll["workspace_results"], json!([]));

    // Omitting the package must not discard the cached inspection either.
    let identity = selection.package.inventory_identity.clone();
    let run = application
        .check_environment(Some(&workspace_ref(&selection)), None)
        .unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
    let fault = terminal.error.unwrap();
    assert_eq!(fault.category, "EnvironmentUnset");
    assert_eq!(fault.context["package_inventory_identity"], identity);
}

#[test]
fn controlled_start_ignores_unavailable_environment_settings() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = package_path();
    let selection = inspect_named(application, "Main", &path).unwrap();
    let workspace = workspace_ref(&selection);
    let settings_path = fixture.root.join("settings.json");
    let preserved = b"unreadable OCR settings must not disable controlled runs";
    fs::write(&settings_path, preserved).unwrap();
    let package = selection.package;
    application
        .start(
            &workspace,
            StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: package.inventory_identity.clone(),
                package_id: package.package_id.clone(),
                schema_identity: package.schema_identity.clone(),
                profile_id: "draft".into(),
                values: package.profiles["template-first"]["options"].clone(),
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
                native_intent: None,
            },
        )
        .unwrap();
    let terminal = settled(application);
    let fault = terminal.error.unwrap();
    assert_eq!(fault.category, "ChildStartup");
    assert_eq!(fault.context["operation_stage"], "execution");
    assert!(fault.context["environment_identity"].is_null());
    assert_eq!(fs::read(settings_path).unwrap(), preserved);
}

#[test]
fn terminal_outcomes_keep_severity_and_distinguish_cleanup_without_private_detail() {
    let passed = json!({"operation":"run","error":null,"result":{
        "status":"PASS","primary":null,"entry_outcome":"Returned",
        "cleanup":{"clean":true,"status":"CleanupFinished"},"forced":false,"exit_code":0}});
    let outcome = TerminalOutcome::from_view(&passed);
    assert_eq!(outcome.notice().0, "info");
    assert!(!outcome.requires_native_reconciliation());
    assert_eq!(
        outcome.fields(&passed["operation"]),
        json!({"action":"run","state":"terminal","status":"PASS","category":null,
            "entry_outcome":"Returned","cleanup_clean":true,"child_started":null,"forced":false})
    );

    // Script failure with clean cleanup: an error, but not an incomplete cleanup.
    let clean_failure = json!({"operation":"run","error":null,"result":{
        "status":"FAIL","entry_outcome":"Returned","forced":false,"exit_code":0,
        "primary":{"category":"Script","message":"recognized private words","context":{"path":"/private/root"}},
        "cleanup":{"clean":true,"status":"CleanupFinished"}}});
    let outcome = TerminalOutcome::from_view(&clean_failure);
    assert_eq!(outcome.notice().0, "error");
    assert!(!outcome.requires_native_reconciliation());
    let fields = outcome.fields(&clean_failure["operation"]);
    assert_eq!(fields["status"], "FAIL");
    assert_eq!(fields["category"], "Script");
    assert_eq!(fields["cleanup_clean"], true);
    assert_eq!(fields["forced"], false);
    assert!(!fields.to_string().contains("private"));

    // Forced containment with a returned entry: distinct from the clean failure above.
    let forced = json!({"operation":"run","error":null,"result":{
        "status":"FAIL","primary":null,"entry_outcome":"Returned","forced":true,"exit_code":124,
        "cleanup":{"clean":false,"status":"IncompleteCleanup","outcome":"ForcedOrIncomplete"}}});
    let outcome = TerminalOutcome::from_view(&forced);
    assert_eq!(outcome.notice().0, "error");
    assert!(outcome.requires_native_reconciliation());
    let fields = outcome.fields(&forced["operation"]);
    assert_eq!(fields["category"], Value::Null);
    assert_eq!(fields["entry_outcome"], "Returned");
    assert_eq!(fields["cleanup_clean"], false);
    assert_eq!(fields["forced"], true);

    // Settled without success but with clean cleanup and no primary stays a warning.
    let unsuccessful = json!({"operation":"run","error":null,"result":{
        "status":"FAIL","primary":null,"entry_outcome":"Returned","forced":false,"exit_code":3,
        "cleanup":{"clean":true}}});
    assert_eq!(TerminalOutcome::from_view(&unsuccessful).notice().0, "warn");

    // Pre-child fault: the worker attests clean cleanup and no child; no status exists.
    let pre_child = json!({"operation":"environment_check","result":null,"error":{
        "category":"EnvironmentUnset","message":"Save an OCR environment","context":{
            "stage":"environment_validation","cleanup":{"clean":true,"child_started":false}}}});
    let outcome = TerminalOutcome::from_view(&pre_child);
    assert_eq!(outcome.notice().0, "error");
    assert!(!outcome.requires_native_reconciliation());
    assert_eq!(
        outcome.fields(&pre_child["operation"]),
        json!({"action":"environment_check","state":"terminal","status":null,
            "category":"EnvironmentUnset","entry_outcome":null,"cleanup_clean":true,
            "child_started":false,"forced":null})
    );

    // A started child without cleanup evidence stays unverified, never clean or false.
    let unattested = json!({"operation":"run","result":null,"error":{
        "category":"ChildStartup","message":"child exited before startup record",
        "context":{"child_started":true,"forced":false,"exit_code":1}}});
    let outcome = TerminalOutcome::from_view(&unattested);
    assert_eq!(outcome.notice().0, "error");
    assert!(outcome.requires_native_reconciliation());
    let fields = outcome.fields(&unattested["operation"]);
    assert_eq!(fields["cleanup_clean"], Value::Null);
    assert_eq!(fields["child_started"], true);
    assert_eq!(fields["forced"], false);
}

#[test]
fn shutdown_during_preparation_persists_a_distinguishable_terminal_record() {
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
    let closing = application.clone();
    let shutdown = std::thread::spawn(move || closing.shutdown());
    // Cancellation can finish before the worker reaches the held store lock,
    // so polling may already report terminal rather than the transient stopping phase.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !matches!(
        application.runner.poll().state.as_str(),
        "stopping" | "terminal"
    ) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(store);
    starter.join().unwrap().unwrap();
    shutdown.join().unwrap().unwrap();
    let status = application.logger.status();
    assert_eq!(status.file_errors, 0);
    assert_eq!(status.file_pending, 0);
    let persisted =
        fs::read_to_string(fixture.root.join("logs").join("application.jsonl")).unwrap();
    let terminal: Vec<crate::logging::LogEntry> = persisted
        .lines()
        .map(|line| serde_json::from_str::<crate::logging::LogEntry>(line).unwrap())
        .filter(|entry| entry.code == "run.terminal")
        .collect();
    assert_eq!(terminal.len(), 1);
    let record = &terminal[0];
    assert_eq!(record.run.as_deref(), Some(run.as_str()));
    assert_eq!(
        record.workspace_id.as_deref(),
        Some(workspace.workspace_id.as_str())
    );
    assert_eq!(record.level, "error");
    assert_eq!(record.fields["action"], "run");
    assert_eq!(record.fields["category"], "Cancelled");
    assert_eq!(record.fields["cleanup_clean"], true);
    assert_eq!(record.fields["child_started"], false);
    assert_eq!(record.fields["status"], Value::Null);
    assert_eq!(record.fields["forced"], Value::Null);
    assert!(!persisted.contains(&selection.package_path));
}

#[cfg(target_os = "macos")]
fn native_request(selection: &crate::application::Selection) -> StartRequest {
    let mut request = request(selection);
    request.lane = "native".into();
    request.native_intent = Some(mado_runtime_comparison::desktop::NativeIntent {
        target_revision: 1,
        target_binding_id: "aaaaaaaaaaaaaaaaaaaa".into(),
        target_declaration_identity: selection.package.target_identity.clone().unwrap(),
        capture_approved: true,
        input_approved: true,
        operation: "One reviewed click".into(),
        visible_postcondition: "The reviewed label changes".into(),
        limits: mado_runtime_comparison::desktop::native_limits(),
    });
    request
}

#[cfg(target_os = "macos")]
#[test]
fn native_review_cannot_substitute_for_a_saved_current_target() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let package = fixture.package_at("native-package");
    declare_target(&package, Some("native-target"));
    let selection = inspect_named(application, "Main", &package).unwrap();
    let workspace = workspace_ref(&selection);
    let run = application
        .start(&workspace, native_request(&selection))
        .unwrap();
    let terminal = settled(application);
    assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
    let fault = terminal.error.unwrap();
    assert_eq!(fault.category, "TargetConflict");
    assert_eq!(
        fault.context["cleanup"],
        json!({"clean":true,"child_started":false})
    );
    let poll = application.poll();
    assert_eq!(poll.controller["workspace_id"], workspace.workspace_id);
    assert!(
        application
            .read_target(&workspace)
            .unwrap()
            .record
            .binding
            .is_none()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn stopped_native_preparation_retains_ownership_until_store_capture_settles() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let package = fixture.package_at("native-package");
    declare_target(&package, Some("native-target"));
    let selection = inspect_named(application, "Main", &package).unwrap();
    let workspace = workspace_ref(&selection);
    let store = lock(&application.store);
    let starting = application.clone();
    let starting_workspace = workspace.clone();
    let native = native_request(&selection);
    let starter = std::thread::spawn(move || starting.start(&starting_workspace, native));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let run = loop {
        let poll = application.poll();
        if poll.controller["progress"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| event["stage"] == "input_capture"))
        {
            break poll.controller["run"].as_str().unwrap().to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native preparation did not reach capture"
        );
        std::thread::yield_now();
    };
    application.stop(&run).unwrap();
    assert_eq!(application.poll().controller["state"], "stopping");
    assert_eq!(
        application
            .check_environment(None, None)
            .unwrap_err()
            .category,
        "RunActive"
    );
    assert_eq!(
        application
            .start(&workspace, request(&selection))
            .unwrap_err()
            .category,
        "RunActive"
    );
    drop(store);
    assert_eq!(starter.join().unwrap().unwrap(), run);
    let terminal = settled(application);
    let fault = terminal.error.unwrap();
    assert_eq!(fault.category, "Cancelled");
    assert_eq!(
        fault.context["cleanup"],
        json!({"clean":true,"child_started":false})
    );
    let next = application
        .start(&workspace, native_request(&selection))
        .unwrap();
    assert_ne!(next, run);
    assert_eq!(
        settled(application).error.unwrap().category,
        "TargetConflict"
    );
}

#[test]
fn another_non_native_operation_cannot_clear_required_native_reconciliation() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let selection = inspect_named(application, "Main", &package_path()).unwrap();
    let workspace = workspace_ref(&selection);
    lock(&application.workspaces).native_cleanup_required = true;
    let mut native = request(&selection);
    native.lane = "native".into();
    assert_eq!(
        application
            .start(&workspace, native.clone())
            .unwrap_err()
            .category,
        "NativeCleanupRequired"
    );
    application.start(&workspace, request(&selection)).unwrap();
    assert_eq!(settled(application).error.unwrap().category, "ChildStartup");
    assert_eq!(
        application.start(&workspace, native).unwrap_err().category,
        "NativeCleanupRequired"
    );
}

#[test]
fn reconstruction_cannot_discard_required_native_reconciliation() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    lock(&application.workspaces).native_cleanup_required = true;
    assert_eq!(
        application.prepare_reconstruction().unwrap_err().category,
        "NativeCleanupRequired"
    );
    assert!(!application.closing.load(Ordering::Acquire));
}
