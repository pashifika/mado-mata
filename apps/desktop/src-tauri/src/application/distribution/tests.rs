use super::*;
use crate::application::test_support::{
    Fixture, inspect_named, package_path, preferences, request, settled, view_ref, workspace_ref,
};
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

#[test]
fn review_export_existing_destination_refuses_before_review() {
    let fixture = Fixture::new();
    let source = release_source(&fixture);
    crate::author_signing::test_signer(&fixture.root);
    let app = &fixture.application;
    let workspace = view_ref(&app.create_workspace("author", "Author").unwrap());
    let editor = app.authoring_open(&workspace, &source).unwrap();
    let destination = fixture.root.join("existing.mmpkg");
    std::fs::write(&destination, b"existing artifact").unwrap();

    let result = app.package_export_prepare(&editor.owner, &editor.revision, &destination);
    assert_eq!(
        result.as_ref().err().map(|fault| fault.category.as_str()),
        Some("PackageDestinationExists")
    );
    assert!(lock(&app.distribution).review.is_none());
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing artifact");
    assert_eq!(app.authoring_owner(), Some(editor.owner));
}

#[test]
fn review_close_busy_command_is_not_package_cleanup() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let _command = app.command_guard().unwrap();
    assert_eq!(
        app.settle_distribution(Instant::now())
            .unwrap_err()
            .category,
        "WorkspaceBusy"
    );
    assert!(lock(&app.distribution).active.is_none());
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

/// Builds and saves a signed release archive in `author`'s isolated data root.
fn exported_release(author: &Fixture) -> PathBuf {
    let source = release_source(author);
    crate::author_signing::test_signer(&author.root);
    let app = &author.application;
    let workspace = view_ref(&app.create_workspace("author", "Author").unwrap());
    let editor = app.authoring_open(&workspace, &source).unwrap();
    let archive = author.root.join("release.mmpkg");
    let review = app
        .package_export_prepare(&editor.owner, &editor.revision, &archive)
        .unwrap();
    assert!(
        app.package_export_publish(&editor.owner, &editor.revision, &review.token)
            .unwrap()
            .committed
    );
    archive
}

#[test]
fn review_import_admits_beside_an_active_run_or_check_without_changing_its_owner() {
    let author = Fixture::new();
    let archive = exported_release(&author);
    for check in [false, true] {
        let fixture = Fixture::new();
        let app = &fixture.application;
        // The running Tab selects the same package ID from its source directory.
        let running = inspect_named(app, "Running", &package_path()).unwrap();
        let running_ref = workspace_ref(&running);
        let importer = view_ref(&app.create_workspace("importer", "Importer").unwrap());
        // Reviewed while idle; another Tab may start work before confirmation.
        let review = app.package_import_prepare(&importer, &archive).unwrap();
        // Preparation waits on the store, so the actual Controlled owner stays
        // non-terminal and no child is launched.
        let store = lock(&app.store);
        let starting = app.clone();
        let starting_ref = running_ref.clone();
        let start_request = request(&running);
        let starter = std::thread::spawn(move || {
            if check {
                starting.check_environment(Some(&starting_ref), None)
            } else {
                starting.start(&starting_ref, start_request)
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let run = loop {
            let poll = app.poll();
            if poll.controller["state"] == "preparing" {
                break poll.controller["run"].as_str().unwrap().to_owned();
            }
            assert!(Instant::now() < deadline, "check={check}");
            std::thread::yield_now();
        };
        let installed = app.package_import_publish(&importer, &review.token);
        let picker = app
            .reserve_distribution_picker(&importer, false)
            .and_then(|guard| guard.validate());
        let reviewed = app.package_import_prepare(&importer, &archive);
        assert_eq!(
            [
                installed.as_ref().err(),
                picker.as_ref().err(),
                reviewed.as_ref().err(),
            ]
            .map(|error| error.map(|error| error.category.as_str())),
            [None::<&str>; 3],
            "check={check}"
        );
        let installed = installed.unwrap();
        assert!(installed.committed);
        let reviewed = reviewed.unwrap();
        assert_eq!(reviewed.facts.artifact_id, installed.artifact_id);
        assert!(
            app.package_import_publish(&importer, &reviewed.token)
                .unwrap()
                .unchanged
        );
        assert_eq!(
            app.package_installed_list().unwrap()[0].artifact_id,
            installed.artifact_id
        );
        // While import owns package admission, poll and Stop stay available and new
        // work waits; import neither replaced nor restarted the owner.
        let guard = app.reserve_distribution_picker(&importer, false).unwrap();
        assert_eq!(
            app.check_environment(None, None).unwrap_err().category,
            "PackageBusy"
        );
        let owned = app.poll();
        assert_eq!(owned.controller["run"], run);
        assert_eq!(owned.controller["state"], "preparing");
        assert_eq!(owned.controller["workspace_id"], running_ref.workspace_id);
        app.stop(&run).unwrap();
        assert_eq!(app.poll().controller["state"], "stopping");
        drop(guard);
        drop(store);
        assert_eq!(starter.join().unwrap().unwrap(), run);
        assert_eq!(settled(app).error.unwrap().category, "Cancelled");
        let poll = app.poll();
        let result = poll
            .workspace_results
            .iter()
            .find(|result| result.workspace == running_ref)
            .unwrap();
        assert_eq!(result.controller["run"], run);
        let catalog = app.workspace_catalog().unwrap();
        let view = |workspace: &WorkspaceRef| {
            catalog
                .open
                .iter()
                .find(|open| open.workspace_id == workspace.workspace_id)
                .unwrap()
        };
        let selected = view(&running_ref);
        assert_eq!(selected.revision, running_ref.revision);
        let selection = selected.selection.as_ref().unwrap();
        assert_eq!(selection.package_path, running.package_path);
        assert_eq!(
            selection.package.inventory_identity,
            running.package.inventory_identity
        );
        let target = view(&importer);
        assert_eq!(target.revision, importer.revision);
        assert!(target.selection.is_none() && target.saved_package.is_none());
    }
}

#[test]
fn review_import_still_refuses_an_edit_lease_without_consuming_the_review() {
    let author = Fixture::new();
    let archive = exported_release(&author);
    let fixture = Fixture::new();
    let app = &fixture.application;
    let importer = view_ref(&app.create_workspace("importer", "Importer").unwrap());
    let editing = view_ref(&app.create_workspace("editor", "Editor").unwrap());
    let review = app.package_import_prepare(&importer, &archive).unwrap();
    let editor = app
        .authoring_open(&editing, &fixture.package_at("sources/source"))
        .unwrap();
    assert_eq!(
        app.reserve_distribution_picker(&importer, false)
            .err()
            .unwrap()
            .category,
        "AuthoringActive"
    );
    assert_eq!(
        app.package_import_prepare(&importer, &archive)
            .err()
            .unwrap()
            .category,
        "AuthoringActive"
    );
    assert_eq!(
        app.package_import_publish(&importer, &review.token)
            .unwrap_err()
            .category,
        "AuthoringActive"
    );
    app.authoring_exit(&editor.owner).unwrap();
    assert!(
        app.package_import_publish(&importer, &review.token)
            .unwrap()
            .committed
    );
}

#[cfg(target_os = "macos")]
#[test]
fn review_import_still_refuses_an_open_target_picker() {
    let author = Fixture::new();
    let archive = exported_release(&author);
    let fixture = Fixture::new();
    let app = &fixture.application;
    let importer = view_ref(&app.create_workspace("importer", "Importer").unwrap());
    let review = app.package_import_prepare(&importer, &archive).unwrap();
    let picked = inspect_named(app, "Picker", &package_path()).unwrap();
    let guard = app.begin_target_picker(&workspace_ref(&picked)).unwrap();
    assert_eq!(
        app.reserve_distribution_picker(&importer, false)
            .err()
            .unwrap()
            .category,
        "TargetPickerBusy"
    );
    assert_eq!(
        app.package_import_prepare(&importer, &archive)
            .err()
            .unwrap()
            .category,
        "TargetPickerBusy"
    );
    assert_eq!(
        app.package_import_publish(&importer, &review.token)
            .unwrap_err()
            .category,
        "TargetPickerBusy"
    );
    drop(guard);
    assert!(
        app.package_import_publish(&importer, &review.token)
            .unwrap()
            .committed
    );
}
