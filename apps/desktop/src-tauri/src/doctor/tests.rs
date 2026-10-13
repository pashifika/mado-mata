use super::*;
use crate::bootstrap::{Bootstrap, Phase};
use crate::configuration::{Capture, capture, tests::Root};
use crate::storage::PackageReference;
use std::path::PathBuf;

fn report(root: &Root) -> DoctorReport {
    observe(Ok(&root.0), &DoctorRequest::default())
}

fn tab(root: &Root, name: &str, packages: &[&str]) {
    let record = TabRecord {
        version: 1,
        internal_name: name.into(),
        display_name: name.into(),
        open: false,
        selected_package_id: packages.first().map(|id| (*id).into()),
        packages: packages.iter().map(|package_id| PackageReference {
            package_id: (*package_id).into(),
            source: PackageSource::Installed {
                package_id: (*package_id).into(),
                artifact_id: "a".repeat(64),
            },
        }).collect(),
    };
    root.put(&format!("tabs/{name}/tab.config"), &serde_json::to_vec(&record).unwrap());
}

fn profile(root: &Root, tab: &str, package: &str) -> String {
    let id = storage::new_id().unwrap();
    root.put(&format!("tabs/{tab}/{package}/{id}.config"), &serde_json::to_vec(&Profile {
        version: 1,
        id: id.clone(),
        name: "Retained".into(),
        package_id: package.into(),
        schema_identity: "b".repeat(64),
        values: json!({"label": "PRIVATE_PROFILE_VALUE"}),
    }).unwrap());
    id
}

// Only isolated fixture trees; links are recorded without following their targets.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            let bytes = if kind.is_dir() {
                directories.push(path.clone());
                Vec::new()
            } else if kind.is_symlink() {
                fs::read_link(&path).unwrap().as_os_str().as_encoded_bytes().to_vec()
            } else {
                fs::read(&path).unwrap()
            };
            result.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
        }
    }
    result
}

fn has(report: &DoctorReport, scope: Scope, owner: Option<&str>, status: Status, code: &str) -> bool {
    report.checks.iter().any(|check| check.scope == scope && check.owner.internal_name.as_deref() == owner
        && check.status == status && check.code == code)
}

#[test]
fn malformed_settings_without_application_does_not_hide_safe_siblings_or_write() {
    let root = Root::new();
    root.put("settings.json", b"{PRIVATE_BAD_SETTINGS");
    root.put("tabs/Bad/tab.config", b"PRIVATE_BAD_TAB");
    tab(&root, "Good", &["sample"]);
    let id = profile(&root, "Good", "sample");
    root.put("keys/private-material", b"PRIVATE_SIGNING_MATERIAL");
    root.put("pkgs/sample/old/main.ts", b"PRIVATE_PACKAGE_SOURCE");
    let before = snapshot(&root.0);
    let bootstrap = Bootstrap::new(Ok(root.0.clone()), None, root.0.join("no-runner"), root.0.join("no-engine"));
    assert!(matches!(bootstrap.status().state, Phase::Loading));
    let diagnosis = bootstrap.configuration_doctor(DoctorRequest::default());
    assert!(has(&diagnosis, Scope::Settings, None, Status::Failed, "invalid_document"));
    assert!(has(&diagnosis, Scope::Tab, Some("Bad"), Status::Failed, "invalid_document"));
    assert!(has(&diagnosis, Scope::Source, Some("Bad"), Status::Unexamined, "owner_unavailable"));
    assert!(has(&diagnosis, Scope::Tab, Some("Good"), Status::Passed, "checked"));
    assert!(diagnosis.checks.iter().any(|check| check.owner.profile_id.as_deref() == Some(&id) && check.status == Status::Passed));
    assert!(!bootstrap.status().application_available);
    assert!(matches!(bootstrap.status().state, Phase::Loading));
    let serialized = serde_json::to_string(&diagnosis).unwrap();
    for private in ["PRIVATE_BAD_SETTINGS", "PRIVATE_BAD_TAB", "PRIVATE_PROFILE_VALUE", "PRIVATE_SIGNING_MATERIAL", "PRIVATE_PACKAGE_SOURCE"] {
        assert!(!serialized.contains(private));
    }
    assert_eq!(snapshot(&root.0), before);
}

#[test]
fn invalid_references_profiles_and_targets_keep_each_other_attributable() {
    let root = Root::new();
    tab(&root, "One", &["bad", "good"]);
    let good = profile(&root, "One", "good");
    let bad = storage::new_id().unwrap();
    root.put(&format!("tabs/One/bad/{bad}.config"), b"invalid profile");
    root.put("tabs/One/good/target.config", b"invalid target");
    tab(&root, "Broken", &["bad"]);
    let path = root.0.join("tabs/Broken/tab.config");
    let mut broken: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    broken["packages"][0]["source"]["artifact_id"] = json!("not-an-artifact");
    fs::write(path, serde_json::to_vec(&broken).unwrap()).unwrap();
    let before = snapshot(&root.0);
    let diagnosis = report(&root);
    assert!(diagnosis.checks.iter().any(|check| check.owner.profile_id.as_deref() == Some(&bad)
        && check.status == Status::Failed && check.code == "invalid_document"));
    assert!(diagnosis.checks.iter().any(|check| check.owner.profile_id.as_deref() == Some(&good)
        && check.status == Status::Passed));
    assert!(has(&diagnosis, Scope::Target, Some("One"), Status::Failed, "invalid_document"));
    assert!(has(&diagnosis, Scope::Tab, Some("Broken"), Status::Failed, "invalid_document"));
    assert!(!diagnosis.checks.iter().any(|check| check.owner.internal_name.as_deref() == Some("Broken")
        && check.routes.contains(&Route::ImportOriginal)));
    assert_eq!(snapshot(&root.0), before);
}

#[test]
fn absent_root_and_optional_target_do_not_initialize_defaults() {
    let root = Root::new();
    let missing = root.0.join("absent");
    let diagnosis = observe(Ok(&missing), &DoctorRequest::default());
    assert!(has(&diagnosis, Scope::Root, None, Status::Failed, "root_unavailable"));
    assert!(!missing.exists());
    tab(&root, "Closed", &["sample"]);
    let before = snapshot(&root.0);
    let diagnosis = report(&root);
    assert!(has(&diagnosis, Scope::Settings, None, Status::Failed, "absent"));
    assert!(has(&diagnosis, Scope::Target, Some("Closed"), Status::Passed, "absent"));
    assert_eq!(snapshot(&root.0), before);
}

#[test]
fn pending_writes_are_reported_without_settlement_or_sibling_suppression() {
    let root = Root::new();
    root.put("settings.pending", b"preserved settings write");
    root.put("unrelated.pending", b"not managed configuration");
    tab(&root, "One", &["sample"]);
    root.put("tabs/One/tab.pending", b"preserved Tab write");
    root.put("tabs/One/sample/target.pending", b"preserved Target write");
    root.put("tabs/One/sample/profile.pending", b"preserved profile write");
    let before = snapshot(&root.0);
    let diagnosis = report(&root);
    assert_eq!(diagnosis.checks.iter().filter(|check| check.code == "pending_transaction").count(), 4);
    assert!(!diagnosis.checks.iter().any(|check| check.routes.contains(&Route::RecoverTransaction)));
    assert!(has(&diagnosis, Scope::Tab, Some("One"), Status::Passed, "checked"));
    assert_eq!(snapshot(&root.0), before);
}

#[test]
fn unbound_targets_report_pending_and_malformed_records_without_reading_profiles() {
    let root = Root::new();
    tab(&root, "Unbound", &[]);
    root.put("tabs/Unbound/bad/target.config", b"PRIVATE_INVALID_TARGET");
    root.put("tabs/Unbound/bad/target.pending", b"retained write");
    let profile = profile(&root, "Unbound", "bad");
    root.put("tabs/Unbound/good/target.config", &serde_json::to_vec(&TargetRecord {
        version: 1, internal_name: "Unbound".into(), package_id: "good".into(),
        revision: 1, binding: None,
    }).unwrap());
    let before = snapshot(&root.0);
    let diagnosis = report(&root);
    for (scope, package, code, status) in [
        (Scope::Transactions, "bad", "pending_transaction", Status::Failed),
        (Scope::Target, "bad", "invalid_document", Status::Failed),
        (Scope::Target, "good", "checked", Status::Passed),
        (Scope::Profiles, "bad", "owner_unavailable", Status::Unexamined),
    ] {
        assert!(diagnosis.checks.iter().any(|check| check.scope == scope
            && check.owner.internal_name.as_deref() == Some("Unbound")
            && check.owner.package_id.as_deref() == Some(package)
            && check.code == code && check.status == status), "{scope:?}/{package}/{code}: {diagnosis:?}");
    }
    assert!(!diagnosis.checks.iter().any(|check| check.owner.profile_id.as_deref() == Some(&profile)));
    assert!(!serde_json::to_string(&diagnosis).unwrap().contains("PRIVATE_"));
    assert_eq!(snapshot(&root.0), before);
}

#[test]
fn supported_and_unsupported_transaction_recovery_are_distinct_observations() {
    let root = Root::new();
    root.put("settings.json", &serde_json::to_vec(&Settings::default()).unwrap());
    let before = capture(&root.0).unwrap();
    let replacement = Capture::from_files(BTreeMap::from([("settings.json".into(), serde_json::to_vec(&Settings {
        gui_log_limit: 500, ..Settings::default()
    }).unwrap())]), true).unwrap();
    crate::restore::tests::interrupt_install(&root.0, replacement, &before.generation);
    let pending = snapshot(&root.0);
    let diagnosis = report(&root);
    assert!(diagnosis.checks.iter().any(|check| check.code == "pending_transaction"
        && check.routes == [Route::RecoverTransaction]));
    assert_eq!(snapshot(&root.0), pending);
    assert!(crate::restore::pending(&root.0).unwrap());
    let unsupported = Root::new();
    unsupported.put(".restore-journal/journal.json", br#"{"version":1,"private":"DO_NOT_DISCLOSE"}"#);
    tab(&unsupported, "Safe", &[]);
    let pending = snapshot(&unsupported.0);
    let diagnosis = report(&unsupported);
    assert!(has(&diagnosis, Scope::Transactions, None, Status::Failed, "recovery_unavailable"));
    assert!(!diagnosis.checks.iter().any(|check| check.routes.contains(&Route::RecoverTransaction)));
    assert!(has(&diagnosis, Scope::Tab, Some("Safe"), Status::Passed, "checked"));
    assert!(!serde_json::to_string(&diagnosis).unwrap().contains("DO_NOT_DISCLOSE"));
    assert_eq!(snapshot(&unsupported.0), pending);
}

#[test]
fn file_and_directory_bounds_are_incomplete_not_clean() {
    let root = Root::new();
    root.put("settings.json", &vec![b' '; MAX_SETTINGS_BYTES + 1]);
    tab(&root, "Good", &[]);
    let before = snapshot(&root.0);
    let diagnosis = report(&root);
    assert!(has(&diagnosis, Scope::Settings, None, Status::Failed, "bound_exceeded"));
    assert!(has(&diagnosis, Scope::Settings, None, Status::Unexamined, "parent_unavailable"));
    assert!(has(&diagnosis, Scope::Tab, Some("Good"), Status::Passed, "checked"));
    assert_eq!(snapshot(&root.0), before);
    for index in 0..MAX_DIRECTORY_ENTRIES { root.put(&format!("tabs/Overflow/item-{index}"), b""); }
    let diagnosis = report(&root);
    assert!(has(&diagnosis, Scope::Tab, Some("Overflow"), Status::Failed, "bound_exceeded"));
    assert!(has(&diagnosis, Scope::Tab, Some("Good"), Status::Passed, "checked"));
}

#[test]
fn report_cap_marks_unvisited_owners_and_bounds_serialization() {
    let root = Root::new();
    for index in 0..MAX_TABS {
        let packages: Vec<_> = (0..16).map(|id| format!("package-{id}")).collect();
        let names: Vec<_> = packages.iter().map(String::as_str).collect();
        tab(&root, &format!("Owner{index:02}"), &names);
    }
    let before = snapshot(&root.0);
    let diagnosis = report(&root);
    assert!(diagnosis.truncated);
    assert!(diagnosis.checks.len() <= MAX_CHECKS);
    assert!(serde_json::to_vec(&diagnosis).unwrap().len() <= MAX_REPORT_BYTES);
    assert!(has(&diagnosis, Scope::Root, None, Status::Unexamined, "bound_exceeded"));
    assert_eq!(snapshot(&root.0), before);
}

#[cfg(unix)]
#[test]
fn protected_links_and_unreadable_owners_refuse_without_reading_descendants() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = Root::new();
    let outside = Root::new();
    outside.put("settings.json", b"PRIVATE_EXTERNAL_DOCUMENT");
    symlink(&outside.0, root.0.join("linked-root")).unwrap();
    let diagnosis = observe(Ok(&root.0.join("linked-root")), &DoctorRequest::default());
    assert!(has(&diagnosis, Scope::Root, None, Status::Failed, "root_unavailable"));
    assert!(has(&diagnosis, Scope::Tabs, None, Status::Unexamined, "parent_unavailable"));
    tab(&root, "Good", &[]);
    symlink(&outside.0, root.0.join("tabs/Linked")).unwrap();
    tab(&root, "Private", &["sample"]);
    profile(&root, "Private", "sample");
    let protected = root.0.join("tabs/Private");
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o755)).unwrap();
    let diagnosis = report(&root);
    assert!(has(&diagnosis, Scope::Tab, Some("Linked"), Status::Failed, "read_refused"));
    assert!(has(&diagnosis, Scope::Tab, Some("Private"), Status::Failed, "read_refused"));
    assert!(has(&diagnosis, Scope::Tab, Some("Good"), Status::Passed, "checked"));
    assert!(!diagnosis.checks.iter().any(|check| check.owner.internal_name.as_deref() == Some("Private") && check.scope == Scope::Profile));
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o700)).unwrap();
    let document = root.0.join("tabs/Private/tab.config");
    fs::set_permissions(&document, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(fs::File::open(&document).is_err(), "fixture requires owner permission enforcement");
    let diagnosis = report(&root);
    assert!(has(&diagnosis, Scope::Tab, Some("Private"), Status::Failed, "read_refused"));
    assert!(has(&diagnosis, Scope::Tab, Some("Good"), Status::Passed, "checked"));
    fs::set_permissions(&document, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&document, root.0.join("retained-alias")).unwrap();
    let diagnosis = report(&root);
    assert!(has(&diagnosis, Scope::Tab, Some("Private"), Status::Failed, "read_refused"));
    assert!(!serde_json::to_string(&diagnosis).unwrap().contains("PRIVATE_EXTERNAL_DOCUMENT"));
    assert!(!root.0.join("logs").exists());
    storage::private_directory(&root.0.join("pkgs")).unwrap();
    symlink(&outside.0, root.0.join("pkgs").join("a".repeat(64))).unwrap();
    tab(&root, "Installed", &["sample"]);
    let diagnosis = report(&root);
    assert!(has(&diagnosis, Scope::Source, Some("Installed"), Status::Failed, "archive_invalid"));
    assert!(!has(&diagnosis, Scope::Source, Some("Installed"), Status::Failed, "archive_missing"));
}

