use mado_mata_desktop::application::{Application, Poll, Selection, WorkspaceRef};
use mado_mata_desktop::storage::{EditableSettings, Profile, Settings};
use mado_runtime_comparison::desktop::StartRequest;
use mado_runtime_comparison::model::Fault;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tauri::Manager;

struct Backend {
    application: Arc<Application>,
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
async fn inspect(
    package_path: String,
    workspace: Option<WorkspaceRef>,
    state: tauri::State<'_, Backend>,
) -> Result<Selection, Fault> {
    let application = state.application.clone();
    background(move || application.inspect(Path::new(&package_path), workspace.as_ref())).await
}

#[tauri::command]
async fn close_workspace(
    workspace: WorkspaceRef,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    let application = state.application.clone();
    background(move || application.close_workspace(&workspace)).await
}

#[tauri::command]
async fn settings(state: tauri::State<'_, Backend>) -> Result<Settings, Fault> {
    let application = state.application.clone();
    background(move || application.settings()).await
}

#[tauri::command]
async fn save_settings(
    settings: EditableSettings,
    state: tauri::State<'_, Backend>,
) -> Result<Settings, Fault> {
    let application = state.application.clone();
    background(move || application.save_settings(settings)).await
}

#[tauri::command]
async fn validate(
    workspace: WorkspaceRef,
    values: Value,
    state: tauri::State<'_, Backend>,
) -> Result<Value, Fault> {
    let application = state.application.clone();
    background(move || application.validate(&workspace, values)).await
}

#[tauri::command]
async fn profiles(
    workspace: WorkspaceRef,
    state: tauri::State<'_, Backend>,
) -> Result<Vec<Profile>, Fault> {
    let application = state.application.clone();
    background(move || application.profiles(&workspace)).await
}

#[tauri::command]
async fn save_profile(
    workspace: WorkspaceRef,
    id: Option<String>,
    name: String,
    values: Value,
    state: tauri::State<'_, Backend>,
) -> Result<Profile, Fault> {
    let application = state.application.clone();
    background(move || application.save_profile(&workspace, id.as_deref(), &name, values)).await
}

#[tauri::command]
async fn rename_profile(
    workspace: WorkspaceRef,
    id: String,
    name: String,
    state: tauri::State<'_, Backend>,
) -> Result<Profile, Fault> {
    let application = state.application.clone();
    background(move || application.rename_profile(&workspace, &id, &name)).await
}

#[tauri::command]
async fn delete_profile(
    workspace: WorkspaceRef,
    id: String,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    let application = state.application.clone();
    background(move || application.delete_profile(&workspace, &id)).await
}

#[tauri::command]
async fn start(
    workspace: WorkspaceRef,
    request: StartRequest,
    state: tauri::State<'_, Backend>,
) -> Result<String, Fault> {
    let application = state.application.clone();
    background(move || application.start(&workspace, request)).await
}

#[tauri::command]
async fn check_environment(
    workspace: Option<WorkspaceRef>,
    replay_descriptor_path: Option<String>,
    state: tauri::State<'_, Backend>,
) -> Result<String, Fault> {
    let application = state.application.clone();
    background(move || application.check_environment(workspace.as_ref(), replay_descriptor_path))
        .await
}

#[tauri::command]
fn stop(run: String, state: tauri::State<'_, Backend>) -> Result<(), Fault> {
    state.application.stop(&run)
}

#[tauri::command]
async fn poll(state: tauri::State<'_, Backend>) -> Result<Poll, Fault> {
    let application = state.application.clone();
    background(move || Ok(application.poll())).await
}

fn close(app: &tauri::AppHandle) {
    let backend = app.state::<Backend>();
    if backend.closing.swap(true, Ordering::SeqCst) {
        return;
    }
    let application = backend.application.clone();
    let app = app.clone();
    std::thread::spawn(move || {
        let outcome = application.shutdown();
        if let Err(error) = &outcome {
            eprintln!("Shutdown: {}", error.category);
        }
        // Serialize the exit request with native termination on the event thread.
        // A late worker must not call app.exit() after native Quit destroyed it.
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
            let root = match &data_root {
                Some(root) => root.clone(),
                None => app.path().app_data_dir()?,
            };
            app.manage(Backend {
                application: Application::new(root, executable.clone(), engine_executable.clone())?,
                closing: AtomicBool::new(false),
                exiting: AtomicBool::new(false),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            inspect,
            close_workspace,
            settings,
            save_settings,
            validate,
            profiles,
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
        .run(|app, event| {
            match event {
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
                    // macOS terminate: skips ExitRequested. This last callback must
                    // wait for bounded containment/flush, including an in-flight close.
                    if let Err(error) = backend.application.shutdown() {
                        eprintln!("Shutdown: {}", error.category);
                    }
                }
                _ => {}
            }
        });
}
