use mado_mata_desktop::application::{Application, Poll, Selection};
use mado_mata_desktop::storage::{Profile, Settings};
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
    state: tauri::State<'_, Backend>,
) -> Result<Selection, Fault> {
    let application = state.application.clone();
    background(move || application.select(Path::new(&package_path))).await
}

#[tauri::command]
async fn settings(state: tauri::State<'_, Backend>) -> Result<Settings, Fault> {
    let application = state.application.clone();
    background(move || application.settings()).await
}

#[tauri::command]
async fn save_settings(
    settings: Settings,
    state: tauri::State<'_, Backend>,
) -> Result<Settings, Fault> {
    let application = state.application.clone();
    background(move || application.save_settings(settings)).await
}

#[tauri::command]
async fn validate(values: Value, state: tauri::State<'_, Backend>) -> Result<Value, Fault> {
    let application = state.application.clone();
    background(move || application.validate(values)).await
}

#[tauri::command]
async fn profiles(state: tauri::State<'_, Backend>) -> Result<Vec<Profile>, Fault> {
    let application = state.application.clone();
    background(move || application.profiles()).await
}

#[tauri::command]
async fn save_profile(
    id: Option<String>,
    name: String,
    values: Value,
    state: tauri::State<'_, Backend>,
) -> Result<Profile, Fault> {
    let application = state.application.clone();
    background(move || application.save_profile(id.as_deref(), &name, values)).await
}

#[tauri::command]
async fn rename_profile(
    id: String,
    name: String,
    state: tauri::State<'_, Backend>,
) -> Result<Profile, Fault> {
    let application = state.application.clone();
    background(move || application.rename_profile(&id, &name)).await
}

#[tauri::command]
async fn delete_profile(id: String, state: tauri::State<'_, Backend>) -> Result<(), Fault> {
    let application = state.application.clone();
    background(move || application.delete_profile(&id)).await
}

#[tauri::command]
async fn start(request: StartRequest, state: tauri::State<'_, Backend>) -> Result<String, Fault> {
    let application = state.application.clone();
    background(move || application.start(request)).await
}

#[tauri::command]
fn stop(run: String, state: tauri::State<'_, Backend>) -> Result<(), Fault> {
    state.application.stop(&run)
}

#[tauri::command]
fn poll(state: tauri::State<'_, Backend>) -> Poll {
    state.application.poll()
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
        app.exit(i32::from(outcome.is_err()));
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
    let builder = tauri::Builder::default()
        .setup(move |app| {
            let root = match &data_root {
                Some(root) => root.clone(),
                None => app.path().app_data_dir()?,
            };
            app.manage(Backend {
                application: Application::new(root, executable.clone())?,
                closing: AtomicBool::new(false),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            inspect,
            settings,
            save_settings,
            validate,
            profiles,
            save_profile,
            rename_profile,
            delete_profile,
            start,
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
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !app.state::<Backend>().closing.load(Ordering::SeqCst) {
                    api.prevent_exit();
                    close(app);
                }
            }
        });
}
