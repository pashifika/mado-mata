use super::super::fixtures::{Directory, preferences, put};
use super::*;
use serde_json::{Value, json};

#[test]
fn invalid_initialization_leaves_absent_destination_and_legacy_source_untouched() {
    let directory = Directory::new();
    let root = directory.0.join("new-root");
    let legacy = directory.0.join("legacy/settings.json");
    put(&legacy, br#"{"version":1,"gui_log_limit":42}"#);
    let original = fs::read(&legacy).unwrap();
    let store = Store::new(root.clone()).unwrap();
    assert!(!root.exists());
    assert_eq!(store.settings().unwrap_err().category, "SettingsMissing");
    let mut invalid = preferences();
    invalid.gui_log_limit = 0;
    assert!(store.initialize(invalid).is_err());
    assert!(!root.exists());
    assert_eq!(fs::read(&legacy).unwrap(), original);
    store.initialize(preferences()).unwrap();
    assert!(root.join("settings.json").exists());
    assert_eq!(fs::read(&legacy).unwrap(), original);
}
#[test]
fn legacy_settings_load_without_rewrite_and_preferences_preserve_current_hint() {
    let directory = Directory::new();
    let store = directory.store();
    let path = directory.0.join("settings.json");
    let original = br#"{ "version":1, "gui_log_limit":12, "package_path":"old-root" }"#;
    put(&path, original);
    let loaded = store.settings().unwrap();
    assert_eq!(loaded.locale, Locale::English);
    assert!(loaded.backup_directory.is_none());
    assert!(loaded.packages_root.is_none());
    assert_eq!(store.packages_root().unwrap(), directory.0.join("sources"));
    assert!(!directory.0.join("sources").exists());
    assert!(!directory.0.join("pkgs").exists());
    assert_eq!(fs::read(&path).unwrap(), original);
    let draft = EditableSettings {
        locale: Locale::Japanese,
        backup_directory: Some(directory.0.join("archives").to_str().unwrap().into()),
        ..preferences()
    };
    put(
        &path,
        br#"{"version":1,"gui_log_limit":12,"package_path":"newer-root"}"#,
    );
    let saved = store.save_preferences(draft).unwrap();
    assert_eq!(saved.package_path.as_deref(), Some("newer-root"));
    assert_eq!(
        directory.store().settings().unwrap().locale,
        Locale::Japanese
    );
    assert_eq!(
        directory.store().settings().unwrap().backup_directory,
        saved.backup_directory
    );
    assert!(!directory.0.join("archives").exists());
}

#[test]
fn malformed_settings_never_become_defaults_or_accept_replacements() {
    let directory = Directory::new();
    let store = directory.store();
    let path = directory.0.join("settings.json");
    for malformed in [
        br#"{"version":1,"gui_log_limit":-1}"#.as_slice(),
        br#"{"version":1,"gui_log_limit":1.5}"#.as_slice(),
        br#"{"version":2,"gui_log_limit":12}"#.as_slice(),
        br#"{"version":2,"version":1,"gui_log_limit":12}"#.as_slice(),
        br#"{"version":1,"gui_log_limit":12,"gui_log_limit":34}"#.as_slice(),
        br#"{"version":1,"gui_log_limit":12,"locale":"invalid","locale":"ja"}"#.as_slice(),
        br#"{"version":1,"gui_log_limit":12,"notifications":{"visible_count":1,"visible_count":2,"timeout_seconds":8,"show_success":true}}"#.as_slice(),
        br#"{"version":1,"gui_log_limit":12,"future":true}"#.as_slice(),
        b"not JSON".as_slice(),
        br#"[1,12,"path"]"#.as_slice(),
    ] {
        put(&path, malformed);
        assert!(store.settings().is_err());
        assert!(store.initialize(preferences()).is_err());
        assert!(store.save_preferences(preferences()).is_err());
        assert_eq!(fs::read(&path).unwrap(), malformed);
    }
}

#[test]
fn invalid_locale_notifications_and_backup_paths_preserve_settings() {
    let directory = Directory::new();
    let store = directory.store();
    store.initialize(preferences()).unwrap();
    let path = directory.0.join("settings.json");
    let before = fs::read(&path).unwrap();
    for value in [
        Value::Null,
        json!("fr"),
        json!(1),
        json!(true),
        json!(["ja"]),
        json!({"ja":null}),
    ] {
        let mut editable = serde_json::to_value(preferences()).unwrap();
        editable["locale"] = value.clone();
        assert!(serde_json::from_value::<EditableSettings>(editable).is_err());
        let mut document = serde_json::to_value(Settings::default()).unwrap();
        document["locale"] = value;
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(store.settings().is_err());
        assert!(store.save_preferences(preferences()).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    fs::write(&path, &before).unwrap();
    let mut missing = serde_json::to_value(preferences()).unwrap();
    missing.as_object_mut().unwrap().remove("locale");
    assert!(serde_json::from_value::<EditableSettings>(missing).is_err());
    for (count, timeout) in [(0, 8), (3, 8), (2, 0), (2, 13)] {
        let mut invalid = preferences();
        invalid.notifications.visible_count = count;
        invalid.notifications.timeout_seconds = timeout;
        assert!(store.save_preferences(invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    for backup in [
        "relative".to_owned(),
        "".to_owned(),
        "/line\nbreak".to_owned(),
        format!("/{}", "x".repeat(MAX_PATH_BYTES)),
    ] {
        let mut invalid = preferences();
        invalid.backup_directory = Some(backup);
        assert!(store.save_preferences(invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn packages_root_persists_without_creating_or_moving_source_and_can_return_to_default() {
    let directory = Directory::new();
    let root = directory.0.join("app");
    let custom = directory.0.join("sources/nested");
    let store = Store::new(root.clone()).unwrap();
    store.initialize(preferences()).unwrap();
    assert_eq!(store.packages_root().unwrap(), root.join("sources"));
    assert!(!root.join("sources").exists());
    assert!(!root.join("pkgs").exists());
    let mut draft = preferences();
    draft.packages_root = Some(custom.to_str().unwrap().into());
    store.save_preferences(draft).unwrap();
    let reopened = Store::new(root.clone()).unwrap();
    assert_eq!(reopened.packages_root().unwrap(), custom);
    assert!(!custom.exists());
    assert!(!directory.0.join("sources").exists());
    let mut legacy = preferences();
    legacy.packages_root = Some(root.join("pkgs").to_str().unwrap().into());
    reopened.save_preferences(legacy).unwrap();
    assert_eq!(
        Store::new(root.clone()).unwrap().packages_root().unwrap(),
        root.join("pkgs")
    );
    assert!(!root.join("pkgs").exists());
    reopened.save_preferences(preferences()).unwrap();
    assert_eq!(reopened.packages_root().unwrap(), root.join("sources"));
    assert!(!root.join("sources").exists());
    assert!(!root.join("pkgs").exists());
}

#[test]
fn invalid_packages_roots_preserve_settings_and_private_configuration() {
    let directory = Directory::new();
    let root = directory.0.join("app");
    let store = Store::new(root.clone()).unwrap();
    store.initialize(preferences()).unwrap();
    let before = fs::read(root.join("settings.json")).unwrap();
    let mut traversal = root.as_os_str().to_os_string();
    traversal.push(std::path::MAIN_SEPARATOR_STR);
    traversal.push("..");
    traversal.push(std::path::MAIN_SEPARATOR_STR);
    traversal.push("outside");
    for path in [
        "relative".into(),
        "".into(),
        "/bad\nroot".into(),
        format!("/{}", "x".repeat(MAX_PATH_BYTES)),
        traversal.to_str().unwrap().into(),
        root.to_str().unwrap().into(),
        directory.0.to_str().unwrap().into(),
        root.join("tabs").to_str().unwrap().into(),
        root.join("profiles").to_str().unwrap().into(),
        root.join("authoring").to_str().unwrap().into(),
        root.join(".restore-journal").to_str().unwrap().into(),
        root.join("pkgs-other").to_str().unwrap().into(),
        root.join("sources-other").to_str().unwrap().into(),
    ] {
        let mut draft = preferences();
        draft.packages_root = Some(path);
        assert!(store.save_preferences(draft).is_err());
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), before);
    }
    assert!(!root.join("pkgs").exists());
    assert!(!root.join("sources").exists());
    assert!(!root.join("tabs").exists());
    assert!(!root.join("authoring").exists());
}
