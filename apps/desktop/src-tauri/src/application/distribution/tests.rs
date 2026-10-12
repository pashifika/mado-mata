use super::*;
use crate::application::test_support::{Fixture, preferences, view_ref, workspace_ref};
use crate::bootstrap::{Bootstrap, Phase};

#[test]
fn picker_cancel_preserves_other_owner_and_holds_admission_until_dismissal() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let first = view_ref(&app.create_workspace("first", "First").unwrap());
    let second = view_ref(&app.create_workspace("second", "Second").unwrap());
    let guard = app.reserve_distribution_picker(&first, false).unwrap();
    assert_eq!(
        app.create_workspace("blocked", "Blocked")
            .unwrap_err()
            .category,
        "PackageBusy"
    );
    assert_eq!(
        app.package_distribution_cancel(&second)
            .unwrap_err()
            .category,
        "PackageReviewStale"
    );
    guard.validate().unwrap();
    assert!(app.package_distribution_cancel(&first).unwrap());
    assert_eq!(guard.validate().unwrap_err().category, "PackageCancelled");
    assert_eq!(
        app.prepare_reconstruction().unwrap_err().category,
        "PackageBusy"
    );
    assert_eq!(
        app.settle_distribution(Instant::now())
            .unwrap_err()
            .category,
        "PackageCleanup"
    );
    drop(guard);
    app.create_workspace("after", "After").unwrap();
    assert_eq!(app.workspace_catalog().unwrap().open.len(), 3);
}

#[test]
fn pending_package_picker_does_not_retire_ready_configuration() {
    let fixture = Fixture::new();
    let bootstrap = Bootstrap::new(
        Ok(fixture.root.join("bootstrap")),
        None,
        fixture.root.join("unused-runner"),
        fixture.root.join("unused-engine"),
    );
    assert!(bootstrap.initialize(preferences(), false).unwrap().state == Phase::Ready);
    let app = bootstrap.application().unwrap();
    let owner = view_ref(&app.create_workspace("transfer", "Transfer").unwrap());
    let guard = app.reserve_distribution_picker(&owner, false).unwrap();
    let status = bootstrap.status();
    assert!(status.state == Phase::Ready);
    assert_eq!(status.fault.unwrap().category, "PackageBusy");
    assert!(Arc::ptr_eq(&app, &bootstrap.application().unwrap()));
    app.package_distribution_cancel(&owner).unwrap();
    drop(guard);
    let status = bootstrap.status();
    assert!(status.state == Phase::Ready);
    assert!(status.fault.is_none());
    assert_eq!(status.catalog.unwrap().open[0].internal_name, "transfer");
    let _ = bootstrap.shutdown();
}

#[test]
fn export_destination_cannot_add_an_undeclared_file_to_the_source() {
    let fixture = Fixture::new();
    let package = fixture.package_at("source");
    let root = package.canonicalize().unwrap();
    assert_eq!(
        export_destination(&root.join("release.mmpkg"), &root)
            .unwrap_err()
            .category,
        "PackageDestination"
    );
    assert_eq!(
        export_destination(&root.join("../source/release.mmpkg"), &root)
            .unwrap_err()
            .category,
        "PackageDestination"
    );
    let destination = fixture.root.join("release.mmpkg");
    assert_eq!(
        export_destination(&destination, &root).unwrap(),
        fixture.root.canonicalize().unwrap().join("release.mmpkg")
    );
    assert!(!package.join("release.mmpkg").exists());
}

fn release_source(fixture: &Fixture) -> PathBuf {
    let source = fixture.package_at("sources/release");
    let manifest = source.join("package.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    value["distribution"] = serde_json::json!({
        "version": 1, "release_version": "1.0.0", "name": "Transfer fixture",
        "description": "Public test data", "author_name": "Fixture", "license": "MIT",
        "game": {"id": "transfer-fixture", "names": {"en": "Fixture"}, "aliases": []}
    });
    std::fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    source
}

#[test]
fn saved_edit_invalidates_export_review_and_external_capture_conflicts_do_not_publish() {
    let fixture = Fixture::new();
    let source = release_source(&fixture);
    crate::author_signing::test_signer(&fixture.root);
    let app = &fixture.application;
    let workspace = view_ref(&app.create_workspace("author", "Author").unwrap());
    let editor = app.authoring_open(&workspace, &source).unwrap();
    let destination = fixture.root.join("release.mmpkg");
    let review = app
        .package_export_prepare(&editor.owner, &editor.revision, &destination)
        .unwrap();
    let script = std::fs::read_to_string(source.join("main.js")).unwrap();
    let saved = app
        .authoring_save(
            &editor.owner,
            &editor.revision,
            "main.js",
            format!("{script}\n// changed\n"),
        )
        .unwrap();
    let current = saved.view.unwrap();
    assert_eq!(
        app.package_export_publish(&current.owner, &current.revision, &review.token)
            .unwrap_err()
            .category,
        "PackageReviewStale"
    );
    assert!(!destination.exists());
    std::fs::write(
        source.join("main.js"),
        format!("{script}\n// outside edit\n"),
    )
    .unwrap();
    assert_eq!(
        app.package_export_prepare(&current.owner, &current.revision, &destination)
            .err()
            .unwrap()
            .category,
        "AuthoringConflict"
    );
    assert!(!destination.exists());
    assert_eq!(app.authoring_owner(), Some(current.owner));
}

#[test]
fn import_confirmation_uses_reviewed_bytes_and_never_selects_a_workspace() {
    let author = Fixture::new();
    let source = release_source(&author);
    crate::author_signing::test_signer(&author.root);
    let app = &author.application;
    let workspace = view_ref(&app.create_workspace("author", "Author").unwrap());
    let editor = app.authoring_open(&workspace, &source).unwrap();
    let archive = author.root.join("release.mmpkg");
    let exported = app
        .package_export_prepare(&editor.owner, &editor.revision, &archive)
        .unwrap();
    assert_eq!(
        app.prepare_distribution_close(None).unwrap_err().category,
        "AuthoringActive"
    );
    let publication = app
        .package_export_publish(&editor.owner, &editor.revision, &exported.token)
        .unwrap();
    assert!(publication.committed);
    assert!(publication.cleanup_error.is_none());

    let recipient = Fixture::new();
    let app = &recipient.application;
    let owner = view_ref(&app.create_workspace("recipient", "Recipient").unwrap());
    let other = view_ref(&app.create_workspace("other", "Other").unwrap());
    let review = app.package_import_prepare(&owner, &archive).unwrap();
    assert_eq!(review.facts.artifact_id, exported.facts.artifact_id);
    assert_eq!(
        app.package_import_publish(&other, &review.token)
            .unwrap_err()
            .category,
        "PackageReviewStale"
    );
    let reviewed = std::fs::read(&archive).unwrap();
    std::fs::write(&archive, b"the original selected file changed after review").unwrap();
    let installed = app.package_import_publish(&owner, &review.token).unwrap();
    assert!(installed.committed);
    assert_eq!(installed.artifact_id, exported.facts.artifact_id);
    // The reviewed destination is the published file and holds exactly the reviewed bytes.
    assert_eq!(installed.path, review.destination);
    assert_eq!(std::fs::read(&installed.path).unwrap(), reviewed);
    assert!(
        app.workspace_catalog()
            .unwrap()
            .open
            .iter()
            .all(|view| view.saved_package.is_none() && view.selection.is_none())
    );
    let bound = app
        .inspect_installed(&owner, &installed.package_id, &installed.artifact_id)
        .unwrap();
    let selection = bound.workspace.selection.unwrap();
    assert_eq!(
        selection.source,
        crate::storage::PackageSource::Installed {
            package_id: installed.package_id.clone(),
            artifact_id: installed.artifact_id.clone(),
        }
    );
    assert_eq!(selection.package.package_id, editor.package_id);
    // Selection runs the verified archive itself; it is never opened as a source directory.
    assert_eq!(selection.package_path, installed.path);
    let installed_archive = Path::new(&installed.path);
    assert_eq!(
        app.authoring_open(&workspace_ref(&selection), installed_archive)
            .unwrap_err()
            .category,
        "ManagedPackage"
    );
    assert!(app.authoring_owner().is_none());
    assert_eq!(
        app.package_installed_list().unwrap()[0].artifact_id,
        installed.artifact_id
    );
    assert!(
        crate::author_signing::status(&recipient.root)
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_unconfirmed_close_does_not_cancel_the_edit_owners_picker() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let workspace = view_ref(&app.create_workspace("author", "Author").unwrap());
    let editor = app
        .authoring_open(&workspace, &fixture.package_at("sources/source"))
        .unwrap();
    let guard = app.reserve_distribution_picker(&workspace, true).unwrap();
    assert_eq!(
        app.prepare_distribution_close(None).unwrap_err().category,
        "AuthoringActive"
    );
    guard.validate().unwrap();
    drop(guard);
    app.prepare_distribution_close(Some(&editor.owner)).unwrap();
    app.prepare_close(Some(&editor.owner)).unwrap();
}
