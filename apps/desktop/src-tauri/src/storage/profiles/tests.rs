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
    let expected = store.recovery_records().unwrap().pop().unwrap();
    let mut malformed = vec![b"not JSON".to_vec()];
    let mut nested = Value::Null;
    for _ in 0..MAX_VALUE_DEPTH {
        nested = json!([nested]);
    }
    for (key, value) in [
        ("version", json!(2)),
        ("id", json!(new_id().unwrap())),
        ("package_id", json!("other-owner")),
        ("values", json!({"nested":{"api_key":"credential"}})),
        ("values", json!({"label":"/private/model"})),
        ("values", json!([])),
        ("values", json!({"items":vec![0; MAX_VALUE_NODES]})),
        ("values", json!({"nested":nested})),
        ("values", json!({"label":"x".repeat(MAX_PROFILE_BYTES)})),
        ("name", json!("../foreign")),
        ("schema_identity", json!("not-an-identity")),
        ("unexpected", json!(true)),
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
        let recovery_fault = store.recovery_records().unwrap_err();
        assert_eq!(recovery_fault.category, fault.category);
        assert_eq!(recovery_fault.context["profile_id"], saved.id);
        assert_eq!(
            store
                .replace_recovery(&inv, &expected, options())
                .unwrap_err()
                .category,
            fault.category
        );
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
    let expected = store.recovery_records().unwrap().pop().unwrap();
    put(&pending, b"unfinished");
    let before = fs::read(store.profile_path(&saved.id)).unwrap();
    assert!(store.list(&inv.package_id, &saved.schema_identity).is_err());
    assert!(
        store
            .save(&inv, Some(&saved.id), "Unsaved", options())
            .is_err()
    );
    assert!(store.delete(&saved.id).is_err());
    assert_eq!(
        store.recovery_records().unwrap_err().category,
        "StoragePending"
    );
    let fault = store
        .replace_recovery(&inv, &expected, options())
        .unwrap_err();
    assert_eq!(fault.category, "StoragePending");
    assert_eq!(fault.context["file"], format!("{}.pending", saved.id));
    assert!(fault.context.get("profile_id").is_none());
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
    let expected = store.recovery_records().unwrap().pop().unwrap();
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
    assert_eq!(
        store.recovery_records().unwrap()[0].fingerprint,
        expected.fingerprint
    );
    let recovered = store
        .replace_recovery(&inv, &expected, json!({"priorities":["left"]}))
        .unwrap();
    assert_eq!(recovered.name, "Original");
    assert_eq!(recovered.values, json!({"priorities":["left"]}));
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
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let expected = store.recovery_records().unwrap().pop().unwrap();
    let path = store.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    inv.schema["properties"]["label"]["default"] = json!("/private/default-model");
    assert_eq!(
        store
            .save(&inv, None, "No authority", json!({"priorities":["left"]}))
            .unwrap_err()
            .category,
        "ProfileAuthority"
    );
    assert_eq!(
        store
            .replace_recovery(&inv, &expected, json!({"priorities":["left"]}))
            .unwrap_err()
            .category,
        "ProfileAuthority"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
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
    let expected = scoped.recovery_records().unwrap().pop().unwrap();
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
    assert!(scoped.recovery_records().is_err());
    assert!(
        scoped
            .replace_recovery(&inventory(), &expected, options())
            .is_err()
    );
    assert_eq!(fs::read(&outside).unwrap(), before);
    fs::remove_file(&path).unwrap();
    fs::hard_link(&outside, &path).unwrap();
    assert!(scoped.delete(&saved.id).is_err());
    assert!(scoped.recovery_records().is_err());
    assert!(
        scoped
            .replace_recovery(&inventory(), &expected, options())
            .is_err()
    );
    fs::remove_file(&path).unwrap();
    fs::rename(&outside, &path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(scoped.recovery_records().is_err());
    assert!(
        scoped
            .replace_recovery(&inventory(), &expected, options())
            .is_err()
    );
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
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
        assert!(scoped.recovery_records().is_err());
        assert!(
            scoped
                .replace_recovery(&inventory(), &expected, options())
                .is_err()
        );
        assert_eq!(fs::metadata(&owner).unwrap().mode() & 0o777, 0o755);
        fs::set_permissions(&owner, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn recovery_replaces_only_owned_values_and_schema_without_loosening_normal_save() {
    use super::super::fixtures::{preferences, tab_path, target_path};

    let directory = Directory::new();
    directory.store().initialize(preferences()).unwrap();
    let store = directory.profiles("Owner");
    let other = directory.profiles("Other");
    let mut inv = inventory();
    let supplied = json!({
        "priorities":["right","left","right"],
        "window":{"width":31,"height":47},
        "label":"retained"
    });
    let saved = store
        .save(&inv, None, "Original", supplied.clone())
        .unwrap();
    let unrelated = store.save(&inv, None, "Unrelated", options()).unwrap();
    let foreign = other.save(&inv, None, "Foreign", options()).unwrap();
    let target = target_path(&directory, "Owner");
    put(&target, b"retained target evidence");
    put(
        &target.with_extension("pending"),
        b"retained target pending",
    );
    put(
        &other.profile_path(&foreign.id).with_extension("pending"),
        b"unrelated pending",
    );
    let unchanged: Vec<_> = [
        directory.0.join("settings.json"),
        tab_path(&directory, "Owner"),
        tab_path(&directory, "Other"),
        store.profile_path(&unrelated.id),
        other.profile_path(&foreign.id),
        other.profile_path(&foreign.id).with_extension("pending"),
        target.clone(),
        target.with_extension("pending"),
    ]
    .into_iter()
    .map(|path| {
        let bytes = fs::read(&path).unwrap();
        (path, bytes)
    })
    .collect();
    let original = fs::read(store.profile_path(&saved.id)).unwrap();
    inv.schema["properties"]["added"] = json!({"type":"boolean","default":true});
    let schema = identity(&inv.schema).unwrap();
    assert!(
        store
            .list(&inv.package_id, &schema)
            .unwrap()
            .profiles
            .is_empty()
    );
    assert!(
        store
            .save(&inv, Some(&saved.id), "Renamed", supplied.clone())
            .is_err()
    );
    let expected = store
        .recovery_records()
        .unwrap()
        .into_iter()
        .find(|record| record.profile.id == saved.id)
        .unwrap();
    assert_eq!(expected.profile.values, supplied);
    assert_eq!(fs::read(store.profile_path(&saved.id)).unwrap(), original);
    let mut candidate = supplied;
    candidate["added"] = json!(true);
    let recovered = store
        .replace_recovery(&inv, &expected, candidate.clone())
        .unwrap();
    assert_eq!(recovered.id, saved.id);
    assert_eq!(recovered.name, saved.name);
    assert_eq!(recovered.package_id, saved.package_id);
    assert_eq!(recovered.version, saved.version);
    assert_eq!(recovered.schema_identity, schema);
    assert_eq!(recovered.values, candidate);
    let reopened = directory
        .store()
        .profile_store("Owner", &inv.package_id)
        .unwrap();
    let listing = reopened.list(&inv.package_id, &schema).unwrap();
    assert_eq!(listing.profiles[0].id, saved.id);
    assert_eq!(listing.profiles[0].values, candidate);
    assert_eq!(listing.rejected[0].context["profile_id"], unrelated.id);
    for (path, bytes) in unchanged {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(store.profile_path(&saved.id)).unwrap().mode() & 0o777,
        0o600
    );
}

#[test]
fn recovery_refuses_invalid_candidates_without_consuming_expected_original() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let mut inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let expected = store.recovery_records().unwrap().pop().unwrap();
    let path = store.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    inv.schema["properties"]["label"]["maxLength"] = json!(20);
    inv.schema["properties"]["window"]["properties"]["width"]["minimum"] = json!(10);
    inv.schema["properties"]["window"]["properties"]["height"]["default"] = json!(20);
    for invalid in [
        json!({}),
        json!({"priorities":"left"}),
        json!({"priorities":["up"]}),
        json!({"priorities":[]}),
        json!({"priorities":["left"],"obsolete":true}),
        json!({"priorities":["left"],"label":"x".repeat(21)}),
        json!({"priorities":["left"],"window":{"width":9,"height":20}}),
        json!({"priorities":["left"],"window":{"width":10}}),
        json!({"priorities":["left"],"label":"/private/model"}),
        json!({"priorities":["left"],"token":"credential"}),
    ] {
        let fault = store
            .replace_recovery(&inv, &expected, invalid)
            .unwrap_err();
        assert_eq!(fault.context["profile_id"], saved.id);
        assert_eq!(fault.context["internal_name"], "Owner");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("pending").exists());
    }
    let recovered = store.replace_recovery(&inv, &expected, options()).unwrap();
    assert_eq!(recovered.id, saved.id);
    assert_eq!(recovered.values, options());
    assert_eq!(recovered.schema_identity, identity(&inv.schema).unwrap());
}

#[test]
fn recovery_compares_original_bytes_and_never_recreates_deleted_records() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let expected = store.recovery_records().unwrap().pop().unwrap();
    let path = store.profile_path(&saved.id);
    let mut changed = fs::read(&path).unwrap();
    changed.push(b'\n');
    fs::write(&path, &changed).unwrap();
    let fault = store
        .replace_recovery(&inv, &expected, options())
        .unwrap_err();
    assert_eq!(fault.category, "ProfileConflict");
    assert_eq!(fault.context["profile_id"], saved.id);
    assert_eq!(fs::read(&path).unwrap(), changed);
    let refreshed = store.recovery_records().unwrap().pop().unwrap();
    assert_eq!(refreshed.profile.values, expected.profile.values);
    assert_ne!(refreshed.fingerprint, expected.fingerprint);
    fs::remove_file(&path).unwrap();
    let fault = store
        .replace_recovery(&inv, &refreshed, options())
        .unwrap_err();
    assert_eq!(fault.category, "ProfileNotFound");
    assert!(!path.exists());
    assert!(!path.with_extension("pending").exists());
    fs::remove_dir(store.directory()).unwrap();
    assert_eq!(
        store
            .replace_recovery(&inv, &refreshed, options())
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert!(!store.directory().exists());
}

#[test]
fn recovery_refuses_foreign_inventory_profiles_and_closed_owners() {
    let directory = Directory::new();
    let first = directory.profiles("First");
    let second = directory.profiles("Second");
    let mut inv = inventory();
    let saved = first.save(&inv, None, "Original", options()).unwrap();
    let expected = first.recovery_records().unwrap().pop().unwrap();
    let path = first.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    assert_eq!(
        second
            .replace_recovery(&inv, &expected, options())
            .unwrap_err()
            .category,
        "ProfileNotFound"
    );
    assert!(!second.directory().exists());
    inv.package_id = "foreign-package".into();
    assert_eq!(
        first
            .replace_recovery(&inv, &expected, options())
            .unwrap_err()
            .category,
        "ProfileIdentity"
    );
    directory.store().set_tab_open("First", false).unwrap();
    assert_eq!(first.recovery_records().unwrap_err().category, "TabClosed");
    assert_eq!(
        first
            .replace_recovery(&inventory(), &expected, options())
            .unwrap_err()
            .category,
        "TabClosed"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn recovery_preserves_record_count_directory_and_owner_byte_bounds() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let expected = store.recovery_records().unwrap().pop().unwrap();
    let path = store.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    for sequence in 1..MAX_PROFILES {
        let retained = legacy_profile(sequence as u64, "Retained");
        put(
            &store.profile_path(&retained.id),
            &encode(&retained, MAX_PROFILE_BYTES).unwrap(),
        );
    }
    let recovered = store.replace_recovery(&inv, &expected, options()).unwrap();
    assert_eq!(recovered.id, saved.id);
    let excess = legacy_profile(MAX_PROFILES as u64, "Excess");
    let excess_path = store.profile_path(&excess.id);
    put(&excess_path, &encode(&excess, MAX_PROFILE_BYTES).unwrap());
    assert_eq!(
        store.recovery_records().unwrap_err().category,
        "StorageLimit"
    );
    assert_eq!(
        store
            .replace_recovery(&inv, &expected, options())
            .unwrap_err()
            .category,
        "StorageLimit"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::remove_file(&excess_path).unwrap();
    for index in 0..MAX_DIRECTORY_ENTRIES - MAX_PROFILES {
        fs::write(
            store.directory().join(format!("note-{index}.txt")),
            b"retained",
        )
        .unwrap();
    }
    assert_eq!(
        store.recovery_records().unwrap_err().category,
        "StorageLimit"
    );
    assert_eq!(
        store
            .replace_recovery(&inv, &expected, options())
            .unwrap_err()
            .category,
        "StorageLimit"
    );
    assert_eq!(fs::read(&path).unwrap(), before);

    let large = directory.profiles("Large");
    let saved = large.save(&inv, None, "Original", options()).unwrap();
    let expected = large.recovery_records().unwrap().pop().unwrap();
    let path = large.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    let large_values = json!({"priorities":["left"],"label":"x".repeat(59 * 1024)});
    for sequence in 1..=17 {
        let mut retained = legacy_profile(sequence, "Large");
        retained.values = large_values.clone();
        put(
            &large.profile_path(&retained.id),
            &encode(&retained, MAX_PROFILE_BYTES).unwrap(),
        );
    }
    for values in [
        large_values,
        json!({"priorities":["left"],"label":"x".repeat(MAX_PROFILE_BYTES)}),
    ] {
        let fault = large.replace_recovery(&inv, &expected, values).unwrap_err();
        assert_eq!(fault.category, "StorageLimit");
        assert_eq!(fault.context["profile_id"], saved.id);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("pending").exists());
    }
    let mut excess = legacy_profile(18, "Excess");
    excess.values = json!({"priorities":["left"],"label":"x".repeat(59 * 1024)});
    put(
        &large.profile_path(&excess.id),
        &encode(&excess, MAX_PROFILE_BYTES).unwrap(),
    );
    assert_eq!(
        large.recovery_records().unwrap_err().category,
        "StorageLimit"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn recovery_refuses_filename_aliases_without_restoring_the_expected_path() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let expected = store.recovery_records().unwrap().pop().unwrap();
    let path = store.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    let alias = store
        .directory()
        .join(format!("{}.config", saved.id.to_uppercase()));
    fs::rename(&path, &alias).unwrap();
    assert!(store.recovery_records().is_err());
    assert!(store.replace_recovery(&inv, &expected, options()).is_err());
    assert_eq!(fs::read(&alias).unwrap(), before);
    assert!(!path.with_extension("pending").exists());
    let names: Vec<_> = fs::read_dir(store.directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [alias.file_name().unwrap().to_os_string()]);
}

#[test]
fn failed_atomic_cleanup_remains_visible_and_blocks_recovery() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Original", options()).unwrap();
    let expected = store.recovery_records().unwrap().pop().unwrap();
    let path = store.profile_path(&saved.id);
    let before = fs::read(&path).unwrap();
    let mut candidate = saved;
    candidate.values = json!({"priorities":["left"]});
    let bytes = encode(&candidate, MAX_PROFILE_BYTES).unwrap();
    let fault = write_atomic(&path, &bytes, |temporary, _| {
        fs::remove_file(temporary)?;
        fs::create_dir(temporary)?;
        Err(std::io::Error::other("forced publication refusal"))
    })
    .unwrap_err();
    assert_eq!(fault.context["temporary_cleanup"], "failed");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(path.with_extension("pending").is_dir());
    assert!(store.recovery_records().is_err());
    assert!(store.replace_recovery(&inv, &expected, options()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(path.with_extension("pending").is_dir());
}

#[test]
fn recovery_preserves_other_profile_fault_attribution() {
    let directory = Directory::new();
    let store = directory.profiles("Owner");
    let inv = inventory();
    let saved = store.save(&inv, None, "Repair", options()).unwrap();
    let other = store.save(&inv, None, "Damaged", options()).unwrap();
    let expected = store
        .recovery_records()
        .unwrap()
        .into_iter()
        .find(|record| record.profile.id == saved.id)
        .unwrap();
    let before = fs::read(store.profile_path(&saved.id)).unwrap();
    fs::write(store.profile_path(&other.id), b"invalid JSON").unwrap();
    let error = store
        .replace_recovery(&inv, &expected, options())
        .unwrap_err();
    assert_eq!(error.category, "StorageFormat");
    assert_eq!(error.context["profile_id"], other.id);
    assert_eq!(fs::read(store.profile_path(&saved.id)).unwrap(), before);
    assert_eq!(
        fs::read(store.profile_path(&other.id)).unwrap(),
        b"invalid JSON"
    );
}
