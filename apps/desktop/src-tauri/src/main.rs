use mado_mata_desktop::application::{
    InspectionOutcome, Poll, ProfileCatalog, RecoveryMutation, RecoveryRef,
    TargetApplicationResponse, TargetCheckResponse, TargetSaveResponse, TargetView,
    WorkspaceCatalog, WorkspaceRef, WorkspaceView,
};
use mado_mata_desktop::backup::SnapshotReceipt;
use mado_mata_desktop::bootstrap::{Bootstrap, BootstrapStatus, selected_roots};
use mado_mata_desktop::storage::{EditableSettings, LegacyImport, Profile, Settings};
use mado_mata_desktop::target::{TargetConfiguration, TargetExpectation, TargetResolution};
use mado_runtime_comparison::desktop::StartRequest;
use mado_runtime_comparison::model::Fault;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tauri::Manager;

#[cfg(target_os = "macos")]
mod picker;

struct Backend {
    bootstrap: Arc<Bootstrap>,
    closing: AtomicBool,
    exiting: AtomicBool,
}

async fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Fault> + Send + 'static,
) -> Result<T, Fault> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| Fault::new("Application", error.to_string()))?
}

#[tauri::command]
async fn bootstrap_status(state: tauri::State<'_, Backend>) -> Result<BootstrapStatus, Fault> {
    let bootstrap = state.bootstrap.clone();
    background(move || bootstrap.ensure_started()).await
}

#[tauri::command]
async fn initialize(
    settings: EditableSettings,
    confirm_fresh: bool,
    state: tauri::State<'_, Backend>,
) -> Result<BootstrapStatus, Fault> {
    let bootstrap = state.bootstrap.clone();
    background(move || bootstrap.initialize(settings, confirm_fresh)).await
}

#[tauri::command]
async fn retry_bootstrap(
    discard: bool,
    state: tauri::State<'_, Backend>,
) -> Result<BootstrapStatus, Fault> {
    let bootstrap = state.bootstrap.clone();
    background(move || bootstrap.retry(discard)).await
}

#[tauri::command]
async fn import_legacy_root(state: tauri::State<'_, Backend>) -> Result<BootstrapStatus, Fault> {
    let bootstrap = state.bootstrap.clone();
    background(move || bootstrap.import_legacy_root()).await
}

#[tauri::command]
async fn snapshot(
    destination: Option<String>,
    state: tauri::State<'_, Backend>,
) -> Result<SnapshotReceipt, Fault> {
    let bootstrap = state.bootstrap.clone();
    background(move || bootstrap.snapshot(destination.as_deref().map(Path::new))).await
}

#[tauri::command]
async fn restore_snapshot(
    archive_path: String,
    receipt_generation: Option<String>,
    confirm: bool,
    discard: bool,
    state: tauri::State<'_, Backend>,
) -> Result<BootstrapStatus, Fault> {
    let bootstrap = state.bootstrap.clone();
    background(move || {
        bootstrap.restore_snapshot(
            Path::new(&archive_path),
            receipt_generation.as_deref(),
            confirm,
            discard,
        )
    })
    .await
}

#[tauri::command]
async fn recover_restore(
    rollback: bool,
    confirm: bool,
    discard: bool,
    state: tauri::State<'_, Backend>,
) -> Result<BootstrapStatus, Fault> {
    let bootstrap = state.bootstrap.clone();
    background(move || bootstrap.recover_restore(rollback, confirm, discard)).await
}

#[tauri::command]
async fn create_workspace(
    internal_name: String,
    display_name: String,
    state: tauri::State<'_, Backend>,
) -> Result<WorkspaceView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.create_workspace(&internal_name, &display_name)).await
}

#[tauri::command]
async fn reopen_workspace(
    internal_name: String,
    state: tauri::State<'_, Backend>,
) -> Result<WorkspaceView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.reopen_workspace(&internal_name)).await
}

#[tauri::command]
async fn workspace_catalog(state: tauri::State<'_, Backend>) -> Result<WorkspaceCatalog, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.workspace_catalog()).await
}

#[tauri::command]
async fn inspect(
    package_path: String,
    workspace: WorkspaceRef,
    state: tauri::State<'_, Backend>,
) -> Result<InspectionOutcome, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.inspect(Path::new(&package_path), &workspace)).await
}

#[tauri::command]
async fn repair_profile(
    context: RecoveryRef,
    id: String,
    values: Value,
    state: tauri::State<'_, Backend>,
) -> Result<RecoveryMutation, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.repair_profile(&context, &id, values)).await
}

#[tauri::command]
async fn reset_profile(
    context: RecoveryRef,
    id: String,
    confirm: bool,
    state: tauri::State<'_, Backend>,
) -> Result<RecoveryMutation, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.reset_profile(&context, &id, confirm)).await
}

#[tauri::command]
async fn retry_binding(
    context: RecoveryRef,
    state: tauri::State<'_, Backend>,
) -> Result<InspectionOutcome, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.retry_binding(&context)).await
}

#[tauri::command]
async fn discard_recovery(
    context: RecoveryRef,
    state: tauri::State<'_, Backend>,
) -> Result<WorkspaceView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.discard_recovery(&context)).await
}

#[tauri::command]
async fn close_workspace(
    workspace: WorkspaceRef,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.close_workspace(&workspace)).await
}

#[tauri::command]
async fn settings(state: tauri::State<'_, Backend>) -> Result<Settings, Fault> {
    let bootstrap = state.bootstrap.clone();
    let application = bootstrap.application()?;
    background(move || {
        let result = application.settings();
        bootstrap.note_settings_result(&application, &result);
        result
    })
    .await
}

#[tauri::command]
async fn save_settings(
    settings: EditableSettings,
    state: tauri::State<'_, Backend>,
) -> Result<Settings, Fault> {
    let bootstrap = state.bootstrap.clone();
    let application = bootstrap.application()?;
    background(move || {
        let result = application.save_settings(settings);
        if result.is_ok() {
            bootstrap.note_settings_result(&application, &result);
        } else {
            bootstrap.note_settings_result(&application, &application.settings());
        }
        result
    })
    .await
}

#[tauri::command]
async fn validate(
    workspace: WorkspaceRef,
    values: Value,
    state: tauri::State<'_, Backend>,
) -> Result<Value, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.validate(&workspace, values)).await
}

#[tauri::command]
async fn profiles(
    workspace: WorkspaceRef,
    state: tauri::State<'_, Backend>,
) -> Result<ProfileCatalog, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.profiles(&workspace)).await
}

#[tauri::command]
async fn read_target(
    workspace: WorkspaceRef,
    state: tauri::State<'_, Backend>,
) -> Result<TargetView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.read_target(&workspace)).await
}

#[tauri::command]
async fn check_target(
    workspace: WorkspaceRef,
    expected: TargetExpectation,
    configuration: TargetConfiguration,
    state: tauri::State<'_, Backend>,
) -> Result<TargetCheckResponse, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.check_target(&workspace, &expected, &configuration)).await
}

#[tauri::command]
async fn save_target(
    workspace: WorkspaceRef,
    expected: TargetExpectation,
    configuration: TargetConfiguration,
    reviewed_resolution: Option<TargetResolution>,
    state: tauri::State<'_, Backend>,
) -> Result<TargetSaveResponse, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.save_target(
            &workspace,
            &expected,
            configuration,
            reviewed_resolution.as_ref(),
        )
    })
    .await
}

#[tauri::command]
async fn remove_target(
    workspace: WorkspaceRef,
    expected: TargetExpectation,
    state: tauri::State<'_, Backend>,
) -> Result<TargetView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.remove_target(&workspace, &expected)).await
}

#[tauri::command]
async fn choose_target_application(
    workspace: WorkspaceRef,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Option<String>, Fault> {
    let application = state.bootstrap.application()?;
    let guard = background(move || application.begin_target_picker(&workspace)).await?;
    #[cfg(target_os = "macos")]
    {
        picker::choose(window, guard).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, guard);
        Err(Fault::new(
            "TargetPlatform",
            "Application selection requires macOS",
        ))
    }
}

#[tauri::command]
fn reserve_running_application(
    workspace: WorkspaceRef,
    request_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    state
        .bootstrap
        .application()?
        .reserve_running_application(&workspace, &request_id)
}

#[tauri::command]
async fn check_running_application(
    workspace: WorkspaceRef,
    expected: TargetExpectation,
    request_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<TargetApplicationResponse, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.check_running_application(&workspace, &expected, &request_id))
        .await
}

#[tauri::command]
fn cancel_running_application(
    workspace: WorkspaceRef,
    request_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<bool, Fault> {
    let application = state.bootstrap.application()?;
    Ok(application.cancel_running_application(&workspace, &request_id))
}

#[tauri::command]
async fn import_legacy_profiles(
    workspace: WorkspaceRef,
    state: tauri::State<'_, Backend>,
) -> Result<LegacyImport, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.import_legacy_profiles(&workspace)).await
}

#[tauri::command]
async fn save_profile(
    workspace: WorkspaceRef,
    id: Option<String>,
    name: String,
    values: Value,
    state: tauri::State<'_, Backend>,
) -> Result<Profile, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.save_profile(&workspace, id.as_deref(), &name, values)).await
}

#[tauri::command]
async fn rename_profile(
    workspace: WorkspaceRef,
    id: String,
    name: String,
    state: tauri::State<'_, Backend>,
) -> Result<Profile, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.rename_profile(&workspace, &id, &name)).await
}

#[tauri::command]
async fn delete_profile(
    workspace: WorkspaceRef,
    id: String,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.delete_profile(&workspace, &id)).await
}

#[tauri::command]
async fn start(
    workspace: WorkspaceRef,
    request: StartRequest,
    state: tauri::State<'_, Backend>,
) -> Result<String, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.start(&workspace, request)).await
}

#[tauri::command]
async fn check_environment(
    workspace: Option<WorkspaceRef>,
    replay_descriptor_path: Option<String>,
    state: tauri::State<'_, Backend>,
) -> Result<String, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.check_environment(workspace.as_ref(), replay_descriptor_path))
        .await
}

#[tauri::command]
fn stop(run: String, state: tauri::State<'_, Backend>) -> Result<(), Fault> {
    state.bootstrap.running_application()?.stop(&run)
}

#[tauri::command]
async fn poll(state: tauri::State<'_, Backend>) -> Result<Poll, Fault> {
    let application = state.bootstrap.running_application()?;
    background(move || Ok(application.poll())).await
}

fn close(app: &tauri::AppHandle) {
    let backend = app.state::<Backend>();
    if backend.closing.swap(true, Ordering::SeqCst) {
        return;
    }
    let bootstrap = backend.bootstrap.clone();
    let app = app.clone();
    std::thread::spawn(move || {
        let outcome = bootstrap.shutdown();
        if let Err(error) = &outcome {
            eprintln!("Shutdown: {}", error.category);
        }
        let exit_app = app.clone();
        if app
            .run_on_main_thread(move || {
                if !exit_app.state::<Backend>().exiting.load(Ordering::SeqCst) {
                    exit_app.exit(i32::from(outcome.is_err()));
                }
            })
            .is_err()
        {
            eprintln!("Shutdown: event loop is no longer accepting exit requests");
        }
    });
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let data_root = match args.as_slice() {
        [] => None,
        [flag, path] if flag == "--data-dir" => Some(PathBuf::from(path)),
        _ => {
            eprintln!("Usage: mado-mata-desktop [--data-dir DIRECTORY]");
            std::process::exit(2);
        }
    };
    let executable = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tools/runtime-comparison/target/debug/mado-runtime-comparison");
    let engine_executable = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../../tools/runtime-comparison/target/desktop-engine/debug/mado-runtime-comparison",
    );
    let builder = tauri::Builder::default()
        .setup(move |app| {
            let home = if data_root.is_some() {
                Ok(PathBuf::new())
            } else {
                app.path()
                    .home_dir()
                    .map_err(|error| Fault::new("HomeDirectory", error.to_string()))
            };
            let (root, legacy) = selected_roots(home, data_root.clone());
            app.manage(Backend {
                bootstrap: Arc::new(Bootstrap::new(
                    root,
                    legacy,
                    executable.clone(),
                    engine_executable.clone(),
                )),
                closing: AtomicBool::new(false),
                exiting: AtomicBool::new(false),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap_status,
            initialize,
            retry_bootstrap,
            import_legacy_root,
            snapshot,
            restore_snapshot,
            recover_restore,
            create_workspace,
            reopen_workspace,
            workspace_catalog,
            inspect,
            repair_profile,
            reset_profile,
            retry_binding,
            discard_recovery,
            close_workspace,
            settings,
            save_settings,
            validate,
            profiles,
            read_target,
            check_target,
            save_target,
            remove_target,
            choose_target_application,
            reserve_running_application,
            check_running_application,
            cancel_running_application,
            import_legacy_profiles,
            save_profile,
            rename_profile,
            delete_profile,
            start,
            check_environment,
            stop,
            poll
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                close(window.app_handle());
            }
        });
    #[cfg(all(feature = "webdriver", debug_assertions))]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder
        .build(tauri::generate_context!())
        .expect("desktop initialization")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { api, .. } => {
                if !app.state::<Backend>().closing.load(Ordering::SeqCst) {
                    api.prevent_exit();
                    close(app);
                }
            }
            tauri::RunEvent::Exit => {
                let backend = app.state::<Backend>();
                backend.exiting.store(true, Ordering::SeqCst);
                backend.closing.store(true, Ordering::SeqCst);
                if let Err(error) = backend.bootstrap.shutdown() {
                    eprintln!("Shutdown: {}", error.category);
                }
            }
            _ => {}
        });
}
