use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    publisher: Publisher,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mado-authoring-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        crate::storage::private_directory(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let publisher = Publisher::new(root.join("private"));
        Self { root, publisher }
    }
    fn package(&self) -> Candidate {
        self.publisher
            .create(&self.root.join("package"), "sample")
            .unwrap()
    }
    fn save(&self, candidate: &Candidate, path: &str, text: &str) -> Candidate {
        self.publisher
            .publish(
                candidate,
                candidate.revision(),
                Edit::Text {
                    path: path.into(),
                    text: text.into(),
                },
            )
            .unwrap()
            .candidate
            .unwrap()
    }
    fn catalog(&self, candidate: &Candidate, edit: CatalogEdit) -> Candidate {
        self.publisher
            .publish(candidate, candidate.revision(), Edit::Catalog(edit))
            .unwrap()
            .candidate
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn add_source(path: &str) -> Edit {
    Edit::Catalog(CatalogEdit::Add {
        path: path.into(),
        file_kind: CatalogFileKind::Source,
        text: Some("export const answer = 42;\n".into()),
        bytes: None,
        id: None,
        module: None,
        format: None,
        width: None,
        height: None,
    })
}

#[test]
fn create_and_duplicate_preserve_portable_ownership_without_local_configuration() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let original_inventory = original.validate().unwrap();
    let duplicate = fixture
        .publisher
        .duplicate(
            &original,
            original.revision(),
            &fixture.root.join("copy"),
            "other",
        )
        .unwrap();
    let copied_inventory = duplicate.validate().unwrap();
    assert_eq!(copied_inventory.package_id, "other");
    assert_eq!(copied_inventory.profiles["default"]["package_id"], "other");
    assert_eq!(copied_inventory.sources, original_inventory.sources);
    assert_eq!(
        original_inventory.identity,
        fixture
            .publisher
            .open(original.root())
            .unwrap()
            .validate()
            .unwrap()
            .identity
    );
    assert!(!duplicate.root().join("tabs").exists());
    assert!(!duplicate.root().join("settings.json").exists());
    assert!(
        fixture
            .publisher
            .duplicate(
                &original,
                original.revision(),
                &fixture.root.join("same-id"),
                "SAMPLE"
            )
            .is_err()
    );
    assert!(!fixture.root.join("same-id").exists());
    let occupied = fixture.root.join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("retained"), b"foreign bytes").unwrap();
    assert!(fixture.publisher.create(&occupied, "new").is_err());
    assert!(
        fixture
            .publisher
            .duplicate(&original, original.revision(), &occupied, "new")
            .is_err()
    );
    assert_eq!(
        fs::read(occupied.join("retained")).unwrap(),
        b"foreign bytes"
    );
}

#[test]
fn nested_destinations_do_not_damage_an_existing_package() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let revision = original.revision().to_owned();
    for destination in [
        original.root().join("child"),
        original.root().join("profiles/child"),
    ] {
        assert_eq!(
            fixture
                .publisher
                .create(&destination, "nested")
                .unwrap_err()
                .category,
            "AuthoringDestination",
        );
        assert!(!destination.exists());
        assert_eq!(
            fixture
                .publisher
                .duplicate(&original, &revision, &destination, "copy")
                .unwrap_err()
                .category,
            "AuthoringDestination",
        );
        assert!(!destination.exists());
    }
    assert_eq!(
        fixture.publisher.open(original.root()).unwrap().revision(),
        revision
    );
}

#[test]
fn saved_invalid_definitions_remain_repairable_without_manufacturing_inventory() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let schema = fs::read_to_string(original.root().join("schema.json")).unwrap();
    let profile = fs::read(original.root().join("profiles/default.json")).unwrap();
    let invalid = fixture.save(&original, "schema.json", "{ invalid schema");
    assert!(invalid.validate().is_err());
    assert!(Inventory::capture(invalid.root(), &limits().unwrap()).is_err());
    let reopened = fixture.publisher.open(invalid.root()).unwrap();
    assert_eq!(
        reopened
            .files()
            .into_iter()
            .find(|file| file.path == "schema.json")
            .unwrap()
            .text
            .as_deref(),
        Some("{ invalid schema")
    );
    assert_eq!(
        fs::read(invalid.root().join("profiles/default.json")).unwrap(),
        profile
    );
    let repaired = fixture.save(&reopened, "schema.json", &schema);
    assert_eq!(
        repaired.validate().unwrap().schema,
        original.validate().unwrap().schema
    );
    let invalid_profile = fixture.save(&repaired, "profiles/default.json", "{");
    assert!(invalid_profile.validate().is_err());
    assert_eq!(
        fixture
            .publisher
            .open(invalid_profile.root())
            .unwrap()
            .revision(),
        invalid_profile.revision()
    );
    let invalid_source = fixture.save(&invalid_profile, "main.ts", "export function workflow( {");
    assert_eq!(
        fs::read_to_string(invalid_source.root().join("main.ts")).unwrap(),
        "export function workflow( {"
    );
}

#[test]
fn stale_revisions_and_external_replacements_preserve_both_versions() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let changed = fixture.save(&original, "main.ts", "export const saved = 1;");
    assert_eq!(
        fixture
            .publisher
            .publish(&original, original.revision(), add_source("helper.ts"))
            .unwrap_err()
            .category,
        "AuthoringConflict"
    );
    assert_eq!(
        fixture
            .publisher
            .publish(&changed, original.revision(), add_source("helper.ts"))
            .unwrap_err()
            .category,
        "AuthoringConflict"
    );
    let external = fixture.root.join("external.ts");
    fs::write(&external, b"export const external = 2;").unwrap();
    fs::rename(external, changed.root().join("main.ts")).unwrap();
    assert_eq!(
        fixture
            .publisher
            .publish(&changed, changed.revision(), add_source("helper.ts"))
            .unwrap_err()
            .category,
        "AuthoringConflict"
    );
    assert_eq!(
        fs::read(changed.root().join("main.ts")).unwrap(),
        b"export const external = 2;"
    );
    assert!(!changed.root().join("helper.ts").exists());
    assert_eq!(
        changed
            .files()
            .into_iter()
            .find(|file| file.path == "main.ts")
            .unwrap()
            .text
            .as_deref(),
        Some("export const saved = 1;")
    );
}

#[test]
fn catalog_updates_required_references_and_never_refactors_imports() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let added = fixture
        .publisher
        .publish(&original, original.revision(), add_source("helper.ts"))
        .unwrap()
        .candidate
        .unwrap();
    let importing = fixture.save(
        &added,
        "main.ts",
        "import { answer } from './helper.js';\nexport { answer };\n",
    );
    let source = fs::read(importing.root().join("main.ts")).unwrap();
    let renamed = fixture.catalog(
        &importing,
        CatalogEdit::Rename {
            path: "helper.ts".into(),
            destination: "lib/answer.ts".into(),
        },
    );
    assert!(!renamed.root().join("helper.ts").exists());
    assert_eq!(fs::read(renamed.root().join("main.ts")).unwrap(), source);
    assert_eq!(
        renamed.draft.manifest().unwrap()["sources"],
        json!(["main.ts", "lib/answer.ts"])
    );
    let entry_renamed = fixture.catalog(
        &renamed,
        CatalogEdit::Rename {
            path: "main.ts".into(),
            destination: "workflow.ts".into(),
        },
    );
    assert_eq!(
        entry_renamed.validate().unwrap().entries.workflow.module,
        "workflow.ts"
    );
    let removed = fixture.catalog(
        &entry_renamed,
        CatalogEdit::Remove {
            path: "lib/answer.ts".into(),
        },
    );
    assert!(!removed.root().join("lib/answer.ts").exists());
    let expected = removed.revision();
    for path in [
        "workflow.ts",
        "schema.json",
        "profiles/default.json",
        "package.json",
    ] {
        assert!(
            fixture
                .publisher
                .publish(
                    &removed,
                    expected,
                    Edit::Catalog(CatalogEdit::Remove { path: path.into() })
                )
                .is_err()
        );
        assert_eq!(
            fixture.publisher.open(removed.root()).unwrap().revision(),
            expected
        );
    }
}

#[test]
fn manifest_edit_accepts_portable_intent_but_refuses_identity_and_declaration_changes() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let mut manifest = original.draft.manifest().unwrap();
    manifest["target"] = json!({"id":"demo","window_title":"Editor target","macos":{"bundle_id":"com.example.demo"}});
    let saved = fixture.save(&original, "package.json", &manifest.to_string());
    assert_eq!(
        saved.validate().unwrap().target().unwrap().unwrap().id,
        "demo"
    );
    let valid = manifest.clone();
    for malformed in [
        "{ broken".to_owned(),
        {
            let mut value = valid.clone();
            value["package_id"] = json!("foreign");
            value.to_string()
        },
        {
            let mut value = valid.clone();
            value["sources"] = json!(["missing.ts"]);
            value.to_string()
        },
        {
            let mut value = valid.clone();
            value["native_config"] = json!({"path":"/tmp/executable"});
            value.to_string()
        },
    ] {
        assert!(
            fixture
                .publisher
                .publish(
                    &saved,
                    saved.revision(),
                    Edit::Text {
                        path: "package.json".into(),
                        text: malformed
                    }
                )
                .is_err()
        );
        assert_eq!(
            fixture.publisher.open(saved.root()).unwrap().revision(),
            saved.revision()
        );
    }
}

#[test]
fn unsafe_catalog_paths_and_case_collisions_never_begin_publication() {
    let fixture = Fixture::new();
    let original = fixture.package();
    for path in [
        "../escape.ts",
        "/absolute.ts",
        "a/../../escape.ts",
        "MAIN.ts",
        "main.ts/child.ts",
        "CON.ts",
        "node_modules/helper.ts",
        "a\\file.ts",
    ] {
        assert!(
            fixture
                .publisher
                .publish(&original, original.revision(), add_source(path))
                .is_err(),
            "{path}"
        );
        assert_eq!(
            fixture.publisher.open(original.root()).unwrap().revision(),
            original.revision()
        );
    }
    assert!(!fixture.root.join("escape.ts").exists());
    assert!(
        fixture
            .publisher
            .create(&fixture.root.join("PACKAGE"), "other")
            .is_err()
    );
    // PathBuf::join removes `..` when the Windows root has a verbatim prefix.
    let mut traversal = fixture.root.as_os_str().to_os_string();
    for component in ["package", "..", "escape"] {
        traversal.push(std::path::MAIN_SEPARATOR_STR);
        traversal.push(component);
    }
    assert_eq!(
        fixture
            .publisher
            .create(Path::new(&traversal), "other")
            .unwrap_err()
            .category,
        "AuthoringPath"
    );
    assert!(!fixture.root.join("escape").exists());
}

#[cfg(unix)]
#[test]
fn symlinks_and_hardlinks_cannot_be_opened_or_written_through() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let original = fixture.package();
    let bytes = fs::read(original.root().join("main.ts")).unwrap();
    let external = fixture.root.join("outside.ts");
    fs::write(&external, &bytes).unwrap();
    fs::remove_file(original.root().join("main.ts")).unwrap();
    symlink(&external, original.root().join("main.ts")).unwrap();
    assert!(fixture.publisher.open(original.root()).is_err());
    assert!(
        fixture
            .publisher
            .publish(&original, original.revision(), add_source("helper.ts"))
            .is_err()
    );
    assert_eq!(fs::read(&external).unwrap(), bytes);
    fs::remove_file(original.root().join("main.ts")).unwrap();
    fs::hard_link(&external, original.root().join("main.ts")).unwrap();
    assert!(fixture.publisher.open(original.root()).is_err());
    let alias = fixture.root.join("alias");
    symlink(original.root(), &alias).unwrap();
    assert!(fixture.publisher.create(&alias, "another").is_err());
    assert!(fixture.publisher.open(&alias).is_err());
}

#[test]
fn interrupted_file_and_manifest_publication_is_refused_until_restart_recovery() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let failure = fixture
        .publisher
        .publish_with(
            &original,
            original.revision(),
            add_source("helper.ts"),
            |_| Err(Fault::new("Interrupted", "simulated process interruption")),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(failure.category, "AuthoringRecoveryRequired");
    assert!(original.root().join("helper.ts").exists());
    assert!(Inventory::capture(original.root(), &limits().unwrap()).is_err());
    assert_eq!(
        fixture
            .publisher
            .check_admission(original.root())
            .unwrap_err()
            .category,
        "AuthoringRecoveryRequired"
    );
    let restarted = Publisher::new(fixture.root.join("private"));
    assert_eq!(
        restarted.recover_pending().unwrap_err().category,
        "AuthoringRecoveryRequired"
    );
    assert!(restarted.recover(&fixture.root).is_err());
    restarted.recover(original.root()).unwrap();
    let recovered = restarted.open(original.root()).unwrap();
    assert_eq!(
        recovered.validate().unwrap().sources["helper.ts"],
        "export const answer = 42;\n"
    );
}

#[test]
fn conflicting_external_bytes_and_durable_preimages_survive_failed_recovery() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let manifest = fs::read(original.root().join("package.json")).unwrap();
    fixture
        .publisher
        .publish_with(
            &original,
            original.revision(),
            add_source("helper.ts"),
            |_| Err(Fault::new("Interrupted", "interruption")),
            || Ok(()),
        )
        .unwrap_err();
    let journal = fixture.root.join("private/authoring/pending.json");
    let retained = fs::read(&journal).unwrap();
    fs::write(original.root().join("helper.ts"), b"external conflict").unwrap();
    let restarted = Publisher::new(fixture.root.join("private"));
    assert_eq!(
        restarted.recover(original.root()).unwrap_err().category,
        "AuthoringRecoveryRequired"
    );
    assert_eq!(
        fs::read(original.root().join("helper.ts")).unwrap(),
        b"external conflict"
    );
    assert_eq!(
        fs::read(original.root().join("package.json")).unwrap(),
        manifest
    );
    assert_eq!(fs::read(&journal).unwrap(), retained);
    let private = fixture.root.join("private");
    crate::configuration::write_private(&private.join("settings.json"), b"{\"version\":1}")
        .unwrap();
    let exported = crate::configuration::capture(&private).unwrap();
    assert_eq!(
        exported.files,
        std::collections::BTreeMap::from([("settings.json".into(), b"{\"version\":1}".to_vec())])
    );
    assert_eq!(
        restarted
            .check_admission(original.root())
            .unwrap_err()
            .category,
        "AuthoringRecoveryRequired"
    );
}

#[test]
fn completed_save_and_failed_refresh_are_distinct_and_old_candidate_cannot_retry() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let saved = "export const persisted = 37;";
    let displaced = fixture.root.join("saved-main.ts");
    let commit = fixture
        .publisher
        .publish_with(
            &original,
            original.revision(),
            Edit::Text {
                path: "main.ts".into(),
                text: saved.into(),
            },
            |_| Ok(()),
            || {
                fs::rename(original.root().join("main.ts"), &displaced)
                    .map_err(|error| io_fault("displace saved source before refresh", error))
            },
        )
        .unwrap();
    assert_eq!(commit.refresh_error.unwrap().context["path"], "main.ts");
    assert!(commit.candidate.is_none());
    assert_eq!(fs::read(&displaced).unwrap(), saved.as_bytes());
    fs::rename(displaced, original.root().join("main.ts")).unwrap();
    assert_eq!(
        fixture.publisher.open(original.root()).unwrap().revision(),
        commit.committed_revision
    );
    assert_eq!(
        fixture
            .publisher
            .publish(&original, original.revision(), add_source("helper.ts"))
            .unwrap_err()
            .category,
        "AuthoringConflict"
    );
}

#[cfg(unix)]
#[test]
fn single_file_save_does_not_replace_unmodified_files() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let original = fixture.package();
    let manifest = fs::metadata(original.root().join("package.json")).unwrap();
    let schema = fs::metadata(original.root().join("schema.json")).unwrap();
    let saved = fixture.save(&original, "main.ts", "export const changed = true;");
    assert_eq!(
        fs::metadata(saved.root().join("package.json"))
            .unwrap()
            .ino(),
        manifest.ino()
    );
    assert_eq!(
        fs::metadata(saved.root().join("schema.json"))
            .unwrap()
            .ino(),
        schema.ino()
    );
}

#[test]
fn excess_bytes_or_undeclared_files_do_not_weaken_capture() {
    let fixture = Fixture::new();
    let original = fixture.package();
    assert!(
        fixture
            .publisher
            .publish(
                &original,
                original.revision(),
                Edit::Text {
                    path: "main.ts".into(),
                    text: "x".repeat(MAX_BYTES)
                }
            )
            .is_err()
    );
    assert_eq!(
        fixture.publisher.open(original.root()).unwrap().revision(),
        original.revision()
    );
    fs::write(original.root().join("unowned.json"), b"{}").unwrap();
    assert!(fixture.publisher.open(original.root()).is_err());
    assert!(Inventory::capture(original.root(), &limits().unwrap()).is_err());
}

#[test]
fn foreign_preset_ownership_blocks_duplicate_without_reassigning_the_original() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let profile = json!({"package_id":"foreign","schema_version":1,"options":{}}).to_string();
    let candidate = fixture.save(&original, "profiles/default.json", &profile);
    let destination = fixture.root.join("copy");
    assert!(
        fixture
            .publisher
            .duplicate(&candidate, candidate.revision(), &destination, "new-owner")
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(candidate.root().join("profiles/default.json")).unwrap(),
        profile
    );
    assert!(!destination.exists());
}

#[test]
fn binary_assets_and_source_maps_use_the_existing_manifest_contract() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let asset = fixture.catalog(
        &original,
        CatalogEdit::Add {
            path: "assets/pixel.rgba".into(),
            file_kind: CatalogFileKind::Asset,
            text: None,
            bytes: Some(vec![1, 2, 3, 255]),
            id: Some("pixel".into()),
            module: None,
            format: Some("raw-rgba8".into()),
            width: Some(1),
            height: Some(1),
        },
    );
    let view = asset
        .files()
        .into_iter()
        .find(|file| file.path == "assets/pixel.rgba")
        .unwrap();
    assert_eq!(view.kind, DraftFileKind::Asset);
    assert_eq!(view.text, None);
    assert_eq!(asset.validate().unwrap().assets["pixel"], [1, 2, 3, 255]);
    let mapped = fixture.catalog(
        &asset,
        CatalogEdit::Add {
            path: "main.ts.map".into(),
            file_kind: CatalogFileKind::SourceMap,
            text: Some("{".into()),
            bytes: None,
            id: None,
            module: Some("main.ts".into()),
            format: None,
            width: None,
            height: None,
        },
    );
    assert!(mapped.validate().is_err());
    let map = json!({"version":3,"sources":["main.ts"],"mappings":""}).to_string();
    let repaired = fixture.save(&mapped, "main.ts.map", &map);
    assert_eq!(repaired.validate().unwrap().source_maps["main.ts"], map);
    let renamed = fixture.catalog(
        &repaired,
        CatalogEdit::Rename {
            path: "main.ts".into(),
            destination: "workflow.ts".into(),
        },
    );
    assert_eq!(renamed.validate().unwrap().source_maps["workflow.ts"], map);
}

#[test]
fn snapshot_file_limit_applies_to_catalog_add_before_any_write() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let mut manifest = original.draft.manifest().unwrap();
    for index in 0..MAX_FILES - original.draft.files().len() {
        let path = format!("helper-{index}.ts");
        fs::write(original.root().join(&path), b"export {};").unwrap();
        manifest["sources"]
            .as_array_mut()
            .unwrap()
            .push(json!(path));
    }
    fs::write(original.root().join("package.json"), manifest.to_string()).unwrap();
    let full = fixture.publisher.open(original.root()).unwrap();
    assert_eq!(full.files().len(), MAX_FILES);
    assert!(
        fixture
            .publisher
            .publish(&full, full.revision(), add_source("over-limit.ts"))
            .is_err()
    );
    assert_eq!(
        fixture.publisher.open(full.root()).unwrap().revision(),
        full.revision()
    );
    assert!(!full.root().join("over-limit.ts").exists());
}

#[test]
fn malformed_pending_journal_refuses_startup_and_preserves_repair_material() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let directory = fixture.root.join("private/authoring");
    crate::storage::private_directory(&directory).unwrap();
    let journal = directory.join("pending.json");
    crate::configuration::write_private(&journal, b"{torn journal").unwrap();
    let restarted = Publisher::new(fixture.root.join("private"));
    assert_eq!(
        restarted.recover_pending().unwrap_err().category,
        "AuthoringRecoveryRequired"
    );
    assert_eq!(
        restarted
            .check_admission(original.root())
            .unwrap_err()
            .category,
        "AuthoringRecoveryRequired"
    );
    assert!(restarted.recover(original.root()).is_err());
    assert_eq!(fs::read(journal).unwrap(), b"{torn journal");
    assert_eq!(
        Inventory::capture(original.root(), &limits().unwrap())
            .unwrap()
            .identity,
        original.validate().unwrap().identity
    );
}

#[test]
fn retained_empty_directories_count_toward_the_prospective_snapshot_budget() {
    let fixture = Fixture::new();
    let original = fixture.package();
    for index in 0..MAX_FILES * 2 - original.draft.files().len() - 1 {
        fs::create_dir(original.root().join(format!("empty-{index}"))).unwrap();
    }
    let full = fixture.publisher.open(original.root()).unwrap();
    assert!(
        fixture
            .publisher
            .publish(&full, full.revision(), add_source("one-too-many.ts"))
            .is_err()
    );
    assert_eq!(
        fixture.publisher.open(full.root()).unwrap().revision(),
        full.revision()
    );
    assert!(!full.root().join("one-too-many.ts").exists());
}

#[test]
fn package_roots_cannot_contain_or_live_inside_application_configuration() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let original_revision = original.revision().to_owned();
    let nested_data = original.root().join("not-created-private-data");
    let nested_publisher = Publisher::new(nested_data.clone());
    assert_eq!(
        nested_publisher.open(original.root()).unwrap_err().category,
        "AuthoringPath"
    );
    assert!(!nested_data.exists());
    let private = fixture.root.join("private");
    crate::storage::private_directory(&private).unwrap();
    let create_destination = private.join("new-package");
    assert_eq!(
        fixture
            .publisher
            .create(&create_destination, "new")
            .unwrap_err()
            .category,
        "AuthoringPath"
    );
    let duplicate_destination = private.join("duplicate");
    assert_eq!(
        fixture
            .publisher
            .duplicate(
                &original,
                original.revision(),
                &duplicate_destination,
                "copy"
            )
            .unwrap_err()
            .category,
        "AuthoringPath"
    );
    assert!(!create_destination.exists());
    assert!(!duplicate_destination.exists());
    assert_eq!(
        fixture.publisher.open(original.root()).unwrap().revision(),
        original_revision
    );
    let inside = private.join("existing-package");
    fs::rename(original.root(), &inside).unwrap();
    assert_eq!(
        fixture.publisher.open(&inside).unwrap_err().category,
        "AuthoringPath"
    );
    assert_eq!(
        Inventory::capture(&inside, &limits().unwrap())
            .unwrap()
            .identity,
        original.validate().unwrap().identity
    );
}
