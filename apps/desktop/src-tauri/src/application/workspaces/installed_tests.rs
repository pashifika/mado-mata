use super::*;
use crate::application::test_support::*;
use crate::application::{InspectionKind, RecoveryStatus};
use crate::distribution::install_for_test_with;
use crate::storage::{Store, TabRecord};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const SIGNER: [u8; 32] = [7; 32];
const OTHER_SIGNER: [u8; 32] = [9; 32];

/// One release of the shared fixture, optionally with another schema, installed through
/// the real export, archive file, import and publication path. The authored source is
/// removed afterwards so nothing can silently read it back.
fn release(
    fixture: &Fixture,
    version: &str,
    signer: [u8; 32],
    schema: Option<Value>,
) -> InstalledIdentity {
    let source = fixture.package_at(&format!("release-{version}-{}", signer[0]));
    let manifest_path = source.join("package.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["distribution"] = json!({
        "version":1,"release_version":version,"name":"M0 workload",
        "description":"Fixture release","author_name":"Fixture author","license":"MIT",
        "game":{"id":"fixture-game","names":{"en":"Fixture Game"},"aliases":["fixture.exe"]},
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    if let Some(schema) = schema {
        fs::write(
            source.join("schema.json"),
            serde_json::to_vec(&schema).unwrap(),
        )
        .unwrap();
    }
    let identity = install_for_test_with(&fixture.root, &source, signer);
    fs::remove_dir_all(&source).unwrap();
    identity
}

fn installed(fixture: &Fixture) -> InstalledIdentity {
    release(fixture, "1.0.0-rc.1+fixture", SIGNER, None)
}

/// The shared fixture schema after one change.
fn schema(change: impl FnOnce(&mut Value)) -> Value {
    let mut schema: Value =
        serde_json::from_slice(&fs::read(package_path().join("schema.json")).unwrap()).unwrap();
    change(&mut schema);
    schema
}

/// A compatible later schema: one more top-level option with a default.
fn with_retries(schema: &mut Value) {
    schema["properties"]["retries"] = json!({"type":"integer","minimum":0,"maximum":5,"default":2});
}

fn reference(identity: &InstalledIdentity) -> PackageSource {
    PackageSource::Installed {
        package_id: identity.package_id.clone(),
        artifact_id: identity.artifact_id.clone(),
    }
}

/// The verified archive of one installed release.
fn archive(fixture: &Fixture, identity: &InstalledIdentity) -> PathBuf {
    distribution::verify_installed(&fixture.root, &identity.package_id, &identity.artifact_id)
        .unwrap()
        .archive_path()
        .to_path_buf()
}

fn select(
    application: &Application,
    workspace: &WorkspaceRef,
    identity: &InstalledIdentity,
) -> Selection {
    let outcome = application
        .inspect_installed(workspace, &identity.package_id, &identity.artifact_id)
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::Bound);
    outcome.workspace.selection.unwrap()
}

/// Imports the exact original bytes of a release again through the application.
fn import_again(
    fixture: &Fixture,
    application: &Application,
    workspace: &WorkspaceRef,
    bytes: &[u8],
) {
    let file = fixture.root.join("original.mmpkg");
    fs::write(&file, bytes).unwrap();
    let review = application
        .package_import_prepare(workspace, &file)
        .unwrap();
    let publication = application
        .package_import_publish(workspace, &review.token)
        .unwrap();
    assert!(publication.committed && publication.cleanup_error.is_none());
    fs::remove_file(&file).unwrap();
}

fn restart(root: &Path) -> Arc<Application> {
    Application::new(
        root.to_path_buf(),
        root.join("absent-runner"),
        root.join("absent-engine"),
    )
    .unwrap()
}

/// Every regular file below `root`, so stored bytes can be compared exactly.
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

/// Installed files apart from the import lock, by absolute path.
fn installed_files(fixture: &Fixture) -> BTreeMap<PathBuf, Vec<u8>> {
    let packages = fixture.root.join("pkgs").canonicalize().unwrap();
    files(&packages)
        .into_iter()
        .filter(|(path, _)| path != Path::new(".import.lock"))
        .map(|(path, bytes)| (packages.join(path), bytes))
        .collect()
}

#[test]
fn installed_release_is_selected_explicitly_and_reverified_on_restart_and_start() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let identity = installed(&fixture);
    let reference = reference(&identity);
    let archive = archive(&fixture, &identity);
    // The installation is exactly the signed archive; nothing is extracted beside it.
    let stored = installed_files(&fixture);
    assert_eq!(stored.keys().collect::<Vec<_>>(), [&archive]);
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

    let selection = select(application, &view_ref(&workspace), &identity);
    assert_eq!(selection.source, reference);
    assert_eq!(PathBuf::from(&selection.package_path), archive);
    let tab = lock(&application.store).tab("Installed").unwrap();
    assert_eq!(tab.packages[0].source, reference);

    // Naming the archive keeps the installed form and its checks.
    let by_path = application
        .inspect(&archive, &workspace_ref(&selection))
        .unwrap();
    assert_eq!(by_path.kind, InspectionKind::Bound);
    let selection = by_path.workspace.selection.unwrap();
    assert_eq!(selection.source, reference);
    assert_eq!(
        lock(&application.store).tab("Installed").unwrap().packages[0].source,
        reference
    );

    // Start verifies the archive itself and only then reaches the absent runner.
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
    // Selection, Start and restart run the captured archive bytes without extracting them.
    assert_eq!(installed_files(&fixture), stored);
}

#[test]
fn changed_or_missing_installed_archive_refuses_until_the_original_is_imported_again() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let identity = installed(&fixture);
    let reference = reference(&identity);
    let archive = archive(&fixture, &identity);
    let original = fs::read(&archive).unwrap();
    let selection = select(application, &view_ref(&workspace), &identity);
    let saved = application
        .save_profile(
            &workspace_ref(&selection),
            None,
            "Kept",
            selection.package.profiles["template-first"]["options"].clone(),
        )
        .unwrap();
    let tab = fixture.root.join("tabs/Installed");
    let stored = files(&tab);

    // Changed archive bytes after selection are caught by Start's own verification.
    let mut changed = original.clone();
    changed.extend_from_slice(b"changed after selection");
    fs::write(&archive, &changed).unwrap();
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
    // The archive path is never adopted as an ordinary package location.
    assert_eq!(
        application
            .inspect(&archive, &workspace_ref(&selection))
            .unwrap_err()
            .category,
        "InstalledIntegrity"
    );
    assert_eq!(files(&tab), stored);

    application.shutdown().unwrap();
    let restarted = restart(&fixture.root);
    let view = restarted.workspace_catalog().unwrap().open.remove(0);
    assert!(view.selection.is_none());
    assert_eq!(view.source_error.unwrap().category, "InstalledIntegrity");
    assert_eq!(view.saved_package.unwrap().source, reference);
    restarted.shutdown().unwrap();

    fs::remove_file(&archive).unwrap();
    let restarted = restart(&fixture.root);
    let view = restarted.workspace_catalog().unwrap().open.remove(0);
    assert!(view.selection.is_none());
    assert_eq!(
        view.source_error.as_ref().unwrap().category,
        "InstalledMissing"
    );
    assert_eq!(view.saved_package.as_ref().unwrap().source, reference);

    // Importing the original archive again restores the unchanged saved reference.
    import_again(&fixture, &restarted, &view_ref(&view), &original);
    restarted.shutdown().unwrap();
    let restored = restart(&fixture.root);
    let view = restored.workspace_catalog().unwrap().open.remove(0);
    assert!(view.source_error.is_none());
    let selection = view.selection.unwrap();
    assert_eq!(selection.source, reference);
    assert_eq!(selection.profiles[0].id, saved.id);
    restored.shutdown().unwrap();
    assert_eq!(fs::read(&archive).unwrap(), original);
    assert_eq!(files(&tab), stored);
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
    let identity = installed(&fixture);

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
fn same_signer_release_upgrade_keeps_profiles_and_recovers_an_incompatible_schema() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let first = installed(&fixture);
    let selection = select(application, &view_ref(&workspace), &first);
    let custom = json!({"priorities":["ocr"],"actions":{"template":"E","ocr":"F"}});
    let saved = application
        .save_profile(&workspace_ref(&selection), None, "Custom", custom)
        .unwrap();

    // A later release of the same signer and package adds an option with a default.
    let added = release(&fixture, "1.1.0", SIGNER, Some(schema(with_retries)));
    let outcome = application
        .inspect_installed(
            &workspace_ref(&selection),
            &added.package_id,
            &added.artifact_id,
        )
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::Bound);
    assert_eq!(outcome.outcomes.len(), 1);
    assert_eq!(outcome.outcomes[0].profile_id, saved.id);
    assert_eq!(outcome.outcomes[0].status, RecoveryStatus::Saved);
    let upgraded = outcome.workspace.selection.unwrap();
    assert_eq!(upgraded.source, reference(&added));
    assert_eq!(upgraded.profiles.len(), 1);
    let profile = &upgraded.profiles[0];
    assert_eq!(profile.id, saved.id);
    assert_eq!(profile.name, "Custom");
    assert_eq!(profile.values["priorities"], json!(["ocr"]));
    assert_eq!(profile.values["actions"], json!({"template":"E","ocr":"F"}));
    assert_eq!(profile.values["retries"], 2);
    assert_eq!(
        lock(&application.store).tab("Installed").unwrap().packages[0].source,
        reference(&added)
    );

    // A release rejecting a saved value leaves the profile for repair, never coerced.
    let narrowed = release(
        &fixture,
        "2.0.0",
        SIGNER,
        Some(schema(|schema| {
            with_retries(schema);
            schema["properties"]["actions"]["properties"]["template"] =
                json!({"type":"string","enum":["A","C"]});
        })),
    );
    let file = profile_path(&fixture, "Installed", &saved);
    let before = fs::read(&file).unwrap();
    let outcome = application
        .inspect_installed(
            &workspace_ref(&upgraded),
            &narrowed.package_id,
            &narrowed.artifact_id,
        )
        .unwrap();
    assert_eq!(outcome.kind, InspectionKind::RecoveryRequired);
    assert_eq!(outcome.outcomes[0].status, RecoveryStatus::RepairRequired);
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(
        lock(&application.store).tab("Installed").unwrap().packages[0].source,
        reference(&added)
    );
    let recovery = outcome.workspace.recovery.unwrap();
    assert_eq!(recovery.profiles[0].profile.id, saved.id);
    let repaired = application
        .repair_profile(
            &recovery.context,
            &saved.id,
            json!({"priorities":["ocr"],"actions":{"template":"C","ocr":"F"},"retries":2}),
        )
        .unwrap();
    assert!(repaired.issue.is_none());
    let bound = application.retry_binding(&recovery.context).unwrap();
    assert_eq!(bound.kind, InspectionKind::Bound);
    let selection = bound.workspace.selection.unwrap();
    assert_eq!(selection.source, reference(&narrowed));
    assert_eq!(selection.profiles[0].id, saved.id);
    assert_eq!(selection.profiles[0].values["actions"]["template"], "C");
}

#[test]
fn another_or_unknown_signer_is_refused_before_any_tab_write() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let first = installed(&fixture);
    let selection = select(application, &view_ref(&workspace), &first);
    application
        .save_profile(
            &workspace_ref(&selection),
            None,
            "Kept",
            json!({"priorities":["ocr"],"actions":{"template":"E","ocr":"F"}}),
        )
        .unwrap();
    let tab = fixture.root.join("tabs/Installed");
    let stored = files(&tab);
    let refused = |identity: &InstalledIdentity| {
        application
            .inspect_installed(
                &workspace_ref(&selection),
                &identity.package_id,
                &identity.artifact_id,
            )
            .unwrap_err()
    };

    // The same package ID under another signing key installs as a distinct release but
    // never takes over this Tab's profiles, Target or binding.
    let foreign = release(&fixture, "1.1.0", OTHER_SIGNER, Some(schema(with_retries)));
    let fault = refused(&foreign);
    assert_eq!(fault.category, "InstalledSigner");
    assert_eq!(
        fault.context["saved_artifact_id"],
        first.artifact_id.as_str()
    );
    assert_eq!(
        application
            .inspect(&archive(&fixture, &foreign), &workspace_ref(&selection))
            .unwrap_err()
            .category,
        "InstalledSigner"
    );
    assert_eq!(files(&tab), stored);
    let view = application.workspace_catalog().unwrap().open.remove(0);
    assert_eq!(view.revision, selection.revision);
    assert_eq!(view.selection.unwrap().source, reference(&first));
    // Another Tab may use that signer's release.
    let other = application.create_workspace("Other", "Other").unwrap();
    assert_eq!(
        select(application, &view_ref(&other), &foreign).source,
        reference(&foreign)
    );

    // Without its archive the saved release's signer is unknown, so even the same
    // signer's later release waits until the original is imported again.
    let later = release(&fixture, "1.2.0", SIGNER, None);
    let saved_archive = archive(&fixture, &first);
    let original = fs::read(&saved_archive).unwrap();
    fs::remove_file(&saved_archive).unwrap();
    let fault = refused(&later);
    assert_eq!(fault.category, "InstalledSigner");
    assert_eq!(fault.context["cause"]["category"], "InstalledMissing");
    assert_eq!(files(&tab), stored);
    import_again(&fixture, application, &workspace_ref(&selection), &original);
    assert_eq!(
        select(application, &workspace_ref(&selection), &later).source,
        reference(&later)
    );
}

#[test]
fn configuration_restore_keeps_installed_references_without_payload_or_keys() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application
        .create_workspace("Installed", "Installed")
        .unwrap();
    let identity = installed(&fixture);
    select(application, &view_ref(&workspace), &identity);
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
fn installed_archives_and_former_containers_are_never_opened_as_source() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let workspace = application.create_workspace("Edit", "Edit").unwrap();
    let identity = installed(&fixture);
    let archive = archive(&fixture, &identity);
    // A former expanded container of the release is recognized only to be refused.
    let former = fixture
        .root
        .join("pkgs")
        .join(&identity.package_id)
        .join(&identity.artifact_id);
    fs::create_dir_all(former.join("package")).unwrap();
    fs::copy(
        package_path().join("package.json"),
        former.join("package/package.json"),
    )
    .unwrap();
    let packages = files(&fixture.root.join("pkgs"));

    for path in [&archive, &former, &former.join("package")] {
        assert_eq!(
            application
                .authoring_open(&view_ref(&workspace), path)
                .unwrap_err()
                .category,
            "ManagedPackage",
            "{path:?}"
        );
        assert!(application.authoring_owner().is_none());
    }
    // No new source can be created at or below an installed release either.
    for path in [archive.join("nested"), former.join("package/nested")] {
        assert_eq!(
            application
                .publisher
                .create(&path, "nested")
                .unwrap_err()
                .category,
            "ManagedPackage",
            "{path:?}"
        );
    }
    assert_eq!(files(&fixture.root.join("pkgs")), packages);
    let view = application.workspace_catalog().unwrap().open.remove(0);
    assert_eq!(view.revision, workspace.revision);
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
