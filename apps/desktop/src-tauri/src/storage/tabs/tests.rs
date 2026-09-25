use super::super::fixtures::{Directory, put, tab_path};
use super::*;

#[test]
fn name_only_tabs_restart_and_closed_state_preserve_names() {
    let directory = Directory::new();
    let store = directory.store();
    let name = format!("0{}_-9", "Ab1".repeat(20));
    let display = "界".repeat(80);
    let created = store.create_tab(&name, &display).unwrap();
    assert!(created.open);
    assert!(created.packages.is_empty());
    assert!(created.selected_package_id.is_none());
    assert_eq!(directory.store().tab(&name).unwrap(), created);
    assert!(!directory.0.join("settings.json").exists());
    store.set_tab_open(&name, false).unwrap();
    assert!(!directory.store().tab(&name).unwrap().open);
    assert_eq!(store.set_tab_open(&name, true).unwrap(), created);
    store.create_tab("DuplicateDisplay", &display).unwrap();
    assert_eq!(store.tabs().unwrap().tabs.len(), 2);
}

#[test]
fn invalid_names_and_case_aliases_preserve_existing_tabs() {
    let directory = Directory::new();
    let store = directory.store();
    let original = store.create_tab("Alpha", "  Preserved name  ").unwrap();
    let before = fs::read(tab_path(&directory, "Alpha")).unwrap();
    for name in [
        "",
        "Alpha１",
        " Alpha",
        "Alpha ",
        ".",
        "..",
        "../Alpha",
        "日",
        &"A".repeat(65),
    ] {
        assert!(store.create_tab(name, "Display").is_err());
    }
    for display in [" \u{3000} ", "line\nbreak", "\u{007f}", &"界".repeat(81)] {
        assert!(store.create_tab("Valid", display).is_err());
    }
    assert!(store.create_tab("alpha", "Alias").is_err());
    assert!(store.tab("ALPHA").is_err());
    assert!(store.create_tab("Alpha", "Replacement").is_err());
    assert_eq!(fs::read(tab_path(&directory, "Alpha")).unwrap(), before);
    assert_eq!(store.tab("Alpha").unwrap(), original);
}

#[test]
fn saved_and_open_bounds_refuse_without_eviction() {
    let directory = Directory::new();
    let store = directory.store();
    let names: Vec<_> = (1..=MAX_TABS).map(|index| "A".repeat(index)).collect();
    for name in &names[..MAX_OPEN_TABS] {
        store.create_tab(name, "Same display").unwrap();
    }
    assert!(store.create_tab(&names[MAX_OPEN_TABS], "Overflow").is_err());
    store.set_tab_open(&names[0], false).unwrap();
    store
        .create_tab(&names[MAX_OPEN_TABS], "Slot reused")
        .unwrap();
    assert!(store.set_tab_open(&names[0], true).is_err());
    for name in &names[..=MAX_OPEN_TABS] {
        store.set_tab_open(name, false).unwrap();
    }
    for name in &names[MAX_OPEN_TABS + 1..] {
        store.create_tab(name, "Saved").unwrap();
        store.set_tab_open(name, false).unwrap();
    }
    assert_eq!(store.tabs().unwrap().tabs.len(), MAX_TABS);
    assert!(store.create_tab("Extra", "Overflow").is_err());
    assert!(store.set_tab_open(&names[0], true).unwrap().open);
}

#[test]
fn package_bounds_aliases_and_unsupported_source_forms_are_preserved() {
    let directory = Directory::new();
    let store = directory.store();
    store.create_tab("Owner", "Owner").unwrap();
    store.bind_package("Owner", "Café", &directory.0).unwrap();
    let path = tab_path(&directory, "Owner");
    let before = fs::read(&path).unwrap();
    for id in [
        "CAFÉ",
        "Cafe\u{301}",
        "../outside",
        "tab.config",
        "NUL",
        "ends.",
        "space ",
        "drive:part",
    ] {
        assert!(store.bind_package("Owner", id, &directory.0).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    for index in 1..MAX_PACKAGES {
        store
            .bind_package("Owner", &format!("package-{index}"), &directory.0)
            .unwrap();
    }
    let complete = fs::read(&path).unwrap();
    assert!(
        store
            .bind_package("Owner", "overflow", &directory.0)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), complete);
    let mut tab = store.tab("Owner").unwrap();
    tab.packages[0].source = PackageSource::CustomArchive {
        path: directory.0.join("package.custom").to_str().unwrap().into(),
    };
    put(&path, &encode(&tab, MAX_TAB_BYTES).unwrap());
    assert_eq!(store.tab("Owner").unwrap(), tab);
    let mut invalid = serde_json::to_value(&tab).unwrap();
    invalid["packages"][0]["source"]["unknown"] = json!(true);
    let bytes = serde_json::to_vec(&invalid).unwrap();
    put(&path, &bytes);
    assert!(store.tab("Owner").is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn filesystem_aliases_cannot_select_retained_package_data() {
    let directory = Directory::new();
    let store = directory.store();
    store.create_tab("Owner", "Owner").unwrap();
    let path = tab_path(&directory, "Owner");
    let before = fs::read(&path).unwrap();
    private_directory(&path.parent().unwrap().join("Café")).unwrap();
    private_directory(&path.parent().unwrap().join("Σ")).unwrap();
    for id in ["CAFÉ", "Cafe\u{301}", "σ", "ς"] {
        assert_eq!(
            store
                .bind_package("Owner", id, &directory.0)
                .unwrap_err()
                .category,
            "StorageAlias"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn malformed_and_orphaned_tabs_remain_attributable_without_rewriting() {
    let directory = Directory::new();
    let store = directory.store();
    store.create_tab("Healthy", "Healthy").unwrap();
    let path = tab_path(&directory, "Broken");
    put(&path, b"broken evidence");
    let orphan = directory.0.join("tabs/Orphan/package/target.config");
    put(&orphan, b"retained");
    private_directory(&directory.0.join("tabs/Empty")).unwrap();
    fs::write(directory.0.join("tabs/.DS_Store"), b"Finder metadata").unwrap();
    let listing = store.tabs().unwrap();
    assert_eq!(
        listing
            .tabs
            .iter()
            .map(|tab| tab.internal_name.as_str())
            .collect::<Vec<_>>(),
        ["Healthy"]
    );
    let owners: BTreeSet<_> = listing
        .faults
        .iter()
        .map(|fault| fault.context["internal_name"].as_str().unwrap())
        .collect();
    assert_eq!(owners, BTreeSet::from(["Broken", "Orphan"]));
    assert!(store.set_tab_open("Broken", false).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"broken evidence");
    assert_eq!(fs::read(&orphan).unwrap(), b"retained");
    assert_eq!(
        fs::read(directory.0.join("tabs/.DS_Store")).unwrap(),
        b"Finder metadata"
    );
    let mut wrong = store.tab("Healthy").unwrap();
    wrong.internal_name = "Different".into();
    put(&path, &encode(&wrong, MAX_TAB_BYTES).unwrap());
    assert_eq!(store.tab("Broken").unwrap_err().category, "TabIdentity");
}

#[test]
fn failed_create_and_reopen_preserve_prior_records_and_pending_evidence() {
    let directory = Directory::new();
    let store = directory.store();
    let creation = tab_path(&directory, "New").with_extension("pending");
    put(&creation, b"interrupted creation");
    assert!(store.create_tab("New", "Unsaved").is_err());
    assert!(!tab_path(&directory, "New").exists());
    assert_eq!(fs::read(&creation).unwrap(), b"interrupted creation");
    fs::remove_file(&creation).unwrap();
    store.create_tab("New", "Saved").unwrap();
    store.set_tab_open("New", false).unwrap();
    let closed = fs::read(tab_path(&directory, "New")).unwrap();
    put(&creation, b"interrupted reopen");
    assert!(store.set_tab_open("New", true).is_err());
    assert!(!store.tab("New").unwrap().open);
    assert_eq!(fs::read(tab_path(&directory, "New")).unwrap(), closed);
    assert_eq!(fs::read(&creation).unwrap(), b"interrupted reopen");
}

#[test]
fn tab_document_bound_is_independent_of_smaller_profile_bound() {
    let directory = Directory::new();
    let store = directory.store();
    store.create_tab("Owner", "Owner").unwrap();
    let path = tab_path(&directory, "Owner");
    let mut padded = fs::read(&path).unwrap();
    padded.resize(100 * 1024, b' ');
    put(&path, &padded);
    assert!(store.tab("Owner").unwrap().open);
    assert!(!store.set_tab_open("Owner", false).unwrap().open);
    padded.resize(MAX_TAB_BYTES + 1, b' ');
    fs::write(&path, &padded).unwrap();
    assert!(store.tab("Owner").is_err());
    assert!(store.set_tab_open("Owner", true).is_err());
    assert_eq!(fs::read(&path).unwrap(), padded);
}
