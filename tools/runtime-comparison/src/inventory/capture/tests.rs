use super::*;
use crate::inventory::PackageDraft;
use crate::model::Plan;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    limits: Limits,
}

impl Fixture {
    fn new() -> Self {
        let plan: Plan =
            serde_json::from_str(include_str!("../../../fixtures/controlled-plan.json")).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mado-metadata-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = directory.join("package");
        fs::create_dir_all(&root).unwrap();
        let draft = PackageDraft::capture(
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
            &plan.limits,
        )
        .unwrap();
        for (path, bytes) in draft.files() {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        fs::create_dir(root.join("folder")).unwrap();
        Self {
            directory,
            root,
            limits: plan.limits,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn capture_observes_owner_stop_between_files_before_its_snapshot_deadline() {
    let fixture = Fixture::new();
    let owner = std::sync::Arc::new(crate::model::Control::new(&fixture.limits));
    let attempt = crate::model::Control::for_attempt(owner.clone(), &fixture.limits, true);
    let mut capture = capture_files(&fixture.root, &fixture.limits, Some(&attempt)).unwrap();
    owner.cancel();
    assert_eq!(
        capture.verify(&fixture.root).unwrap_err().category,
        "Cancelled"
    );
    capture.deadline = Instant::now();
    assert_eq!(
        capture.verify(&fixture.root).unwrap_err().category,
        "Cancelled"
    );
}

#[test]
fn os_metadata_preserves_strict_inventory_draft_and_recovery_identity() {
    let fixture = Fixture::new();
    let original = Inventory::capture(&fixture.root, &fixture.limits).unwrap();
    let draft = PackageDraft::capture(&fixture.root, &fixture.limits).unwrap();
    let names = [
        ".DS_Store",
        "._package.json",
        "tHuMbS.dB",
        "EHTHUMBS.DB",
        "EhThumbs_Vista.Db",
        "DESKTOP.INI",
        "folder/.DS_Store",
        "folder/._local file.png",
    ];
    for name in names {
        fs::write(fixture.root.join(name), b"OS metadata, not package bytes").unwrap();
    }
    let actual = Inventory::capture(&fixture.root, &fixture.limits).unwrap();
    assert_eq!(actual.identity, original.identity);
    assert_eq!(actual.metadata, original.metadata);
    let reopened = PackageDraft::capture(&fixture.root, &fixture.limits).unwrap();
    assert_eq!(reopened.revision(), draft.revision());
    assert_eq!(reopened.files(), draft.files());
    assert_eq!(
        PackageDraft::capture_recovery_bytes(&fixture.root, &fixture.limits).unwrap(),
        *draft.files()
    );
    for name in names {
        assert_eq!(
            fs::read(fixture.root.join(name)).unwrap(),
            b"OS metadata, not package bytes"
        );
    }
}

#[test]
fn metadata_can_appear_change_and_disappear_between_capture_passes() {
    let fixture = Fixture::new();
    let capture = capture_files(&fixture.root, &fixture.limits, None).unwrap();
    for name in [".DS_Store", "folder/._image.png", "folder/Thumbs.db"] {
        let path = fixture.root.join(name);
        fs::write(&path, b"first").unwrap();
        capture.verify(&fixture.root).unwrap();
        fs::write(&path, b"updated metadata").unwrap();
        capture.verify(&fixture.root).unwrap();
        fs::remove_file(&path).unwrap();
        capture.verify(&fixture.root).unwrap();
    }
    let final_capture = capture_files(&fixture.root, &fixture.limits, None).unwrap();
    assert_eq!(final_capture.files, capture.files);
}

#[test]
fn capture_still_refuses_content_membership_and_directory_identity_changes() {
    let fixture = Fixture::new();
    let capture = capture_files(&fixture.root, &fixture.limits, None).unwrap();
    let added = fixture.root.join("folder/unknown.json");
    fs::write(&added, b"{}").unwrap();
    assert!(capture.verify(&fixture.root).is_err());
    fs::remove_file(&added).unwrap();

    let original = fs::read(fixture.root.join("package.json")).unwrap();
    fs::remove_file(fixture.root.join("package.json")).unwrap();
    assert!(capture.verify(&fixture.root).is_err());
    fs::write(fixture.root.join("package.json"), &original).unwrap();

    let capture = capture_files(&fixture.root, &fixture.limits, None).unwrap();
    let mut changed_bytes = original;
    changed_bytes.push(b' ');
    fs::write(fixture.root.join("package.json"), changed_bytes).unwrap();
    assert!(capture.verify(&fixture.root).is_err());

    let capture = capture_files(&fixture.root, &fixture.limits, None).unwrap();
    fs::rename(
        fixture.root.join("folder"),
        fixture.directory.join("retained"),
    )
    .unwrap();
    assert!(capture.verify(&fixture.root).is_err());
    fs::create_dir(fixture.root.join("folder")).unwrap();
    assert!(capture.verify(&fixture.root).is_err());
}

#[test]
fn unknown_files_and_metadata_named_directories_are_not_ignored() {
    let fixture = Fixture::new();
    for name in [
        ".gitignore",
        ".ds_store",
        "._",
        ".DS_Store.backup",
        "Thumbs.db.bak",
        "unknown.json",
    ] {
        let path = fixture.root.join(name);
        fs::write(&path, b"retained unknown bytes").unwrap();
        assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture(&fixture.root, &fixture.limits).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"retained unknown bytes");
        fs::remove_file(path).unwrap();
    }
    for name in [".DS_Store", "._package.json"] {
        let path = fixture.root.join(name);
        fs::create_dir(&path).unwrap();
        assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture_recovery_bytes(&fixture.root, &fixture.limits).is_err());
        fs::remove_dir(path).unwrap();
    }
    let directory = fixture.root.join("Thumbs.db");
    fs::create_dir(&directory).unwrap();
    let capture = capture_files(&fixture.root, &fixture.limits, None).unwrap();
    assert!(capture.directories().any(|path| path == "Thumbs.db"));
    fs::write(directory.join("retained.json"), b"{}").unwrap();
    assert!(capture.verify(&fixture.root).is_err());
    assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
    assert!(PackageDraft::capture(&fixture.root, &fixture.limits).is_err());
}

#[cfg(unix)]
#[test]
fn appledouble_dot_suffixes_are_not_ignored() {
    let fixture = Fixture::new();
    for name in ["._.", "._.."] {
        let path = fixture.root.join(name);
        fs::write(&path, b"retained invalid suffix bytes").unwrap();
        assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture_recovery_bytes(&fixture.root, &fixture.limits).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"retained invalid suffix bytes");
        fs::remove_file(path).unwrap();
    }
}


#[test]
fn os_metadata_names_cannot_be_declared_as_package_files() {
    let fixture = Fixture::new();
    let original = PackageDraft::capture(&fixture.root, &fixture.limits).unwrap();
    for name in [
        ".DS_Store",
        "._schema.json",
        "desktop.ini",
        "folder/THUMBS.DB",
    ] {
        let mut files = original.files().clone();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&files["package.json"]).unwrap();
        let schema = files.remove(manifest["schema"].as_str().unwrap()).unwrap();
        manifest["schema"] = json!(name);
        files.insert(name.into(), schema);
        files.insert(
            "package.json".into(),
            PayloadBytes::new(serde_json::to_vec(&manifest).unwrap()).unwrap(),
        );
        assert!(inventory_from_files(files.clone(), &fixture.limits).is_err());
        assert!(PackageDraft::from_files(files, &fixture.limits).is_err());
    }
}

#[cfg(unix)]
#[test]
fn os_metadata_names_do_not_hide_links_or_special_files() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let fixture = Fixture::new();
    let outside = fixture.directory.join("outside");
    fs::write(&outside, b"outside retained bytes").unwrap();
    for name in [".DS_Store", "._package.json", "desktop.ini"] {
        let path = fixture.root.join(name);
        symlink(&outside, &path).unwrap();
        assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture_recovery_bytes(&fixture.root, &fixture.limits).is_err());
        fs::remove_file(&path).unwrap();

        fs::hard_link(&outside, &path).unwrap();
        assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture_recovery_bytes(&fixture.root, &fixture.limits).is_err());
        fs::remove_file(&path).unwrap();

        let socket_path = fixture.directory.join("socket");
        let socket = UnixListener::bind(&socket_path).unwrap();
        fs::rename(socket_path, &path).unwrap();
        assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture(&fixture.root, &fixture.limits).is_err());
        assert!(PackageDraft::capture_recovery_bytes(&fixture.root, &fixture.limits).is_err());
        drop(socket);
        fs::remove_file(&path).unwrap();
    }
    assert_eq!(fs::read(outside).unwrap(), b"outside retained bytes");
}

#[cfg(windows)]
#[test]
fn windows_metadata_hard_links_are_not_excluded() {
    let fixture = Fixture::new();
    let outside = fixture.directory.join("outside");
    fs::write(&outside, b"outside retained bytes").unwrap();
    let path = fixture.root.join("Thumbs.db");
    fs::hard_link(&outside, &path).unwrap();
    assert!(Inventory::capture(&fixture.root, &fixture.limits).is_err());
    assert!(PackageDraft::capture(&fixture.root, &fixture.limits).is_err());
    assert!(PackageDraft::capture_recovery_bytes(&fixture.root, &fixture.limits).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"outside retained bytes");
}
