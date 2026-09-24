use super::super::fixtures::{Directory, inventory, put, stored_target, target_path};
use super::*;

#[test]
fn target_reads_are_lazy_and_removal_retains_revision_against_null_aba() {
    let directory = Directory::new();
    let profiles = directory.profiles("Owner");
    let store = directory.store();
    let package = inventory().package_id;
    let absent = store.read_target("Owner", &package).unwrap();
    assert_eq!(absent.revision, 0);
    assert_eq!(absent.binding, None);
    assert!(!profiles.directory().exists());
    let removed = store
        .remove_target("Owner", &package, &absent.expectation())
        .unwrap();
    assert_eq!(removed.revision, 1);
    assert_eq!(removed.binding, None);
    let bytes = fs::read(target_path(&directory, "Owner")).unwrap();
    assert_eq!(
        store
            .remove_target("Owner", &package, &absent.expectation())
            .unwrap_err()
            .category,
        "TargetConflict"
    );
    assert_eq!(fs::read(target_path(&directory, "Owner")).unwrap(), bytes);
    assert_eq!(
        directory.store().read_target("Owner", &package).unwrap(),
        removed
    );
    let second = store
        .remove_target("Owner", &package, &removed.expectation())
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(second.binding, None);
}

#[test]
fn target_storage_rejects_schema_owner_size_and_revision_without_reset_and_reloads_repairs() {
    let directory = Directory::new();
    directory.profiles("Owner");
    directory.profiles("Other");
    let store = directory.store();
    let package = inventory().package_id;
    let path = target_path(&directory, "Owner");
    let saved = stored_target("Owner");
    let good = serde_json::to_value(&saved).unwrap();
    let bytes = serde_json::to_vec(&saved).unwrap();
    put(&path, &bytes);
    let mut invalid_documents = vec![
        b"{".to_vec(),
        b"[]".to_vec(),
        br#"{"version":1,"version":1,"internal_name":"Owner","package_id":"portable-options","revision":1,"binding":null}"#.to_vec(),
    ];
    for (field, value) in [
        ("version", json!(2)),
        ("internal_name", json!("Other")),
        ("package_id", json!("different")),
        ("revision", json!(0)),
        ("revision", json!(crate::target::MAX_TARGET_REVISION + 1)),
        ("authority", json!(true)),
        ("binding", json!([null])),
    ] {
        let mut invalid = good.clone();
        invalid[field] = value;
        invalid_documents.push(serde_json::to_vec(&invalid).unwrap());
    }
    for (field, value) in [
        ("package_id", json!("different")),
        ("declaration_identity", json!("not-a-hash")),
        ("target_id", json!("../escape")),
        ("configuration", json!([])),
        (
            "resolution",
            json!({"game": {"path": "/offline/game", "executable": "/different"}, "launcher": null, "working_directory": null}),
        ),
    ] {
        let mut invalid = good.clone();
        invalid["binding"][field] = value;
        invalid_documents.push(serde_json::to_vec(&invalid).unwrap());
    }
    let mut oversized = bytes.clone();
    oversized.resize(MAX_TARGET_BYTES + 1, b' ');
    invalid_documents.push(oversized);
    for invalid in invalid_documents {
        fs::write(&path, &invalid).unwrap();
        let fault = store.read_target("Owner", &package).unwrap_err();
        assert_eq!(fault.context["owner"], "target");
        assert_eq!(fault.context["internal_name"], "Owner");
        assert!(
            store
                .remove_target("Owner", &package, &saved.expectation())
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), invalid);
        assert_eq!(store.read_target("Other", &package).unwrap().revision, 0);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(store.read_target("Owner", &package).unwrap(), saved);
    }
    let mut exhausted = saved;
    exhausted.revision = crate::target::MAX_TARGET_REVISION;
    put(&path, &serde_json::to_vec(&exhausted).unwrap());
    let before = fs::read(&path).unwrap();
    assert_eq!(
        store
            .remove_target("Owner", &package, &exhausted.expectation())
            .unwrap_err()
            .category,
        "TargetRevision"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}
#[test]
fn target_atomic_failure_retains_preimage_and_only_cleans_its_own_pending_file() {
    let directory = Directory::new();
    directory.profiles("Owner");
    let store = directory.store();
    let mut record = stored_target("Owner");
    let path = target_path(&directory, "Owner");
    put(&path, &serde_json::to_vec(&record).unwrap());
    let before = fs::read(&path).unwrap();
    record.revision += 1;
    record.binding = None;
    let failure = store.write_target(&record, |temporary, destination| {
        assert_eq!(
            decode::<TargetRecord>(&fs::read(temporary)?).unwrap(),
            record
        );
        fs::rename(temporary, destination.join("not-a-directory"))
    });
    assert!(failure.is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!path.with_extension("pending").exists());
    put(&path.with_extension("pending"), b"other pending write");
    assert!(
        store
            .write_target(&record, |from, to| fs::rename(from, to))
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read(path.with_extension("pending")).unwrap(),
        b"other pending write"
    );
}
#[cfg(unix)]
#[test]
fn target_alias_changes_need_exact_review_and_save_rechecks_metadata() {
    use crate::target::tests::{MetadataFixture, configuration, declaration};
    let metadata = MetadataFixture::new();
    let first = metadata.executable("first");
    let second = metadata.executable("second");
    let third = metadata.executable("third");
    let alias = metadata.0.join("selected");
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    let configuration = configuration(alias.to_str().unwrap());
    let directory = Directory::new();
    directory.profiles("Owner");
    let store = directory.store();
    let package = inventory().package_id;
    let empty = store.read_target("Owner", &package).unwrap();
    let (saved, _) = store
        .save_target(
            "Owner",
            &package,
            &declaration(),
            &empty.expectation(),
            configuration.clone(),
            None,
        )
        .unwrap();
    let before = fs::read(target_path(&directory, "Owner")).unwrap();
    fs::write(&first, b"ordinary executable update in place").unwrap();
    assert!(
        !store
            .check_target(
                "Owner",
                &package,
                &declaration(),
                &saved.expectation(),
                &configuration
            )
            .unwrap()
            .resolution_changed
    );
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&second, &alias).unwrap();
    let check = store
        .check_target(
            "Owner",
            &package,
            &declaration(),
            &saved.expectation(),
            &configuration,
        )
        .unwrap();
    assert!(check.resolution_changed);
    assert_eq!(
        check.previous_resolution,
        Some(saved.binding.as_ref().unwrap().resolution.clone())
    );
    let refusal = store
        .save_target(
            "Owner",
            &package,
            &declaration(),
            &saved.expectation(),
            configuration.clone(),
            None,
        )
        .unwrap_err();
    assert_eq!(refusal.category, "TargetResolutionChanged");
    assert_eq!(
        refusal.context["resolution"],
        serde_json::to_value(&check.resolution).unwrap()
    );
    assert_eq!(fs::read(target_path(&directory, "Owner")).unwrap(), before);
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&third, &alias).unwrap();
    assert_eq!(
        store
            .save_target(
                "Owner",
                &package,
                &declaration(),
                &saved.expectation(),
                configuration.clone(),
                Some(&check.resolution)
            )
            .unwrap_err()
            .category,
        "TargetResolutionChanged"
    );
    let latest = store
        .check_target(
            "Owner",
            &package,
            &declaration(),
            &saved.expectation(),
            &configuration,
        )
        .unwrap();
    let (adopted, _) = store
        .save_target(
            "Owner",
            &package,
            &declaration(),
            &saved.expectation(),
            configuration.clone(),
            Some(&latest.resolution),
        )
        .unwrap();
    assert_eq!(
        adopted.binding.as_ref().unwrap().configuration.game.path,
        configuration.game.path
    );
    assert_eq!(
        adopted.binding.as_ref().unwrap().resolution,
        latest.resolution
    );
    assert_eq!(
        adopted.binding.as_ref().unwrap().id,
        saved.binding.as_ref().unwrap().id
    );
    let mut explicit_change = configuration.clone();
    explicit_change.game.path = second.to_str().unwrap().into();
    assert_eq!(
        store
            .save_target(
                "Owner",
                &package,
                &declaration(),
                &adopted.expectation(),
                explicit_change,
                None
            )
            .unwrap_err()
            .category,
        "TargetResolutionChanged"
    );
    let adopted_bytes = fs::read(target_path(&directory, "Owner")).unwrap();
    fs::remove_file(&third).unwrap();
    assert_eq!(
        store
            .save_target(
                "Owner",
                &package,
                &declaration(),
                &adopted.expectation(),
                configuration,
                Some(&latest.resolution)
            )
            .unwrap_err()
            .category,
        "TargetMetadata"
    );
    assert_eq!(
        fs::read(target_path(&directory, "Owner")).unwrap(),
        adopted_bytes
    );
}

#[cfg(unix)]
#[test]
fn target_bundle_executable_metadata_changes_require_review_and_pending_save_preserves_it() {
    use crate::target::tests::{MetadataFixture, configuration, declaration};
    let metadata = MetadataFixture::new();
    let bundle = metadata.bundle(true);
    let mut configuration = configuration(bundle.to_str().unwrap());
    configuration.game.kind = "bundle".into();
    let directory = Directory::new();
    directory.profiles("Owner");
    let store = directory.store();
    let package = inventory().package_id;
    let empty = store.read_target("Owner", &package).unwrap();
    let (saved, _) = store
        .save_target(
            "Owner",
            &package,
            &declaration(),
            &empty.expectation(),
            configuration.clone(),
            None,
        )
        .unwrap();
    let path = target_path(&directory, "Owner");
    let before = fs::read(&path).unwrap();
    let replacement = metadata.executable("Binary.app/Contents/MacOS/Updated");
    plist::Value::Dictionary(plist::Dictionary::from_iter([(
        "CFBundleExecutable",
        plist::Value::String("Updated".into()),
    )]))
    .to_file_binary(bundle.join("Contents/Info.plist"))
    .unwrap();
    let check = store
        .check_target(
            "Owner",
            &package,
            &declaration(),
            &saved.expectation(),
            &configuration,
        )
        .unwrap();
    assert!(check.resolution_changed);
    assert_eq!(
        check.resolution.game.path,
        saved.binding.as_ref().unwrap().resolution.game.path
    );
    assert_eq!(
        check.resolution.game.executable,
        fs::canonicalize(replacement).unwrap().to_str().unwrap()
    );
    assert_eq!(
        store
            .save_target(
                "Owner",
                &package,
                &declaration(),
                &saved.expectation(),
                configuration.clone(),
                None
            )
            .unwrap_err()
            .category,
        "TargetResolutionChanged"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let pending = path.with_extension("pending");
    put(&pending, b"interrupted original save");
    assert_eq!(
        store
            .save_target(
                "Owner",
                &package,
                &declaration(),
                &saved.expectation(),
                configuration.clone(),
                Some(&check.resolution)
            )
            .unwrap_err()
            .category,
        "StoragePending"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read(&pending).unwrap(), b"interrupted original save");
    fs::remove_file(pending).unwrap();
    let (updated, _) = store
        .save_target(
            "Owner",
            &package,
            &declaration(),
            &saved.expectation(),
            configuration,
            Some(&check.resolution),
        )
        .unwrap();
    assert_eq!(
        updated.binding.as_ref().unwrap().resolution,
        check.resolution
    );
    assert_eq!(
        updated.binding.as_ref().unwrap().id,
        saved.binding.as_ref().unwrap().id
    );
}
