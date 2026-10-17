use super::*;
use crate::application::test_support::{Fixture, inspect_named, request, view_ref, workspace_ref};
#[cfg(unix)]
use crate::application::test_support::{declare_target, target_configuration, target_expectation};
use crate::application::{AuthoringView, Poll};
use mado_runtime_comparison::desktop::{FunctionInvocation, NativeIntent, native_limits};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Trial {
    fixture: Fixture,
    package: PathBuf,
    workspace: crate::application::WorkspaceRef,
    editor: AuthoringView,
}

impl Trial {
    /// An Edit session over a package the controlled runner cannot launch: admission,
    /// saved-source capture and function preflight are real, the child start is not.
    fn new() -> Self {
        let fixture = Fixture::new();
        let package = fixture.package_at("sources/trial");
        let app = &fixture.application;
        let selection = inspect_named(app, "owner", &package).unwrap();
        let workspace = workspace_ref(&selection);
        let editor = app.authoring_open(&workspace, &package).unwrap();
        Self {
            fixture,
            package,
            workspace,
            editor,
        }
    }

    fn app(&self) -> &Arc<Application> {
        &self.fixture.application
    }

    fn context(&self) -> TrialContext {
        self.app()
            .authoring_trial_context(&self.editor.owner, &self.editor.revision)
            .unwrap()
    }
}

fn decide(values: &Value) -> FunctionInvocation {
    FunctionInvocation {
        module: "decisions.js".into(),
        function: "decide".into(),
        arguments: vec![values.clone(), Value::Null, Value::Null],
    }
}

fn trial_request(context: &TrialContext, function: Option<FunctionInvocation>) -> StartRequest {
    StartRequest {
        package_path: String::new(),
        inventory_identity: context.package.inventory_identity.clone(),
        package_id: context.package.package_id.clone(),
        schema_identity: context.package.schema_identity.clone(),
        profile_id: "draft".into(),
        values: context.package.profiles["template-first"]["options"].clone(),
        lane: "controlled".into(),
        scenario: "workflow".into(),
        replay_descriptor_path: None,
        native_intent: None,
        function,
    }
}

fn script_intent() -> NativeIntent {
    NativeIntent {
        target_revision: 1,
        target_binding_id: "saved-target".into(),
        target_declaration_identity: "saved-declaration".into(),
        capture_approved: true,
        input_approved: true,
        launch_approved: false,
        max_exit_recoveries: 0,
        review: NativeReview::Script {},
        limits: native_limits(),
    }
}

fn terminal(app: &Application, run: &str) -> Poll {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let poll = app.poll();
        if poll.controller["run"] == run && poll.controller["state"] == "terminal" {
            return poll;
        }
        assert!(Instant::now() < deadline, "trial did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_new_uninspected_package_runs_explicit_values_without_binding_or_profiles() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let workspace = app.create_workspace("new-owner", "New owner").unwrap();
    let workspace = view_ref(&workspace);
    let editor = app.authoring_create(&workspace, "fresh-trial").unwrap();
    let tab_path = fixture.root.join("tabs/new-owner/tab.config");
    let tab_before = fs::read(&tab_path).unwrap();
    let context = app
        .authoring_trial_context(&editor.owner, &editor.revision)
        .unwrap();
    assert!(context.profiles.is_empty());
    assert!(context.profiles_error.is_none());
    assert!(context.recovery.is_none());
    assert!(app.poll().controller["run"].is_null());
    assert!(
        app.workspace_catalog().unwrap().open[0].selection.is_none(),
        "trial setup must not inspect or bind an ordinary selection"
    );
    let mut request = StartRequest {
        package_path: String::new(),
        inventory_identity: context.package.inventory_identity.clone(),
        package_id: context.package.package_id.clone(),
        schema_identity: context.package.schema_identity.clone(),
        profile_id: "draft".into(),
        values: json!({}),
        lane: "controlled".into(),
        scenario: "workflow".into(),
        replay_descriptor_path: None,
        native_intent: None,
        function: None,
    };
    let run = app
        .authoring_trial_start(&editor.owner, &editor.revision, request.clone())
        .unwrap();
    let result = terminal(app, &run);
    assert_eq!(result.controller["error"]["category"], "ChildStartup");
    assert_eq!(
        result.controller["error"]["context"]["operation_stage"],
        "execution"
    );
    assert_eq!(result.authoring.as_ref(), Some(&editor.owner));

    request.values = json!([]);
    let run = app
        .authoring_trial_start(&editor.owner, &editor.revision, request.clone())
        .unwrap();
    let result = terminal(app, &run);
    assert_eq!(result.controller["error"]["category"], "Profile");
    request.values = json!({});
    request.profile_id = "not-owned".into();
    let run = app
        .authoring_trial_start(&editor.owner, &editor.revision, request)
        .unwrap();
    assert_eq!(
        terminal(app, &run).controller["error"]["category"],
        "ProfileIdentity"
    );
    assert_eq!(fs::read(tab_path).unwrap(), tab_before);
    let store = lock(&app.store);
    assert!(store.tab("new-owner").unwrap().packages.is_empty());
    assert_eq!(
        store
            .profile_store("new-owner", "fresh-trial")
            .err()
            .unwrap()
            .category,
        "ProfileIdentity"
    );
}

#[test]
fn an_unregistered_profile_directory_is_not_saved_profile_authority() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let package = fixture.package_at("sources/uninspected");
    let workspace = view_ref(&app.create_workspace("owner", "Owner").unwrap());
    let editor = app.authoring_open(&workspace, &package).unwrap();
    let residue = fixture.root.join("tabs/owner").join(&editor.package_id);
    fs::create_dir(&residue).unwrap();
    fs::write(residue.join("unowned.config"), b"not a profile").unwrap();
    let context = app
        .authoring_trial_context(&editor.owner, &editor.revision)
        .unwrap();
    assert!(context.profiles.is_empty());
    assert!(context.profiles_error.is_none());
    assert!(context.recovery.is_none());
    let run = app
        .authoring_trial_start(
            &editor.owner,
            &editor.revision,
            trial_request(&context, None),
        )
        .unwrap();
    assert_eq!(
        terminal(app, &run).controller["error"]["category"],
        "ChildStartup"
    );
    assert_eq!(
        fs::read(residue.join("unowned.config")).unwrap(),
        b"not a profile"
    );
    assert!(lock(&app.store).tab("owner").unwrap().packages.is_empty());
}

#[test]
fn registered_profile_corruption_stays_visible_without_reset_authority() {
    let trial = Trial::new();
    let directory = trial
        .fixture
        .root
        .join("tabs/owner")
        .join(&trial.editor.package_id);
    fs::create_dir_all(&directory).unwrap();
    let file = directory.join("broken.config");
    fs::write(&file, b"not a profile").unwrap();
    let context = trial.context();
    assert!(context.profiles.is_empty());
    assert!(context.profiles_error.is_some());
    assert!(context.recovery.as_ref().is_none_or(|recovery| {
        recovery.profiles.is_empty() && recovery.profiles_error.is_some()
    }));
    assert_eq!(fs::read(file).unwrap(), b"not a profile");
}

#[test]
fn a_function_trial_runs_under_the_edit_owner_and_keeps_the_lease() {
    let trial = Trial::new();
    let app = trial.app();
    let owner = &trial.editor.owner;
    let context = trial.context();
    assert_eq!(context.owner, *owner);
    assert_eq!(context.revision, trial.editor.revision);
    assert!(context.profiles_error.is_none());
    assert!(context.target_error.is_none());
    assert_eq!(
        context.target.as_ref().unwrap().context.workspace,
        owner.workspace
    );
    let decide_candidate = context
        .functions
        .iter()
        .find(|candidate| candidate.module == "decisions.js" && candidate.function == "decide")
        .expect("file-level decision function is selectable");
    assert_eq!(decide_candidate.required_arguments, 3);
    assert!(
        app.poll().controller["run"].is_null(),
        "discovery is non-executing"
    );

    let values = context.package.profiles["template-first"]["options"].clone();
    let run = app
        .authoring_trial_start(
            owner,
            &trial.editor.revision,
            trial_request(&context, Some(decide(&values))),
        )
        .unwrap();
    let poll = terminal(app, &run);
    assert_eq!(poll.controller["operation"], "authoring_trial");
    assert_eq!(
        poll.controller["workspace_id"],
        owner.workspace.workspace_id
    );
    assert_eq!(poll.authoring.as_ref(), Some(owner));
    let fault = &poll.controller["error"];
    assert_eq!(fault["category"], "ChildStartup");
    assert_eq!(fault["context"]["operation_stage"], "execution");
    let retained = poll
        .workspace_results
        .iter()
        .find(|result| result.workspace == owner.workspace)
        .unwrap();
    assert_eq!(retained.controller["run"], run);

    // Ordinary Start cannot carry a function, inside or outside Edit.
    let mut ordinary = trial_request(&context, Some(decide(&values)));
    ordinary.package_path = trial.package.to_string_lossy().into_owned();
    assert_eq!(
        app.start(&trial.workspace, ordinary.clone())
            .unwrap_err()
            .category,
        "Authority"
    );
    assert!(!app.authoring_stop(owner).unwrap());
    assert_eq!(
        app.authoring_refresh(owner).unwrap().revision,
        trial.editor.revision
    );
    let exited = app.authoring_exit(owner).unwrap();
    let selection = app
        .inspect(&trial.package, &view_ref(&exited))
        .unwrap()
        .workspace
        .selection
        .unwrap();
    assert_eq!(
        app.start(&workspace_ref(&selection), ordinary)
            .unwrap_err()
            .category,
        "Authority"
    );
}

#[test]
fn trial_admission_is_owner_bound_exclusive_and_stoppable_without_locks() {
    let trial = Trial::new();
    let app = trial.app();
    let owner = &trial.editor.owner;
    let revision = &trial.editor.revision;
    let context = trial.context();
    let mut forged = owner.clone();
    forged.token.push_str("-forged");
    assert_eq!(
        app.authoring_trial_start(&forged, revision, trial_request(&context, None))
            .unwrap_err()
            .category,
        "StaleAuthoring"
    );
    assert_eq!(
        app.authoring_trial_context(&forged, revision)
            .unwrap_err()
            .category,
        "StaleAuthoring"
    );
    assert_eq!(
        app.authoring_trial_start(owner, "stale-revision", trial_request(&context, None))
            .unwrap_err()
            .category,
        "AuthoringConflict"
    );
    // This poll also settles the owner's saved-target check before Store is held.
    assert!(app.poll().controller["run"].is_null());

    let store = lock(&app.store);
    let starting = app.clone();
    let starting_owner = owner.clone();
    let starting_revision = revision.clone();
    let starting_request = trial_request(&context, None);
    let starter = std::thread::spawn(move || {
        starting.authoring_trial_start(&starting_owner, &starting_revision, starting_request)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let run = loop {
        let poll = app.poll();
        if let Some(run) = poll.controller["run"].as_str() {
            break run.to_owned();
        }
        assert!(Instant::now() < deadline, "trial was not reserved");
        std::thread::yield_now();
    };
    assert_eq!(
        app.authoring_trial_start(owner, revision, trial_request(&context, None))
            .unwrap_err()
            .category,
        "RunActive"
    );
    assert_eq!(
        app.authoring_trial_context(owner, revision)
            .unwrap_err()
            .category,
        "RunActive"
    );
    assert_eq!(
        app.authoring_validate(owner, revision)
            .unwrap_err()
            .category,
        "RunActive"
    );
    assert_eq!(
        app.authoring_save(owner, revision, "main.js", "changed".into())
            .unwrap_err()
            .category,
        "RunActive"
    );
    assert_eq!(app.authoring_exit(owner).unwrap_err().category, "RunActive");
    assert_eq!(
        app.start(&trial.workspace, trial_request(&context, None))
            .unwrap_err()
            .category,
        "AuthoringActive"
    );
    {
        let _workspaces = lock(&app.workspaces);
        assert!(app.authoring_stop(owner).unwrap());
    }
    drop(store);
    assert_eq!(starter.join().unwrap().unwrap(), run);
    let poll = terminal(app, &run);
    assert_eq!(poll.controller["error"]["category"], "Cancelled");
    assert_eq!(
        poll.controller["error"]["context"]["cleanup"],
        json!({"clean":true,"child_started":false})
    );
    assert_eq!(poll.authoring.as_ref(), Some(owner));
    assert!(!app.authoring_stop(owner).unwrap());

    // An explicit rerun after settlement is a fresh invocation, never a queued one.
    let rerun = app
        .authoring_trial_start(owner, revision, trial_request(&context, None))
        .unwrap();
    assert_ne!(rerun, run);
    assert_eq!(
        terminal(app, &rerun).controller["error"]["category"],
        "ChildStartup"
    );
}

#[test]
fn trials_refuse_changed_saved_source_stale_setup_and_unknown_profiles() {
    let trial = Trial::new();
    let app = trial.app();
    let owner = &trial.editor.owner;
    let context = trial.context();
    let decisions = trial.package.join("decisions.js");
    let mut changed = fs::read_to_string(&decisions).unwrap();
    changed.push_str("\nexport function later() { return 1; }\n");
    fs::write(&decisions, &changed).unwrap();

    let run = app
        .authoring_trial_start(owner, &trial.editor.revision, trial_request(&context, None))
        .unwrap();
    let poll = terminal(app, &run);
    assert_eq!(poll.controller["error"]["category"], "AuthoringConflict");
    assert_eq!(poll.authoring.as_ref(), Some(owner));
    assert_eq!(fs::read_to_string(&decisions).unwrap(), changed);

    let refreshed = app.authoring_refresh(owner).unwrap();
    assert_ne!(refreshed.revision, trial.editor.revision);
    let run = app
        .authoring_trial_start(owner, &refreshed.revision, trial_request(&context, None))
        .unwrap();
    assert_eq!(
        terminal(app, &run).controller["error"]["category"],
        "StaleIdentity"
    );

    let current = app
        .authoring_trial_context(owner, &refreshed.revision)
        .unwrap();
    assert!(
        current.functions.iter().any(|candidate| {
            candidate.module == "decisions.js" && candidate.function == "later"
        })
    );
    let mut unknown = trial_request(&current, None);
    unknown.profile_id = "missing-profile".into();
    let run = app
        .authoring_trial_start(owner, &refreshed.revision, unknown)
        .unwrap();
    assert_eq!(
        terminal(app, &run).controller["error"]["category"],
        "ProfileNotFound"
    );
}

#[test]
fn native_trials_require_script_review_and_both_permissions_before_reservation() {
    let trial = Trial::new();
    let app = trial.app();
    let context = trial.context();
    for function in [None, Some(decide(&json!({})))] {
        let mut native = trial_request(&context, function);
        native.lane = "native".into();
        let mut operation = script_intent();
        operation.review = NativeReview::Operation {
            operation: "Click the selected button once".into(),
            visible_postcondition: "The selected label changes".into(),
        };
        let mut no_capture = script_intent();
        no_capture.capture_approved = false;
        let mut no_input = script_intent();
        no_input.input_approved = false;
        for intent in [None, Some(operation), Some(no_capture), Some(no_input)] {
            native.native_intent = intent;
            assert_eq!(
                app.authoring_trial_start(
                    &trial.editor.owner,
                    &trial.editor.revision,
                    native.clone(),
                )
                .unwrap_err()
                .category,
                "NativeRefused"
            );
            assert!(app.poll().controller["run"].is_null());
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn script_review_cannot_authorize_changed_saved_source_or_another_owner() {
    let trial = Trial::new();
    let app = trial.app();
    let context = trial.context();
    let mut native = trial_request(&context, None);
    native.lane = "native".into();
    native.native_intent = Some(script_intent());
    let mut foreign = trial.editor.owner.clone();
    foreign.token.push_str("-foreign");
    assert_eq!(
        app.authoring_trial_start(&foreign, &trial.editor.revision, native.clone())
            .unwrap_err()
            .category,
        "StaleAuthoring"
    );
    assert_eq!(
        app.authoring_trial_start(&trial.editor.owner, "old-revision", native.clone())
            .unwrap_err()
            .category,
        "AuthoringConflict"
    );
    let decisions = trial.package.join("decisions.js");
    let mut changed = fs::read_to_string(&decisions).unwrap();
    changed.push_str("\nexport function changedAfterReview() { return 1; }\n");
    fs::write(&decisions, changed).unwrap();
    let run = app
        .authoring_trial_start(&trial.editor.owner, &trial.editor.revision, native)
        .unwrap();
    let poll = terminal(app, &run);
    assert_eq!(poll.controller["error"]["category"], "AuthoringConflict");
    assert_eq!(
        poll.controller["error"]["context"]["cleanup"],
        json!({"clean":true,"child_started":false})
    );
    assert_eq!(poll.authoring.as_ref(), Some(&trial.editor.owner));
}

#[test]
fn native_trials_and_ordinary_native_runs_share_the_reconciliation_gate() {
    let trial = Trial::new();
    let app = trial.app();
    let owner = &trial.editor.owner;
    let context = trial.context();
    lock(&app.workspaces).native_cleanup_required = true;
    let mut native = trial_request(&context, None);
    native.lane = "native".into();
    assert_eq!(
        app.authoring_trial_start(owner, &trial.editor.revision, native)
            .unwrap_err()
            .category,
        "NativeCleanupRequired"
    );
    assert!(app.poll().controller["run"].is_null());
    let exited = app.authoring_exit(owner).unwrap();
    let mut ordinary = request(
        &app.inspect(&trial.package, &view_ref(&exited))
            .unwrap()
            .workspace
            .selection
            .unwrap(),
    );
    ordinary.lane = "native".into();
    assert_eq!(
        app.start(&view_ref(&exited), ordinary)
            .unwrap_err()
            .category,
        "NativeCleanupRequired"
    );
}

#[test]
fn trial_containment_distinguishes_child_ownership_and_native_cleanup() {
    let reaped = json!({"result":{"cleanup":{"clean":false},"primary":null,
        "observations":{"child_reaped":true}},"error":null});
    assert!(containment(&reaped, false).is_none());
    let trial = Trial::new();
    lock(&trial.app().workspaces)
        .authoring
        .as_mut()
        .unwrap()
        .settle_trial(&reaped, true);
    assert_eq!(
        trial
            .app()
            .authoring_trial_context(&trial.editor.owner, &trial.editor.revision)
            .unwrap_err()
            .category,
        "TrialCleanup"
    );
    let unreaped = json!({"result":{"cleanup":{"clean":false},"primary":null,
        "observations":{}},"error":null});
    assert_eq!(
        containment(&unreaped, false).unwrap().category,
        "TrialCleanup"
    );
    let contained = json!({"result":{"cleanup":{"clean":true},
        "primary":{"category":"Containment"},"observations":{"child_reaped":true}},"error":null});
    assert!(containment(&contained, false).is_some());
    let before_child = json!({"result":null,"error":{"category":"Cancelled",
        "context":{"cleanup":{"clean":true,"child_started":false}}}});
    assert!(containment(&before_child, false).is_none());
    let panicked = json!({"result":null,"error":{"category":"Controller",
        "context":{"cleanup":{"clean":false,"child_started":null}}}});
    assert!(containment(&panicked, false).is_some());
}

#[cfg(unix)]
#[test]
fn edit_owner_configures_the_same_machine_local_target_without_ordinary_admission() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let package = fixture.package_at("sources/target");
    declare_target(&package, Some("metadata-fixture"));
    let selection = inspect_named(app, "owner", &package).unwrap();
    let workspace = workspace_ref(&selection);
    let editor = app.authoring_open(&workspace, &package).unwrap();
    let owner = &editor.owner;
    let initial = app.authoring_read_target(owner, &editor.revision).unwrap();
    assert_eq!(initial.context.workspace, workspace);
    assert_eq!(
        initial.context.declaration_identity,
        selection.package.target_identity
    );
    assert_eq!(
        app.save_target(
            &workspace,
            &target_expectation(&initial),
            target_configuration(),
            None
        )
        .unwrap_err()
        .category,
        "AuthoringActive"
    );
    assert_eq!(
        app.authoring_save_target(
            owner,
            "stale-revision",
            &target_expectation(&initial),
            target_configuration(),
            None,
        )
        .unwrap_err()
        .category,
        "AuthoringConflict"
    );
    let saved = app
        .authoring_save_target(
            owner,
            &editor.revision,
            &target_expectation(&initial),
            target_configuration(),
            None,
        )
        .unwrap();
    assert!(saved.view.compatible);
    let checked = app
        .authoring_check_target(
            owner,
            &editor.revision,
            &target_expectation(&saved.view),
            &target_configuration(),
        )
        .unwrap();
    assert_eq!(checked.context.workspace, workspace);
    let exited = app.authoring_exit(owner).unwrap();
    let reinspected = app
        .inspect(&package, &view_ref(&exited))
        .unwrap()
        .workspace
        .selection
        .unwrap();
    assert_eq!(
        app.read_target(&workspace_ref(&reinspected))
            .unwrap()
            .record,
        saved.view.record
    );
}
