use super::*;
use crate::configuration::tests::Root;
use crate::storage::{PackageReference, PackageSource, Settings, Store};
use crate::target::{ResolvedLocation, TargetBinding, TargetResolution};
use mado_runtime_comparison::inventory::{Entries, Entry, Inventory};
use serde_json::json;

pub(crate) fn old_id(sequence: u64) -> String {
    format!("p-{sequence:032x}-00000001-0000000000000001")
}

fn profile(package: &str) -> Profile {
    Profile {
        version: 1,
        id: old_id(1),
        name: "Preserved profile".into(),
        package_id: package.into(),
        schema_identity: "a".repeat(64),
        values: json!({"label": old_id(1), "region": "r1", "source": format!("const alias = '{}';", old_id(1))}),
    }
}

pub(crate) fn fixture() -> (Root, Capture) {
    let root = Root::new();
    root.put(
        "settings.json",
        &serde_json::to_vec(&Settings::default()).unwrap(),
    );
    for tab in ["Alpha", "Beta"] {
        let record = TabRecord {
            version: 1,
            internal_name: tab.into(),
            display_name: format!("{tab} display"),
            open: true,
            packages: ["pkg", "other"]
                .into_iter()
                .map(|package| PackageReference {
                    package_id: package.into(),
                    source: PackageSource::Directory {
                        path: root
                            .0
                            .join("sources")
                            .join(package)
                            .to_str()
                            .unwrap()
                            .into(),
                    },
                })
                .collect(),
            selected_package_id: Some("pkg".into()),
        };
        root.put(
            &format!("tabs/{tab}/tab.config"),
            &serde_json::to_vec(&record).unwrap(),
        );
    }
    for (tab, package) in [("Alpha", "pkg"), ("Alpha", "other"), ("Beta", "pkg")] {
        root.put(
            &format!("tabs/{tab}/{package}/{}.config", old_id(1)),
            &serde_json::to_vec_pretty(&profile(package)).unwrap(),
        );
    }
    let path = format!("/offline/{}", old_id(1));
    let mut configuration = crate::target::tests::configuration(&path);
    configuration.arguments.push(old_id(1));
    let target = TargetRecord {
        version: 1,
        internal_name: "Alpha".into(),
        package_id: "pkg".into(),
        revision: 7,
        binding: Some(TargetBinding {
            id: old_id(1),
            package_id: "pkg".into(),
            target_id: "metadata-game".into(),
            declaration_identity: crate::target::tests::declaration().identity().unwrap(),
            configuration,
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
    root.put(
        &format!("profiles/{}.json", old_id(1)),
        &serde_json::to_vec_pretty(&profile("pkg")).unwrap(),
    );
    root.put("sources/pkg/main.ts", old_id(1).as_bytes());
    root.put("sources/pkg/assets/r1.png", b"existing asset bytes");
    let captured = configuration::capture(&root.0).unwrap();
    (root, captured)
}

pub(crate) fn normalized(before: &Capture) -> Capture {
    normalize_with(before.clone(), before, &mut storage::new_id).unwrap()
}

#[test]
fn strict_current_ids_and_exact_conversion_decoder_remain_separate() {
    for id in ["00000000000000000000", "9m4e2mr0ui3e8a215n4g"] {
        validate_id(id).unwrap();
    }
    for id in [
        "00000000000000000001",
        "0000000000000000000v",
        "0000000000000000000G",
        "0000000000000000000",
        "000000000000000000000",
        "../../target.config",
        "vvvvvvvvvvvvvvvvvvvw",
    ] {
        assert!(validate_id(id).is_err(), "{id}");
    }
    let old = old_id(1);
    assert!(validate_id(&old).is_err());
    validate_ingress_id(&old).unwrap();
    for invalid in [
        old.to_uppercase(),
        old.replace("00000001", "0000000g"),
        format!("{old}0"),
        old.replace('-', "_"),
    ] {
        assert!(validate_ingress_id(&invalid).is_err());
    }
}

#[test]
fn typed_conversion_is_owner_isolated_and_preserves_every_other_field() {
    let (root, before) = fixture();
    let after = normalized(&before);
    let ledger = Ledger::read(&after.files).unwrap();
    assert_eq!(ledger.entries.len(), 4);
    let ids: BTreeSet<_> = ledger.entries.iter().map(|entry| &entry.xid).collect();
    assert_eq!(ids.len(), 4);
    for entry in &ledger.entries {
        validate_id(&entry.xid).unwrap();
        let prefix = format!("tabs/{}/{}", entry.internal_name, entry.package_id);
        match entry.kind {
            EntityKind::Profile => {
                let mut actual: Profile =
                    decode(&after.files[&format!("{prefix}/{}.config", entry.xid)]).unwrap();
                actual.id = entry.legacy_id.clone();
                let expected: Profile =
                    decode(&before.files[&format!("{prefix}/{}.config", entry.legacy_id)]).unwrap();
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
                assert!(
                    !after
                        .files
                        .contains_key(&format!("{prefix}/{}.config", entry.legacy_id))
                );
            }
            EntityKind::TargetBinding => {
                let mut actual: TargetRecord =
                    decode(&after.files[&format!("{prefix}/target.config")]).unwrap();
                actual.binding.as_mut().unwrap().id = entry.legacy_id.clone();
                let expected: TargetRecord =
                    decode(&before.files[&format!("{prefix}/target.config")]).unwrap();
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
            }
        }
    }
    for path in [
        "settings.json",
        "tabs/Alpha/tab.config",
        "tabs/Beta/pkg/target.config",
        &format!("profiles/{}.json", old_id(1)),
    ] {
        assert_eq!(after.files[path], before.files[path]);
    }
    assert_eq!(
        normalize_with(after.clone(), &after, &mut || panic!(
            "current IDs must not allocate"
        ))
        .unwrap(),
        after
    );
    migrate(&root.0).unwrap();
    let installed = configuration::capture(&root.0).unwrap();
    migrate(&root.0).unwrap();
    assert_eq!(configuration::capture(&root.0).unwrap(), installed);
    assert_eq!(
        fs::read(root.0.join("sources/pkg/main.ts")).unwrap(),
        old_id(1).as_bytes()
    );
    assert_eq!(
        fs::read(root.0.join("sources/pkg/assets/r1.png")).unwrap(),
        b"existing asset bytes"
    );
}

#[test]
fn current_records_and_unassigned_sources_do_not_create_metadata() {
    let root = Root::new();
    root.put(
        "profiles/unassigned.json",
        b"raw malformed preservation material",
    );
    root.put(
        "tabs/Owner/pkg/00000000000000000000.config",
        b"scoped malformed current profile",
    );
    root.put("tabs/Owner/pkg/target.config", b"scoped malformed target");
    let before = configuration::capture(&root.0).unwrap();
    migrate(&root.0).unwrap();
    assert_eq!(configuration::capture(&root.0).unwrap(), before);
    assert!(!root.0.join(LEDGER).exists());
}

#[test]
fn migration_candidate_requires_complete_safe_owned_scope_without_schema_repair() {
    let (root, before) = fixture();
    for (path, bytes) in [
        (
            "tabs/Alpha/pkg/unknown.config".to_owned(),
            b"invalid owned profile".to_vec(),
        ),
        (
            format!("tabs/Alpha/pkg/{}.config", old_id(2)),
            serde_json::to_vec(&profile("pkg")).unwrap(),
        ),
        (
            "tabs/Orphan/pkg/target.config".into(),
            before.files["tabs/Alpha/pkg/target.config"].clone(),
        ),
    ] {
        root.put(&path, &bytes);
        let observed = configuration::capture(&root.0).unwrap();
        assert!(migrate(&root.0).is_err());
        assert_eq!(configuration::capture(&root.0).unwrap(), observed);
        assert!(!crate::restore::pending(&root.0).unwrap());
        fs::remove_file(root.0.join(&path)).unwrap();
    }
    let mut future = before.clone();
    let path = format!("tabs/Alpha/pkg/{}.config", old_id(1));
    let mut record = profile("pkg");
    record.version = 2;
    future
        .files
        .insert(path, serde_json::to_vec(&record).unwrap());
    let future = Capture::from_files(future.files, true).unwrap();
    assert!(normalize_with(future.clone(), &future, &mut storage::new_id).is_err());
}

#[test]
fn migration_capture_refusals_identify_the_managed_path_without_mutation() {
    for path in [
        "settings.pending",
        "profiles/legacy.pending",
        "profiles/foreign.JSON",
        "tabs/Beta/tab.pending",
        "tabs/Beta/pkg/00000000000000000000.pending",
        "tabs/Beta/pkg/foreign.CONFIG",
    ] {
        let (root, before) = fixture();
        let interrupted = b"preserve the interrupted or aliased configuration";
        root.put(path, interrupted);
        let error = migrate(&root.0).unwrap_err();
        assert_eq!(
            Path::new(error.context["path"].as_str().unwrap()),
            Path::new(path)
        );
        assert_eq!(fs::read(root.0.join(path)).unwrap(), interrupted);
        assert!(!crate::restore::pending(&root.0).unwrap());
        fs::remove_file(root.0.join(path)).unwrap();
        assert_eq!(configuration::capture(&root.0).unwrap(), before);
    }
}

fn inventory() -> Inventory {
    Inventory {
        identity: "fixture".into(),
        package_id: "pkg".into(),
        sources: BTreeMap::new(),
        assets: BTreeMap::new(),
        schema: json!({"version":1,"type":"object","additionalProperties":false,"properties":{"label":{"type":"string"},"region":{"type":"string"},"source":{"type":"string"}},"required":["label"]}),
        profiles: BTreeMap::new(),
        entries: Entries {
            readiness: Entry {
                module: "main.js".into(),
                function: "ready".into(),
            },
            workflow: Entry {
                module: "main.js".into(),
                function: "run".into(),
            },
        },
        metadata: json!({}),
        source_maps: BTreeMap::new(),
    }
}

#[test]
fn stale_schema_stays_rejected_until_explicit_repair_and_retains_committed_xid() {
    let (root, _) = fixture();
    migrate(&root.0).unwrap();
    let store = Store::new(root.0.clone()).unwrap();
    let scoped = store.profile_store("Alpha", "pkg").unwrap();
    let inv = inventory();
    let schema = mado_runtime_comparison::model::identity(&inv.schema).unwrap();
    let listing = scoped.list("pkg", &schema).unwrap();
    assert!(listing.profiles.is_empty());
    assert_eq!(listing.rejected.len(), 1);
    let record = scoped.recovery_records().unwrap().pop().unwrap();
    assert_eq!(record.profile.values, profile("pkg").values);
    assert_eq!(record.profile.schema_identity, "a".repeat(64));
    let id = record.profile.id.clone();
    let repaired = scoped
        .replace_recovery(&inv, &record, record.profile.values.clone())
        .unwrap();
    assert_eq!(repaired.id, id);
    migrate(&root.0).unwrap();
    let reopened = Store::new(root.0.clone())
        .unwrap()
        .profile_store("Alpha", "pkg")
        .unwrap()
        .list("pkg", &schema)
        .unwrap();
    assert_eq!(reopened.profiles[0].id, id);
    assert_eq!(reopened.profiles[0].values, profile("pkg").values);
}

fn assignment(index: u64) -> Assignment {
    Assignment {
        kind: EntityKind::Profile,
        internal_name: "Retired".into(),
        package_id: "pkg".into(),
        legacy_id: old_id(index),
        xid: format!("{index:019x}0"),
    }
}

#[test]
fn ledger_rejects_duplicates_conflicts_unknown_fields_versions_and_unsafe_owners() {
    let base = json!({"version":1,"entries":[assignment(1)]});
    let mut cases = Vec::new();
    let mut value = base.clone();
    value["version"] = json!(2);
    cases.push(value);
    let mut value = base.clone();
    value["unknown"] = json!(true);
    cases.push(value);
    let mut value = base.clone();
    value["entries"][0]["unknown"] = json!(true);
    cases.push(value);
    let mut value = base.clone();
    value["entries"][0]["internal_name"] = json!("../escape");
    cases.push(value);
    let mut value = base.clone();
    value["entries"][0]["package_id"] = json!("../escape");
    cases.push(value);
    let mut value = base.clone();
    value["entries"][0]["xid"] = json!("00000000000000000001");
    cases.push(value);
    let mut value = base.clone();
    value["entries"][0]["legacy_id"] = json!("p-invalid");
    cases.push(value);
    let mut value = base.clone();
    value["entries"] = json!([assignment(1), assignment(1)]);
    cases.push(value);
    let mut conflicting = assignment(1);
    conflicting.xid = assignment(2).xid;
    let mut value = base.clone();
    value["entries"] = json!([assignment(1), conflicting]);
    cases.push(value);
    let mut duplicate_destination = assignment(2);
    duplicate_destination.xid = assignment(1).xid;
    let mut value = base;
    value["entries"] = json!([assignment(1), duplicate_destination]);
    cases.push(value);
    for value in cases {
        assert!(
            Ledger::decode(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{value}"
        );
    }
    assert!(Ledger::decode(br#"{"version":1,"version":1,"entries":[]}"#).is_err());
    assert!(
        Ledger::decode(br#"{"version":1,"entries":[["profile","Owner","pkg","old","new"]]}"#)
            .is_err()
    );
}

#[test]
fn ledger_capacity_and_managed_budgets_never_evict_reservations() {
    let ledger = Ledger {
        version: 1,
        entries: (0..MAX_ENTRIES as u64).map(assignment).collect(),
    };
    let bytes = encode(&ledger, MAX_LEDGER_BYTES).unwrap();
    assert_eq!(Ledger::decode(&bytes).unwrap().entries.len(), MAX_ENTRIES);
    let mut ledger = Ledger::decode(&bytes).unwrap();
    let mut used = BTreeSet::new();
    assert!(
        ledger
            .assign(
                EntityKind::Profile,
                "New",
                "pkg",
                &old_id(9999),
                &mut used,
                &mut || panic!("capacity is checked before allocation")
            )
            .is_err()
    );
    assert_eq!(encode(&ledger, MAX_LEDGER_BYTES).unwrap(), bytes);
    ledger.entries.push(assignment(MAX_ENTRIES as u64));
    assert!(Ledger::decode(&serde_json::to_vec(&ledger).unwrap()).is_err());
    assert!(Ledger::decode(&vec![b' '; MAX_LEDGER_BYTES + 1]).is_err());
    let mut files = BTreeMap::from([(LEDGER.into(), bytes)]);
    for index in 0..configuration::MAX_FILES {
        files.insert(format!("tabs/Owner/pkg/{index}.config"), Vec::new());
    }
    assert!(Capture::from_files(files, true).is_err());
}

#[test]
fn generation_faults_and_exhausted_collisions_publish_nothing() {
    let (root, before) = fixture();
    let failure = normalize_with(before.clone(), &before, &mut || {
        Err(Fault::new("IdentityGeneration", "checked provider fault"))
    })
    .unwrap_err();
    assert_eq!(failure.category, "IdentityGeneration");
    let mut files = before.files.clone();
    let mut current = profile("pkg");
    current.id = VALIDATION_ID.into();
    files.insert(
        format!("tabs/Alpha/pkg/{VALIDATION_ID}.config"),
        serde_json::to_vec(&current).unwrap(),
    );
    let occupied = Capture::from_files(files, true).unwrap();
    let mut calls = 0;
    assert!(
        normalize_with(occupied.clone(), &occupied, &mut || {
            calls += 1;
            Ok(VALIDATION_ID.into())
        })
        .is_err()
    );
    assert_eq!(calls, ATTEMPTS);
    assert_eq!(configuration::capture(&root.0).unwrap(), before);
    assert!(!crate::restore::pending(&root.0).unwrap());
}

#[test]
fn reserved_destinations_never_overwrite_or_adopt_existing_entities() {
    for (tab, kind) in [
        ("Alpha", EntityKind::Profile),
        ("Beta", EntityKind::Profile),
        ("Alpha", EntityKind::TargetBinding),
    ] {
        let (root, _) = fixture();
        let ledger = Ledger {
            version: 1,
            entries: vec![Assignment {
                kind: EntityKind::Profile,
                internal_name: "Alpha".into(),
                package_id: "pkg".into(),
                legacy_id: old_id(1),
                xid: VALIDATION_ID.into(),
            }],
        };
        root.put(LEDGER, &encode(&ledger, MAX_LEDGER_BYTES).unwrap());
        if kind == EntityKind::Profile {
            let mut occupied = profile("pkg");
            occupied.id = VALIDATION_ID.into();
            occupied.name = "Existing current profile".into();
            root.put(
                &format!("tabs/{tab}/pkg/{VALIDATION_ID}.config"),
                &serde_json::to_vec(&occupied).unwrap(),
            );
        } else {
            let path = root.0.join("tabs/Alpha/pkg/target.config");
            let mut occupied: TargetRecord = decode(&fs::read(&path).unwrap()).unwrap();
            occupied.binding.as_mut().unwrap().id = VALIDATION_ID.into();
            fs::write(path, serde_json::to_vec(&occupied).unwrap()).unwrap();
        }
        let before = configuration::capture(&root.0).unwrap();
        let error = migrate(&root.0).unwrap_err();
        assert_eq!(error.category, "IdentityMigration");
        assert_eq!(configuration::capture(&root.0).unwrap(), before);
        assert!(!crate::restore::pending(&root.0).unwrap());
    }
}

#[test]
fn archive_replay_reuses_live_mappings_and_refuses_a_conflicting_lineage() {
    let (root, archive) = fixture();
    let after = normalized(&archive);
    let replay = normalize_restore(archive.clone(), &after).unwrap();
    assert_eq!(replay, after);
    let other = normalized(&archive);
    assert!(normalize_restore(other, &after).is_err());
    assert_eq!(configuration::capture(&root.0).unwrap(), archive);
    let rollback = rollback_capture(&archive, &after).unwrap();
    for (path, bytes) in &archive.files {
        assert_eq!(&rollback.files[path], bytes);
    }
    assert_eq!(
        Ledger::read(&rollback.files).unwrap().entries,
        Ledger::read(&after.files).unwrap().entries
    );
    assert_eq!(normalize_restore(archive, &rollback).unwrap(), after);
}

#[test]
fn tombstones_survive_owner_retirement_and_do_not_recreate_configuration() {
    let (root, before) = fixture();
    let after = normalized(&before);
    let settings_only = Capture::from_files(
        BTreeMap::from([(
            "settings.json".into(),
            before.files["settings.json"].clone(),
        )]),
        true,
    )
    .unwrap();
    let retired = normalize_restore(settings_only, &after).unwrap();
    assert_eq!(retired.files.len(), 2);
    assert_eq!(retired.files[LEDGER], after.files[LEDGER]);
    let observed = configuration::capture(&root.0).unwrap();
    let generation = observed.generation.clone();
    let plan = crate::restore::prepare(retired.clone(), observed, Some(&generation)).unwrap();
    crate::restore::install(&root.0, plan).unwrap();
    assert_eq!(configuration::capture(&root.0).unwrap(), retired);
    migrate(&root.0).unwrap();
    assert_eq!(configuration::capture(&root.0).unwrap(), retired);
    assert!(
        Store::new(root.0.clone())
            .unwrap()
            .tabs()
            .unwrap()
            .tabs
            .is_empty()
    );
}

#[test]
fn raw_backup_preserves_malformed_ledger_and_v2_reader_rejects_it_for_restore() {
    let root = Root::new();
    root.put(
        "settings.json",
        &serde_json::to_vec(&Settings::default()).unwrap(),
    );
    root.put(LEDGER, b"{broken ledger retained exactly");
    let before = configuration::capture(&root.0).unwrap();
    let receipt = crate::backup::write(&root.0, before.clone(), None).unwrap();
    let archived = crate::backup::read(Path::new(&receipt.path)).unwrap();
    assert_eq!(archived, before);
    assert_eq!(crate::backup::Manifest::new(&archived).version, 2);
    let mut old_manifest = crate::backup::Manifest::new(&archived);
    old_manifest.version = 1;
    assert!(old_manifest.check().is_err());
    assert!(crate::restore::validate(&archived).is_err());
    assert_eq!(configuration::capture(&root.0).unwrap(), before);
}

#[test]
fn migration_journal_validation_refuses_nonidentity_edits_or_missing_assignments() {
    let (_, before) = fixture();
    let after = normalized(&before);
    validate_transition(&before, &after, &Operation::IdentityMigration).unwrap();
    let mut files = after.files.clone();
    files.insert(
        "settings.json".into(),
        serde_json::to_vec(&Settings {
            gui_log_limit: 1200,
            ..Settings::default()
        })
        .unwrap(),
    );
    assert!(
        validate_transition(
            &before,
            &Capture::from_files(files, true).unwrap(),
            &Operation::IdentityMigration
        )
        .is_err()
    );
    let mut files = after.files;
    files.remove(LEDGER);
    assert!(
        validate_transition(
            &before,
            &Capture::from_files(files, true).unwrap(),
            &Operation::IdentityMigration
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn ledger_paths_refuse_aliases_links_and_pending_writes_without_mutation() {
    use std::os::unix::fs::symlink;
    let root = Root::new();
    let bytes = br#"{"version":1,"entries":[]}"#;
    root.put("Identity-Migrations.config", bytes);
    assert!(migrate(&root.0).is_err());
    assert_eq!(
        fs::read(root.0.join("Identity-Migrations.config")).unwrap(),
        bytes
    );
    fs::remove_file(root.0.join("Identity-Migrations.config")).unwrap();
    root.put("retained", bytes);
    symlink(root.0.join("retained"), root.0.join(LEDGER)).unwrap();
    assert!(migrate(&root.0).is_err());
    fs::remove_file(root.0.join(LEDGER)).unwrap();
    fs::hard_link(root.0.join("retained"), root.0.join(LEDGER)).unwrap();
    assert!(migrate(&root.0).is_err());
    fs::remove_file(root.0.join(LEDGER)).unwrap();
    root.put("identity-migrations.pending", b"partial");
    assert!(migrate(&root.0).is_err());
    assert_eq!(
        fs::read(root.0.join("identity-migrations.pending")).unwrap(),
        b"partial"
    );
    assert_eq!(fs::read(root.0.join("retained")).unwrap(), bytes);
}

#[test]
fn rollback_reservation_budget_is_admitted_before_publication() {
    let mut files = BTreeMap::new();
    for index in 0..configuration::MAX_FILES {
        files.insert(format!("tabs/Owner/pkg/{index}.config"), Vec::new());
    }
    let before = Capture::from_files(files, true).unwrap();
    let ledger = Ledger {
        version: 1,
        entries: vec![assignment(1)],
    };
    let after = Capture::from_files(
        BTreeMap::from([(LEDGER.into(), encode(&ledger, MAX_LEDGER_BYTES).unwrap())]),
        true,
    )
    .unwrap();
    assert!(check_rollback_budget(&before, &after).is_err());
    assert!(rollback_capture(&before, &after).is_err());
    assert_eq!(before.files.len(), configuration::MAX_FILES);
    assert!(!before.files.contains_key(LEDGER));
}
