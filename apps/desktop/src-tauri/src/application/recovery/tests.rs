use super::*;
use crate::application::test_support::*;
use crate::application::{RecoveryStatus, Selection};
use crate::storage::{MAX_PROFILE_BYTES, Profile};
use std::fs;
use std::path::PathBuf;

fn schema(maximum: u64) -> Value {
    json!({
        "version":1,"type":"object","additionalProperties":false,"required":["count"],
        "properties":{
            "count":{"type":"integer","minimum":0,"maximum":maximum,"default":1},
            "order":{"type":"array","items":{"type":"string"},"default":["first"]},
            "optional":{"type":"string"}
        }
    })
}

fn install_schema(path: &Path, schema: &Value, preset_values: Value) {
    fs::write(
        path.join("schema.json"),
        serde_json::to_vec(schema).unwrap(),
    )
    .unwrap();
    for name in ["template-first", "ocr-first"] {
        let file = path.join("profiles").join(format!("{name}.json"));
        let mut preset: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        preset["options"] = preset_values.clone();
        fs::write(file, serde_json::to_vec(&preset).unwrap()).unwrap();
    }
}

fn foreign_package(fixture: &Fixture, name: &str, package_id: &str) -> PathBuf {
    let path = fixture.package_at(name);
    for relative in [
        "package.json",
        "profiles/template-first.json",
        "profiles/ocr-first.json",
    ] {
        let file = path.join(relative);
        let mut document: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        document["package_id"] = json!(package_id);
        fs::write(file, serde_json::to_vec(&document).unwrap()).unwrap();
    }
    path
}

fn initial(fixture: &Fixture) -> (PathBuf, Selection) {
    let path = fixture.package_at("source");
    install_schema(&path, &schema(10), json!({}));
    let selection = inspect_named(&fixture.application, "Main", &path).unwrap();
    (path, selection)
}

fn save(fixture: &Fixture, selection: &Selection, name: &str, values: Value) -> Profile {
    fixture
        .application
        .save_profile(&workspace_ref(selection), None, name, values)
        .unwrap()
}

fn recovery(fixture: &Fixture, path: &Path, selection: &Selection) -> RecoveryRef {
    fixture
        .application
        .inspect(path, &workspace_ref(selection))
        .unwrap()
        .workspace
        .recovery
        .unwrap()
        .context
}

/// Drains the command records attributed to one workspace since the previous drain.
fn command_records(application: &Application, workspace_id: &str) -> Vec<(String, Value, Value)> {
    application
        .poll()
        .logs
        .entries
        .into_iter()
        .filter(|entry| {
            entry.workspace_id.as_deref() == Some(workspace_id)
                && matches!(
                    entry.code.as_str(),
                    "command.failed" | "workspace.reinspected" | "profile.saved"
                )
        })
        .map(|entry| {
            (
                entry.code,
                entry.fields["action"].clone(),
                entry.fields["category"].clone(),
            )
        })
        .collect()
}

#[test]
fn passive_restore_catalog_validation_and_start_preserve_bytes_until_explicit_inspection() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (path, selected) = initial(&fixture);
    let saved = save(
        &fixture,
        &selected,
        "Ordered",
        json!({"count":2,"order":["second","first"]}),
    );
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    let mut updated = schema(9);
    updated["properties"]["label"] = json!({"type":"string","default":"new"});
    install_schema(&path, &updated, json!({}));
    application
        .close_workspace(&workspace_ref(&selected))
        .unwrap();
    let reopened = application.reopen_workspace("Main").unwrap();
    assert!(reopened.recovery.is_none());
    let current = view_ref(&reopened);
    assert_eq!(
        application
            .profiles(&current)
            .unwrap()
            .profiles_error
            .unwrap()
            .category,
        "ProfileRejected"
    );
    assert_eq!(
        application.validate(&current, json!({"count":2})).unwrap()["label"],
        "new"
    );
    let mut start = request(reopened.selection.as_ref().unwrap());
    start.profile_id = saved.id.clone();
    start.values = saved.values.clone();
    application.start(&current, start).unwrap();
    assert_eq!(
        settled(application).error.unwrap().category,
        "ProfileNotFound"
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    let outcome = application.inspect(&path, &current).unwrap();
    assert_eq!(outcome.kind, InspectionKind::Bound);
    assert_eq!(outcome.outcomes[0].status, RecoveryStatus::Saved);
    assert!(outcome.workspace.recovery.is_none());
    let selected = outcome.workspace.selection.unwrap();
    let recovered = &selected.profiles[0];
    assert_eq!(recovered.id, saved.id);
    assert_eq!(recovered.name, saved.name);
    assert_eq!(recovered.version, saved.version);
    assert_eq!(recovered.package_id, saved.package_id);
    assert_eq!(
        recovered.values,
        json!({"count":2,"order":["second","first"],"label":"new"})
    );
    assert_ne!(recovered.schema_identity, saved.schema_identity);
    let after = fs::read(&file).unwrap();
    let repeated = application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert!(repeated.outcomes.is_empty());
    assert_eq!(fs::read(&file).unwrap(), after);
    updated["properties"]["count"]["maximum"] = json!(8);
    install_schema(&path, &updated, json!({}));
    let schema_only = application
        .inspect(&path, &view_ref(&repeated.workspace))
        .unwrap();
    assert_eq!(schema_only.outcomes[0].status, RecoveryStatus::Saved);
    let unchanged = &schema_only.workspace.selection.as_ref().unwrap().profiles[0];
    assert_eq!(unchanged.values, recovered.values);
    assert_eq!(unchanged.id, recovered.id);
    assert_eq!(unchanged.name, recovered.name);
}

#[test]
fn compatible_and_rejected_profiles_coexist_and_repair_changes_only_its_owner() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (path, selected) = initial(&fixture);
    let good = save(&fixture, &selected, "Compatible", json!({"count":2}));
    let bad = save(&fixture, &selected, "Needs repair", json!({"count":9}));
    let other = inspect_named(application, "Other", &path).unwrap();
    let foreign = application
        .save_profile(&workspace_ref(&other), None, "Foreign", json!({"count":8}))
        .unwrap();
    let other_file = profile_path(&fixture, "Other", &foreign);
    let other_bytes = fs::read(&other_file).unwrap();
    let settings = fs::read(fixture.root.join("settings.json")).unwrap();
    let tab = fs::read(fixture.root.join("tabs/Main/tab.config")).unwrap();
    let target_file = profile_path(&fixture, "Main", &good).with_file_name("target.config");
    fs::write(
        &target_file,
        serde_json::to_vec(
            &application
                .read_target(&workspace_ref(&selected))
                .unwrap()
                .record,
        )
        .unwrap(),
    )
    .unwrap();
    let target_bytes = fs::read(&target_file).unwrap();
    let bad_file = profile_path(&fixture, "Main", &bad);
    let bad_bytes = fs::read(&bad_file).unwrap();
    install_schema(&path, &schema(5), json!({}));
    let outcome = application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::Bound);
    let view = outcome.workspace;
    let reference = view_ref(&view);
    let context = view.recovery.unwrap().context;
    assert_eq!(view.selection.unwrap().profiles[0].id, good.id);
    assert_eq!(
        outcome
            .outcomes
            .iter()
            .find(|outcome| outcome.profile_id == good.id)
            .unwrap()
            .status,
        RecoveryStatus::Saved
    );
    assert_eq!(
        outcome
            .outcomes
            .iter()
            .find(|outcome| outcome.profile_id == bad.id)
            .unwrap()
            .status,
        RecoveryStatus::RepairRequired
    );
    assert_eq!(
        application
            .validate(&reference, json!({"count":3}))
            .unwrap()["count"],
        3
    );
    assert!(
        application
            .save_profile(&reference, Some(&bad.id), "Ordinary", json!({"count":3}))
            .is_err()
    );
    assert!(
        application
            .rename_profile(&reference, &bad.id, "Ordinary")
            .is_err()
    );
    assert!(application.delete_profile(&reference, &bad.id).is_err());
    assert_eq!(fs::read(&bad_file).unwrap(), bad_bytes);
    assert_eq!(
        application
            .repair_profile(&context, &foreign.id, json!({"count":3}))
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    let repaired = application
        .repair_profile(&context, &bad.id, json!({"count":4}))
        .unwrap();
    assert_eq!(repaired.saved.as_ref().unwrap().id, bad.id);
    assert_eq!(repaired.saved.unwrap().name, bad.name);
    assert!(repaired.recovery.unwrap().profiles.is_empty());
    let closed = application.discard_recovery(&context).unwrap();
    assert!(closed.recovery.is_none());
    assert_eq!(view_ref(&closed), reference);
    assert!(closed.selection.is_some());
    assert_eq!(repaired.catalog.unwrap().profiles.len(), 2);
    assert_eq!(fs::read(other_file).unwrap(), other_bytes);
    assert_eq!(
        fs::read(fixture.root.join("settings.json")).unwrap(),
        settings
    );
    assert_eq!(
        fs::read(fixture.root.join("tabs/Main/tab.config")).unwrap(),
        tab
    );
    assert_eq!(fs::read(target_file).unwrap(), target_bytes);
}

#[test]
fn incomplete_reset_returns_declared_defaults_without_writing_and_can_be_completed() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Required input", json!({"count":2}));
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    let mut updated = schema(5);
    updated["properties"]["label"] = json!({"type":"string","minLength":1});
    updated["required"] = json!(["count", "label"]);
    install_schema(&path, &updated, json!({"label":"preset"}));
    let context = recovery(&fixture, &path, &selected);
    assert_eq!(
        fixture
            .application
            .reset_profile(&context, &saved.id, false)
            .unwrap_err()
            .category,
        "ConfirmationRequired"
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    let reset = fixture
        .application
        .reset_profile(&context, &saved.id, true)
        .unwrap();
    assert!(reset.saved.is_none());
    assert_eq!(reset.issue.unwrap().context["path"], "$.label");
    let mut draft = reset.draft.unwrap();
    assert_eq!(draft, json!({"count":1,"order":["first"]}));
    assert_eq!(fs::read(&file).unwrap(), before);
    draft["label"] = json!("completed");
    let repaired = fixture
        .application
        .repair_profile(&context, &saved.id, draft)
        .unwrap()
        .saved
        .unwrap();
    assert_eq!(repaired.id, saved.id);
    assert_eq!(repaired.name, saved.name);
    assert_eq!(repaired.values["label"], "completed");
}

#[test]
fn confirmed_reset_replaces_only_values_and_preserves_other_profile_bytes() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let first = save(&fixture, &selected, "Reset me", json!({"count":9}));
    let second = save(&fixture, &selected, "Leave me", json!({"count":8}));
    let second_file = profile_path(&fixture, "Main", &second);
    let before = fs::read(&second_file).unwrap();
    install_schema(&path, &schema(5), json!({}));
    let context = recovery(&fixture, &path, &selected);
    let reset = fixture
        .application
        .reset_profile(&context, &first.id, true)
        .unwrap()
        .saved
        .unwrap();
    assert_eq!(reset.values, json!({"count":1,"order":["first"]}));
    assert_eq!(
        (reset.id, reset.name, reset.version, reset.package_id),
        (first.id, first.name, first.version, first.package_id)
    );
    assert_eq!(fs::read(second_file).unwrap(), before);
}

#[test]
fn missing_source_relocation_requires_repair_then_explicit_durable_retry() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (old, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Relocate", json!({"count":9}));
    let tab_file = fixture.root.join("tabs/Main/tab.config");
    let original_tab = fs::read(&tab_file).unwrap();
    let candidate = fixture.root.join("relocated");
    fs::rename(&old, &candidate).unwrap();
    install_schema(&candidate, &schema(5), json!({}));
    let outcome = application
        .inspect(&candidate, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::RecoveryRequired);
    assert!(outcome.workspace.selection.is_none());
    assert_eq!(
        outcome.workspace.saved_package.as_ref().unwrap().source,
        PackageSource::Directory {
            path: selected.package_path.clone()
        }
    );
    let context = outcome.workspace.recovery.unwrap().context;
    let reference = &context.workspace;
    assert_eq!(
        application
            .start(reference, request(&selected))
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .check_environment(Some(reference), None)
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .validate(reference, json!({"count":1}))
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application.profiles(reference).unwrap_err().category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .save_profile(reference, None, "Forged", json!({"count":1}))
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .rename_profile(reference, &saved.id, "Forged")
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .delete_profile(reference, &saved.id)
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .import_legacy_profiles(reference)
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application.retry_binding(&context).unwrap().kind,
        InspectionKind::RecoveryRequired
    );
    let repaired = application
        .repair_profile(&context, &saved.id, json!({"count":3}))
        .unwrap();
    assert_eq!(repaired.saved.unwrap().id, saved.id);
    assert_eq!(repaired.recovery.unwrap().context, context);
    assert_eq!(fs::read(&tab_file).unwrap(), original_tab);
    assert!(
        application.workspace_catalog().unwrap().open[0]
            .selection
            .is_none()
    );
    let saved_bytes = fs::read(profile_path(&fixture, "Main", &saved)).unwrap();
    let pending = tab_file.with_extension("pending");
    fs::write(&pending, b"interrupted binding").unwrap();
    let failed = application.retry_binding(&context).unwrap();
    assert_eq!(failed.kind, InspectionKind::BindingFailed);
    assert_eq!(failed.outcomes[0].status, RecoveryStatus::Saved);
    assert!(failed.workspace.selection.is_none());
    assert_eq!(fs::read(&tab_file).unwrap(), original_tab);
    assert_eq!(
        fs::read(profile_path(&fixture, "Main", &saved)).unwrap(),
        saved_bytes
    );
    fs::remove_file(pending).unwrap();
    let rebound = application.retry_binding(&context).unwrap();
    assert_eq!(rebound.kind, InspectionKind::Bound);
    assert_eq!(
        rebound.workspace.selection.unwrap().profiles[0].id,
        saved.id
    );
    assert_eq!(
        rebound.workspace.saved_package.unwrap().source,
        PackageSource::Directory {
            path: candidate
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        }
    );
    assert_eq!(
        application.retry_binding(&context).unwrap_err().category,
        "StaleIdentity"
    );
}

#[test]
fn original_fingerprint_is_not_refreshed_by_passive_catalog_and_missing_records_are_not_recreated()
{
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Expected bytes", json!({"count":9}));
    let file = profile_path(&fixture, "Main", &saved);
    install_schema(&path, &schema(5), json!({}));
    let context = recovery(&fixture, &path, &selected);
    let mut newer = fs::read(&file).unwrap();
    newer.push(b'\n');
    fs::write(&file, &newer).unwrap();
    fixture.application.workspace_catalog().unwrap();
    fixture.application.profiles(&context.workspace).unwrap();
    let repaired = fixture
        .application
        .repair_profile(&context, &saved.id, json!({"count":2}))
        .unwrap();
    assert!(repaired.saved.is_none());
    assert_eq!(repaired.issue.unwrap().category, "ProfileConflict");
    let reset = fixture
        .application
        .reset_profile(&context, &saved.id, true)
        .unwrap();
    assert_eq!(reset.issue.unwrap().category, "ProfileConflict");
    assert_eq!(fs::read(&file).unwrap(), newer);
    fs::remove_file(&file).unwrap();
    let missing = fixture
        .application
        .repair_profile(&context, &saved.id, json!({"count":2}))
        .unwrap();
    assert!(missing.saved.is_none());
    assert_eq!(missing.issue.unwrap().category, "ProfileNotFound");
    assert!(!file.exists());
}

#[test]
fn discarded_superseded_foreign_and_closed_contexts_cannot_mutate() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Owner", json!({"count":9}));
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    install_schema(&path, &schema(5), json!({}));
    let first = recovery(&fixture, &path, &selected);
    let other = inspect_named(application, "Other", &path).unwrap();
    let forged = RecoveryRef {
        workspace: workspace_ref(&other),
        token: first.token.clone(),
    };
    assert_eq!(
        application
            .repair_profile(&forged, &saved.id, json!({"count":1}))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    let second = application
        .inspect(&path, &first.workspace)
        .unwrap()
        .workspace
        .recovery
        .unwrap()
        .context;
    assert_eq!(
        application
            .reset_profile(&first, &saved.id, true)
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    let discarded = application.discard_recovery(&second).unwrap();
    assert!(discarded.selection.is_some());
    assert_eq!(view_ref(&discarded), second.workspace);
    assert_eq!(
        application.retry_binding(&second).unwrap_err().category,
        "StaleIdentity"
    );
    let third = application
        .inspect(&path, &view_ref(&discarded))
        .unwrap()
        .workspace
        .recovery
        .unwrap()
        .context;
    application.close_workspace(&third.workspace).unwrap();
    application.reopen_workspace("Main").unwrap();
    assert_eq!(
        application
            .repair_profile(&third, &saved.id, json!({"count":1}))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(fs::read(file).unwrap(), before);
}

#[test]
fn changed_candidate_inventory_refuses_repair_reset_and_retry() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Captured", json!({"count":9}));
    install_schema(&path, &schema(5), json!({}));
    let context = recovery(&fixture, &path, &selected);
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    install_schema(&path, &schema(4), json!({}));
    assert_eq!(
        fixture
            .application
            .repair_profile(&context, &saved.id, json!({"count":1}))
            .unwrap_err()
            .category,
        "InventoryChanged"
    );
    assert_eq!(
        fixture
            .application
            .reset_profile(&context, &saved.id, true)
            .unwrap_err()
            .category,
        "InventoryChanged"
    );
    assert_eq!(
        fixture
            .application
            .retry_binding(&context)
            .unwrap_err()
            .category,
        "InventoryChanged"
    );
    assert_eq!(fs::read(file).unwrap(), before);
}

#[test]
fn saved_auto_reconciliation_survives_durable_binding_failure_and_discard() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (_, selected) = initial(&fixture);
    let saved = save(
        &fixture,
        &selected,
        "Saved before binding",
        json!({"count":2}),
    );
    // DEL is a valid filesystem character on Windows too, but not a persisted source character.
    let candidate = fixture.package_at("invalid\u{7f}source-reference");
    install_schema(&candidate, &schema(5), json!({}));
    let tab = fixture.root.join("tabs/Main/tab.config");
    let original_tab = fs::read(&tab).unwrap();
    command_records(application, &selected.workspace_id);
    let outcome = application
        .inspect(&candidate, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::BindingFailed);
    assert_eq!(outcome.binding_error.unwrap().category, "PackageSource");
    assert_eq!(outcome.outcomes[0].status, RecoveryStatus::Saved);
    assert_eq!(fs::read(&tab).unwrap(), original_tab);
    // The candidate gains nothing: the prior selection is reissued under the new
    // revision, and the saved reference keeps its own (absent) source fault.
    let view = outcome.workspace;
    let retained = view
        .selection
        .as_ref()
        .expect("prior selection survives the failed binding");
    assert_eq!(retained.package_path, selected.package_path);
    assert_eq!(retained.revision, selected.revision + 1);
    assert_eq!(
        view.saved_package.as_ref().unwrap().source,
        PackageSource::Directory {
            path: selected.package_path.clone()
        }
    );
    assert!(view.source_error.is_none());
    let recovery = view.recovery.as_ref().unwrap();
    assert!(recovery.binding_required);
    assert!(recovery.relocation);
    assert_eq!(
        recovery.package_path.as_str(),
        candidate.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert!(recovery.profiles.is_empty());
    let context = recovery.context.clone();
    let current = view_ref(&view);
    // Commands resolve the retained selection, never the candidate: 7 is valid only
    // under the prior schema's maximum.
    assert_eq!(
        application
            .validate(&workspace_ref(&selected), json!({"count":7}))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(
        application.validate(&current, json!({"count":7})).unwrap()["count"],
        7
    );
    // The migrated profile is a committed fact the retained selection reports as rejected.
    assert!(retained.profiles.is_empty());
    assert_eq!(
        retained.profiles_error.as_ref().unwrap().category,
        "ProfileRejected"
    );
    let file = profile_path(&fixture, "Main", &saved);
    let committed = fs::read(&file).unwrap();
    let recovered: Profile = serde_json::from_slice(&committed).unwrap();
    assert_eq!(recovered.id, saved.id);
    assert_ne!(recovered.schema_identity, saved.schema_identity);
    let retried = application.retry_binding(&context).unwrap();
    assert_eq!(retried.kind, InspectionKind::BindingFailed);
    assert_eq!(retried.outcomes[0].status, RecoveryStatus::Saved);
    assert_eq!(view_ref(&retried.workspace), current);
    assert_eq!(
        retried.workspace.selection.as_ref().unwrap().package_path,
        selected.package_path
    );
    assert!(retried.workspace.recovery.unwrap().binding_required);
    assert_eq!(
        command_records(application, &selected.workspace_id),
        [
            (
                "command.failed".to_owned(),
                json!("inspect"),
                json!("PackageSource")
            ),
            (
                "command.failed".to_owned(),
                json!("validate"),
                json!("StaleIdentity")
            ),
            (
                "command.failed".to_owned(),
                json!("retry_binding"),
                json!("PackageSource")
            ),
        ]
    );
    let discarded = application.discard_recovery(&context).unwrap();
    assert_eq!(view_ref(&discarded), current);
    assert_eq!(
        discarded.selection.as_ref().unwrap().package_path,
        selected.package_path
    );
    assert!(discarded.source_error.is_none());
    assert!(discarded.recovery.is_none());
    assert_eq!(
        application.retry_binding(&context).unwrap_err().category,
        "StaleIdentity"
    );
    assert_eq!(
        application.validate(&current, json!({"count":7})).unwrap()["count"],
        7
    );
    assert_eq!(fs::read(file).unwrap(), committed);
    assert_eq!(fs::read(tab).unwrap(), original_tab);
}

#[test]
fn failed_candidate_and_discard_preserve_the_saved_source_fault() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (path, selected) = initial(&fixture);
    application.close_workspace(&workspace_ref(&selected)).unwrap();
    let candidate = fixture.root.join("moved");
    fs::rename(path, &candidate).unwrap();
    let reopened = application.reopen_workspace("Main").unwrap();
    assert!(reopened.selection.is_none());
    let source_error = serde_json::to_value(reopened.source_error.as_ref().unwrap()).unwrap();
    let tab_file = fixture.root.join("tabs/Main/tab.config");
    let before = fs::read(&tab_file).unwrap();
    let pending = tab_file.with_extension("pending");
    fs::write(&pending, b"interrupted Tab write").unwrap();

    let failed = application.inspect(&candidate, &view_ref(&reopened)).unwrap();
    assert_eq!(failed.kind, InspectionKind::BindingFailed);
    assert!(failed.binding_error.is_some());
    assert!(failed.workspace.selection.is_none());
    assert_eq!(
        serde_json::to_value(failed.workspace.source_error.as_ref().unwrap()).unwrap(),
        source_error
    );
    let recovery = failed.workspace.recovery.unwrap();
    assert!(recovery.binding_required);
    assert!(recovery.relocation);
    let discarded = application.discard_recovery(&recovery.context).unwrap();
    assert!(discarded.selection.is_none());
    assert!(discarded.recovery.is_none());
    assert_eq!(
        discarded.saved_package.as_ref(),
        reopened.saved_package.as_ref()
    );
    assert_eq!(
        serde_json::to_value(discarded.source_error.as_ref().unwrap()).unwrap(),
        source_error
    );
    assert_eq!(fs::read(&tab_file).unwrap(), before);

    fs::remove_file(pending).unwrap();
    let bound = application.inspect(&candidate, &view_ref(&discarded)).unwrap();
    assert_eq!(bound.kind, InspectionKind::Bound);
    assert!(bound.workspace.selection.is_some());
    assert!(bound.workspace.source_error.is_none());
    assert!(bound.workspace.recovery.is_none());
}

#[test]
fn numeric_unsafe_and_structurally_invalid_originals_never_receive_reset_authority() {
    for damage in ["numeric", "foreign", "corrupt", "version", "pending"] {
        let fixture = Fixture::new();
        let (path, selected) = initial(&fixture);
        let saved = save(&fixture, &selected, "Unsafe", json!({"count":9}));
        let file = profile_path(&fixture, "Main", &saved);
        let mut raw = serde_json::to_value(&saved).unwrap();
        match damage {
            "numeric" => raw["values"]["count"] = json!(9_007_199_254_740_993u64),
            "foreign" => raw["package_id"] = json!("foreign-owner"),
            "version" => raw["version"] = json!(2),
            _ => {}
        }
        fs::write(
            &file,
            if damage == "corrupt" {
                b"broken profile".to_vec()
            } else {
                serde_json::to_vec(&raw).unwrap()
            },
        )
        .unwrap();
        if damage == "pending" {
            fs::write(file.with_extension("pending"), b"pending original").unwrap();
        }
        let before = fs::read(&file).unwrap();
        install_schema(&path, &schema(5), json!({}));
        let outcome = fixture
            .application
            .inspect(&path, &workspace_ref(&selected))
            .unwrap();
        let recovery = outcome.workspace.recovery.unwrap();
        assert!(recovery.profiles.is_empty(), "{damage}");
        assert!(recovery.profiles_error.is_some(), "{damage}");
        assert!(
            outcome.workspace.selection.unwrap().profiles.is_empty(),
            "{damage}"
        );
        assert_eq!(
            fixture
                .application
                .reset_profile(&recovery.context, &saved.id, true)
                .unwrap_err()
                .category,
            "ProfileNotFound"
        );
        if damage == "numeric" {
            assert_eq!(outcome.outcomes[0].status, RecoveryStatus::StorageFailed);
            assert_eq!(
                outcome.outcomes[0].issue.as_ref().unwrap().category,
                "NumericPrecision"
            );
        }
        assert_eq!(fs::read(file).unwrap(), before, "{damage}");
    }
}

#[test]
fn publication_budget_failure_does_not_roll_back_another_profile() {
    let fixture = Fixture::new();
    let path = fixture.package_at("source");
    let mut original_schema = schema(10);
    original_schema["properties"]["blob"] = json!({"type":"string"});
    install_schema(&path, &original_schema, json!({}));
    let selected = inspect_named(&fixture.application, "Main", &path).unwrap();
    let good = save(&fixture, &selected, "Small", json!({"count":2}));
    let bad = save(
        &fixture,
        &selected,
        "Too large after defaults",
        json!({"count":2,"blob":"x".repeat(20_000)}),
    );
    let bad_file = profile_path(&fixture, "Main", &bad);
    let before = fs::read(&bad_file).unwrap();
    original_schema["properties"]["added"] = json!({"type":"string","default":"y".repeat(50_000)});
    install_schema(&path, &original_schema, json!({}));
    let outcome = fixture
        .application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(
        outcome
            .outcomes
            .iter()
            .find(|outcome| outcome.profile_id == good.id)
            .unwrap()
            .status,
        RecoveryStatus::Saved
    );
    assert_eq!(
        outcome
            .outcomes
            .iter()
            .find(|outcome| outcome.profile_id == bad.id)
            .unwrap()
            .status,
        RecoveryStatus::StorageFailed
    );
    assert_eq!(outcome.workspace.selection.unwrap().profiles[0].id, good.id);
    assert_eq!(fs::read(bad_file).unwrap(), before);
}

#[test]
fn automatic_reconciliation_never_coerces_deletes_or_recursively_fills_values() {
    for (label, supplied, replacement) in [
        (
            "unknown",
            json!({"extra":true}),
            json!({"type":"integer","default":1}),
        ),
        (
            "type",
            json!({"item":2}),
            json!({"type":"string","default":"replacement"}),
        ),
        (
            "enum",
            json!({"item":"old"}),
            json!({"type":"string","enum":["new"],"default":"new"}),
        ),
        (
            "range",
            json!({"item":9}),
            json!({"type":"integer","maximum":5,"default":1}),
        ),
        (
            "nested",
            json!({"item":{"kept":"value"}}),
            json!({
                "type":"object","additionalProperties":false,"required":["kept","new"],
                "properties":{"kept":{"type":"string"},"new":{"type":"integer","default":1}}
            }),
        ),
        (
            "array-item",
            json!({"item":[{}]}),
            json!({
                "type":"array","items":{"type":"object","additionalProperties":false,
                    "required":["new"],"properties":{"new":{"type":"integer","default":1}}}
            }),
        ),
    ] {
        let fixture = Fixture::new();
        let (path, selected) = initial(&fixture);
        let saved = save(&fixture, &selected, label, json!({"count":2}));
        let file = profile_path(&fixture, "Main", &saved);
        let mut original = serde_json::to_value(&saved).unwrap();
        original["values"] = supplied;
        fs::write(&file, serde_json::to_vec(&original).unwrap()).unwrap();
        let before = fs::read(&file).unwrap();
        let mut updated = schema(5);
        updated["properties"]["item"] = replacement;
        install_schema(&path, &updated, json!({}));
        let outcome = fixture
            .application
            .inspect(&path, &workspace_ref(&selected))
            .unwrap();
        assert_eq!(
            outcome.outcomes[0].status,
            RecoveryStatus::RepairRequired,
            "{label}"
        );
        let entry = &outcome.workspace.recovery.as_ref().unwrap().profiles[0];
        assert_eq!(entry.profile.values, original["values"], "{label}");
        assert!(
            entry.issue.context["path"]
                .as_str()
                .unwrap()
                .starts_with("$."),
            "{label}"
        );
        assert_eq!(fs::read(file).unwrap(), before, "{label}");
    }
}

#[test]
fn active_operation_refuses_recovery_mutation_binding_and_discard_without_losing_stop() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Busy", json!({"count":9}));
    install_schema(&path, &schema(5), json!({}));
    let context = recovery(&fixture, &path, &selected);
    let running = inspect_named(application, "Running", &package_path()).unwrap();
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    let store = lock(&application.store);
    let starting = application.clone();
    let reference = workspace_ref(&running);
    let starter = std::thread::spawn(move || starting.start(&reference, request(&running)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let run = loop {
        let poll = application.poll();
        if poll.controller["state"] == "preparing" {
            break poll.controller["run"].as_str().unwrap().to_owned();
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    };
    let repair = application.repair_profile(&context, &saved.id, json!({"count":1}));
    let reset = application.reset_profile(&context, &saved.id, true);
    let retry = application.retry_binding(&context);
    let discard = application.discard_recovery(&context);
    let inspect = application.inspect(&path, &context.workspace);
    let stopped = application.stop(&run);
    drop(store);
    starter.join().unwrap().unwrap();
    assert_eq!(repair.unwrap_err().category, "RunActive");
    assert_eq!(reset.unwrap_err().category, "RunActive");
    assert_eq!(retry.unwrap_err().category, "RunActive");
    assert_eq!(discard.unwrap_err().category, "RunActive");
    assert_eq!(inspect.unwrap_err().category, "RunActive");
    stopped.unwrap();
    assert_eq!(settled(application).error.unwrap().category, "Cancelled");
    assert_eq!(fs::read(file).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn linked_original_is_a_storage_fault_not_a_repair_or_reset_route() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Linked original", json!({"count":9}));
    let file = profile_path(&fixture, "Main", &saved);
    let original = fixture.root.join("retained-original.config");
    fs::rename(&file, &original).unwrap();
    let before = fs::read(&original).unwrap();
    std::os::unix::fs::symlink(&original, &file).unwrap();
    install_schema(&path, &schema(5), json!({}));
    let outcome = fixture
        .application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    let recovery = outcome.workspace.recovery.unwrap();
    assert!(recovery.profiles.is_empty());
    assert!(recovery.profiles_error.is_some());
    assert_eq!(
        fixture
            .application
            .reset_profile(&recovery.context, &saved.id, true)
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert!(fs::symlink_metadata(file).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(original).unwrap(), before);
}

#[test]
fn startup_does_not_reconcile_stale_profiles_or_grant_recovery_authority() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Startup", json!({"count":2}));
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    fixture.application.shutdown().unwrap();
    install_schema(&path, &schema(5), json!({}));
    let restarted = Application::new(
        fixture.root.clone(),
        fixture.root.join("absent-runner"),
        fixture.root.join("absent-engine"),
    )
    .unwrap();
    let workspace = restarted.workspace_catalog().unwrap().open.remove(0);
    assert!(workspace.recovery.is_none());
    let selection = workspace.selection.unwrap();
    assert!(selection.profiles.is_empty());
    assert_eq!(
        selection.profiles_error.unwrap().category,
        "ProfileRejected"
    );
    assert_eq!(fs::read(file).unwrap(), before);
    restarted.shutdown().unwrap();
}

#[test]
fn current_schema_invalid_original_remains_outside_ordinary_crud_and_start() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Invalid values", json!({"count":2}));
    let file = profile_path(&fixture, "Main", &saved);
    let mut raw = serde_json::to_value(&saved).unwrap();
    raw["values"]["count"] = json!(11);
    fs::write(&file, serde_json::to_vec(&raw).unwrap()).unwrap();
    let before = fs::read(&file).unwrap();
    let outcome = fixture
        .application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::Bound);
    let reference = view_ref(&outcome.workspace);
    assert_eq!(outcome.outcomes[0].status, RecoveryStatus::RepairRequired);
    assert!(
        fixture
            .application
            .save_profile(&reference, Some(&saved.id), "Bypass", json!({"count":2}))
            .is_err()
    );
    assert!(
        fixture
            .application
            .rename_profile(&reference, &saved.id, "Bypass")
            .is_err()
    );
    assert!(
        fixture
            .application
            .delete_profile(&reference, &saved.id)
            .is_err()
    );
    let mut forged = request(outcome.workspace.selection.as_ref().unwrap());
    forged.profile_id = saved.id;
    forged.values = json!({"count":2});
    fixture.application.start(&reference, forged).unwrap();
    assert_eq!(
        settled(&fixture.application).error.unwrap().category,
        "ProfileIdentity"
    );
    assert_eq!(fs::read(file).unwrap(), before);
}

#[test]
fn nonportable_reset_defaults_remain_an_unsaved_draft() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Portable original", json!({"count":2}));
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    let mut updated = schema(5);
    updated["properties"]["label"] = json!({"type":"string","default":"/machine-local-location"});
    install_schema(&path, &updated, json!({}));
    let inspected = fixture
        .application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(inspected.outcomes[0].status, RecoveryStatus::RepairRequired);
    assert_eq!(
        inspected.outcomes[0].issue.as_ref().unwrap().context["path"],
        "$.label"
    );
    let context = inspected.workspace.recovery.unwrap().context;
    let reset = fixture
        .application
        .reset_profile(&context, &saved.id, true)
        .unwrap();
    assert!(reset.saved.is_none());
    let issue = reset.issue.unwrap();
    assert_eq!(issue.category, "ProfileAuthority");
    assert_eq!(issue.context["path"], "$.label");
    assert_eq!(issue.context["profile_id"], saved.id);
    assert_eq!(issue.context["internal_name"], "Main");
    assert_eq!(issue.context["package_id"], saved.package_id);
    assert_eq!(reset.draft.unwrap()["label"], "/machine-local-location");
    assert_eq!(fs::read(file).unwrap(), before);
}

#[test]
fn reset_returns_a_draft_for_invalid_defaults_but_not_for_aggregate_exhaustion() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = fixture.package_at("source");
    let mut original = schema(10);
    original["properties"]["blob"] = json!({"type":"string"});
    install_schema(&path, &original, json!({}));
    let selected = inspect_named(application, "Main", &path).unwrap();
    let target = save(&fixture, &selected, "Reset target", json!({"count":2}));
    // Fill the aggregate budget with profiles the new schema rejects without rewriting.
    for index in 0..16 {
        save(
            &fixture,
            &selected,
            &format!("Filler {index:02}"),
            json!({"count":9,"blob":"x".repeat(63_000)}),
        );
    }
    let file = profile_path(&fixture, "Main", &target);
    let before = fs::read(&file).unwrap();
    let mut updated = schema(5);
    updated["properties"]["blob"] = json!({"type":"string"});
    updated["properties"]["added"] = json!({"type":"string","default":"y".repeat(60_000)});
    install_schema(&path, &updated, json!({}));
    let inspected = application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(inspected.kind, InspectionKind::Bound);
    let outcome = inspected
        .outcomes
        .iter()
        .find(|outcome| outcome.profile_id == target.id)
        .unwrap();
    assert_eq!(outcome.status, RecoveryStatus::StorageFailed);
    assert_eq!(outcome.issue.as_ref().unwrap().category, "StorageLimit");
    assert_eq!(fs::read(&file).unwrap(), before);
    let view = inspected.workspace;
    let context = view.recovery.unwrap().context;
    let bound = view.selection.unwrap();
    command_records(application, &selected.workspace_id);
    // Valid defaults refused by the aggregate budget are not a draft to complete.
    let exhausted = application
        .reset_profile(&context, &target.id, true)
        .unwrap();
    assert!(exhausted.saved.is_none());
    assert!(exhausted.draft.is_none());
    assert_eq!(exhausted.issue.unwrap().category, "StorageLimit");
    assert!(exhausted.recovery.is_some());
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(
        command_records(application, &selected.workspace_id),
        [(
            "command.failed".to_owned(),
            json!("reset_profile"),
            json!("StorageLimit")
        )]
    );
    // Distinguish invalid strings, oversized values, and full-record overhead from
    // aggregate exhaustion. Every individual-profile limit still returns a draft.
    let mut empty_defaults = top_level_defaults(&updated);
    empty_defaults["added"] = json!("");
    let overhead = serde_json::to_vec(&empty_defaults).unwrap().len();
    let mut current = workspace_ref(&bound);
    for (length, status) in [
        (70_000, RecoveryStatus::RepairRequired),
        (MAX_PROFILE_BYTES, RecoveryStatus::StorageFailed),
        (MAX_PROFILE_BYTES - overhead, RecoveryStatus::StorageFailed),
    ] {
        updated["properties"]["added"] = json!({"type":"string","default":"y".repeat(length)});
        install_schema(&path, &updated, json!({}));
        let reinspected = application.inspect(&path, &current).unwrap();
        assert_eq!(reinspected.kind, InspectionKind::Bound);
        assert_eq!(
            reinspected
                .outcomes
                .iter()
                .find(|outcome| outcome.profile_id == target.id)
                .unwrap()
                .status,
            status
        );
        current = view_ref(&reinspected.workspace);
        let context = reinspected.workspace.recovery.unwrap().context;
        let oversized = application
            .reset_profile(&context, &target.id, true)
            .unwrap();
        assert!(oversized.saved.is_none());
        let issue = oversized.issue.unwrap();
        assert_eq!(issue.category, "StorageLimit");
        assert_eq!(issue.context["profile_id"], target.id);
        assert_eq!(issue.context["package_id"], target.package_id);
        assert_eq!(
            oversized.draft.unwrap()["added"].as_str().unwrap().len(),
            length
        );
        assert_eq!(fs::read(&file).unwrap(), before);
    }
}

#[test]
fn inspection_publication_retains_a_terminal_result_collected_by_the_command() {
    for retry in [false, true] {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let (path, selected) = initial(&fixture);
        let mut profile = save(&fixture, &selected, "Repair externally", json!({"count":9}));
        install_schema(&path, &schema(5), json!({}));
        let inspected = application
            .inspect(&path, &workspace_ref(&selected))
            .unwrap()
            .workspace;
        let reference = view_ref(&inspected);
        let selected = inspected.selection.as_ref().unwrap();
        profile.values = json!({"count":2});
        profile.schema_identity = selected.package.schema_identity.clone();
        fs::write(
            profile_path(&fixture, "Main", &profile),
            serde_json::to_vec(&profile).unwrap(),
        )
        .unwrap();
        application.start(&reference, request(selected)).unwrap();
        let terminal = settled(application);
        let run = serde_json::to_value(terminal).unwrap()["run"].clone();
        if retry {
            application
                .retry_binding(&inspected.recovery.unwrap().context)
                .unwrap();
        } else {
            application.inspect(&path, &reference).unwrap();
        }
        let poll = application.poll();
        let retained = poll
            .workspace_results
            .iter()
            .find(|result| result.workspace == reference)
            .expect("inspection must preserve the just-collected originating result");
        assert_eq!(retained.controller["run"], run);
        assert_eq!(retained.controller["state"], "terminal");
    }
}

#[test]
fn recovery_refuses_changed_durable_package_ownership() {
    for damage in ["removed", "redirected", "selected"] {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let (path, selected) = initial(&fixture);
        let saved = save(&fixture, &selected, "Rejected owner", json!({"count":9}));
        let candidate = fixture.root.join("moved");
        fs::rename(&path, &candidate).unwrap();
        install_schema(&candidate, &schema(5), json!({}));
        let outcome = application
            .inspect(&candidate, &workspace_ref(&selected))
            .unwrap();
        assert_eq!(outcome.kind, InspectionKind::RecoveryRequired);
        let context = outcome.workspace.recovery.unwrap().context;
        if damage == "selected" {
            application
                .repair_profile(&context, &saved.id, json!({"count":2}))
                .unwrap();
        }
        let profile_file = profile_path(&fixture, "Main", &saved);
        let profile_before = fs::read(&profile_file).unwrap();
        let mut tab = lock(&application.store).tab("Main").unwrap();
        match damage {
            "removed" => {
                tab.packages.clear();
                tab.selected_package_id = None;
            }
            "redirected" => {
                tab.packages[0].source = PackageSource::Directory {
                    path: fixture
                        .root
                        .join("different-owner-source")
                        .to_string_lossy()
                        .into_owned(),
                }
            }
            _ => {
                tab.packages.push(PackageReference {
                    package_id: "other-package".into(),
                    source: tab.packages[0].source.clone(),
                });
                tab.selected_package_id = Some("other-package".into());
            }
        }
        let tab_file = fixture.root.join("tabs/Main/tab.config");
        let tab_before = serde_json::to_vec(&tab).unwrap();
        fs::write(&tab_file, &tab_before).unwrap();
        let retry = application.retry_binding(&context).unwrap();
        assert_eq!(retry.kind, InspectionKind::BindingFailed);
        assert_eq!(retry.binding_error.unwrap().category, "StaleIdentity");
        assert!(retry.workspace.selection.is_none());
        assert_eq!(
            application
                .repair_profile(&context, &saved.id, json!({"count":2}))
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(
            application
                .reset_profile(&context, &saved.id, true)
                .unwrap_err()
                .category,
            "StaleIdentity"
        );
        assert_eq!(fs::read(profile_file).unwrap(), profile_before);
        assert_eq!(fs::read(tab_file).unwrap(), tab_before);
    }
}

#[test]
fn different_package_candidate_never_touches_the_original_packages_profiles() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Original owner", json!({"count":9}));
    let file = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&file).unwrap();
    let profile_names = || {
        let mut names: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let names_before = profile_names();
    install_schema(&path, &schema(5), json!({}));
    let original = application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(original.kind, InspectionKind::Bound);
    let original_ref = view_ref(&original.workspace);
    let original_source = original.workspace.saved_package.as_ref().unwrap().clone();
    let original_context = original.workspace.recovery.unwrap().context;

    let candidate = foreign_package(&fixture, "different-package", "second-workload");
    install_schema(&candidate, &schema(1), json!({}));
    let tab_file = fixture.root.join("tabs/Main/tab.config");
    let tab_before = fs::read(&tab_file).unwrap();
    let pending = tab_file.with_extension("pending");
    fs::write(&pending, b"interrupted Tab write").unwrap();
    let inspected = application.inspect(&candidate, &original_ref).unwrap();
    assert_eq!(inspected.kind, InspectionKind::BindingFailed);
    assert!(inspected.outcomes.is_empty());
    let failed_ref = view_ref(&inspected.workspace);
    assert_eq!(failed_ref.revision, original_ref.revision + 1);
    let retained = inspected.workspace.selection.as_ref().unwrap();
    assert_eq!(retained.package.package_id, saved.package_id);
    assert_eq!(retained.package_path, selected.package_path);
    assert_eq!(retained.revision, failed_ref.revision);
    assert_eq!(
        inspected.workspace.saved_package.as_ref(),
        Some(&original_source)
    );
    assert!(inspected.workspace.source_error.is_none());
    // This value is valid for retained A, but invalid for candidate B.
    assert_eq!(
        application.validate(&failed_ref, json!({"count":2})).unwrap()["count"],
        2
    );
    let recovery = inspected.workspace.recovery.unwrap();
    assert!(recovery.binding_required);
    assert!(!recovery.relocation);
    assert_eq!(recovery.package.package_id, "second-workload");
    assert!(recovery.profiles.is_empty());
    assert!(recovery.profiles_error.is_none());
    let candidate_context = recovery.context;
    assert_eq!(
        application
            .repair_profile(&candidate_context, &saved.id, json!({"count":1}))
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert_eq!(
        application
            .reset_profile(&candidate_context, &saved.id, true)
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert_eq!(
        application
            .repair_profile(&original_context, &saved.id, json!({"count":2}))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(profile_names(), names_before);
    assert_eq!(fs::read(&tab_file).unwrap(), tab_before);
    assert!(!fixture.root.join("tabs/Main/second-workload").exists());

    fs::remove_file(pending).unwrap();
    let retried = application.retry_binding(&candidate_context).unwrap();
    assert_eq!(retried.kind, InspectionKind::Bound);
    assert!(retried.binding_error.is_none());
    assert!(retried.outcomes.is_empty());
    let rebound_ref = view_ref(&retried.workspace);
    assert_eq!(rebound_ref.revision, failed_ref.revision + 1);
    let rebound = retried.workspace.selection.as_ref().unwrap();
    assert_eq!(rebound.package.package_id, "second-workload");
    assert!(rebound.profiles.is_empty());
    assert!(rebound.profiles_error.is_none());
    assert!(retried.workspace.recovery.is_none());
    let tab = lock(&application.store).tab("Main").unwrap();
    assert_eq!(tab.packages.len(), 2);
    assert_eq!(tab.selected_package_id.as_deref(), Some("second-workload"));
    assert_eq!(
        tab.packages
            .iter()
            .find(|reference| reference.package_id == saved.package_id),
        Some(&original_source)
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(profile_names(), names_before);

    let returned = application.inspect(&path, &rebound_ref).unwrap();
    assert_eq!(returned.kind, InspectionKind::Bound);
    assert_eq!(
        returned.workspace.selection.as_ref().unwrap().package.package_id,
        saved.package_id
    );
    let recovery = returned.workspace.recovery.unwrap();
    assert!(!recovery.binding_required);
    assert_eq!(recovery.profiles.len(), 1);
    assert_eq!(recovery.profiles[0].profile.id, saved.id);
    assert_eq!(recovery.profiles[0].profile.package_id, saved.package_id);
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(profile_names(), names_before);
    let repaired = application
        .repair_profile(&recovery.context, &saved.id, json!({"count":2}))
        .unwrap()
        .saved
        .unwrap();
    assert_eq!(repaired.values["count"], 2);
    assert_eq!(repaired.package_id, saved.package_id);
    assert_eq!(repaired.id, saved.id);
    let persisted: Profile = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(persisted.values, repaired.values);
    assert_eq!(persisted.package_id, saved.package_id);
}

#[test]
fn inspection_recaptures_selected_package_authority_after_its_successful_bind() {
    let fixture = Fixture::new();
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Select back", json!({"count":9}));
    let mut tab = lock(&fixture.application.store).tab("Main").unwrap();
    tab.packages.push(PackageReference {
        package_id: "other-package".into(),
        source: tab.packages[0].source.clone(),
    });
    tab.selected_package_id = Some("other-package".into());
    fs::write(
        fixture.root.join("tabs/Main/tab.config"),
        serde_json::to_vec(&tab).unwrap(),
    )
    .unwrap();
    install_schema(&path, &schema(5), json!({}));
    let inspected = fixture
        .application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(inspected.kind, InspectionKind::Bound);
    let context = inspected.workspace.recovery.unwrap().context;
    let repaired = fixture
        .application
        .repair_profile(&context, &saved.id, json!({"count":2}))
        .unwrap();
    assert_eq!(repaired.saved.unwrap().values["count"], 2);
    assert_eq!(
        lock(&fixture.application.store)
            .tab("Main")
            .unwrap()
            .selected_package_id
            .as_deref(),
        Some(saved.package_id.as_str())
    );
}

#[test]
fn in_place_binding_retry_keeps_unrepaired_profiles_editable() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let (path, selected) = initial(&fixture);
    let saved = save(&fixture, &selected, "Needs repair", json!({"count":9}));
    let other = save(&fixture, &selected, "Also stale", json!({"count":8}));
    install_schema(&path, &schema(5), json!({}));
    let pending = fixture.root.join("tabs/Main/tab.pending");
    fs::write(&pending, b"interrupted Tab write").unwrap();
    command_records(application, &selected.workspace_id);
    let inspected = application
        .inspect(&path, &workspace_ref(&selected))
        .unwrap();
    assert_eq!(inspected.kind, InspectionKind::BindingFailed);
    let category = inspected.binding_error.as_ref().unwrap().category.clone();
    let view = inspected.workspace;
    // Same path, same package: not a relocation, but the Tab write never published.
    assert_eq!(
        view.selection.as_ref().unwrap().package_path,
        selected.package_path
    );
    assert!(view.source_error.is_none());
    let recovery = view.recovery.unwrap();
    assert!(recovery.binding_required);
    assert!(!recovery.relocation);
    let rejected: Vec<&str> = recovery
        .profiles
        .iter()
        .map(|entry| entry.profile.id.as_str())
        .collect();
    assert_eq!(rejected, [other.id.as_str(), saved.id.as_str()]);
    let context = recovery.context;
    fs::remove_file(&pending).unwrap();
    let retried = application.retry_binding(&context).unwrap();
    assert_eq!(retried.kind, InspectionKind::Bound);
    assert!(retried.binding_error.is_none());
    let bound = retried.workspace;
    assert_eq!(bound.revision, context.workspace.revision + 1);
    assert_eq!(
        bound.selection.as_ref().unwrap().package.schema_identity,
        recovery.package.schema_identity
    );
    // The bound context keeps both rejected profiles repairable under the same token.
    let recovery = bound
        .recovery
        .as_ref()
        .expect("unrepaired profiles remain recoverable");
    assert!(!recovery.binding_required);
    assert!(!recovery.relocation);
    assert_eq!(recovery.context.token, context.token);
    assert_eq!(recovery.context.workspace, view_ref(&bound));
    assert_eq!(recovery.profiles.len(), 2);
    assert_eq!(
        application
            .repair_profile(&context, &saved.id, json!({"count":2}))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    let repaired = application
        .repair_profile(&recovery.context, &saved.id, json!({"count":2}))
        .unwrap();
    assert_eq!(repaired.saved.unwrap().values["count"], 2);
    let remaining = repaired.recovery.unwrap();
    assert!(!remaining.binding_required);
    assert_eq!(remaining.profiles[0].profile.id, other.id);
    let reset = application
        .reset_profile(&recovery.context, &other.id, true)
        .unwrap();
    assert_eq!(reset.saved.unwrap().values["count"], 1);
    assert!(reset.recovery.unwrap().profiles.is_empty());
    assert_eq!(
        command_records(application, &selected.workspace_id),
        [
            ("command.failed".to_owned(), json!("inspect"), json!(category)),
            (
                "workspace.reinspected".to_owned(),
                json!("retry_binding"),
                Value::Null
            ),
            (
                "command.failed".to_owned(),
                json!("repair_profile"),
                json!("StaleIdentity")
            ),
            ("profile.saved".to_owned(), json!("repair_profile"), Value::Null),
            ("profile.saved".to_owned(), json!("reset_profile"), Value::Null),
        ]
    );
}
