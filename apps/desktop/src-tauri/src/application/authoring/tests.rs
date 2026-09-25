use super::*;
use crate::application::test_support::profile_path;
use crate::application::test_support::{Fixture, inspect_named, request, view_ref, workspace_ref};
use std::fs;
use std::sync::{Barrier, mpsc};
use std::time::Instant;

struct Sources {
    fixture: Fixture,
    root: PathBuf,
    package: PathBuf,
}

impl Sources {
    fn new() -> Self {
        let fixture = Fixture::new();
        let root = fixture.root.with_extension("authoring-sources");
        fs::create_dir(&root).unwrap();
        let package = root.join("original");
        fs::rename(fixture.package_at("source"), &package).unwrap();
        Self {
            fixture,
            root,
            package,
        }
    }

    fn app(&self) -> &Arc<Application> {
        &self.fixture.application
    }
}

impl Drop for Sources {
    fn drop(&mut self) {
        let _ = self.app().shutdown();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn lease_excludes_all_ordinary_admission_and_invalidates_shared_source_on_exit() {
    let sources = Sources::new();
    let app = sources.app();
    let first = inspect_named(app, "first", &sources.package).unwrap();
    let second = inspect_named(app, "second", &sources.package).unwrap();
    let first_ref = workspace_ref(&first);
    let second_ref = workspace_ref(&second);
    let editor = app.authoring_open(&first_ref, &sources.package).unwrap();
    assert_eq!(editor.owner.workspace, first_ref);
    assert_eq!(app.poll().authoring, Some(editor.owner.clone()));
    assert!(
        app.poll().controller["run"].is_null(),
        "idle Edit must not launch a timed worker"
    );
    assert_eq!(
        app.start(&second_ref, request(&second))
            .unwrap_err()
            .category,
        "AuthoringActive"
    );
    assert_eq!(
        app.check_environment(None, None).unwrap_err().category,
        "AuthoringActive"
    );
    assert_eq!(
        app.check_environment(Some(&second_ref), None)
            .unwrap_err()
            .category,
        "AuthoringActive"
    );
    assert_eq!(
        app.authoring_open(&second_ref, &sources.package)
            .unwrap_err()
            .category,
        "AuthoringActive"
    );
    assert_eq!(
        app.authoring_create(&second_ref, "new-package")
            .unwrap_err()
            .category,
        "AuthoringActive"
    );
    assert_eq!(
        app.prepare_reconstruction().unwrap_err().category,
        "AuthoringActive"
    );
    assert_eq!(
        app.prepare_close(None).unwrap_err().category,
        "AuthoringActive"
    );
    assert_eq!(
        app.close_workspace(&first_ref).unwrap_err().category,
        "AuthoringActive"
    );
    assert!(!app.authoring_stop(&editor.owner).unwrap());

    let forged = AuthoringRef {
        workspace: second_ref.clone(),
        token: editor.owner.token.clone(),
    };
    assert_eq!(
        app.authoring_save(&forged, &editor.revision, "main.js", "changed".into())
            .unwrap_err()
            .category,
        "StaleAuthoring"
    );
    assert_eq!(
        app.authoring_stop(&forged).unwrap_err().category,
        "StaleAuthoring"
    );
    let exited = app.authoring_exit(&editor.owner).unwrap();
    assert_eq!(exited.revision, first_ref.revision + 1);
    assert!(exited.selection.is_none());
    assert_eq!(
        exited.saved_package.as_ref().unwrap().package_id,
        first.package.package_id
    );
    let catalog = app.workspace_catalog().unwrap();
    let other = catalog
        .open
        .iter()
        .find(|workspace| workspace.workspace_id == second_ref.workspace_id)
        .unwrap();
    assert!(other.selection.is_none());
    assert_eq!(other.revision, second_ref.revision + 1);
    assert_eq!(
        app.start(&second_ref, request(&second))
            .unwrap_err()
            .category,
        "StaleIdentity"
    );
    assert_eq!(
        app.authoring_stop(&editor.owner).unwrap_err().category,
        "StaleAuthoring"
    );
}

#[test]
fn duplicate_rotates_owner_without_rebinding_local_configuration() {
    let sources = Sources::new();
    let app = sources.app();
    let selection = inspect_named(app, "owner", &sources.package).unwrap();
    let workspace = workspace_ref(&selection);
    let original = fs::read(sources.package.join("package.json")).unwrap();
    let editor = app.authoring_open(&workspace, &sources.package).unwrap();
    let duplicate = app
        .authoring_duplicate(&editor.owner, &editor.revision, "copied-package")
        .unwrap();
    assert_ne!(duplicate.owner.token, editor.owner.token);
    assert_eq!(duplicate.owner.workspace, editor.owner.workspace);
    assert_eq!(duplicate.package_id, "copied-package");
    assert_eq!(
        fs::read(sources.package.join("package.json")).unwrap(),
        original
    );
    assert_eq!(
        app.authoring_refresh(&editor.owner).unwrap_err().category,
        "StaleAuthoring"
    );
    let exited = app.authoring_exit(&duplicate.owner).unwrap();
    assert_eq!(
        exited.saved_package.unwrap().package_id,
        selection.package.package_id
    );
    assert!(exited.selection.is_none());
}

#[test]
fn source_conflict_requires_deliberate_refresh_and_never_overwrites_external_bytes() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let editor = app
        .authoring_open(&view_ref(&workspace), &sources.package)
        .unwrap();
    let external = "export const external = 3;";
    fs::write(sources.package.join("decisions.js"), external).unwrap();
    assert_eq!(
        app.authoring_save(
            &editor.owner,
            &editor.revision,
            "decisions.js",
            "stale draft".into()
        )
        .unwrap_err()
        .category,
        "AuthoringConflict"
    );
    assert_eq!(
        fs::read_to_string(sources.package.join("decisions.js")).unwrap(),
        external
    );
    let refreshed = app.authoring_refresh(&editor.owner).unwrap();
    assert_ne!(refreshed.revision, editor.revision);
    assert_eq!(
        refreshed
            .files
            .iter()
            .find(|file| file.path == "decisions.js")
            .unwrap()
            .text
            .as_deref(),
        Some(external)
    );
    app.authoring_exit(&editor.owner).unwrap();
}

#[test]
fn validation_is_non_evaluating_and_reports_real_typescript_source_locations() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(sources.package.join("package.json")).unwrap()).unwrap();
    manifest["runtime"] = json!("typescript");
    manifest["sources"] = json!(["main.ts"]);
    manifest["entries"]["readiness"]["module"] = json!("main.ts");
    manifest["entries"]["workflow"]["module"] = json!("main.ts");
    fs::write(
        sources.package.join("package.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::remove_file(sources.package.join("main.js")).unwrap();
    fs::remove_file(sources.package.join("decisions.js")).unwrap();
    fs::write(sources.package.join("main.ts"), "throw new Error('AUTHORING MUST NOT EVALUATE');\nexport function readiness(): MadoReady { return 'Ready'; }\nexport function workflow(): void {}\n").unwrap();
    let editor = app
        .authoring_open(&view_ref(&workspace), &sources.package)
        .unwrap();
    let valid = app
        .authoring_validate(&editor.owner, &editor.revision)
        .unwrap();
    assert!(valid.valid, "{:?}", valid.diagnostics);
    assert_eq!(valid.revision, editor.revision);
    assert_eq!(app.poll().controller["operation"], "authoring_validate");
    assert_eq!(app.poll().authoring, Some(editor.owner.clone()));
    fs::write(
        sources.package.join("main.ts"),
        "export function readiness( {",
    )
    .unwrap();
    let invalid = app.authoring_refresh(&editor.owner).unwrap();
    let validation = app
        .authoring_validate(&editor.owner, &invalid.revision)
        .unwrap();
    assert!(!validation.valid);
    assert_eq!(validation.revision, invalid.revision);
    assert!(
        validation.diagnostics.iter().any(|fault| {
            let context = &fault.context;
            context["module"] == "main.ts"
                || context["diagnostics"]
                    .as_array()
                    .is_some_and(|diagnostics| {
                        diagnostics.iter().any(|diagnostic| {
                            diagnostic["file"] == "main.ts" || diagnostic["module"] == "main.ts"
                        })
                    })
        }),
        "{:?}",
        validation.diagnostics
    );
    app.authoring_exit(&editor.owner).unwrap();
}

#[test]
fn malformed_schema_can_be_repaired_without_resetting_saved_profiles() {
    let sources = Sources::new();
    let app = sources.app();
    let selection = inspect_named(app, "owner", &sources.package).unwrap();
    let workspace = workspace_ref(&selection);
    let profile = app
        .save_profile(
            &workspace,
            None,
            "Operator choices",
            request(&selection).values,
        )
        .unwrap();
    let profile_file = profile_path(&sources.fixture, "owner", &profile);
    let before = fs::read(&profile_file).unwrap();
    let schema = fs::read_to_string(sources.package.join("schema.json")).unwrap();
    fs::write(sources.package.join("schema.json"), "{ malformed schema").unwrap();
    let editor = app.authoring_open(&workspace, &sources.package).unwrap();
    let invalid = app
        .authoring_validate(&editor.owner, &editor.revision)
        .unwrap();
    assert!(!invalid.valid);
    assert_eq!(fs::read(&profile_file).unwrap(), before);
    let saved = app
        .authoring_save(&editor.owner, &editor.revision, "schema.json", schema)
        .unwrap();
    assert!(
        app.authoring_validate(&editor.owner, &saved.committed_revision)
            .unwrap()
            .valid
    );
    let exited = app.authoring_exit(&editor.owner).unwrap();
    assert!(exited.selection.is_none());
    assert_eq!(fs::read(&profile_file).unwrap(), before);
    let reinspected = app.inspect(&sources.package, &view_ref(&exited)).unwrap();
    let selected = reinspected.workspace.selection.unwrap();
    assert_eq!(
        selected
            .profiles
            .iter()
            .find(|saved| saved.id == profile.id)
            .unwrap()
            .values,
        profile.values
    );
    assert_eq!(fs::read(&profile_file).unwrap(), before);
}

#[test]
fn stop_is_independent_and_exit_waits_for_the_reserved_validation_worker() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let editor = app
        .authoring_open(&view_ref(&workspace), &sources.package)
        .unwrap();
    let (started, entered) = mpsc::sync_channel(1);
    let (release, released) = mpsc::channel();
    *lock(&app.authoring_validation_gate) = Some((started, released));
    let worker_app = app.clone();
    let owner = editor.owner.clone();
    let revision = editor.revision.clone();
    let worker = std::thread::spawn(move || worker_app.authoring_validate(&owner, &revision));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(app.poll().controller["state"], "preparing");
    assert_eq!(
        app.authoring_exit(&editor.owner).unwrap_err().category,
        "WorkspaceBusy"
    );
    assert_eq!(
        app.prepare_reconstruction().unwrap_err().category,
        "WorkspaceBusy"
    );
    {
        let _store = lock(&app.store);
        let _workspaces = lock(&app.workspaces);
        assert!(app.authoring_stop(&editor.owner).unwrap());
        assert_eq!(app.runner.poll().state, "stopping");
    }
    assert_eq!(app.authoring_owner(), Some(editor.owner.clone()));
    release.send(()).unwrap();
    let result = worker.join().unwrap().unwrap();
    assert!(!result.valid);
    assert_eq!(result.diagnostics[0].category, "Cancelled");
    assert_eq!(app.poll().controller["state"], "terminal");
    assert_eq!(app.authoring_owner(), Some(editor.owner.clone()));
    assert!(!app.authoring_stop(&editor.owner).unwrap());
    app.authoring_exit(&editor.owner).unwrap();
}

#[test]
fn racing_start_and_edit_can_never_both_own_admission() {
    let sources = Sources::new();
    let app = sources.app();
    let selection = inspect_named(app, "owner", &sources.package).unwrap();
    let workspace = workspace_ref(&selection);
    let store = lock(&app.store);
    let barrier = Arc::new(Barrier::new(3));
    let start_app = app.clone();
    let start_barrier = barrier.clone();
    let start_ref = workspace.clone();
    let start = std::thread::spawn(move || {
        start_barrier.wait();
        start_app.start(&start_ref, request(&selection))
    });
    let edit_app = app.clone();
    let edit_barrier = barrier.clone();
    let path = sources.package.clone();
    let edit = std::thread::spawn(move || {
        edit_barrier.wait();
        edit_app.authoring_open(&workspace, &path)
    });
    barrier.wait();
    let edit = edit.join().unwrap();
    drop(store);
    let start = start.join().unwrap();
    assert_ne!(edit.is_ok(), start.is_ok());
    if let Ok(editor) = edit {
        assert!(matches!(
            start.unwrap_err().category.as_str(),
            "WorkspaceBusy" | "AuthoringActive"
        ));
        app.authoring_exit(&editor.owner).unwrap();
    } else {
        let run = start.unwrap();
        app.stop(&run).unwrap();
    }
}

#[test]
fn shutdown_cancels_and_retains_child_ownership_until_reaped() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let editor = app
        .authoring_open(&view_ref(&workspace), &sources.package)
        .unwrap();
    let (started, entered) = mpsc::sync_channel(1);
    let (release, released) = mpsc::channel();
    *lock(&app.authoring_validation_gate) = Some((started, released));
    let validate_app = app.clone();
    let owner = editor.owner.clone();
    let validation =
        std::thread::spawn(move || validate_app.authoring_validate(&owner, &editor.revision));
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        app.prepare_close(None).unwrap_err().category,
        "AuthoringActive"
    );
    app.prepare_close(Some(&editor.owner)).unwrap();
    let shutdown_app = app.clone();
    let shutdown = std::thread::spawn(move || shutdown_app.shutdown());
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.runner.poll().state != "stopping" {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(app.authoring_owner().is_some());
    assert!(!shutdown.is_finished());
    release.send(()).unwrap();
    assert_eq!(
        validation.join().unwrap().unwrap().diagnostics[0].category,
        "Cancelled"
    );
    shutdown.join().unwrap().unwrap();
    assert!(app.authoring_owner().is_none());
    assert_eq!(app.runner.poll().state, "terminal");
}

#[test]
fn pending_publication_blocks_start_inspect_and_restart_restore_before_snapshot_capture() {
    let sources = Sources::new();
    let app = sources.app();
    let selection = inspect_named(app, "owner", &sources.package).unwrap();
    let workspace = workspace_ref(&selection);
    let journal = sources.fixture.root.join("authoring/pending.json");
    crate::storage::private_directory(journal.parent().unwrap()).unwrap();
    crate::configuration::write_private(&journal, b"interrupted journal").unwrap();
    assert_eq!(
        app.start(&workspace, request(&selection))
            .unwrap_err()
            .category,
        "AuthoringRecoveryRequired"
    );
    assert_eq!(
        app.inspect(&sources.package, &workspace)
            .unwrap_err()
            .category,
        "AuthoringRecoveryRequired"
    );
    assert_eq!(
        app.authoring_open(&workspace, &sources.package)
            .unwrap_err()
            .category,
        "AuthoringRecoveryRequired"
    );
    assert_eq!(
        app.prepare_reconstruction().unwrap_err().category,
        "AuthoringRecoveryRequired"
    );
    app.shutdown().unwrap();
    let restarted = Application::new(
        sources.fixture.root.clone(),
        "unused-runner".into(),
        "unused-engine".into(),
    )
    .unwrap();
    let catalog = restarted.workspace_catalog().unwrap();
    let restored = &catalog.open[0];
    assert!(restored.selection.is_none());
    assert_eq!(
        restored.source_error.as_ref().unwrap().category,
        "AuthoringRecoveryRequired"
    );
    assert_eq!(
        restored.saved_package.as_ref().unwrap().package_id,
        selection.package.package_id
    );
    assert_eq!(
        restarted
            .authoring_recover(&sources.package)
            .unwrap_err()
            .category,
        "AuthoringRecoveryRequired"
    );
    assert_eq!(fs::read(&journal).unwrap(), b"interrupted journal");
    restarted.shutdown().unwrap();
}

#[test]
fn create_and_save_accept_incomplete_text_but_refuse_stale_revisions() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let editor = app
        .authoring_create(&view_ref(&workspace), "created-package")
        .unwrap();
    assert_eq!(editor.package_id, "created-package");
    assert_eq!(
        Path::new(&editor.package_path),
        sources
            .fixture
            .root
            .canonicalize()
            .unwrap()
            .join("pkgs/created-package")
    );
    let saved = app
        .authoring_save(
            &editor.owner,
            &editor.revision,
            "main.ts",
            "export const unfinished =".into(),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(Path::new(&editor.package_path).join("main.ts")).unwrap(),
        "export const unfinished ="
    );
    assert_ne!(saved.committed_revision, editor.revision);
    assert_eq!(
        app.authoring_save(&editor.owner, &editor.revision, "main.ts", "stale".into())
            .unwrap_err()
            .category,
        "AuthoringConflict"
    );
    assert!(
        !app.authoring_validate(&editor.owner, &saved.committed_revision)
            .unwrap()
            .valid
    );
    app.authoring_exit(&editor.owner).unwrap();
}

#[test]
fn restarted_generation_cannot_reuse_an_authoring_token() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let editor = app
        .authoring_open(&view_ref(&workspace), &sources.package)
        .unwrap();
    app.shutdown().unwrap();
    let restarted = Application::new(
        sources.fixture.root.clone(),
        "unused-runner".into(),
        "unused-engine".into(),
    )
    .unwrap();
    let current = view_ref(&restarted.workspace_catalog().unwrap().open[0]);
    let reopened = restarted
        .authoring_open(&current, &sources.package)
        .unwrap();
    assert_ne!(reopened.owner.token, editor.owner.token);
    assert_eq!(
        restarted
            .authoring_refresh(&editor.owner)
            .unwrap_err()
            .category,
        "StaleAuthoring"
    );
    let replayed = AuthoringRef {
        workspace: current,
        token: editor.owner.token,
    };
    assert_eq!(
        restarted.authoring_exit(&replayed).unwrap_err().category,
        "StaleAuthoring"
    );
    assert_eq!(restarted.authoring_owner(), Some(reopened.owner.clone()));
    restarted.authoring_exit(&reopened.owner).unwrap();
    restarted.shutdown().unwrap();
}

#[test]
fn saved_packages_root_changes_only_future_destinations_and_survives_restart() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let original = app
        .authoring_create(&view_ref(&workspace), "original")
        .unwrap();
    let original_root = PathBuf::from(&original.package_path);
    let before = fs::read(original_root.join("package.json")).unwrap();
    let custom = sources.root.join("collections/new");
    let mut preferences = crate::application::test_support::preferences();
    preferences.packages_root = Some(custom.to_str().unwrap().into());
    app.save_settings(preferences).unwrap();
    assert!(!custom.exists());
    assert_eq!(app.authoring_owner(), Some(original.owner.clone()));
    assert_eq!(
        app.authoring_refresh(&original.owner).unwrap().package_path,
        original.package_path
    );
    let duplicate = app
        .authoring_duplicate(&original.owner, &original.revision, "copy")
        .unwrap();
    assert_eq!(
        Path::new(&duplicate.package_path),
        custom.canonicalize().unwrap().join("copy")
    );
    assert_eq!(
        fs::read(original_root.join("package.json")).unwrap(),
        before
    );
    app.authoring_exit(&duplicate.owner).unwrap();
    app.shutdown().unwrap();
    let restarted = Application::new(
        sources.fixture.root.clone(),
        "unused-runner".into(),
        "unused-engine".into(),
    )
    .unwrap();
    assert_eq!(
        restarted.settings().unwrap().packages_root.as_deref(),
        custom.to_str()
    );
    let current = view_ref(&restarted.workspace_catalog().unwrap().open[0]);
    let created = restarted.authoring_create(&current, "later").unwrap();
    assert_eq!(
        Path::new(&created.package_path),
        custom.canonicalize().unwrap().join("later")
    );
    let next = restarted.authoring_exit(&created.owner).unwrap();
    assert!(
        restarted
            .authoring_create(&view_ref(&next), "COPY")
            .is_err()
    );
    assert_eq!(
        fs::read(original_root.join("package.json")).unwrap(),
        before
    );
    restarted.shutdown().unwrap();
}

#[test]
fn invalid_package_ids_do_not_create_the_missing_collection() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    for id in [
        "",
        "../escape",
        "a/b",
        "a\\b",
        ".hidden",
        "ending.",
        "CON",
        "node_modules",
    ] {
        assert!(
            app.authoring_create(&view_ref(&workspace), id).is_err(),
            "{id}"
        );
        assert!(!sources.fixture.root.join("pkgs").exists());
        assert!(app.authoring_owner().is_none());
    }
}

#[test]
fn host_publication_refuses_unsafe_and_incoherent_edits_without_touching_source() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let editor = app
        .authoring_open(&view_ref(&workspace), &sources.package)
        .unwrap();
    let original = fs::read(sources.package.join("package.json")).unwrap();
    let external = sources.root.join("outside.txt");
    fs::write(&external, "external owner").unwrap();
    assert!(
        app.authoring_save(
            &editor.owner,
            &editor.revision,
            "../outside.txt",
            "overwrite".into()
        )
        .is_err()
    );
    assert!(
        app.authoring_save(
            &editor.owner,
            &editor.revision,
            "package.json",
            "{ malformed".into()
        )
        .is_err()
    );
    let edit = CatalogEdit::Rename {
        path: "main.js".into(),
        destination: "../escaped.js".into(),
    };
    assert!(
        app.authoring_catalog(&editor.owner, &editor.revision, edit)
            .is_err()
    );
    let remove = CatalogEdit::Remove {
        path: "main.js".into(),
    };
    assert!(
        app.authoring_catalog(&editor.owner, &editor.revision, remove)
            .is_err()
    );
    assert_eq!(fs::read_to_string(external).unwrap(), "external owner");
    assert_eq!(
        fs::read(sources.package.join("package.json")).unwrap(),
        original
    );
    assert_eq!(
        app.authoring_refresh(&editor.owner).unwrap().revision,
        editor.revision
    );
    app.authoring_exit(&editor.owner).unwrap();
}

#[test]
fn retired_application_can_still_close() {
    let sources = Sources::new();
    let app = sources.app();
    app.prepare_reconstruction().unwrap();
    app.prepare_close(None).unwrap();
    assert_eq!(
        app.create_workspace("late", "Late").unwrap_err().category,
        "Closing"
    );
}

#[test]
fn confirmed_close_retains_containment_and_rejects_a_stale_editor() {
    let sources = Sources::new();
    let app = sources.app();
    let workspace = app.create_workspace("owner", "Owner").unwrap();
    let editor = app
        .authoring_open(&view_ref(&workspace), &sources.package)
        .unwrap();
    lock(&app.workspaces)
        .authoring
        .as_mut()
        .unwrap()
        .containment = Some(Fault::new(
        "CompilerContainment",
        "child cleanup is unverified",
    ));
    assert_eq!(
        app.authoring_exit(&editor.owner).unwrap_err().category,
        "CompilerContainment"
    );
    assert_eq!(
        app.prepare_close(None).unwrap_err().category,
        "AuthoringActive"
    );
    let mut stale = editor.owner.clone();
    stale.token.push_str("-stale");
    assert_eq!(
        app.prepare_close(Some(&stale)).unwrap_err().category,
        "AuthoringActive"
    );
    assert!(!app.closing.load(Ordering::Acquire));
    app.prepare_close(Some(&editor.owner)).unwrap();
    assert_eq!(app.shutdown().unwrap_err().category, "CompilerContainment");
    assert_eq!(app.authoring_owner(), Some(editor.owner));
    app.prepare_close(None).unwrap();
}
