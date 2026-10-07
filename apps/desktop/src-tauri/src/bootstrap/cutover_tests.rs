use super::*;
use crate::configuration::{HISTORICAL_LEDGER, tests::Root};
use crate::restore::cutover_tests::{fixture, install_old_record, interrupt_restore, old_id};

fn bootstrap(root: &Path) -> Bootstrap {
    Bootstrap::new(
        Ok(root.to_path_buf()),
        None,
        root.join("no-runner"),
        root.join("no-engine"),
    )
}

#[test]
fn startup_keeps_old_record_faults_scoped_and_never_converts_or_rewrites() {
    for target in [false, true] {
        let (root, _) = fixture();
        install_old_record(&root, target);
        root.put(HISTORICAL_LEDGER, b"unparsed ledger\xff");
        let before = configuration::capture(&root.0).unwrap();
        let app = bootstrap(&root.0);
        let status = app.ensure_started().unwrap();
        assert!(matches!(status.state, Phase::Ready));
        assert!(!status.pending_restore);
        assert!(!status.recovery_supported);
        let store = Store::new(root.0.clone()).unwrap();
        if target {
            assert_eq!(
                store.read_target("Alpha", "pkg").unwrap_err().category,
                "TargetConfiguration"
            );
        } else {
            let owner = store.profile_store("Alpha", "pkg").unwrap();
            assert!(owner.list("pkg", &"a".repeat(64)).is_err());
            assert!(owner.recovery_records().is_err());
        }
        let other = store.profile_store("Beta", "pkg").unwrap();
        assert_eq!(
            other.list("pkg", &"a".repeat(64)).unwrap().profiles.len(),
            1
        );
        let null = store.read_target("Beta", "pkg").unwrap();
        assert_eq!(null.revision, 11);
        assert!(null.binding.is_none());
        assert_eq!(configuration::capture(&root.0).unwrap(), before);
        app.shutdown().unwrap();
        let reopened = bootstrap(&root.0);
        assert!(matches!(
            reopened.ensure_started().unwrap().state,
            Phase::Ready
        ));
        assert_eq!(configuration::capture(&root.0).unwrap(), before);
        reopened.shutdown().unwrap();
    }
}

#[test]
fn historical_installation_preserves_inactive_bytes_and_refuses_old_active_records() {
    let (source, _) = fixture();
    source.put(HISTORICAL_LEDGER, b"malformed historical ledger\0");
    // A source basename does not change the historical-profile byte budget.
    source.put("profiles/settings.json", &vec![b'x'; 48 * 1024]);
    let original = configuration::capture(&source.0).unwrap();
    let parent = Root::new();
    let destination = parent.0.join("imported");
    import_legacy(&source.0, &destination).unwrap();
    assert_eq!(configuration::capture(&destination).unwrap(), original);
    assert_eq!(configuration::capture(&source.0).unwrap(), original);
    assert!(!destination.join("sources").exists());
    assert_eq!(
        fs::read(source.0.join("sources/pkg/main.ts")).unwrap(),
        old_id(1).as_bytes()
    );
    let app = bootstrap(&destination);
    assert!(matches!(app.ensure_started().unwrap().state, Phase::Ready));
    assert_eq!(configuration::capture(&destination).unwrap(), original);
    app.shutdown().unwrap();
    for target in [false, true] {
        let (old, _) = fixture();
        install_old_record(&old, target);
        let before = configuration::capture(&old.0).unwrap();
        let refused = parent.0.join(format!("refused-{target}"));
        assert!(import_legacy(&old.0, &refused).is_err());
        assert!(!refused.exists());
        assert_eq!(configuration::capture(&old.0).unwrap(), before);
    }
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
fn old_active_restore_refuses_before_retiring_a_usable_application() {
    for target in [false, true] {
        let (source, _) = fixture();
        install_old_record(&source, target);
        let archive =
            backup::write(&source.0, configuration::capture(&source.0).unwrap(), None).unwrap();
        let archive_bytes = fs::read(&archive.path).unwrap();
        let (root, before) = fixture();
        let app = bootstrap(&root.0);
        assert!(matches!(app.ensure_started().unwrap().state, Phase::Ready));
        let previous = app.application().unwrap();
        let catalog = previous.workspace_catalog().unwrap();
        let workspace = crate::application::WorkspaceRef {
            workspace_id: catalog.open[0].workspace_id.clone(),
            revision: catalog.open[0].revision,
        };
        let preimages = Root::new();
        let receipt = app.snapshot(Some(&preimages.0)).unwrap();
        assert!(
            app.restore_snapshot(
                Path::new(&archive.path),
                Some(&receipt.generation),
                true,
                true
            )
            .is_err()
        );
        assert!(matches!(app.status().state, Phase::Ready));
        assert!(Arc::ptr_eq(&previous, &app.application().unwrap()));
        assert_eq!(previous.capture_configuration().unwrap(), before);
        assert_eq!(fs::read(&archive.path).unwrap(), archive_bytes);
        previous.close_workspace(&workspace).unwrap();
        app.shutdown().unwrap();
    }
}

#[test]
fn current_restore_keeps_receipt_disposal_and_stale_session_authority_separate() {
    let (root, original) = fixture();
    root.put(HISTORICAL_LEDGER, b"displaced opaque bytes");
    let archive_root = Root::new();
    let archive = backup::write(&root.0, original.clone(), Some(&archive_root.0)).unwrap();
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
        backup::read(Path::new(&receipt.path)).unwrap().files[HISTORICAL_LEDGER],
        b"displaced opaque bytes"
    );
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
    let restored = app
        .restore_snapshot(
            Path::new(&archive.path),
            Some(&receipt.generation),
            true,
            true,
        )
        .unwrap();
    assert!(matches!(restored.state, Phase::Ready));
    assert_eq!(configuration::capture(&root.0).unwrap(), original);
    assert_eq!(
        previous.workspace_catalog().unwrap_err().category,
        "Closing"
    );
    assert_eq!(
        app.application()
            .unwrap()
            .profiles(&old_workspace)
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(fs::read(&archive.path).unwrap(), archive_bytes);
    app.shutdown().unwrap();
}

#[test]
fn unsupported_pending_evidence_has_exit_retry_and_no_in_version_recovery_or_replacement() {
    for marker in [
        ".restore-journal/journal.json",
        ".restore-completion",
        "identity-migrations.pending",
    ] {
        let (root, before) = fixture();
        let evidence =
            br#"{"version":2,"operation":{"kind":"identity_migration"},"retained":"evidence"}"#;
        root.put(marker, evidence);
        let app = bootstrap(&root.0);
        let status = app.ensure_started().unwrap();
        assert!(matches!(status.state, Phase::Recovery));
        assert!(status.pending_restore);
        assert!(!status.recovery_supported);
        assert!(!status.application_available);
        assert_eq!(status.fault.unwrap().category, "RestoreUnsupported");
        assert!(app.application().is_err());
        assert!(app.running_application().is_err());
        assert!(!root.0.join("logs").exists());
        for rollback in [false, true] {
            assert_eq!(
                app.recover_restore(rollback, true, true)
                    .err()
                    .unwrap()
                    .category,
                "RestoreUnsupported"
            );
        }
        let preferences = EditableSettings {
            locale: storage::Locale::English,
            gui_log_limit: 1000,
            ocr_environment: None,
            notifications: storage::NotificationPreferences::default(),
            editor_completion: storage::EditorCompletionPreferences::default(),
            backup_directory: None,
            packages_root: None,
            capture_cache_enabled: false,
        };
        assert_eq!(
            app.initialize(preferences, true).err().unwrap().category,
            "RestorePending"
        );
        assert_eq!(app.snapshot(None).unwrap_err().category, "RestorePending");
        assert_eq!(
            app.restore_snapshot(&root.0.join("unused-archive"), None, true, true)
                .err()
                .unwrap()
                .category,
            "RestorePending"
        );
        assert!(!app.retry(true).unwrap().recovery_supported);
        let destination = root.0.join("refused-historical-import");
        assert!(import_legacy(&root.0, &destination).is_err());
        assert!(!destination.exists());
        for (path, bytes) in &before.files {
            assert_eq!(fs::read(root.0.join(path)).unwrap(), *bytes);
        }
        assert_eq!(fs::read(root.0.join(marker)).unwrap(), evidence);
        app.shutdown().unwrap();
        assert_eq!(fs::read(root.0.join(marker)).unwrap(), evidence);
    }
}

#[test]
fn supported_v3_recovery_is_available_but_an_old_completion_overrides_a_current_journal() {
    let (root, before) = fixture();
    let mut files = before.files.clone();
    files.insert(HISTORICAL_LEDGER.into(), b"selected ledger".to_vec());
    let after = configuration::Capture::from_files(files, true).unwrap();
    interrupt_restore(&root.0, after.clone());
    let app = bootstrap(&root.0);
    let status = app.ensure_started().unwrap();
    assert!(status.pending_restore);
    assert!(status.recovery_supported);
    assert!(!status.application_available);
    let marker = br#"{"version":1,"rollback":false,"target":{}}"#;
    root.put(".restore-completion", marker);
    assert!(!app.status().recovery_supported);
    assert_eq!(
        app.recover_restore(false, true, false)
            .err()
            .unwrap()
            .category,
        "RestoreUnsupported"
    );
    assert_eq!(
        fs::read(root.0.join(".restore-completion")).unwrap(),
        marker
    );
    assert_eq!(configuration::capture(&root.0).unwrap(), before);
    fs::remove_file(root.0.join(".restore-completion")).unwrap();
    let recovered = app.recover_restore(false, true, false).unwrap();
    assert!(matches!(recovered.state, Phase::Ready));
    assert!(!recovered.pending_restore);
    assert!(!recovered.recovery_supported);
    assert_eq!(configuration::capture(&root.0).unwrap(), after);
    app.shutdown().unwrap();
}
