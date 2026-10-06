use super::*;
use crate::configuration::tests::Root;
use crate::identity_migrations::tests::{fixture, normalized, old_id};
use crate::identity_migrations::{self, LEDGER};

fn bootstrap(root: &Path) -> Bootstrap {
    Bootstrap::new(
        Ok(root.to_path_buf()),
        None,
        root.join("no-runner"),
        root.join("no-engine"),
    )
}

#[test]
fn loading_converts_before_application_construction_and_failed_construction_keeps_ids() {
    fn failed(
        root: PathBuf,
        _: PathBuf,
        _: PathBuf,
        _: Arc<Mutex<ObservationSlot>>,
    ) -> Result<Arc<Application>, Fault> {
        let captured = configuration::capture(&root)?;
        assert!(captured.files.contains_key(LEDGER));
        restore::validate_installed(&captured)?;
        Err(Fault::new(
            "MigrationConstructionTest",
            "refused after identifier conversion",
        ))
    }
    let (root, before) = fixture();
    let mut app = bootstrap(&root.0);
    app.make_application = failed;
    assert!(matches!(app.status().state, Phase::Loading));
    assert!(!root.0.join(LEDGER).exists());
    let status = app.ensure_started().unwrap();
    assert!(matches!(status.state, Phase::Recovery));
    assert_eq!(status.stage, "application");
    assert!(!status.application_available);
    assert!(!root.0.join("logs").exists());
    let committed = configuration::capture(&root.0).unwrap();
    assert_ne!(committed.generation, before.generation);
    app.make_application = Application::with_observation_slot;
    assert!(matches!(app.retry(false).unwrap().state, Phase::Ready));
    assert_eq!(configuration::capture(&root.0).unwrap(), committed);
    let target = Store::new(root.0.clone())
        .unwrap()
        .read_target("Alpha", "pkg")
        .unwrap();
    assert_eq!(target.revision, 7);
    assert_eq!(
        target
            .compare(&crate::target::TargetExpectation {
                revision: 7,
                binding_id: Some(old_id(1))
            })
            .unwrap_err()
            .category,
        "TargetConflict"
    );
    app.shutdown().unwrap();
}

#[test]
fn unresolved_migration_has_recovery_exit_and_no_application_or_polling_owner() {
    let (root, _) = fixture();
    crate::restore::migration_tests::interrupt_migration(&root.0);
    let app = bootstrap(&root.0);
    let status = app.ensure_started().unwrap();
    assert!(matches!(status.state, Phase::Recovery));
    assert!(status.pending_restore);
    assert!(!status.application_available);
    assert!(app.application().is_err());
    assert!(app.running_application().is_err());
    assert!(!root.0.join("logs").exists());
    app.shutdown().unwrap();
    let reopened = bootstrap(&root.0);
    assert!(matches!(
        reopened.ensure_started().unwrap().state,
        Phase::Recovery
    ));
    assert_eq!(
        reopened
            .recover_restore(false, false, false)
            .err()
            .unwrap()
            .category,
        "RestoreConfirmation"
    );
    let status = reopened.recover_restore(false, true, false).unwrap();
    assert!(matches!(status.state, Phase::Ready));
    assert!(!status.pending_restore);
    assert!(status.application_available);
    reopened.shutdown().unwrap();
}

#[test]
fn historical_staging_normalizes_owned_records_but_never_assigns_unassigned_sources() {
    let (source, original) = fixture();
    let parent = Root::new();
    let destination = parent.0.join("imported");
    import_legacy(&source.0, &destination).unwrap();
    let installed = configuration::capture(&destination).unwrap();
    restore::validate_installed(&installed).unwrap();
    assert!(installed.files.contains_key(LEDGER));
    assert_eq!(
        installed.files[&format!("profiles/{}.json", old_id(1))],
        original.files[&format!("profiles/{}.json", old_id(1))]
    );
    assert_eq!(configuration::capture(&source.0).unwrap(), original);
    assert!(!destination.join("sources").exists());
    assert_eq!(
        fs::read(source.0.join("sources/pkg/main.ts")).unwrap(),
        old_id(1).as_bytes()
    );
    let before = configuration::capture(&destination).unwrap();
    assert!(import_legacy(&source.0, &destination).is_err());
    assert_eq!(configuration::capture(&destination).unwrap(), before);
}

#[test]
fn historical_import_refuses_appearing_destination_and_changed_source_after_staging() {
    let (source, original) = fixture();
    let parent = Root::new();
    let destination = parent.0.join("appeared");
    assert!(
        import_legacy_with(&source.0, &destination, || {
            storage::private_directory(&destination).unwrap();
            configuration::write_private(
                &destination.join("operator.txt"),
                b"destination appeared",
            )
            .unwrap();
        })
        .is_err()
    );
    assert_eq!(
        fs::read(destination.join("operator.txt")).unwrap(),
        b"destination appeared"
    );
    assert!(!destination.join(LEDGER).exists());
    assert_eq!(configuration::capture(&source.0).unwrap(), original);
    let changed = parent.0.join("changed");
    assert!(
        import_legacy_with(&source.0, &changed, || {
            fs::write(
                source.0.join("settings.json"),
                serde_json::to_vec(&Settings {
                    gui_log_limit: 1200,
                    ..Settings::default()
                })
                .unwrap(),
            )
            .unwrap();
        })
        .is_err()
    );
    assert!(!changed.exists());
    assert_ne!(configuration::capture(&source.0).unwrap(), original);
}

#[test]
fn legacy_restore_keeps_receipt_disposal_and_stale_session_authority_separate() {
    let (root, original) = fixture();
    let archive = backup::write(&root.0, original, None).unwrap();
    let archive_bytes = fs::read(&archive.path).unwrap();
    let app = bootstrap(&root.0);
    app.ensure_started().unwrap();
    let previous = app.application().unwrap();
    let catalog = previous.workspace_catalog().unwrap();
    let old_workspace = crate::application::WorkspaceRef {
        workspace_id: catalog.open[0].workspace_id.clone(),
        revision: catalog.open[0].revision,
    };
    assert_eq!(
        app.restore_snapshot(Path::new(&archive.path), None, true, true)
            .err()
            .unwrap()
            .category,
        "PreimageRequired"
    );
    let preimages = Root::new();
    let receipt = app.snapshot(Some(&preimages.0)).unwrap();
    assert_eq!(
        app.restore_snapshot(
            Path::new(&archive.path),
            Some(&receipt.generation),
            true,
            false
        )
        .err()
        .unwrap()
        .category,
        "DiscardRequired"
    );
    let before = configuration::capture(&root.0).unwrap();
    let restored = app
        .restore_snapshot(
            Path::new(&archive.path),
            Some(&receipt.generation),
            true,
            true,
        )
        .unwrap();
    assert!(matches!(restored.state, Phase::Ready));
    assert_eq!(configuration::capture(&root.0).unwrap(), before);
    assert_eq!(
        previous.workspace_catalog().unwrap_err().category,
        "Closing"
    );
    let current = app.application().unwrap();
    assert_eq!(
        current.profiles(&old_workspace).unwrap_err().category,
        "StaleIdentity"
    );
    assert_eq!(fs::read(&archive.path).unwrap(), archive_bytes);
    app.shutdown().unwrap();
}

#[test]
fn foreign_restore_lineage_refusal_preserves_the_ready_application_and_workspace() {
    let (root, original) = fixture();
    let foreign = normalized(&original);
    let source = Root::new();
    for (path, bytes) in &foreign.files {
        source.put(path, bytes);
    }
    let archive = backup::write(&source.0, foreign, None).unwrap();
    let archive_bytes = fs::read(&archive.path).unwrap();
    let app = bootstrap(&root.0);
    assert!(matches!(app.ensure_started().unwrap().state, Phase::Ready));
    let previous = app.application().unwrap();
    let catalog = previous.workspace_catalog().unwrap();
    let workspace = crate::application::WorkspaceRef {
        workspace_id: catalog.open[0].workspace_id.clone(),
        revision: catalog.open[0].revision,
    };
    let before = previous.capture_configuration().unwrap();
    let preimages = Root::new();
    let receipt = app.snapshot(Some(&preimages.0)).unwrap();
    let error = app
        .restore_snapshot(
            Path::new(&archive.path),
            Some(&receipt.generation),
            true,
            true,
        )
        .err()
        .unwrap();
    assert_eq!(error.category, "IdentityMigration");
    let status = app.status();
    assert!(matches!(status.state, Phase::Ready));
    assert!(status.application_available);
    assert!(!status.pending_restore);
    assert!(Arc::ptr_eq(&previous, &app.application().unwrap()));
    assert_eq!(previous.capture_configuration().unwrap(), before);
    assert_eq!(fs::read(&archive.path).unwrap(), archive_bytes);
    previous.close_workspace(&workspace).unwrap();
    assert!(
        previous
            .workspace_catalog()
            .unwrap()
            .open
            .iter()
            .all(|open| open.workspace_id != workspace.workspace_id)
    );
    app.shutdown().unwrap();
}

#[test]
fn restore_rollback_budget_refusal_does_not_retire_the_ready_session() {
    let (source, incoming) = fixture();
    let archive = backup::write(&source.0, incoming.clone(), None).unwrap();
    let root = Root::new();
    for path in ["settings.json", "tabs/Alpha/tab.config"] {
        root.put(path, &incoming.files[path]);
    }
    // Raw preservation admits these unassigned recovery records. Retaining the
    // incoming identity ledger on rollback would require one more managed file.
    for index in 0..configuration::MAX_FILES - 2 {
        root.put(&format!("profiles/{index:020}.json"), b"unreadable source");
    }
    let app = bootstrap(&root.0);
    assert!(matches!(app.ensure_started().unwrap().state, Phase::Ready));
    let previous = app.application().unwrap();
    let catalog = previous.workspace_catalog().unwrap();
    let workspace = crate::application::WorkspaceRef {
        workspace_id: catalog.open[0].workspace_id.clone(),
        revision: catalog.open[0].revision,
    };
    let before = previous.capture_configuration().unwrap();
    assert_eq!(before.files.len(), configuration::MAX_FILES);
    assert!(!before.files.contains_key(LEDGER));
    let preimages = Root::new();
    let receipt = app.snapshot(Some(&preimages.0)).unwrap();
    let error = app
        .restore_snapshot(
            Path::new(&archive.path),
            Some(&receipt.generation),
            true,
            true,
        )
        .err()
        .unwrap();
    assert_eq!(error.category, "IdentityMigration");
    assert!(matches!(app.status().state, Phase::Ready));
    assert!(!app.status().pending_restore);
    assert!(Arc::ptr_eq(&previous, &app.application().unwrap()));
    assert_eq!(previous.capture_configuration().unwrap(), before);
    previous.close_workspace(&workspace).unwrap();
    app.shutdown().unwrap();
}

#[test]
fn current_scoped_profile_failure_does_not_become_migration_recovery() {
    let (root, _) = fixture();
    identity_migrations::migrate(&root.0).unwrap();
    root.put(
        "tabs/Alpha/pkg/00000000000000000000.config",
        b"scoped unreadable profile",
    );
    let before = configuration::capture(&root.0).unwrap();
    let app = bootstrap(&root.0);
    let status = app.ensure_started().unwrap();
    assert!(matches!(status.state, Phase::Ready));
    assert!(
        Store::new(root.0.clone())
            .unwrap()
            .profile_store("Alpha", "pkg")
            .unwrap()
            .recovery_records()
            .is_err()
    );
    assert_eq!(configuration::capture(&root.0).unwrap(), before);
    app.shutdown().unwrap();
}
