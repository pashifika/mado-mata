use super::{
    EditableSettings, Locale, NotificationPreferences, ProfileStore, Store, new_id,
    private_directory, write_atomic,
};
use crate::target::{TargetBinding, TargetRecord, TargetResolution};
use mado_runtime_comparison::inventory::{Entries, Entry, Inventory};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) struct Directory(pub(super) PathBuf);

impl Directory {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mado-storage-{}", new_id().unwrap()));
        private_directory(&path).unwrap();
        Self(path)
    }

    pub(super) fn store(&self) -> Store {
        Store::new(self.0.clone()).unwrap()
    }

    pub(super) fn profiles(&self, name: &str) -> ProfileStore {
        let store = self.store();
        store.create_tab(name, "Workspace").unwrap();
        store
            .bind_package(name, &inventory().package_id, &self.0.join("source"))
            .unwrap();
        store.profile_store(name, &inventory().package_id).unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn inventory() -> Inventory {
    Inventory {
        identity: "fixture".into(),
        package_id: "portable-options".into(),
        sources: BTreeMap::new(),
        assets: BTreeMap::new(),
        schema: json!({
            "version": 1, "type": "object", "additionalProperties": false,
            "properties": {
                "priorities": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": ["left", "right"]}},
                "window": {"type": "object", "additionalProperties": false,
                    "properties": {"width": {"type": "integer"}, "height": {"type": "integer"}},
                    "required": ["width", "height"], "default": {"width": 10, "height": 20}},
                "label": {"type": "string", "default": "secret token"}
            }, "required": ["priorities", "window"]
        }),
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

pub(super) fn options() -> Value {
    json!({"priorities": ["right", "left"], "label": "assets/token.png"})
}

pub(super) fn preferences() -> EditableSettings {
    EditableSettings {
        locale: Locale::English,
        gui_log_limit: 1000,
        ocr_environment: None,
        notifications: NotificationPreferences::default(),
        backup_directory: None,
    }
}

pub(super) fn put(path: &Path, bytes: &[u8]) {
    private_directory(path.parent().unwrap()).unwrap();
    write_atomic(path, bytes, |from, to| fs::rename(from, to)).unwrap();
}

pub(super) fn tab_path(directory: &Directory, name: &str) -> PathBuf {
    directory.0.join("tabs").join(name).join("tab.config")
}
pub(super) fn target_path(directory: &Directory, tab: &str) -> PathBuf {
    directory
        .0
        .join("tabs")
        .join(tab)
        .join(inventory().package_id)
        .join("target.config")
}

pub(super) fn stored_target(tab: &str) -> TargetRecord {
    let declaration = crate::target::tests::declaration();
    TargetRecord {
        version: 1,
        internal_name: tab.into(),
        package_id: inventory().package_id,
        revision: 1,
        binding: Some(TargetBinding {
            id: new_id().unwrap(),
            package_id: inventory().package_id,
            target_id: declaration.id.clone(),
            declaration_identity: declaration.identity().unwrap(),
            configuration: crate::target::tests::configuration("/offline/game"),
            resolution: TargetResolution {
                game: crate::target::ResolvedLocation {
                    path: "/offline/game".into(),
                    executable: "/offline/game".into(),
                },
                launcher: None,
                working_directory: None,
            },
        }),
    }
}
