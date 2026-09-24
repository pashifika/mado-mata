use super::*;
use crate::target::{TargetConfiguration, TargetExpectation};
use mado_runtime_comparison::desktop::StartRequest;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) application: Arc<Application>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mado-application-{}-{nonce}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
        ));
        Store::new(root.clone())
            .unwrap()
            .initialize(preferences())
            .unwrap();
        let application = Application::new(
            root.clone(),
            root.join("runner-must-not-be-launched"),
            root.join("engine-must-not-be-launched"),
        )
        .unwrap();
        Self { root, application }
    }

    pub(super) fn package_at(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        for relative in [
            "package.json",
            "main.js",
            "decisions.js",
            "schema.json",
            "profiles/template-first.json",
            "profiles/ocr-first.json",
            "assets/marker.rgba",
        ] {
            let destination = path.join(relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(package_path().join(relative), destination).unwrap();
        }
        path
    }

    pub(super) fn numeric_package(&self) -> PathBuf {
        let path = self.package_at("package");
        let schema_path = path.join("schema.json");
        let mut schema: Value = serde_json::from_slice(&fs::read(&schema_path).unwrap()).unwrap();
        schema["properties"]["amount"] = json!({"type":"number","default":1});
        schema["properties"]["numbers"] = json!({"type":"array","items":{"type":"number"}});
        fs::write(schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.application.shutdown();
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn package_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tools/runtime-comparison/fixtures/javascript")
        .canonicalize()
        .unwrap()
}

pub(super) fn workspace_ref(selection: &Selection) -> WorkspaceRef {
    WorkspaceRef {
        workspace_id: selection.workspace_id.clone(),
        revision: selection.revision,
    }
}

pub(super) fn view_ref(workspace: &WorkspaceView) -> WorkspaceRef {
    WorkspaceRef {
        workspace_id: workspace.workspace_id.clone(),
        revision: workspace.revision,
    }
}

pub(super) fn inspect_named(
    application: &Application,
    internal_name: &str,
    path: &Path,
) -> Result<Selection, Fault> {
    let workspace = application.create_workspace(internal_name, internal_name)?;
    let outcome = application.inspect(path, &view_ref(&workspace))?;
    assert_eq!(outcome.kind, InspectionKind::Bound);
    Ok(outcome
        .workspace
        .selection
        .expect("new fixture package is bound"))
}

pub(super) fn preferences() -> EditableSettings {
    let settings = Settings::default();
    EditableSettings {
        locale: settings.locale,
        gui_log_limit: settings.gui_log_limit,
        ocr_environment: settings.ocr_environment,
        notifications: settings.notifications,
        backup_directory: settings.backup_directory,
    }
}

pub(super) fn profile_path(fixture: &Fixture, internal_name: &str, profile: &Profile) -> PathBuf {
    fixture
        .root
        .join("tabs")
        .join(internal_name)
        .join(&profile.package_id)
        .join(format!("{}.config", profile.id))
}

pub(super) fn request(selection: &Selection) -> StartRequest {
    StartRequest {
        package_path: selection.package_path.clone(),
        inventory_identity: selection.package.inventory_identity.clone(),
        package_id: selection.package.package_id.clone(),
        schema_identity: selection.package.schema_identity.clone(),
        profile_id: "draft".into(),
        values: selection.package.profiles["template-first"]["options"].clone(),
        lane: "controlled".into(),
        scenario: "workflow".into(),
        replay_descriptor_path: None,
    }
}

pub(super) fn target_configuration() -> TargetConfiguration {
    serde_json::from_value(json!({
        "platform":"macos",
        "game":{"kind":"executable","path":std::env::current_exe().unwrap()},
        "launcher":null,
        "arguments":["", "literal $HOME", "two words"],
        "working_directory":null,
        "window_title":"Target metadata fixture",
        "input":{"route":"process_directed","focus":"preserve",
            "pointer_mode":"core_graphics","click_hold_ms":0}
    }))
    .unwrap()
}

#[cfg(unix)]
pub(super) fn declare_target(path: &Path, id: Option<&str>) {
    let manifest = path.join("package.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    match id {
        Some(id) => {
            value["target"] = json!({"id":id});
        }
        None => {
            value.as_object_mut().unwrap().remove("target");
        }
    }
    fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[cfg(unix)]
pub(super) fn target_expectation(view: &TargetView) -> TargetExpectation {
    TargetExpectation {
        revision: view.record.revision,
        binding_id: view
            .record
            .binding
            .as_ref()
            .map(|binding| binding.id.clone()),
    }
}

pub(super) fn settled(
    application: &Application,
) -> mado_runtime_comparison::desktop::ControllerView {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let view = application.runner.poll();
        if view.state == "terminal" {
            return view;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "operation did not settle"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
