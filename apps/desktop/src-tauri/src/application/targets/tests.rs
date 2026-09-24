use super::*;
use crate::application::test_support::*;
use serde_json::json;

#[test]
fn target_operations_require_inspected_declaration_and_current_workspace() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let empty = application.create_workspace("Empty", "Empty").unwrap();
    let empty_ref = view_ref(&empty);
    let configuration = target_configuration();
    let expected = TargetExpectation {
        revision: 0,
        binding_id: None,
    };
    assert_eq!(
        application.read_target(&empty_ref).unwrap_err().category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .check_target(&empty_ref, &expected, &configuration)
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .save_target(&empty_ref, &expected, configuration.clone(), None)
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    assert_eq!(
        application
            .remove_target(&empty_ref, &expected)
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    let selection = application
        .inspect(&package_path(), &empty_ref)
        .unwrap()
        .workspace
        .selection
        .unwrap();
    let workspace = workspace_ref(&selection);
    assert_eq!(
        application.read_target(&empty_ref).unwrap_err().category,
        "StaleIdentity"
    );
    assert_eq!(
        application
            .check_target(&workspace, &expected, &configuration)
            .unwrap_err()
            .category,
        "TargetUndeclared"
    );
    assert_eq!(
        application
            .save_target(&workspace, &expected, configuration, None)
            .unwrap_err()
            .category,
        "TargetUndeclared"
    );
    let target = application.read_target(&workspace).unwrap();
    assert_eq!(target.record.revision, 0);
    assert!(target.record.binding.is_none());
    assert!(application.runner.poll().run.is_none());
}

#[cfg(unix)]
#[test]
fn changed_target_resolution_warns_without_saving_until_reviewed() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = fixture.package_at("review-target");
    declare_target(&path, Some("metadata-fixture"));
    let selected = inspect_named(application, "Review", &path).unwrap();
    let owner = workspace_ref(&selected);
    let initial = application.read_target(&owner).unwrap();
    let mut configuration = target_configuration();
    let saved = application
        .save_target(
            &owner,
            &target_expectation(&initial),
            configuration.clone(),
            None,
        )
        .unwrap();
    let expected = target_expectation(&saved.view);
    application.poll();
    configuration.working_directory = Some(fixture.root.to_string_lossy().into_owned());
    let refusal = application
        .save_target(&owner, &expected, configuration.clone(), None)
        .unwrap_err();
    assert_eq!(refusal.category, "TargetResolutionChanged");
    assert_eq!(
        application.read_target(&owner).unwrap().record,
        saved.view.record
    );
    let entries = application.poll().logs.entries;
    let warning = entries
        .iter()
        .find(|entry| entry.code == "target.review_required")
        .unwrap();
    assert!(warning.level.eq_ignore_ascii_case("warn"));
    assert_eq!(
        warning.workspace_id.as_deref(),
        Some(owner.workspace_id.as_str())
    );
    assert_eq!(
        warning.fields,
        json!({"action":"save_target","category":"TargetResolutionChanged"})
    );
    assert!(!entries.iter().any(|entry| entry.code == "command.failed"));
    let reviewed: TargetResolution =
        serde_json::from_value(refusal.context["resolution"].clone()).unwrap();
    let accepted = application
        .save_target(&owner, &expected, configuration.clone(), Some(&reviewed))
        .unwrap();
    assert_eq!(accepted.view.record.revision, expected.revision + 1);
    assert_eq!(
        accepted.view.record.binding.unwrap().configuration,
        configuration
    );
    application.poll();
    configuration.working_directory =
        Some(fixture.root.join("absent").to_string_lossy().into_owned());
    assert_eq!(
        application
            .save_target(
                &owner,
                &target_expectation(&application.read_target(&owner).unwrap()),
                configuration,
                None,
            )
            .unwrap_err()
            .category,
        "TargetMetadata"
    );
    let entries = application.poll().logs.entries;
    let failure = entries
        .iter()
        .find(|entry| entry.code == "command.failed")
        .unwrap();
    assert!(failure.level.eq_ignore_ascii_case("error"));
    assert_eq!(failure.fields["category"], "TargetMetadata");
    assert_eq!(failure.fields["action"], "save_target");
}
