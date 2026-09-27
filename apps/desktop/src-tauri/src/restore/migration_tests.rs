use super::*;
use crate::identity_migrations::tests::{fixture, normalized, old_id};
use crate::identity_migrations::{self, LEDGER, Operation};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub(crate) fn interrupt_migration(root: &Path) {
    let before = capture(root).unwrap();
    let after = normalized(&before);
    assert!(
        catch_unwind(AssertUnwindSafe(|| publish(
            root,
            before,
            after,
            Operation::IdentityMigration,
            &mut |point| {
                if point == Point::AfterJournal {
                    panic!("simulated process exit after durable migration plan");
                }
                Ok(())
            }
        )))
        .is_err()
    );
    assert!(pending(root).unwrap());
}

fn assignment_bytes(root: &Path) -> Vec<u8> {
    let directory = root.join(JOURNAL);
    let journal: Journal =
        decode(&read_bytes(&directory.join("journal.json"), MAX_JOURNAL).unwrap()).unwrap();
    assert_eq!(journal.version, 2);
    assert_eq!(journal.operation, Some(Operation::IdentityMigration));
    load_capture(&directory, "new", &journal.after)
        .unwrap()
        .files[LEDGER]
        .clone()
}

#[test]
fn migration_failure_at_each_publication_boundary_rolls_back_content_but_keeps_assignments() {
    for selected in 0..4 {
        let (root, before) = fixture();
        let after = normalized(&before);
        let paths: BTreeSet<_> = before
            .files
            .keys()
            .chain(after.files.keys())
            .cloned()
            .collect();
        let profile_index = paths
            .iter()
            .position(|path| path.starts_with("tabs/Alpha/other/") && !path.contains("p-"))
            .unwrap();
        let target_index = paths
            .iter()
            .position(|path| path == "tabs/Alpha/pkg/target.config")
            .unwrap();
        let point = match selected {
            0 => Point::AfterJournal,
            1 => Point::Installed {
                index: profile_index,
                rollback: false,
            },
            2 => Point::Installed {
                index: target_index,
                rollback: false,
            },
            _ => Point::Verified { rollback: false },
        };
        let error = publish(
            &root.0,
            before.clone(),
            after.clone(),
            Operation::IdentityMigration,
            &mut |at| {
                if at == point {
                    Err(invalid("injected migration publication failure"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.context["rolled_back"], true);
        assert!(!pending(&root.0).unwrap());
        let expected = identity_migrations::rollback_capture(&before, &after).unwrap();
        assert_eq!(capture(&root.0).unwrap(), expected);
        identity_migrations::migrate(&root.0).unwrap();
        assert_eq!(capture(&root.0).unwrap(), after);
    }
}

#[test]
fn restart_uses_durable_assignments_and_refuses_changed_preimages() {
    let (root, before) = fixture();
    interrupt_migration(&root.0);
    let assignments = assignment_bytes(&root.0);
    let path = root.0.join(format!("tabs/Alpha/pkg/{}.config", old_id(1)));
    let original = fs::read(&path).unwrap();
    fs::write(&path, b"external edit must survive").unwrap();
    assert!(recover(&root.0, false).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"external edit must survive");
    assert!(pending(&root.0).unwrap());
    assert_eq!(assignment_bytes(&root.0), assignments);
    fs::write(path, original).unwrap();
    recover(&root.0, true).unwrap();
    let restored = capture(&root.0).unwrap();
    for (path, bytes) in &before.files {
        assert_eq!(&restored.files[path], bytes);
    }
    assert_eq!(restored.files[LEDGER], assignments);
    identity_migrations::migrate(&root.0).unwrap();
    assert_eq!(capture(&root.0).unwrap().files[LEDGER], assignments);
}

#[test]
fn real_target_install_failure_retains_partial_migration_for_restart() {
    let (root, before) = fixture();
    let after = normalized(&before);
    let paths: BTreeSet<_> = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect();
    let target_index = paths
        .iter()
        .position(|path| path == "tabs/Alpha/pkg/target.config")
        .unwrap();
    let target = root.0.join("tabs/Alpha/pkg/target.config");
    let error = publish(
        &root.0,
        before,
        after.clone(),
        Operation::IdentityMigration,
        &mut |point| {
            if point
                == (Point::Displaced {
                    index: target_index,
                    rollback: false,
                })
            {
                configuration::create_private_directory(&target)?;
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.category, "ConfigurationIo");
    assert_eq!(error.context["pending_restore"], true);
    assert!(target.is_dir());
    assert!(pending(&root.0).unwrap());
    assert_eq!(fs::read(root.0.join(LEDGER)).unwrap(), after.files[LEDGER]);
    fs::remove_dir(target).unwrap();
    recover(&root.0, false).unwrap();
    assert_eq!(capture(&root.0).unwrap(), after);
    assert!(!pending(&root.0).unwrap());
}

#[test]
fn reservation_preservation_failure_keeps_journal_until_validated_rollback() {
    let (root, before) = fixture();
    let after = normalized(&before);
    let ledger = root.0.join(LEDGER);
    let error = publish(
        &root.0,
        before.clone(),
        after.clone(),
        Operation::IdentityMigration,
        &mut |point| {
            if point == Point::AfterJournal {
                configuration::create_private_directory(&ledger)?;
                return Err(invalid("fail before reservation publication"));
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.context["pending_restore"], true);
    assert!(pending(&root.0).unwrap());
    assert_ne!(error.context["rolled_back"], true);
    assert_eq!(assignment_bytes(&root.0), after.files[LEDGER]);
    fs::remove_dir(ledger).unwrap();
    recover(&root.0, true).unwrap();
    assert_eq!(
        capture(&root.0).unwrap(),
        identity_migrations::rollback_capture(&before, &after).unwrap()
    );
}

#[test]
fn committed_migration_cleanup_only_resumes_in_its_original_direction() {
    let (root, before) = fixture();
    let after = normalized(&before);
    assert!(
        catch_unwind(AssertUnwindSafe(|| publish(
            &root.0,
            before,
            after.clone(),
            Operation::IdentityMigration,
            &mut |point| {
                if point == (Point::CleanupRemoved { index: 0 }) {
                    panic!("exit after committed partial cleanup");
                }
                Ok(())
            }
        )))
        .is_err()
    );
    assert!(pending(&root.0).unwrap());
    assert_eq!(capture(&root.0).unwrap(), after);
    assert!(recover(&root.0, true).is_err());
    assert_eq!(capture(&root.0).unwrap(), after);
    recover(&root.0, false).unwrap();
    assert_eq!(capture(&root.0).unwrap(), after);
    assert!(!pending(&root.0).unwrap());
}

#[test]
fn malformed_or_ambiguous_migration_journal_never_guesses_a_direction() {
    for value in [json!(3), json!(1)] {
        let (root, before) = fixture();
        interrupt_migration(&root.0);
        let path = root.0.join(JOURNAL).join("journal.json");
        let mut journal: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        journal["version"] = value;
        let malformed = serde_json::to_vec(&journal).unwrap();
        fs::write(&path, &malformed).unwrap();
        assert!(recover(&root.0, false).is_err());
        assert!(recover(&root.0, true).is_err());
        assert_eq!(fs::read(&path).unwrap(), malformed);
        assert_eq!(capture(&root.0).unwrap(), before);
        assert!(pending(&root.0).unwrap());
    }
}

#[test]
fn repeated_legacy_restore_preserves_archive_and_live_lineage_across_rollback() {
    let (source, original) = fixture();
    let receipt = crate::backup::write(&source.0, original.clone(), None).unwrap();
    let archive_path = Path::new(&receipt.path);
    let archive_bytes = fs::read(archive_path).unwrap();
    let destination = crate::configuration::tests::Root::new();
    install(
        &destination.0,
        crate::backup::read(archive_path).unwrap(),
        None,
    )
    .unwrap();
    let first = capture(&destination.0).unwrap();
    install(
        &destination.0,
        crate::backup::read(archive_path).unwrap(),
        Some(&first.generation),
    )
    .unwrap();
    assert_eq!(capture(&destination.0).unwrap(), first);
    let error = install_with(
        &destination.0,
        original,
        Some(&first.generation),
        &mut |point| {
            if point == Point::AfterJournal {
                Err(invalid("refuse after assignments are durable"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.context["rolled_back"], true);
    assert_eq!(capture(&destination.0).unwrap(), first);
    assert_eq!(fs::read(archive_path).unwrap(), archive_bytes);
    install(&destination.0, first.clone(), Some(&first.generation)).unwrap();
    assert_eq!(capture(&destination.0).unwrap(), first);
}

#[cfg(unix)]
#[test]
fn real_partial_migration_journal_write_leaves_original_generation_admissible() {
    const CHILD: &str = "MADO_IDENTITY_JOURNAL_FILE_LIMIT";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("sh")
            .args(["-c", "trap '' XFSZ; ulimit -f 128 || exit; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "restore::migration_tests::real_partial_migration_journal_write_leaves_original_generation_admissible", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = crate::configuration::tests::Root::new();
    root.put(
        "settings.json",
        &serde_json::to_vec(&Settings::default()).unwrap(),
    );
    for index in 0..8 {
        let tab = format!("Owner{index}");
        let record = TabRecord {
            version: 1,
            internal_name: tab.clone(),
            display_name: tab.clone(),
            open: true,
            packages: vec![crate::storage::PackageReference {
                package_id: "pkg".into(),
                source: crate::storage::PackageSource::Directory {
                    path: root.0.join("sources/pkg").to_str().unwrap().into(),
                },
            }],
            selected_package_id: Some("pkg".into()),
        };
        root.put(
            &format!("tabs/{tab}/tab.config"),
            &serde_json::to_vec(&record).unwrap(),
        );
        for sequence in 0..60 {
            let n = index * 60 + sequence;
            let id = if n == 0 {
                old_id(1)
            } else {
                format!("{n:019x}0")
            };
            let profile = Profile {
                version: 1,
                id: id.clone(),
                name: "Preserved".into(),
                package_id: "pkg".into(),
                schema_identity: "a".repeat(64),
                values: json!({}),
            };
            root.put(
                &format!("tabs/{tab}/pkg/{id}.config"),
                &serde_json::to_vec(&profile).unwrap(),
            );
        }
    }
    let before = capture(&root.0).unwrap();
    let error = identity_migrations::migrate(&root.0).unwrap_err();
    assert_eq!(error.category, "ConfigurationIo");
    assert_eq!(error.context["kind"], "FileTooLarge");
    assert!(!pending(&root.0).unwrap());
    assert_eq!(capture(&root.0).unwrap(), before);
    assert!(!root.0.join(LEDGER).exists());
}

#[test]
fn rollback_reuses_pretty_archive_ledger_bytes_after_interruption() {
    let (root, before) = fixture();
    let mut files = normalized(&before).files;
    let ledger: serde_json::Value = serde_json::from_slice(&files[LEDGER]).unwrap();
    let pretty = serde_json::to_vec_pretty(&ledger).unwrap();
    files.insert(LEDGER.into(), pretty.clone());
    let after = Capture::from_files(files, true).unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| publish(
            &root.0,
            before.clone(),
            after.clone(),
            Operation::Restore,
            &mut |point| {
                if point == Point::AfterJournal {
                    return Err(invalid("fail before installing archive"));
                }
                if point
                    == (Point::Installed {
                        index: 0,
                        rollback: true,
                    })
                {
                    panic!("exit after preserving archive reservations during rollback");
                }
                Ok(())
            }
        )))
        .is_err()
    );
    assert!(pending(&root.0).unwrap());
    assert_eq!(fs::read(root.0.join(LEDGER)).unwrap(), pretty);
    recover(&root.0, true).unwrap();
    let restored = capture(&root.0).unwrap();
    assert_eq!(restored.files[LEDGER], pretty);
    for (path, bytes) in &before.files {
        assert_eq!(&restored.files[path], bytes);
    }
    assert!(!pending(&root.0).unwrap());
}

#[test]
fn conflicting_archive_lineage_refuses_before_live_mutation() {
    let (root, original) = fixture();
    identity_migrations::migrate(&root.0).unwrap();
    let before = capture(&root.0).unwrap();
    let foreign = normalized(&original);
    let source = crate::configuration::tests::Root::new();
    for (path, bytes) in &foreign.files {
        source.put(path, bytes);
    }
    let archive = crate::backup::write(&source.0, foreign, None).unwrap();
    let archive_bytes = fs::read(&archive.path).unwrap();
    assert!(
        install(
            &root.0,
            crate::backup::read(Path::new(&archive.path)).unwrap(),
            Some(&before.generation)
        )
        .is_err()
    );
    assert_eq!(capture(&root.0).unwrap(), before);
    assert_eq!(fs::read(&archive.path).unwrap(), archive_bytes);
    assert!(!pending(&root.0).unwrap());
}
