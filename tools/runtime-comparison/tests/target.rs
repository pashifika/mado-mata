use mado_runtime_comparison::{
    desktop::DesktopController,
    inventory::{Inventory, TargetDeclaration},
    model::{Fault, Plan, identity},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct FixtureTree(PathBuf);

impl FixtureTree {
    fn new() -> Self {
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
            "mado-target-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create private fixture root");
        let tree = Self(root);
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/javascript"),
            &tree.package(),
        )
        .expect("copy controlled package");
        tree
    }

    fn package(&self) -> PathBuf {
        self.0.join("package")
    }

    fn set_target(&self, target: &str) {
        let manifest = include_str!("../fixtures/javascript/package.json")
            .trim_end()
            .strip_suffix('}')
            .unwrap();
        std::fs::write(
            self.package().join("package.json"),
            format!("{manifest},\"target\":{target}}}"),
        )
        .unwrap();
    }

    fn capture(&self) -> Result<Inventory, Fault> {
        let plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        Inventory::capture(&self.package(), &plan.limits)
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn controller() -> DesktopController {
    DesktopController::new("unused-runner".into(), "unused-engine".into())
}

fn target_identity(inventory: &Inventory) -> String {
    inventory.target().unwrap().unwrap().identity().unwrap()
}

#[test]
fn target_inspection_preserves_profiles_and_does_not_evaluate_package_code() {
    let tree = FixtureTree::new();
    let source_path = tree.package().join("main.js");
    let mut source = std::fs::read_to_string(&source_path).unwrap();
    source.push_str("\nthrow new Error('inspection must not evaluate package code');\n");
    std::fs::write(source_path, source).unwrap();
    let without_target = tree.capture().unwrap();
    assert_eq!(without_target.target().unwrap(), None);
    let controller = controller();
    let without_target_info = controller.inspect(&tree.package()).unwrap();
    assert_eq!(without_target_info.target, None);
    assert_eq!(without_target_info.target_identity, None);

    let declaration = TargetDeclaration {
        id: "game.example".into(),
        window_title: Some("Exact ゲーム Title".into()),
    };
    tree.set_target(&serde_json::to_string(&declaration).unwrap());
    let inventory = tree.capture().unwrap();
    let inspected = controller.inspect(&tree.package()).unwrap();
    assert_eq!(inventory.target().unwrap(), Some(declaration.clone()));
    assert_eq!(inspected.target, Some(declaration.clone()));
    assert_eq!(
        inspected.target_identity,
        Some(declaration.identity().unwrap())
    );
    assert_ne!(inventory.identity, without_target.identity);
    assert_eq!(inspected.inventory_identity, inventory.identity);
    assert_eq!(inspected.package_id, without_target_info.package_id);
    assert_eq!(
        inspected.schema_identity,
        without_target_info.schema_identity
    );
    assert_eq!(inspected.profiles, without_target_info.profiles);
    assert_eq!(
        inspected.effective_defaults,
        without_target_info.effective_defaults
    );

    tree.set_target(r#"{"id":"other-game","window_title":"Other"}"#);
    assert_eq!(inventory.target().unwrap(), Some(declaration));
    inventory.validate().unwrap();
}

#[test]
fn target_identity_changes_for_id_exact_title_and_title_requirement() {
    let tree = FixtureTree::new();
    tree.set_target(r#"{"id":"game","window_title":"Exact Title"}"#);
    let original = tree.capture().unwrap();
    let declaration_identity = target_identity(&original);
    for target in [
        r#"{"id":"other-game","window_title":"Exact Title"}"#,
        r#"{"id":"Game","window_title":"Exact Title"}"#,
        r#"{"id":"game","window_title":"Exact Title "}"#,
        r#"{"id":"game"}"#,
    ] {
        tree.set_target(target);
        let changed = tree.capture().unwrap();
        assert_ne!(changed.identity, original.identity, "{target}");
        assert_ne!(target_identity(&changed), declaration_identity, "{target}");
    }
    std::fs::write(
        tree.package().join("package.json"),
        include_str!("../fixtures/javascript/package.json"),
    )
    .unwrap();
    let removed = tree.capture().unwrap();
    assert_ne!(removed.identity, original.identity);
    assert_eq!(removed.target().unwrap(), None);
    let inspected = controller().inspect(&tree.package()).unwrap();
    assert_eq!(inspected.target, None);
    assert_eq!(inspected.target_identity, None);
}

#[test]
fn target_identity_survives_unrelated_source_schema_and_manifest_format_changes() {
    let tree = FixtureTree::new();
    tree.set_target(r#"{"id":"game","window_title":"Exact Title"}"#);
    let original = tree.capture().unwrap();
    let declaration_identity = target_identity(&original);

    let source_path = tree.package().join("main.js");
    let mut source = std::fs::read_to_string(&source_path).unwrap();
    source.push_str("\nexport const authoringRevision = 2;\n");
    std::fs::write(source_path, source).unwrap();
    let source_changed = tree.capture().unwrap();
    assert_ne!(source_changed.identity, original.identity);
    assert_eq!(target_identity(&source_changed), declaration_identity);

    let schema_path = tree.package().join("schema.json");
    let mut schema: Value = serde_json::from_slice(&std::fs::read(&schema_path).unwrap()).unwrap();
    schema["properties"]["recognition"]["default"]["threshold"] = json!(0.85);
    std::fs::write(schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
    let schema_changed = tree.capture().unwrap();
    assert_ne!(schema_changed.identity, source_changed.identity);
    assert_ne!(
        identity(&schema_changed.schema).unwrap(),
        identity(&original.schema).unwrap()
    );
    assert_eq!(target_identity(&schema_changed), declaration_identity);

    tree.set_target("{ \"window_title\" : \"Exact Title\",\n  \"id\" : \"game\" }");
    let reformatted = tree.capture().unwrap();
    assert_ne!(reformatted.identity, schema_changed.identity);
    assert_eq!(target_identity(&reformatted), declaration_identity);
    let inspected = controller().inspect(&tree.package()).unwrap();
    assert_eq!(inspected.target_identity, Some(declaration_identity));
    assert_eq!(
        inspected.schema_identity,
        identity(&schema_changed.schema).unwrap()
    );
}

#[test]
fn omitted_and_null_optional_titles_have_the_same_target_identity() {
    let tree = FixtureTree::new();
    tree.set_target(r#"{"id":"game"}"#);
    let omitted = tree.capture().unwrap();
    tree.set_target(r#"{"window_title":null,"id":"game"}"#);
    let explicit = tree.capture().unwrap();
    assert_ne!(explicit.identity, omitted.identity);
    assert_eq!(explicit.target().unwrap().unwrap().window_title, None);
    assert_eq!(target_identity(&explicit), target_identity(&omitted));
}

#[test]
fn invalid_present_target_declarations_cannot_be_captured_or_inspected() {
    let tree = FixtureTree::new();
    let controller = controller();
    for (name, target) in [
        ("null declaration", "null"),
        ("scalar declaration", "true"),
        ("positional declaration", r#"["game","Title"]"#),
        ("missing ID", "{}"),
        ("null ID", r#"{"id":null}"#),
        ("empty ID", r#"{"id":""}"#),
        ("path ID", r#"{"id":"../game"}"#),
        ("reserved ID", r#"{"id":"CON"}"#),
        ("control ID", r#"{"id":"game\n"}"#),
        ("empty title", r#"{"id":"game","window_title":""}"#),
        (
            "control title",
            r#"{"id":"game","window_title":"Title\u0085"}"#,
        ),
        ("non-string title", r#"{"id":"game","window_title":42}"#),
        ("duplicate ID", r#"{"id":"game","id":"other"}"#),
        (
            "duplicate title",
            r#"{"id":"game","window_title":null,"window_title":"Title"}"#,
        ),
        ("local path", r#"{"id":"game","path":"/private/game"}"#),
        ("launch recipe", r#"{"id":"game","arguments":["--launch"]}"#),
        (
            "permission grant",
            r#"{"id":"game","permissions":{"input":true}}"#,
        ),
        ("process identity", r#"{"id":"game","pid":42}"#),
        (
            "credentials",
            r#"{"id":"game","credentials":{"token":"not-supported"}}"#,
        ),
    ] {
        tree.set_target(target);
        assert_eq!(tree.capture().unwrap_err().category, "Inventory", "{name}");
        assert_eq!(
            controller.inspect(&tree.package()).unwrap_err().category,
            "Inventory",
            "{name}"
        );
    }
    tree.set_target(r#"{"id":"game"},"target":{"id":"other"}"#);
    assert_eq!(tree.capture().unwrap_err().category, "Inventory");
}

#[test]
fn target_declaration_bounds_count_utf8_bytes_without_changing_exact_titles() {
    let tree = FixtureTree::new();
    let title = format!("{}ab", "界".repeat(170));
    let declaration = TargetDeclaration {
        id: "g".repeat(128),
        window_title: Some(title),
    };
    tree.set_target(&serde_json::to_string(&declaration).unwrap());
    assert_eq!(
        tree.capture().unwrap().target().unwrap(),
        Some(declaration.clone())
    );
    for invalid in [
        TargetDeclaration {
            id: "g".repeat(129),
            ..declaration.clone()
        },
        TargetDeclaration {
            window_title: Some("界".repeat(171)),
            ..declaration
        },
    ] {
        assert_eq!(invalid.identity().unwrap_err().category, "Inventory");
        tree.set_target(&serde_json::to_string(&invalid).unwrap());
        assert_eq!(tree.capture().unwrap_err().category, "Inventory");
    }
}

#[test]
fn captured_target_validation_cannot_be_bypassed_by_refreshing_inventory_identity() {
    let tree = FixtureTree::new();
    tree.set_target(r#"{"id":"game","window_title":"Title"}"#);
    let original = tree.capture().unwrap();
    for target in [
        Value::Null,
        json!(["game", "Title"]),
        json!({"id":"../game"}),
        json!({"id":"game","window_title":"Title\n"}),
        json!({"id":"game","permission":true}),
    ] {
        let mut changed = original.clone();
        changed.metadata["manifest"]["target"] = target;
        assert_eq!(changed.target().unwrap_err().category, "Inventory");
        assert_eq!(changed.validate().unwrap_err().category, "Inventory");
        assert_eq!(
            changed.refresh_identity().unwrap_err().category,
            "Inventory"
        );
        assert_eq!(changed.identity, original.identity);
    }
}
