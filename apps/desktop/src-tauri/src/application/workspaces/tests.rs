use super::*;
use crate::application::test_support::*;
use crate::storage::{PackageReference, Store};
use std::fs;

#[test]
fn naming_boundaries_preserve_scalars_and_closed_name_ownership() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    for (internal, display) in [
        ("", "Name"),
        ("name１", "Name"),
        ("../escape", "Name"),
        (" white", "Name"),
        ("Valid", " \t"),
        ("Valid", "line\nbreak"),
    ] {
        assert!(application.create_workspace(internal, display).is_err());
    }
    assert!(
        application
            .create_workspace(&"a".repeat(65), "Name")
            .is_err()
    );
    assert!(
        application
            .create_workspace("LongDisplay", &"\u{10400}".repeat(81))
            .is_err()
    );
    assert!(application.workspace_catalog().unwrap().open.is_empty());
    let internal = "A".repeat(64);
    let display = "\u{10400}".repeat(80);
    let saved = application.create_workspace(&internal, &display).unwrap();
    assert_eq!(saved.display_name, display);
    application.close_workspace(&view_ref(&saved)).unwrap();
    assert!(
        application
            .create_workspace(&internal.to_lowercase(), "Alias")
            .is_err()
    );
    let duplicate = application.create_workspace("Duplicate", &display).unwrap();
    assert_eq!(duplicate.display_name, display);
    let reopened = application.reopen_workspace(&internal).unwrap();
    assert_eq!(reopened.internal_name, internal);
    assert_eq!(reopened.display_name, display);
    assert_ne!(reopened.workspace_id, saved.workspace_id);
    assert!(reopened.selection.is_none());
}

#[test]
fn omitted_display_name_survives_close_reopen_and_restart() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let saved = application.create_workspace("0-copy_9", "").unwrap();
    assert_eq!(saved.display_name, "0-copy_9");
    application.close_workspace(&view_ref(&saved)).unwrap();
    let catalog = application.workspace_catalog().unwrap();
    assert_eq!(catalog.closed[0].display_name, "0-copy_9");
    let reopened = application.reopen_workspace("0-copy_9").unwrap();
    assert_eq!(reopened.display_name, "0-copy_9");
    assert_ne!(reopened.workspace_id, saved.workspace_id);
    assert_eq!(
        Store::new(fixture.root.clone())
            .unwrap()
            .tab("0-copy_9")
            .unwrap()
            .display_name,
        "0-copy_9"
    );
}

#[test]
fn failed_durable_bind_close_and_reopen_preserve_host_and_saved_authority() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let empty = application.create_workspace("Main", "Saved name").unwrap();
    let reference = view_ref(&empty);
    let path = fixture.root.join("tabs/Main/tab.config");
    let pending = path.with_extension("pending");
    let before = fs::read(&path).unwrap();
    fs::write(&pending, b"interrupted Tab write").unwrap();
    assert!(application.inspect(&package_path(), &reference).is_err());
    assert!(application.close_workspace(&reference).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    let current = application.workspace_catalog().unwrap().open.remove(0);
    assert_eq!(view_ref(&current), reference);
    assert!(current.selection.is_none());
    assert_eq!(fs::read(&pending).unwrap(), b"interrupted Tab write");
    fs::remove_file(&pending).unwrap();
    let bound = application.inspect(&package_path(), &reference).unwrap();
    let bound_ref = workspace_ref(&bound);
    let bound_bytes = fs::read(&path).unwrap();
    fs::write(&pending, b"interrupted Tab write").unwrap();
    assert!(
        application
            .inspect(&fixture.numeric_package(), &bound_ref)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), bound_bytes);
    assert_eq!(
        application
            .validate(&bound_ref, json!({"priorities":["ocr"]}))
            .unwrap()["priorities"],
        json!(["ocr"])
    );
    fs::remove_file(&pending).unwrap();
    application.close_workspace(&bound_ref).unwrap();
    let closed_bytes = fs::read(&path).unwrap();
    fs::write(&pending, b"interrupted reopen").unwrap();
    assert!(application.reopen_workspace("Main").is_err());
    assert!(application.workspace_catalog().unwrap().open.is_empty());
    assert_eq!(fs::read(&path).unwrap(), closed_bytes);
    fs::remove_file(pending).unwrap();
    let reopened = application.reopen_workspace("Main").unwrap();
    assert_ne!(reopened.workspace_id, reference.workspace_id);
    assert!(reopened.selection.is_some());
}

#[test]
fn saved_package_projects_the_durable_reference_without_granting_authority() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let empty = application.create_workspace("Empty", "No package").unwrap();
    assert!(empty.saved_package.is_none());
    let source = fixture.package_at("movable-source");
    let bound = inspect_named(application, "Moved", &source).unwrap();
    let reference = PackageReference {
        package_id: bound.package.package_id.clone(),
        source: PackageSource::Directory {
            path: source
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        },
    };
    let catalog = application.workspace_catalog().unwrap();
    let moved = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Moved")
        .unwrap();
    assert_eq!(moved.saved_package.as_ref(), Some(&reference));
    inspect_named(application, "Archive", &package_path()).unwrap();
    application.shutdown().unwrap();
    let relocated = fixture.root.join("relocated-source");
    fs::rename(&source, &relocated).unwrap();
    let archive_path = fixture.root.join("tabs/Archive/tab.config");
    let mut archive = lock(&application.store).tab("Archive").unwrap();
    archive.packages[0].source = PackageSource::CustomArchive {
        path: fixture
            .root
            .join("unsupported.czip")
            .to_string_lossy()
            .into_owned(),
    };
    fs::write(&archive_path, serde_json::to_vec(&archive).unwrap()).unwrap();
    let restarted = Application::new(
        fixture.root.clone(),
        fixture.root.join("absent-runner"),
        fixture.root.join("absent-engine"),
    )
    .unwrap();
    let catalog = restarted.workspace_catalog().unwrap();
    let moved = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Moved")
        .unwrap();
    assert!(moved.selection.is_none());
    assert!(moved.source_error.is_some());
    assert_eq!(moved.saved_package.as_ref(), Some(&reference));
    assert_eq!(
        restarted.profiles(&view_ref(moved)).unwrap_err().category,
        "WorkspaceUnbound"
    );
    let unsupported = catalog
        .open
        .iter()
        .find(|tab| tab.internal_name == "Archive")
        .unwrap();
    assert!(unsupported.selection.is_none());
    assert_eq!(
        unsupported.saved_package.as_ref(),
        Some(&archive.packages[0])
    );
    assert!(
        catalog
            .open
            .iter()
            .find(|tab| tab.internal_name == "Empty")
            .unwrap()
            .saved_package
            .is_none()
    );
    let rebound = restarted.inspect(&relocated, &view_ref(moved)).unwrap();
    let repaired = restarted
        .workspace_catalog()
        .unwrap()
        .open
        .into_iter()
        .find(|tab| tab.internal_name == "Moved")
        .unwrap();
    assert_eq!(
        repaired.saved_package,
        Some(PackageReference {
            package_id: rebound.package.package_id.clone(),
            source: PackageSource::Directory {
                path: relocated
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            },
        })
    );
    assert_eq!(
        repaired.selection.as_ref().unwrap().revision,
        rebound.revision
    );
    restarted.shutdown().unwrap();
}
