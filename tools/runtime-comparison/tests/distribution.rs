//! Optional distribution metadata at the capture, draft, inventory and desktop
//! inspection boundaries.

use mado_runtime_comparison::{
    desktop::DesktopController,
    images,
    inventory::{Inventory, PackageDraft},
    model::{Fault, Limits, Plan},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const LETTERS: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
/// A canonical language identifier of exactly 64 ASCII bytes.
const LONG_KEY: &str = "en-aaaaaaaa-bbbbbbbb-cccccccc-dddddddd-eeeeeeee-ffffffff-ggggggg";

struct FixtureTree {
    root: PathBuf,
    manifest: String,
}

impl FixtureTree {
    fn new(runtime: &str) -> Self {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        fn copy(source: &Path, destination: &Path) -> std::io::Result<()> {
            std::fs::create_dir(destination)?;
            for entry in std::fs::read_dir(source)? {
                let entry = entry?;
                let target = destination.join(entry.file_name());
                if entry.file_type()?.is_dir() {
                    copy(&entry.path(), &target)?;
                } else {
                    std::fs::copy(entry.path(), target)?;
                }
            }
            Ok(())
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mado-distribution-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create private fixture root");
        let mut tree = Self {
            root,
            manifest: String::new(),
        };
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(runtime),
            &tree.package(),
        )
        .expect("copy controlled package");
        tree.manifest = std::fs::read_to_string(tree.package().join("package.json")).unwrap();
        tree
    }

    fn package(&self) -> PathBuf {
        self.root.join("package")
    }

    /// Appends raw members so duplicate and malformed JSON reaches every parser.
    fn write_members(&self, members: &str) {
        let manifest = self.manifest.trim_end().strip_suffix('}').unwrap();
        std::fs::write(
            self.package().join("package.json"),
            format!("{manifest}{members}}}"),
        )
        .unwrap();
    }

    fn set_distribution(&self, distribution: &str) {
        self.write_members(&format!(",\"distribution\":{distribution}"));
    }

    fn capture(&self) -> Result<Inventory, Fault> {
        Inventory::capture(&self.package(), &limits())
    }

    fn draft(&self) -> Result<PackageDraft, Fault> {
        PackageDraft::capture(&self.package(), &limits())
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Desktop inspection bounds.
fn limits() -> Limits {
    let mut plan: Plan =
        serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
    plan.limits.snapshot_bytes = images::PACKAGE_BYTES;
    plan.limits
}

fn controller() -> DesktopController {
    DesktopController::new("unused-runner".into(), "unused-engine".into())
}

fn complete() -> Value {
    json!({
        "version": 1,
        "release_version": "1.2.3-rc.1+build.0042",
        "name": "Example automation",
        "description": "<b>Plain</b> text & notes\nsecond line",
        "author_name": "Example Author",
        "license": "MIT OR Apache-2.0",
        "game": {
            "id": "example-game",
            "names": {
                "en": "Example Game",
                "ja": "例のゲーム",
                "pt-BR": "Jogo de exemplo",
                "sl-biske-rozaj": "Primer igre",
                "zh-Hant": "範例遊戲"
            },
            "aliases": ["EG", "ExampleGame.exe", "Fate/stay night", "Re:Example"]
        },
        "tags": ["internal", ""]
    })
}

fn with(change: impl FnOnce(&mut Value)) -> Value {
    let mut distribution = complete();
    change(&mut distribution);
    distribution
}

fn remove(object: &mut Value, member: &str) {
    object.as_object_mut().unwrap().remove(member);
}

fn short_keys(count: usize) -> impl Iterator<Item = String> {
    (0..count).map(|index| {
        format!(
            "q{}{}",
            char::from(LETTERS[index / 26]),
            char::from(LETTERS[index % 26])
        )
    })
}

fn names(keys: impl Iterator<Item = String>, name: &str) -> Value {
    Value::Object(keys.map(|key| (key, json!(name))).collect())
}

fn target_identity(inventory: &Inventory) -> String {
    inventory.target().unwrap().unwrap().identity().unwrap()
}

#[test]
fn absent_distribution_keeps_ordinary_directory_packages_valid() {
    let tree = FixtureTree::new("javascript");
    let draft = tree.draft().unwrap();
    assert_eq!(draft.distribution(), None);
    assert!(draft.manifest().unwrap().get("distribution").is_none());
    let inventory = tree.capture().unwrap();
    assert_eq!(inventory.distribution().unwrap(), None);
    assert!(inventory.metadata["manifest"].get("distribution").is_none());
    assert_eq!(draft.validate().unwrap().identity, inventory.identity);
    let inspected = controller().inspect(&tree.package()).unwrap();
    assert_eq!(inspected.inventory_identity, inventory.identity);
}

#[test]
fn complete_distribution_round_trips_through_draft_inventory_and_compiler_inspection() {
    let tree = FixtureTree::new("typescript");
    // Raw layout differs from the serialized form and must remain the captured source.
    tree.set_distribution(&serde_json::to_string_pretty(&complete()).unwrap());
    let saved = std::fs::read(tree.package().join("package.json")).unwrap();
    let draft = tree.draft().unwrap();
    assert_eq!(draft.files()["package.json"].as_slice(), saved.as_slice());
    let distribution = draft.distribution().unwrap();
    assert_eq!(distribution.release_version, "1.2.3-rc.1+build.0042");
    assert_eq!(distribution.game.names["zh-Hant"], "範例遊戲");
    assert_eq!(
        distribution.description,
        "<b>Plain</b> text & notes\nsecond line"
    );
    assert_eq!(
        distribution.tags,
        Some(vec!["internal".to_owned(), String::new()])
    );
    assert_eq!(serde_json::to_value(distribution).unwrap(), complete());
    assert_eq!(draft.manifest().unwrap()["distribution"], complete());
    assert_eq!(draft.package_id(), "m0-workload");

    let inventory = tree.capture().unwrap();
    assert_eq!(
        inventory.distribution().unwrap().as_ref(),
        Some(distribution)
    );
    assert_eq!(inventory.metadata["manifest"]["distribution"], complete());
    assert_eq!(
        std::fs::read(tree.package().join("package.json")).unwrap(),
        saved
    );

    let controller = controller();
    let by_path = controller.inspect(&tree.package()).unwrap();
    let captured = controller
        .inspect_inventory(draft.validate().unwrap())
        .unwrap();
    assert_eq!(by_path.inventory_identity, inventory.identity);
    assert_eq!(captured.inventory_identity, inventory.identity);
    assert_eq!(captured.package_id, "m0-workload");

    for (tags, expected) in [(None, None), (Some(json!([])), Some(Vec::new()))] {
        let distribution = with(|value| match tags {
            Some(tags) => value["tags"] = tags,
            None => remove(value, "tags"),
        });
        tree.set_distribution(&distribution.to_string());
        let reopened = tree.draft().unwrap();
        assert_eq!(reopened.distribution().unwrap().tags, expected);
        assert_eq!(reopened.manifest().unwrap()["distribution"], distribution);
    }
}

#[test]
fn field_and_aggregate_maxima_are_accepted_together() {
    let tree = FixtureTree::new("javascript");
    let text = format!("{}a", "界".repeat(85));
    assert_eq!(text.len(), 256);
    assert_eq!(LONG_KEY.len(), 64);
    let mut localized = names(short_keys(63), &text);
    localized[LONG_KEY] = json!(text);
    let maximal = json!({
        "version": 1,
        "release_version": format!("1.0.0+{}", "a".repeat(122)),
        "name": text,
        "description": "d".repeat(8 * 1024),
        "author_name": text,
        "license": text,
        "game": {
            "id": "g".repeat(128),
            "names": localized,
            "aliases": vec!["a".repeat(256); 64]
        },
        "tags": vec!["t".repeat(128); 64]
    });
    tree.set_distribution(&maximal.to_string());
    let draft = tree.draft().unwrap();
    assert_eq!(draft.distribution().unwrap().game.names.len(), 64);
    let inventory = tree.capture().unwrap();
    assert_eq!(inventory.metadata["manifest"]["distribution"], maximal);
    controller()
        .inspect_inventory(draft.validate().unwrap())
        .unwrap();
}

#[test]
fn semver_core_integers_are_not_limited_by_machine_word_size() {
    let tree = FixtureTree::new("javascript");
    let release = format!("{}.0.0-alpha.1+build.00", "9".repeat(107));
    let value = with(|value| value["release_version"] = json!(release));
    tree.set_distribution(&value.to_string());
    let draft = tree.draft().unwrap();
    assert_eq!(draft.distribution().unwrap().release_version, release);
    assert_eq!(
        tree.capture().unwrap().metadata["manifest"]["distribution"],
        value
    );
}

#[test]
fn invalid_distribution_is_refused_by_every_consumer() {
    let base = complete().to_string();
    let over = format!("{}ab", "界".repeat(85));
    let mut cases: Vec<(String, String)> = vec![
        ("null object".into(), "null".into()),
        ("scalar object".into(), "true".into()),
        (
            "positional object".into(),
            r#"[1,"1.0.0","Name","","Author","MIT",{"id":"game","names":{"en":"Game"},"aliases":[]}]"#.into(),
        ),
        (
            "duplicate member".into(),
            base.replacen('{', r#"{"version":1,"#, 1),
        ),
        (
            "duplicate game member".into(),
            base.replacen(r#""game":{"#, r#""game":{"id":"other","#, 1),
        ),
        (
            "duplicate language key".into(),
            base.replacen(r#""names":{"#, r#""names":{"en":"Again","#, 1),
        ),
        (
            "duplicate distribution object".into(),
            format!("{base},\"distribution\":{base}"),
        ),
    ];
    let mut add = |name: &str, distribution: Value| {
        cases.push((name.to_owned(), distribution.to_string()));
    };
    add(
        "unknown member",
        with(|d| d["homepage"] = json!("https://example.invalid")),
    );
    add(
        "duplicated package ID",
        with(|d| d["package_id"] = json!("m0-workload")),
    );
    add(
        "unknown game member",
        with(|d| d["game"]["executable"] = json!("game.exe")),
    );
    add("unsupported version", with(|d| d["version"] = json!(2)));
    add("string version", with(|d| d["version"] = json!("1")));
    add("fractional version", with(|d| d["version"] = json!(1.0)));
    for member in [
        "version",
        "release_version",
        "name",
        "description",
        "author_name",
        "license",
        "game",
    ] {
        add(&format!("missing {member}"), with(|d| remove(d, member)));
    }
    for member in ["id", "names", "aliases"] {
        add(
            &format!("missing game {member}"),
            with(|d| remove(&mut d["game"], member)),
        );
    }
    for version in [
        "1.0",
        "v1.0.0",
        "01.0.0",
        "1.0.0-01",
        "1.0.0-alpha..1",
        "1.0.0+",
        " 1.0.0",
        "1.0.0 ",
        "1.0.0-é",
    ] {
        add(
            &format!("release version {version:?}"),
            with(|d| d["release_version"] = json!(version)),
        );
    }
    add(
        "release version over bound",
        with(|d| d["release_version"] = json!(format!("1.0.0+{}", "a".repeat(123)))),
    );
    for member in ["name", "author_name", "license"] {
        add(&format!("empty {member}"), with(|d| d[member] = json!("")));
        add(
            &format!("{member} over bound"),
            with(|d| d[member] = json!(over)),
        );
        add(&format!("null {member}"), with(|d| d[member] = Value::Null));
    }
    add(
        "description over bound",
        with(|d| d["description"] = json!("d".repeat(8 * 1024 + 1))),
    );
    let long_id = "g".repeat(129);
    for id in ["", "../game", "CON", "Game Name", long_id.as_str()] {
        add(
            &format!("search ID {id:?}"),
            with(|d| d["game"]["id"] = json!(id)),
        );
    }
    add("empty names", with(|d| d["game"]["names"] = json!({})));
    add(
        "names over bound",
        with(|d| d["game"]["names"] = names(short_keys(65), "Game")),
    );
    add(
        "names as rows",
        with(|d| d["game"]["names"] = json!([["en", "A"], ["en", "B"]])),
    );
    let long_key = format!("{LONG_KEY}g");
    for key in [
        "EN",
        "zh-hant",
        "pt-br",
        "en_US",
        "sl-rozaj-biske",
        "en-fonipa-fonipa",
        "en-u-ca-gregory",
        "x-private",
        "i-klingon",
        "zh-yue",
        "abcd",
        "abcde",
        "",
        "日本",
        long_key.as_str(),
    ] {
        add(
            &format!("language key {key:?}"),
            with(|d| d["game"]["names"] = names(std::iter::once(key.to_owned()), "Game")),
        );
    }
    add(
        "empty game name",
        with(|d| d["game"]["names"]["en"] = json!("")),
    );
    add(
        "game name over bound",
        with(|d| d["game"]["names"]["en"] = json!(over)),
    );
    add(
        "null game name",
        with(|d| d["game"]["names"]["en"] = Value::Null),
    );
    add(
        "aliases over bound",
        with(|d| d["game"]["aliases"] = json!(vec!["EG"; 65])),
    );
    add(
        "aliases as text",
        with(|d| d["game"]["aliases"] = json!("EG")),
    );
    for alias in [
        Value::Null,
        json!(""),
        json!("a".repeat(257)),
        json!("/Applications/Example.app"),
        json!("\\\\server\\share\\Example.exe"),
        json!("C:\\Games\\Example.exe"),
        json!("c:/games/example.exe"),
        json!("~/Games/example"),
    ] {
        add(
            &format!("alias {alias}"),
            with(|d| d["game"]["aliases"] = json!([alias])),
        );
    }
    add("null tags", with(|d| d["tags"] = Value::Null));
    add("tags as text", with(|d| d["tags"] = json!("internal")));
    add(
        "tags over bound",
        with(|d| d["tags"] = json!(vec!["t"; 65])),
    );
    add(
        "tag over bound",
        with(|d| d["tags"] = json!(["t".repeat(129)])),
    );
    // Each name is within its own bound, but escaping exceeds the 64 KiB object bound.
    add(
        "serialized aggregate over bound",
        with(|d| d["game"]["names"] = names(short_keys(64), &"\u{1}".repeat(256))),
    );

    let tree = FixtureTree::new("javascript");
    let controller = controller();
    for (name, distribution) in &cases {
        tree.set_distribution(distribution);
        for fault in [
            tree.capture().unwrap_err(),
            tree.draft().unwrap_err(),
            controller.inspect(&tree.package()).unwrap_err(),
        ] {
            assert_eq!(fault.category, "Inventory", "{name}: {}", fault.message);
        }
    }

    tree.set_distribution(&with(|d| d["game"]["names"] = json!({"zh-hant": "Game"})).to_string());
    let fault = tree.draft().unwrap_err();
    assert_eq!(fault.context["field"], "distribution.game.names");
    assert_eq!(fault.context["key"], "zh-hant");
    assert_eq!(fault.context["canonical"], "zh-Hant");
}

#[test]
fn inventory_metadata_cannot_bypass_distribution_validation() {
    let tree = FixtureTree::new("javascript");
    tree.set_distribution(&complete().to_string());
    let original = tree.capture().unwrap();
    let controller = controller();
    for distribution in [
        Value::Null,
        json!([1]),
        with(|d| d["homepage"] = json!("https://example.invalid")),
        with(|d| d["version"] = json!(2)),
        with(|d| d["release_version"] = json!("1.0")),
        with(|d| d["game"]["names"] = json!({"EN": "Game"})),
        with(|d| d["game"]["names"] = json!({})),
        with(|d| d["game"]["aliases"] = json!(["/private/game"])),
        with(|d| d["tags"] = Value::Null),
    ] {
        let mut changed = original.clone();
        changed.metadata["manifest"]["distribution"] = distribution;
        assert_eq!(changed.distribution().unwrap_err().category, "Inventory");
        assert_eq!(changed.validate().unwrap_err().category, "Inventory");
        assert_eq!(
            controller
                .inspect_inventory(changed.clone())
                .unwrap_err()
                .category,
            "Inventory"
        );
        assert_eq!(
            changed.refresh_identity().unwrap_err().category,
            "Inventory"
        );
        assert_eq!(changed.identity, original.identity);
    }
}

#[test]
fn search_metadata_edits_change_inventory_identity_but_not_target_identity() {
    let tree = FixtureTree::new("javascript");
    let target = r#","target":{"id":"game","window_title":"Exact Title"}"#;
    let set = |distribution: &str| {
        tree.write_members(&format!("{target},\"distribution\":{distribution}"));
    };
    set(&complete().to_string());
    let original = tree.capture().unwrap();
    let original_revision = tree.draft().unwrap().revision().to_owned();
    let declaration = target_identity(&original);
    let controller = controller();
    let inspected = controller.inspect(&tree.package()).unwrap();
    assert_eq!(inspected.target_identity.as_ref(), Some(&declaration));
    for (name, distribution) in [
        (
            "search ID",
            with(|d| d["game"]["id"] = json!("other-game")).to_string(),
        ),
        (
            "localized name",
            with(|d| d["game"]["names"]["ko"] = json!("예제 게임")).to_string(),
        ),
        (
            "aliases",
            with(|d| d["game"]["aliases"] = json!(["Other.exe"])).to_string(),
        ),
        ("tags", with(|d| d["tags"] = json!(["changed"])).to_string()),
        (
            "release",
            with(|d| d["release_version"] = json!("1.2.4")).to_string(),
        ),
        (
            "formatting only",
            serde_json::to_string_pretty(&complete()).unwrap(),
        ),
    ] {
        set(&distribution);
        let changed = tree.capture().unwrap();
        assert_ne!(changed.identity, original.identity, "{name}");
        assert_ne!(
            tree.draft().unwrap().revision(),
            original_revision,
            "{name}"
        );
        assert_eq!(
            changed.distribution().unwrap() == original.distribution().unwrap(),
            name == "formatting only",
            "{name}"
        );
        assert_eq!(
            changed.target().unwrap(),
            original.target().unwrap(),
            "{name}"
        );
        assert_eq!(target_identity(&changed), declaration, "{name}");
        let reinspected = controller.inspect(&tree.package()).unwrap();
        assert_eq!(reinspected.inventory_identity, changed.identity, "{name}");
        assert_eq!(reinspected.target, inspected.target, "{name}");
        assert_eq!(
            reinspected.target_identity, inspected.target_identity,
            "{name}"
        );
    }
    tree.write_members(target);
    let removed = tree.capture().unwrap();
    assert_ne!(removed.identity, original.identity);
    assert_eq!(removed.distribution().unwrap(), None);
    assert_eq!(target_identity(&removed), declaration);
}

#[test]
fn inspect_inventory_validates_the_captured_bytes_without_rereading_source() {
    let tree = FixtureTree::new("javascript");
    let source = tree.package().join("main.js");
    let mut text = std::fs::read_to_string(&source).unwrap();
    text.push_str("\nthrow new Error('inspection must not evaluate package code');\n");
    std::fs::write(&source, text).unwrap();
    tree.set_distribution(&complete().to_string());
    let controller = controller();
    let by_path = controller.inspect(&tree.package()).unwrap();
    let draft = tree.draft().unwrap();
    let inventory = draft.validate().unwrap();
    assert_eq!(
        draft.distribution(),
        inventory.distribution().unwrap().as_ref()
    );

    std::fs::remove_dir_all(tree.package()).unwrap();
    assert!(controller.inspect(&tree.package()).is_err());
    let captured = controller.inspect_inventory(inventory.clone()).unwrap();
    assert_eq!(captured.inventory_identity, by_path.inventory_identity);
    assert_eq!(captured.schema_identity, by_path.schema_identity);
    assert_eq!(captured.profiles, by_path.profiles);
    assert_eq!(captured.target_identity, by_path.target_identity);

    let mut tampered = inventory.clone();
    tampered
        .sources
        .get_mut("main.js")
        .unwrap()
        .push_str("\nexport const tampered = true;\n");
    assert_eq!(
        controller.inspect_inventory(tampered).unwrap_err().category,
        "Inventory"
    );

    // Different capture bounds would yield an identity path-based inspection never reports.
    let plan: Plan = serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
    assert_ne!(plan.limits.snapshot_bytes, limits().snapshot_bytes);
    let other = PackageDraft::from_files(draft.files().clone(), &plan.limits)
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(
        controller.inspect_inventory(other).unwrap_err().category,
        "Inventory"
    );

    controller.shutdown().unwrap();
    assert_eq!(
        controller
            .inspect_inventory(inventory)
            .unwrap_err()
            .category,
        "ControllerClosed"
    );
}
