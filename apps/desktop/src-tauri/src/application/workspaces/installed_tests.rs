use super::*;
use crate::application::InspectionKind;
use crate::application::test_support::*;
use crate::authoring::CatalogEdit;
use crate::distribution::install_for_test;
use crate::storage::{Store, TabRecord};
use mado_runtime_comparison::inventory::DraftFileKind;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// A distributable copy of the shared fixture installed through the real import path.
/// The authored source is removed afterwards so nothing can silently read it back.
fn installed(fixture: &Fixture, name: &str) -> InstalledIdentity {
    let source = fixture.package_at(name);
    let manifest_path = source.join("package.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["distribution"] = json!({
        "version":1,"release_version":"1.0.0-rc.1+fixture","name":"M0 workload",
        "description":"Fixture release","author_name":"Fixture author","license":"MIT",
        "game":{"id":"fixture-game","names":{"en":"Fixture Game"},"aliases":["fixture.exe"]},
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let identity = install_for_test(&fixture.root, &source);
    fs::remove_dir_all(&source).unwrap();
    identity
}

fn reference(identity: &InstalledIdentity) -> PackageSource {
    PackageSource::Installed {
        package_id: identity.package_id.clone(),
        artifact_id: identity.artifact_id.clone(),
    }
}

fn container(fixture: &Fixture, identity: &InstalledIdentity) -> PathBuf {
    fixture
        .root
        .join("pkgs")
        .join(&identity.package_id)
        .join(&identity.artifact_id)
}

fn restart(root: &Path) -> Arc<Application> {
    Application::new(
        root.to_path_buf(),
        root.join("absent-runner"),
        root.join("absent-engine"),
    )
    .unwrap()
}

/// Every regular file below `root`, so the installed original can be compared exactly.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    found
}

#[test]
fn installed_release_is_selected_explicitly_and_reverified_on_restart_and_start() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let identity = installed(&fixture, "release");
    let reference = reference(&identity);
    // Installing a release neither selects nor binds it.
    let before = application.workspace_catalog().unwrap();
    assert!(before.open[0].selection.is_none());
    assert!(before.open[0].saved_package.is_none());
    assert!(
        lock(&application.store)
            .tab("Installed")
            .unwrap()
            .packages
            .is_empty()
    );

    let outcome = application
        .inspect_installed(
            &view_ref(&workspace),
            &identity.package_id,
            &identity.artifact_id,
        )
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::Bound);
    let selection = outcome.workspace.selection.unwrap();
    assert_eq!(selection.source, reference);
    assert_eq!(
        PathBuf::from(&selection.package_path),
        container(&fixture, &identity)
            .join("package")
            .canonicalize()
            .unwrap()
    );
    assert_eq!(outcome.workspace.saved_package.unwrap().source, reference);
    let tab = lock(&application.store).tab("Installed").unwrap();
    assert_eq!(tab.packages[0].source, reference);

    // Naming the managed payload as a directory keeps the installed form and checks.
    let by_path = application
        .inspect(
            Path::new(&selection.package_path),
            &workspace_ref(&selection),
        )
        .unwrap();
    assert_eq!(by_path.kind, InspectionKind::Bound);
    let selection = by_path.workspace.selection.unwrap();
    assert_eq!(selection.source, reference);
    assert_eq!(
        lock(&application.store).tab("Installed").unwrap().packages[0].source,
        reference
    );

    // Start verifies the release itself and only then reaches the absent runner.
    application
        .start(&workspace_ref(&selection), request(&selection))
        .unwrap();
    assert_eq!(settled(application).error.unwrap().category, "ChildStartup");

    application.shutdown().unwrap();
    let restarted = restart(&fixture.root);
    let view = restarted.workspace_catalog().unwrap().open.remove(0);
    assert!(view.source_error.is_none());
    let restored = view.selection.unwrap();
    assert_eq!(restored.source, reference);
    assert_eq!(
        restored.package.inventory_identity,
        selection.package.inventory_identity
    );
    restarted.shutdown().unwrap();
}

#[test]
fn changed_or_missing_installed_content_refuses_and_keeps_the_saved_reference() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let identity = installed(&fixture, "release");
    let reference = reference(&identity);
    let selection = application
        .inspect_installed(
            &view_ref(&workspace),
            &identity.package_id,
            &identity.artifact_id,
        )
        .unwrap()
        .workspace
        .selection
        .unwrap();
    let saved = application
        .save_profile(
            &workspace_ref(&selection),
            None,
            "Kept",
            selection.package.profiles["template-first"]["options"].clone(),
        )
        .unwrap();
    let tab_path = fixture.root.join("tabs/Installed/tab.config");
    let tab_before = fs::read(&tab_path).unwrap();
    let profile_before = fs::read(profile_path(&fixture, "Installed", &saved)).unwrap();

    // Changed bytes after selection are caught by Start's own verification.
    let payload = PathBuf::from(&selection.package_path);
    let source = payload.join("main.js");
    let mut changed = fs::read(&source).unwrap();
    changed.extend_from_slice(b"\n// changed after selection\n");
    fs::write(&source, changed).unwrap();
    application
        .start(&workspace_ref(&selection), request(&selection))
        .unwrap();
    assert_eq!(
        settled(application).error.unwrap().category,
        "InstalledIntegrity"
    );
    assert_eq!(
        application
            .save_profile(
                &workspace_ref(&selection),
                Some(&saved.id),
                "Kept",
                saved.values.clone(),
            )
            .unwrap_err()
            .category,
        "InstalledIntegrity"
    );
    assert_eq!(
        application
            .inspect_installed(
                &workspace_ref(&selection),
                &identity.package_id,
                &identity.artifact_id,
            )
            .unwrap_err()
            .category,
        "InstalledIntegrity"
    );
    // The payload path is never adopted as an ordinary directory to bypass integrity.
    assert_eq!(
        application
            .inspect(&payload, &workspace_ref(&selection))
            .unwrap_err()
            .category,
        "InstalledIntegrity"
    );
    assert_eq!(fs::read(&tab_path).unwrap(), tab_before);
    assert_eq!(
        fs::read(profile_path(&fixture, "Installed", &saved)).unwrap(),
        profile_before
    );

    application.shutdown().unwrap();
    let restarted = restart(&fixture.root);
    let view = restarted.workspace_catalog().unwrap().open.remove(0);
    assert!(view.selection.is_none());
    assert_eq!(view.source_error.unwrap().category, "InstalledIntegrity");
    assert_eq!(view.saved_package.unwrap().source, reference);
    restarted.shutdown().unwrap();

    fs::remove_dir_all(container(&fixture, &identity)).unwrap();
    let restarted = restart(&fixture.root);
    let view = restarted.workspace_catalog().unwrap().open.remove(0);
    assert!(view.selection.is_none());
    assert_eq!(view.source_error.unwrap().category, "InstalledMissing");
    assert_eq!(view.saved_package.unwrap().source, reference);
    restarted.shutdown().unwrap();
    assert_eq!(fs::read(&tab_path).unwrap(), tab_before);
    assert_eq!(
        fs::read(profile_path(&fixture, "Installed", &saved)).unwrap(),
        profile_before
    );
}

#[test]
fn selecting_an_installed_release_relocates_through_profile_checks_without_transfer() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let directory = fixture.package_at("directory");
    let selection = inspect_named(application, "Owner", &directory).unwrap();
    let saved = application
        .save_profile(
            &workspace_ref(&selection),
            None,
            "Owned",
            selection.package.profiles["template-first"]["options"].clone(),
        )
        .unwrap();
    let profile_before = fs::read(profile_path(&fixture, "Owner", &saved)).unwrap();
    let other = application.create_workspace("Other", "Other").unwrap();
    let identity = installed(&fixture, "release");

    let outcome = application
        .inspect_installed(
            &workspace_ref(&selection),
            &identity.package_id,
            &identity.artifact_id,
        )
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::Bound);
    assert!(outcome.outcomes.is_empty());
    let bound = outcome.workspace.selection.unwrap();
    assert_eq!(bound.source, reference(&identity));
    assert_eq!(bound.profiles.len(), 1);
    assert_eq!(bound.profiles[0].id, saved.id);
    assert_eq!(
        fs::read(profile_path(&fixture, "Owner", &saved)).unwrap(),
        profile_before
    );
    // No other Tab gains a binding or profile.
    let untouched = lock(&application.store).tab("Other").unwrap();
    assert!(untouched.packages.is_empty());
    assert!(
        application
            .workspace_catalog()
            .unwrap()
            .open
            .iter()
            .find(|view| view.workspace_id == other.workspace_id)
            .unwrap()
            .selection
            .is_none()
    );
}

#[test]
fn configuration_restore_keeps_installed_references_without_payload_or_keys() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let identity = installed(&fixture, "release");
    application
        .inspect_installed(
            &view_ref(&workspace),
            &identity.package_id,
            &identity.artifact_id,
        )
        .unwrap();
    let capture = application.capture_configuration().unwrap();
    assert!(capture.files.keys().all(|path| !path.starts_with("pkgs/")));
    let tab: TabRecord =
        serde_json::from_slice(&capture.files["tabs/Installed/tab.config"]).unwrap();
    assert_eq!(tab.packages[0].source, reference(&identity));
    crate::restore::validate(&capture).unwrap();

    let destination = fixture.root.with_extension("restored");
    Store::new(destination.clone())
        .unwrap()
        .initialize(preferences())
        .unwrap();
    let live = crate::configuration::capture(&destination).unwrap();
    let generation = live.generation.clone();
    let plan = crate::restore::prepare(capture, live, Some(&generation)).unwrap();
    crate::restore::install(&destination, plan).unwrap();
    let restored = restart(&destination);
    let view = restored.workspace_catalog().unwrap().open.remove(0);
    assert!(view.selection.is_none());
    assert_eq!(view.source_error.unwrap().category, "InstalledMissing");
    assert_eq!(view.saved_package.unwrap().source, reference(&identity));
    restored.shutdown().unwrap();
    assert!(!destination.join("pkgs").exists());
    assert!(!destination.join("keys").exists());
    fs::remove_dir_all(&destination).unwrap();
}

#[test]
fn managed_payload_is_read_only_until_duplicated_to_a_new_source() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application.create_workspace("Edit", "Edit").unwrap();
    let identity = installed(&fixture, "release");
    let container = container(&fixture, &identity);
    let original = files(&container);

    let editor = application
        .authoring_open(&view_ref(&workspace), &container.join("package"))
        .unwrap();
    assert!(editor.managed);
    for refusal in [
        application.authoring_save(
            &editor.owner,
            &editor.revision,
            "main.js",
            "export const replaced = true;\n".into(),
        ),
        application.authoring_catalog(
            &editor.owner,
            &editor.revision,
            CatalogEdit::Remove {
                path: "profiles/ocr-first.json".into(),
            },
        ),
    ] {
        assert_eq!(refusal.unwrap_err().category, "ManagedPackage");
    }
    // No new source can be created inside the release container either.
    assert_eq!(
        application
            .publisher
            .create(&container.join("package/nested"), "nested")
            .unwrap_err()
            .category,
        "ManagedPackage"
    );
    assert_eq!(files(&container), original);

    let copy = application
        .authoring_duplicate(&editor.owner, &editor.revision, "m0-copy")
        .unwrap();
    assert!(!copy.managed);
    let copy_root = PathBuf::from(&copy.package_path);
    assert!(!copy_root.starts_with(&container));
    let text = |path: &str| -> Value {
        serde_json::from_str(
            copy.files
                .iter()
                .find(|file| file.path == path)
                .unwrap()
                .text
                .as_deref()
                .unwrap(),
        )
        .unwrap()
    };
    // Owner records follow the new ID; authored release facts stay as plain copied metadata.
    let manifest = text("package.json");
    assert_eq!(manifest["package_id"], "m0-copy");
    assert_eq!(manifest["distribution"]["author_name"], "Fixture author");
    for preset in copy
        .files
        .iter()
        .filter(|file| file.kind == DraftFileKind::Profile)
    {
        assert_eq!(text(&preset.path)["package_id"], "m0-copy");
    }
    // Signed-envelope evidence never travels with the editable copy.
    for evidence in ["index.json", "signature.json", "receipt.json"] {
        assert!(!copy_root.join(evidence).exists());
    }
    let saved = application
        .authoring_save(
            &copy.owner,
            &copy.revision,
            "decisions.js",
            "export const edited = true;\n".into(),
        )
        .unwrap();
    assert!(saved.view.is_some_and(|view| !view.managed));
    assert_eq!(files(&container), original);
    application.authoring_exit(&copy.owner).unwrap();
}

#[test]
fn ordinary_directories_under_pkgs_keep_directory_authoring_and_binding() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let plain = fixture.root.join("pkgs/plain");
    fs::create_dir_all(plain.parent().unwrap()).unwrap();
    fs::rename(fixture.package_at("plain-source"), &plain).unwrap();
    let workspace = application.create_workspace("Plain", "Plain").unwrap();
    let editor = application
        .authoring_open(&view_ref(&workspace), &plain)
        .unwrap();
    assert!(!editor.managed);
    let saved = application
        .authoring_save(
            &editor.owner,
            &editor.revision,
            "decisions.js",
            format!(
                "{}\n// edited\n",
                fs::read_to_string(plain.join("decisions.js")).unwrap()
            ),
        )
        .unwrap();
    let exited = application.authoring_exit(&saved.owner).unwrap();
    let selection = application
        .inspect(&plain, &view_ref(&exited))
        .unwrap()
        .workspace
        .selection
        .unwrap();
    assert_eq!(
        selection.source,
        PackageSource::Directory {
            path: plain.canonicalize().unwrap().to_str().unwrap().into()
        }
    );
}
