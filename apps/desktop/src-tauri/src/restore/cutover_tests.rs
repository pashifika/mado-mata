use super::*;
use crate::configuration::{HISTORICAL_LEDGER, tests::Root};
use crate::storage::{PackageReference, PackageSource, Store};
use crate::target::{ResolvedLocation, TargetBinding, TargetResolution};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub(crate) fn old_id(sequence: u64) -> String {
    format!("p-{sequence:032x}-00000001-0000000000000001")
}

pub(crate) fn fixture() -> (Root, Capture) {
    let root = Root::new();
    root.put(
        "settings.json",
        &serde_json::to_vec(&Settings::default()).unwrap(),
    );
    for tab in ["Alpha", "Beta"] {
        let owner = TabRecord {
            version: 1,
            internal_name: tab.into(),
            display_name: tab.into(),
            open: true,
            packages: vec![PackageReference {
                package_id: "pkg".into(),
                source: PackageSource::Directory {
                    path: root.0.join("sources/pkg").to_str().unwrap().into(),
                },
            }],
            selected_package_id: Some("pkg".into()),
        };
        root.put(
            &format!("tabs/{tab}/tab.config"),
            &serde_json::to_vec(&owner).unwrap(),
        );
        let profile = Profile {
            version: 1,
            id: "00000000000000000000".into(),
            name: "Preserved profile".into(),
            package_id: "pkg".into(),
            schema_identity: "a".repeat(64),
            values: json!({"label": old_id(1), "source": format!("const alias = '{}';", old_id(1))}),
        };
        root.put(
            &format!("tabs/{tab}/pkg/{}.config", profile.id),
            &serde_json::to_vec_pretty(&profile).unwrap(),
        );
    }
    let path = format!("/offline/{}", old_id(1));
    let target = TargetRecord {
        version: 1,
        internal_name: "Alpha".into(),
        package_id: "pkg".into(),
        revision: 7,
        binding: Some(TargetBinding {
            id: "00000000000000000010".into(),
            package_id: "pkg".into(),
            target_id: "metadata-game".into(),
            declaration_identity: crate::target::tests::declaration().identity().unwrap(),
            configuration: crate::target::tests::configuration(&path),
            resolution: TargetResolution {
                game: ResolvedLocation {
                    path: path.clone(),
                    executable: path,
                },
                launcher: None,
                working_directory: None,
            },
        }),
    };
    root.put(
        "tabs/Alpha/pkg/target.config",
        &serde_json::to_vec_pretty(&target).unwrap(),
    );
    let empty = TargetRecord {
        version: 1,
        internal_name: "Beta".into(),
        package_id: "pkg".into(),
        revision: 11,
        binding: None,
    };
    root.put(
        "tabs/Beta/pkg/target.config",
        &serde_json::to_vec_pretty(&empty).unwrap(),
    );
    let mut original: Profile =
        decode(&fs::read(root.0.join("tabs/Alpha/pkg/00000000000000000000.config")).unwrap())
            .unwrap();
    original.id = old_id(1);
    root.put(
        &format!("profiles/{}.json", original.id),
        &serde_json::to_vec_pretty(&original).unwrap(),
    );
    // Inactive source preservation also accepts bytes that are not JSON at all.
    root.put(
        "profiles/unreadable.json",
        b"unsupported old source\0original",
    );
    root.put("sources/pkg/main.ts", old_id(1).as_bytes());
    root.put("sources/pkg/assets/r1.png", b"existing asset bytes");
    let before = capture(&root.0).unwrap();
    (root, before)
}

pub(crate) fn install_old_record(root: &Root, target: bool) {
    if target {
        let path = root.0.join("tabs/Alpha/pkg/target.config");
        let mut record: TargetRecord = decode(&fs::read(&path).unwrap()).unwrap();
        record.binding.as_mut().unwrap().id = old_id(1);
        fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    } else {
        let path = root.0.join("tabs/Alpha/pkg/00000000000000000000.config");
        let mut profile: Profile = decode(&fs::read(&path).unwrap()).unwrap();
        profile.id = old_id(1);
        let destination = root.0.join(format!("tabs/Alpha/pkg/{}.config", profile.id));
        fs::rename(path, &destination).unwrap();
        fs::write(destination, serde_json::to_vec_pretty(&profile).unwrap()).unwrap();
    }
}

pub(crate) fn interrupt_restore(root: &Path, after: Capture) {
    let before = capture(root).unwrap();
    let plan = prepare(after, before.clone(), Some(&before.generation)).unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| install_with(
            root,
            plan,
            &mut |point| {
                if point == Point::AfterJournal {
                    panic!("exit after publishing version-3 journal");
                }
                Ok(())
            }
        )))
        .is_err()
    );
}

#[test]
fn archive_versions_preserve_inactive_sources_and_exact_selected_ledger_or_absence() {
    for live_ledger in [None, Some(b"live malformed ledger\0".as_slice())] {
        for selected_ledger in [
            None,
            Some(b"different archive ledger\xff".as_slice()),
            live_ledger,
        ] {
            let (root, _) = fixture();
            if let Some(bytes) = live_ledger {
                root.put(HISTORICAL_LEDGER, bytes);
            }
            let before = capture(&root.0).unwrap();
            let preimage = crate::backup::write(&root.0, before.clone(), None).unwrap();
            let mut files = before.files.clone();
            match selected_ledger {
                Some(bytes) => {
                    files.insert(HISTORICAL_LEDGER.into(), bytes.to_vec());
                }
                None => {
                    files.remove(HISTORICAL_LEDGER);
                }
            }
            let after = Capture::from_files(files, true).unwrap();
            validate(&after).unwrap();
            assert_eq!(
                serde_json::to_value(Manifest::new(&after)).unwrap()["version"],
                if selected_ledger.is_some() { 2 } else { 1 }
            );
            let archives = Root::new();
            let archive = crate::backup::write(&root.0, after.clone(), Some(&archives.0)).unwrap();
            let archive_path = Path::new(&archive.path);
            let archive_bytes = fs::read(archive_path).unwrap();
            let incoming = crate::backup::read(archive_path).unwrap();
            assert_eq!(incoming, after);
            let plan = prepare(incoming.clone(), before.clone(), Some(&before.generation)).unwrap();
            let error = install_with(&root.0, plan, &mut |point| {
                if point == (Point::Verified { rollback: false }) {
                    return Err(invalid("fail before commit"));
                }
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error.context["rolled_back"], true);
            assert_eq!(capture(&root.0).unwrap(), before);
            assert_eq!(
                crate::backup::read(Path::new(&preimage.path)).unwrap(),
                before
            );
            let plan = prepare(incoming, before.clone(), Some(&before.generation)).unwrap();
            install(&root.0, plan).unwrap();
            assert_eq!(capture(&root.0).unwrap(), after);
            assert_eq!(fs::read(archive_path).unwrap(), archive_bytes);
            assert!(!pending(&root.0).unwrap());
            let reopened = Store::new(root.0.clone()).unwrap();
            assert_eq!(reopened.read_target("Alpha", "pkg").unwrap().revision, 7);
            let empty = reopened.read_target("Beta", "pkg").unwrap();
            assert_eq!(empty.revision, 11);
            assert!(empty.binding.is_none());
        }
    }
}

#[test]
fn old_active_id_archives_are_preserved_but_never_installed() {
    for target in [false, true] {
        let (source, _) = fixture();
        install_old_record(&source, target);
        let old = capture(&source.0).unwrap();
        let archive = crate::backup::write(&source.0, old.clone(), None).unwrap();
        let bytes = fs::read(&archive.path).unwrap();
        let (live, before) = fixture();
        assert!(
            prepare(
                crate::backup::read(Path::new(&archive.path)).unwrap(),
                before.clone(),
                Some(&before.generation)
            )
            .is_err()
        );
        assert_eq!(capture(&live.0).unwrap(), before);
        assert_eq!(capture(&source.0).unwrap(), old);
        assert_eq!(fs::read(&archive.path).unwrap(), bytes);
        assert!(!pending(&live.0).unwrap());
    }
}

#[test]
fn opaque_preservation_still_enforces_file_bounds_and_aliases() {
    let (root, before) = fixture();
    for (path, maximum) in [
        (
            HISTORICAL_LEDGER.to_owned(),
            configuration::MAX_LEDGER_BYTES,
        ),
        (
            format!("profiles/{}.json", old_id(2)),
            storage::MAX_PROFILE_BYTES,
        ),
    ] {
        let mut files = before.files.clone();
        files.insert(path.clone(), vec![0xff; maximum]);
        validate(&Capture::from_files(files.clone(), true).unwrap()).unwrap();
        files.get_mut(&path).unwrap().push(0xff);
        assert!(Capture::from_files(files, true).is_err());
    }
    root.put("Identity-Migrations.config", b"alias");
    assert!(capture(&root.0).is_err());
    assert_eq!(
        fs::read(root.0.join("Identity-Migrations.config")).unwrap(),
        b"alias"
    );
}

#[cfg(unix)]
#[test]
fn opaque_ledger_links_remain_refused_without_following_or_mutation() {
    let (root, before) = fixture();
    let outside = Root::new();
    outside.put("ledger", b"outside original");
    let ledger = root.0.join(HISTORICAL_LEDGER);
    std::os::unix::fs::symlink(outside.0.join("ledger"), &ledger).unwrap();
    assert!(capture(&root.0).is_err());
    fs::remove_file(&ledger).unwrap();
    fs::hard_link(outside.0.join("ledger"), &ledger).unwrap();
    assert!(capture(&root.0).is_err());
    fs::remove_file(ledger).unwrap();
    assert_eq!(capture(&root.0).unwrap(), before);
    assert_eq!(
        fs::read(outside.0.join("ledger")).unwrap(),
        b"outside original"
    );
}

#[test]
fn supported_restore_interruptions_recover_exact_opaque_preimage_or_selection() {
    for rollback in [false, true] {
        let (root, _) = fixture();
        root.put(HISTORICAL_LEDGER, b"preimage\0");
        let before = capture(&root.0).unwrap();
        let mut files = before.files.clone();
        files.insert(HISTORICAL_LEDGER.into(), b"selected\xff".to_vec());
        let after = Capture::from_files(files, true).unwrap();
        let plan = prepare(after.clone(), before.clone(), Some(&before.generation)).unwrap();
        assert!(
            catch_unwind(AssertUnwindSafe(|| install_with(
                &root.0,
                plan,
                &mut |point| {
                    if matches!(
                        point,
                        Point::Displaced {
                            rollback: false,
                            ..
                        }
                    ) {
                        panic!("exit during replacement");
                    }
                    Ok(())
                }
            )))
            .is_err()
        );
        check_recovery(&root.0).unwrap();
        recover(&root.0, rollback).unwrap();
        assert_eq!(
            capture(&root.0).unwrap(),
            if rollback { before } else { after }
        );
        assert!(!pending(&root.0).unwrap());
    }
}

fn profile_import_fixture() -> (Root, Capture, Capture, Operation) {
    let (root, _) = fixture();
    root.put(HISTORICAL_LEDGER, b"opaque retained ledger\0");
    let profile = Profile {
        version: 1,
        id: "00000000000000000020".into(),
        name: "Selected import".into(),
        package_id: "pkg".into(),
        schema_identity: "a".repeat(64),
        values: json!({"label":"unchanged"}),
    };
    root.put(
        &format!("profiles/{}.json", profile.id),
        &serde_json::to_vec_pretty(&profile).unwrap(),
    );
    let before = capture(&root.0).unwrap();
    let mut files = before.files.clone();
    files.insert(
        format!("tabs/Alpha/pkg/{}.config", profile.id),
        encode(&profile, storage::MAX_PROFILE_BYTES).unwrap(),
    );
    let after = Capture::from_files(files, true).unwrap();
    let operation = Operation::ProfileImport {
        internal_name: "Alpha".into(),
        package_id: "pkg".into(),
        source_id: profile.id,
    };
    (root, before, after, operation)
}

#[test]
fn profile_import_recovery_preserves_sources_and_refuses_unrelated_transitions() {
    for rollback in [false, true] {
        let (root, before, after, operation) = profile_import_fixture();
        let mut changed = after.files.clone();
        changed.insert(HISTORICAL_LEDGER.into(), b"unrelated change".to_vec());
        assert!(
            prepare_publication(
                before.clone(),
                Capture::from_files(changed, true).unwrap(),
                operation.clone()
            )
            .is_err()
        );
        let prepared = prepare_publication(before.clone(), after.clone(), operation).unwrap();
        assert!(
            catch_unwind(AssertUnwindSafe(|| publish(
                &root.0,
                prepared,
                &mut |point| {
                    if matches!(
                        point,
                        Point::Installed {
                            rollback: false,
                            ..
                        }
                    ) {
                        panic!("exit after profile installation");
                    }
                    Ok(())
                }
            )))
            .is_err()
        );
        check_recovery(&root.0).unwrap();
        let source = "profiles/00000000000000000020.json";
        fs::write(root.0.join(source), b"external source edit").unwrap();
        assert!(check_recovery(&root.0).is_err());
        assert!(recover(&root.0, rollback).is_err());
        assert_eq!(
            fs::read(root.0.join(source)).unwrap(),
            b"external source edit"
        );
        fs::write(root.0.join(source), &before.files[source]).unwrap();
        recover(&root.0, rollback).unwrap();
        assert_eq!(
            capture(&root.0).unwrap(),
            if rollback { before } else { after }
        );
        assert!(!pending(&root.0).unwrap());
    }
}

#[test]
fn unsupported_journals_and_completion_only_markers_are_untouched() {
    for version in [1, 2, 4] {
        for completion_only in [false, true] {
            let (root, before) = fixture();
            let path = if completion_only {
                root.0.join(COMPLETION)
            } else {
                configuration::create_private_directory(&root.0.join(JOURNAL)).unwrap();
                root.put(&format!("{JOURNAL}/old-0"), b"preimage evidence");
                root.0.join(JOURNAL).join("journal.json")
            };
            let bytes = serde_json::to_vec(&json!({"version":version,"operation":{"kind":"identity_migration"},"evidence":"preserved"})).unwrap();
            configuration::write_private(&path, &bytes).unwrap();
            assert!(pending(&root.0).unwrap());
            assert_eq!(
                check_recovery(&root.0).unwrap_err().category,
                "RestoreUnsupported"
            );
            for rollback in [false, true] {
                assert_eq!(
                    recover(&root.0, rollback).unwrap_err().category,
                    "RestoreUnsupported"
                );
                assert_eq!(fs::read(&path).unwrap(), bytes);
                assert_eq!(capture(&root.0).unwrap(), before);
                if !completion_only {
                    assert_eq!(
                        fs::read(root.0.join(JOURNAL).join("old-0")).unwrap(),
                        b"preimage evidence"
                    );
                }
            }
        }
    }
    let (root, before, after, operation) = profile_import_fixture();
    let prepared = prepare_publication(before.clone(), after, operation).unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| publish(
            &root.0,
            prepared,
            &mut |point| {
                if point == Point::AfterJournal {
                    panic!("exit after journal");
                }
                Ok(())
            }
        )))
        .is_err()
    );
    let path = root.0.join(JOURNAL).join("journal.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(journal["version"], 3);
    journal["operation"]["kind"] = json!("identity_migration");
    let bytes = serde_json::to_vec(&journal).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(check_recovery(&root.0).is_err());
    for rollback in [false, true] {
        assert!(recover(&root.0, rollback).is_err());
    }
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(capture(&root.0).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn real_partial_v3_journal_write_leaves_original_generation_admissible() {
    const CHILD: &str = "MADO_CONFIGURATION_JOURNAL_FILE_LIMIT";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("sh")
            .args(["-c", "trap '' XFSZ; ulimit -f 128 || exit; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "restore::cutover_tests::real_partial_v3_journal_write_leaves_original_generation_admissible", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = Root::new();
    root.put(
        "settings.json",
        &serde_json::to_vec(&Settings::default()).unwrap(),
    );
    for index in 0..8 {
        let tab = format!("Owner{index}");
        let owner = TabRecord {
            version: 1,
            internal_name: tab.clone(),
            display_name: tab.clone(),
            open: true,
            packages: vec![PackageReference {
                package_id: "pkg".into(),
                source: PackageSource::Directory {
                    path: root.0.join("sources/pkg").to_str().unwrap().into(),
                },
            }],
            selected_package_id: Some("pkg".into()),
        };
        root.put(
            &format!("tabs/{tab}/tab.config"),
            &serde_json::to_vec(&owner).unwrap(),
        );
        for sequence in 0..60 {
            let profile = Profile {
                version: 1,
                id: format!("{:019x}0", index * 60 + sequence),
                name: "Preserved".into(),
                package_id: "pkg".into(),
                schema_identity: "a".repeat(64),
                values: json!({}),
            };
            root.put(
                &format!("tabs/{tab}/pkg/{}.config", profile.id),
                &serde_json::to_vec(&profile).unwrap(),
            );
        }
    }
    let before = capture(&root.0).unwrap();
    let plan = prepare(before.clone(), before.clone(), Some(&before.generation)).unwrap();
    let error = install(&root.0, plan).unwrap_err();
    assert_eq!(error.category, "ConfigurationIo");
    assert_eq!(error.context["kind"], "FileTooLarge");
    assert!(!pending(&root.0).unwrap());
    assert_eq!(capture(&root.0).unwrap(), before);
}

#[test]
fn real_target_replacement_failure_retains_v3_recovery_and_exact_ledger_generations() {
    for rollback in [false, true] {
        let (root, _) = fixture();
        root.put(HISTORICAL_LEDGER, b"original ledger");
        let before = capture(&root.0).unwrap();
        let mut files = before.files.clone();
        files.insert(HISTORICAL_LEDGER.into(), b"selected ledger".to_vec());
        let target_path = "tabs/Alpha/pkg/target.config";
        let mut target: TargetRecord = decode(&files[target_path]).unwrap();
        target.revision += 1;
        files.insert(
            target_path.into(),
            serde_json::to_vec_pretty(&target).unwrap(),
        );
        let after = Capture::from_files(files, true).unwrap();
        let index = before
            .files
            .keys()
            .position(|path| path == target_path)
            .unwrap();
        let target_file = root.0.join(target_path);
        let plan = prepare(after.clone(), before.clone(), Some(&before.generation)).unwrap();
        let fault = install_with(&root.0, plan, &mut |point| {
            if point
                == (Point::Displaced {
                    index,
                    rollback: false,
                })
            {
                configuration::create_private_directory(&target_file)?;
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(fault.category, "ConfigurationIo");
        assert_eq!(fault.context["pending_restore"], true);
        assert!(target_file.is_dir());
        assert_eq!(
            fs::read(root.0.join(HISTORICAL_LEDGER)).unwrap(),
            b"selected ledger"
        );
        assert!(pending(&root.0).unwrap());
        fs::remove_dir(&target_file).unwrap();
        check_recovery(&root.0).unwrap();
        recover(&root.0, rollback).unwrap();
        assert_eq!(
            capture(&root.0).unwrap(),
            if rollback { before } else { after }
        );
        assert!(!pending(&root.0).unwrap());
    }
}

#[test]
fn current_completion_cannot_authorize_cleanup_of_an_older_retained_journal() {
    let (root, before, after, operation) = profile_import_fixture();
    let prepared = prepare_publication(before.clone(), after.clone(), operation).unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| publish(
            &root.0,
            prepared,
            &mut |point| {
                if point == Point::AfterCleanupCommit {
                    panic!("exit before committed cleanup");
                }
                Ok(())
            }
        )))
        .is_err()
    );
    let marker = fs::read(root.0.join(COMPLETION)).unwrap();
    let path = root.0.join(JOURNAL).join("journal.json");
    let current = fs::read(&path).unwrap();
    let mut journal: serde_json::Value = serde_json::from_slice(&current).unwrap();
    journal["version"] = json!(2);
    let older = serde_json::to_vec(&journal).unwrap();
    fs::write(&path, &older).unwrap();
    assert_eq!(
        check_recovery(&root.0).unwrap_err().category,
        "RestoreUnsupported"
    );
    for rollback in [false, true] {
        assert_eq!(
            recover(&root.0, rollback).unwrap_err().category,
            "RestoreUnsupported"
        );
        assert_eq!(fs::read(&path).unwrap(), older);
        assert_eq!(fs::read(root.0.join(COMPLETION)).unwrap(), marker);
        assert_eq!(capture(&root.0).unwrap(), after);
        assert_eq!(
            fs::read(root.0.join(JOURNAL).join("old-0")).unwrap(),
            *before.files.values().next().unwrap()
        );
    }
    fs::write(path, current).unwrap();
    recover(&root.0, false).unwrap();
    assert_eq!(capture(&root.0).unwrap(), after);
}
