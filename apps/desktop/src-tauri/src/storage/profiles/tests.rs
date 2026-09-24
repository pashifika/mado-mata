use super::super::MAX_DIRECTORY_ENTRIES;
use super::super::fixtures::{Directory, inventory, options, put};
use super::*;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn schema_mismatch_and_invalid_replacement_preserve_original_bytes() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let mut inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let path = store.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    assert!(
        store
            .save(
                &inv,
                Some(&saved.id),
                "Invalid",
                json!({"priorities":["left"],"window":{"width":3}})
            )
            .is_err()
    );
    inv.schema["properties"]["label"]["maxLength"] = json!(80);
    let schema = identity(&inv.schema).unwrap();
    let rejected = store.list(&inv.package_id, &schema).unwrap();
    assert!(rejected.profiles.is_empty());
    assert_eq!(rejected.rejected[0].context["profile_id"], saved.id);
    assert!(store.rename(&inv, &saved.id, "Changed").is_err());
    assert!(
        store
            .save(&inv, Some(&saved.id), "Changed", options())
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let compatible = store.save(&inv, None, "Compatible", options()).unwrap();
    assert_eq!(
        store.list(&inv.package_id, &schema).unwrap().profiles[0].id,
        compatible.id
    );
}

#[test]
fn malformed_profiles_and_wrong_containing_identities_preserve_data() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let path = store.profile_path(&saved.id);
    let mut malformed = vec![b"not JSON".to_vec()];
    for (key, value) in [
        ("version", json!(2)),
        ("id", json!(new_id().unwrap())),
        ("package_id", json!("other-owner")),
        ("values", json!({"nested":{"api_key":"credential"}})),
    ] {
        let mut document = serde_json::to_value(&saved).unwrap();
        document[key] = value;
        malformed.push(serde_json::to_vec(&document).unwrap());
    }
    for bytes in malformed {
        fs::write(&path, &bytes).unwrap();
        let fault = store
            .list(&inv.package_id, &saved.schema_identity)
            .unwrap_err();
        assert_eq!(fault.context["profile_id"], saved.id);
        assert_eq!(fault.context["internal_name"], "Owner");
        assert!(store.save(&inv, None, "Replacement", options()).is_err());
        assert!(
            store
                .save(&inv, Some(&saved.id), "Replacement", options())
                .is_err()
        );
        assert!(store.rename(&inv, &saved.id, "Renamed").is_err());
        assert!(store.delete(&saved.id).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    assert!(store.delete("../settings").is_err());
}

#[test]
fn profile_size_count_and_aggregate_bounds_preserve_existing_profiles() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let path = store.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    assert!(
        store
            .save(
                &inv,
                Some(&saved.id),
                "Too large",
                json!({"priorities":["left"],"label":"a".repeat(MAX_PROFILE_BYTES)})
            )
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    for index in 1..MAX_PROFILES {
        store
            .save(&inv, None, &format!("Profile {index}"), options())
            .unwrap();
    }
    assert!(store.save(&inv, None, "Overflow", options()).is_err());
    assert_eq!(
        store.rename(&inv, &saved.id, "At capacity").unwrap().id,
        saved.id
    );
    assert_eq!(
        store
            .list(&inv.package_id, &saved.schema_identity)
            .unwrap()
            .profiles
            .len(),
        MAX_PROFILES
    );
    let large = directory.profiles("Large");
    let values = json!({"priorities":["left"],"label":"x".repeat(59 * 1024)});
    for index in 0..17 {
        large
            .save(&inv, None, &format!("Large {index}"), values.clone())
            .unwrap();
    }
    assert!(large.save(&inv, None, "Over one MiB", values).is_err());
    assert_eq!(
        large
            .list(&inv.package_id, &saved.schema_identity)
            .unwrap()
            .profiles
            .len(),
        17
    );
}
#[test]
fn pending_profiles_block_mutation_but_foreign_notes_remain_untouched() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let notes = store.directory().join("notes.txt");
    fs::write(&notes, b"operator notes").unwrap();
    store.rename(&inv, &saved.id, "Renamed").unwrap();
    let pending = store.profile_path(&saved.id).with_extension("pending");
    put(&pending, b"unfinished");
    let before = fs::read(store.profile_path(&saved.id)).unwrap();
    assert!(store.list(&inv.package_id, &saved.schema_identity).is_err());
    assert!(
        store
            .save(&inv, Some(&saved.id), "Unsaved", options())
            .is_err()
    );
    assert!(store.delete(&saved.id).is_err());
    assert_eq!(fs::read(store.profile_path(&saved.id)).unwrap(), before);
    assert_eq!(fs::read(&pending).unwrap(), b"unfinished");
    assert_eq!(fs::read(&notes).unwrap(), b"operator notes");
}

#[test]
fn foreign_entries_consume_bounded_directory_capacity() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    private_directory(&store.directory()).unwrap();
    for index in 0..MAX_DIRECTORY_ENTRIES - 2 {
        fs::write(store.directory().join(format!("note-{index}.txt")), b"").unwrap();
    }
    let inv = inventory();
    let saved = store.save(&inv, None, "Last slot", options()).unwrap();
    store.rename(&inv, &saved.id, "At capacity").unwrap();
    let before = fs::read(store.profile_path(&saved.id)).unwrap();
    fs::write(store.directory().join("one-too-many.txt"), b"").unwrap();
    assert!(store.list(&inv.package_id, &saved.schema_identity).is_err());
    assert!(store.save(&inv, None, "Overflow", options()).is_err());
    assert_eq!(fs::read(store.profile_path(&saved.id)).unwrap(), before);
}

#[test]
fn failed_profile_publication_retains_previous_bytes() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let mut profile = store.save(&inv, None, "Original", options()).unwrap();
    let path = store.profile_path(&profile.id);
    let before = fs::read(&path).unwrap();
    profile.name = "Unsaved".into();
    let bytes = encode(&profile, MAX_PROFILE_BYTES).unwrap();
    assert!(
        write_atomic(&path, &bytes, |temporary, destination| {
            assert_eq!(fs::read(temporary)?, bytes);
            fs::rename(temporary, destination.join("not-a-directory"))
        })
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!path.with_extension("pending").exists());
    let reopened = directory
        .store()
        .profile_store("Owner", &inv.package_id)
        .unwrap();
    assert_eq!(
        reopened.read_profile(&profile.id).unwrap().0.name,
        "Original"
    );
}

#[test]
fn portability_checks_fields_paths_and_defaults_not_arbitrary_words() {
    for portable in [
        json!({"label":"secret token password"}),
        json!({"asset":"assets/right.png"}),
        json!({"key":"Enter","item":"game token","label":"A: strategy"}),
    ] {
        portable_values(&portable).unwrap();
    }
    for forbidden in [
        json!({"nested":{"api_key":"value"}}),
        json!({"token":"value"}),
        json!({"inputAuthority":false}),
        json!({"target_executable_path":"relative-game"}),
        json!({"asset":"/private/model"}),
        json!({"asset":"C:\\models\\ocr"}),
        json!({"asset":"../outside"}),
    ] {
        assert_eq!(
            portable_values(&forbidden).unwrap_err().category,
            "ProfileAuthority"
        );
    }
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let mut inv = inventory();
    inv.schema["properties"]["label"]["default"] = json!("/private/default-model");
    assert_eq!(
        store
            .save(&inv, None, "No authority", json!({"priorities":["left"]}))
            .unwrap_err()
            .category,
        "ProfileAuthority"
    );
}

fn legacy_profile(sequence: u64, name: &str) -> Profile {
    let inv = inventory();
    Profile {
        version: VERSION,
        id: format!("p-{sequence:032x}-{:08x}-{:016x}", 1, 1),
        name: name.into(),
        package_id: inv.package_id,
        schema_identity: identity(&inv.schema).unwrap(),
        values: options(),
    }
}

fn legacy(directory: &Directory, profile: &Profile) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(profile).unwrap();
    put(
        &directory
            .0
            .join("profiles")
            .join(format!("{}.json", profile.id)),
        &bytes,
    );
    bytes
}

#[test]
fn explicit_legacy_import_is_exact_idempotent_and_not_shared() {
    let directory = Directory::new();
    let first = directory.profiles("First");
    let second = directory.profiles("Second");
    let profile = legacy_profile(1, "Legacy");
    let bytes = legacy(&directory, &profile);
    let inv = inventory();
    assert!(
        first
            .list(&inv.package_id, &profile.schema_identity)
            .unwrap()
            .profiles
            .is_empty()
    );
    let store = directory.store();
    let imported = store.import_legacy_profiles("First", &inv).unwrap();
    assert_eq!(imported.imported, [profile.id.clone()]);
    assert!(imported.fault.is_none());
    assert_eq!(fs::read(first.profile_path(&profile.id)).unwrap(), bytes);
    let retry = store.import_legacy_profiles("First", &inv).unwrap();
    assert!(retry.imported.is_empty());
    assert_eq!(retry.unchanged, [profile.id.clone()]);
    assert!(retry.fault.is_none());
    assert!(
        second
            .list(&inv.package_id, &profile.schema_identity)
            .unwrap()
            .profiles
            .is_empty()
    );
    assert_eq!(
        fs::read(
            directory
                .0
                .join("profiles")
                .join(format!("{}.json", profile.id))
        )
        .unwrap(),
        bytes
    );
}

#[test]
fn legacy_conflict_reports_committed_subset_and_retry_preserves_sources() {
    let directory = Directory::new();
    let scoped = directory.profiles("Owner");
    let first = legacy_profile(1, "First");
    let second = legacy_profile(2, "Second");
    let first_bytes = legacy(&directory, &first);
    let second_bytes = legacy(&directory, &second);
    let mut conflict = second.clone();
    conflict.name = "Different retained value".into();
    let conflicting_bytes = encode(&conflict, MAX_PROFILE_BYTES).unwrap();
    put(&scoped.profile_path(&second.id), &conflicting_bytes);
    let store = directory.store();
    let result = store.import_legacy_profiles("Owner", &inventory()).unwrap();
    assert_eq!(result.imported, [first.id.clone()]);
    assert_eq!(result.fault.unwrap().category, "LegacyConflict");
    assert_eq!(
        fs::read(scoped.profile_path(&first.id)).unwrap(),
        first_bytes
    );
    assert_eq!(
        fs::read(scoped.profile_path(&second.id)).unwrap(),
        conflicting_bytes
    );
    let retry = store.import_legacy_profiles("Owner", &inventory()).unwrap();
    assert!(retry.imported.is_empty());
    assert_eq!(retry.unchanged, [first.id.clone()]);
    assert_eq!(retry.fault.unwrap().category, "LegacyConflict");
    assert_eq!(
        fs::read(
            directory
                .0
                .join("profiles")
                .join(format!("{}.json", second.id))
        )
        .unwrap(),
        second_bytes
    );
}

#[test]
fn interrupted_legacy_batch_resumes_after_external_source_repair() {
    let directory = Directory::new();
    let scoped = directory.profiles("Owner");
    let first = legacy_profile(1, "First");
    let second = legacy_profile(2, "Second");
    let first_bytes = legacy(&directory, &first);
    let broken = directory
        .0
        .join("profiles")
        .join(format!("{}.json", second.id));
    put(&broken, b"interrupted source");
    let store = directory.store();
    let partial = store.import_legacy_profiles("Owner", &inventory()).unwrap();
    assert_eq!(partial.imported, [first.id.clone()]);
    assert_eq!(partial.fault.unwrap().category, "StorageFormat");
    assert_eq!(
        fs::read(scoped.profile_path(&first.id)).unwrap(),
        first_bytes
    );
    assert!(!scoped.profile_path(&second.id).exists());
    assert_eq!(fs::read(&broken).unwrap(), b"interrupted source");
    let second_bytes = legacy(&directory, &second);
    let resumed = store.import_legacy_profiles("Owner", &inventory()).unwrap();
    assert_eq!(resumed.unchanged, [first.id.clone()]);
    assert_eq!(resumed.imported, [second.id.clone()]);
    assert!(resumed.fault.is_none());
    assert_eq!(
        fs::read(scoped.profile_path(&second.id)).unwrap(),
        second_bytes
    );
    assert_eq!(
        fs::read(scoped.profile_path(&first.id)).unwrap(),
        first_bytes
    );
}

#[test]
fn legacy_import_skips_incompatible_data_and_preserves_malformed_sources() {
    let directory = Directory::new();
    let scoped = directory.profiles("Owner");
    let mut profile = legacy_profile(1, "Other package");
    profile.package_id = "different-package".into();
    legacy(&directory, &profile);
    let store = directory.store();
    let result = store.import_legacy_profiles("Owner", &inventory()).unwrap();
    assert!(result.imported.is_empty());
    assert!(result.fault.is_none());
    assert!(!scoped.directory().exists());
    let path = directory
        .0
        .join("profiles")
        .join(format!("{}.json", profile.id));
    put(&path, b"malformed source");
    let result = store.import_legacy_profiles("Owner", &inventory()).unwrap();
    assert_eq!(result.fault.unwrap().category, "StorageFormat");
    assert_eq!(fs::read(&path).unwrap(), b"malformed source");
    assert!(!scoped.directory().exists());
}

#[cfg(unix)]
#[test]
fn unsafe_owner_directories_and_linked_profiles_are_refused_without_repair() {
    let directory = Directory::new();
    let scoped = directory.profiles("Owner");
    let saved = scoped
        .save(&inventory(), None, "Original", options())
        .unwrap();
    let path = scoped.profile_path(&saved.id);
    assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(scoped.directory()).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let outside = directory.0.join("original.json");
    fs::rename(&path, &outside).unwrap();
    let before = fs::read(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(
        scoped
            .list(&inventory().package_id, &saved.schema_identity)
            .is_err()
    );
    assert!(
        scoped
            .save(&inventory(), Some(&saved.id), "Replacement", options())
            .is_err()
    );
    assert!(scoped.delete(&saved.id).is_err());
    assert_eq!(fs::read(&outside).unwrap(), before);
    fs::remove_file(&path).unwrap();
    fs::hard_link(&outside, &path).unwrap();
    assert!(scoped.delete(&saved.id).is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(&outside, &path).unwrap();
    for owner in [
        directory.0.clone(),
        directory.0.join("tabs"),
        directory.0.join("tabs/Owner"),
        scoped.directory(),
    ] {
        fs::set_permissions(&owner, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            scoped
                .list(&inventory().package_id, &saved.schema_identity)
                .is_err()
        );
        assert_eq!(fs::metadata(&owner).unwrap().mode() & 0o777, 0o755);
        fs::set_permissions(&owner, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(fs::read(&path).unwrap(), before);
}
