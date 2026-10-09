use crate::{Backend, background};
use mado_mata_desktop::application::SetupOperation;
use mado_mata_desktop::ocr_setup::{self, NativeSelection, SetupView};
use mado_runtime_comparison::environment::OcrEnvironment;
use mado_runtime_comparison::model::Fault;

#[tauri::command]
pub fn ocr_setup_catalog() -> Result<SetupView, Fault> {
    ocr_setup::catalog_view()
}

#[tauri::command]
pub async fn ocr_setup_start(
    resource_id: String,
    environment: Option<OcrEnvironment>,
    native_selection: Option<NativeSelection>,
    state: tauri::State<'_, Backend>,
) -> Result<String, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.ocr_setup_start(resource_id, environment, native_selection))
        .await
}

#[tauri::command]
pub fn ocr_setup_poll(state: tauri::State<'_, Backend>) -> Result<Option<SetupOperation>, Fault> {
    Ok(state.bootstrap.running_application()?.ocr_setup_poll())
}

#[tauri::command]
pub fn ocr_setup_cancel(
    operation_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    state
        .bootstrap
        .running_application()?
        .ocr_setup_cancel(&operation_id)
}

#[tauri::command]
pub async fn ocr_setup_pick_folder(window: tauri::WebviewWindow) -> Result<Option<String>, Fault> {
    #[cfg(target_os = "macos")]
    {
        crate::picker::choose_directory(window).await
    }
    #[cfg(windows)]
    {
        crate::windows_shell::choose_directory(window).await
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = window;
        Err(Fault::new(
            "OcrSetupPlatform",
            "Folder selection requires macOS or Windows",
        ))
    }
}

#[tauri::command]
pub async fn ocr_setup_open_link(
    resource_id: String,
    index: usize,
    app: tauri::AppHandle,
) -> Result<(), Fault> {
    let url = ocr_setup::guidance_link(&resource_id, index)?;
    native_action(app, move || open_link(&url)).await
}

#[tauri::command]
pub async fn ocr_setup_copy_command(
    resource_id: String,
    index: usize,
    app: tauri::AppHandle,
) -> Result<(), Fault> {
    let command = ocr_setup::guidance_command(&resource_id, index)?;
    native_action(app, move || copy_command(&command)).await
}

async fn native_action(
    app: tauri::AppHandle,
    action: impl FnOnce() -> Result<(), Fault> + Send + 'static,
) -> Result<(), Fault> {
    let (send, mut receive) = tauri::async_runtime::channel(1);
    app.run_on_main_thread(move || {
        let _ = send.try_send(action());
    })
    .map_err(|_| Fault::new("OcrSetupGuidance", "Could not schedule the native action"))?;
    receive
        .recv()
        .await
        .ok_or_else(|| Fault::new("OcrSetupGuidance", "The native action did not complete"))?
}

#[cfg(target_os = "macos")]
fn open_link(url: &str) -> Result<(), Fault> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};
    let url = NSURL::URLWithString(&NSString::from_str(url))
        .ok_or_else(|| Fault::new("OcrSetupGuidance", "Invalid catalog URL"))?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(Fault::new(
            "OcrSetupGuidance",
            "Could not open the catalog link",
        ))
    }
}

#[cfg(target_os = "macos")]
fn copy_command(command: &str) -> Result<(), Fault> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    let pasteboard = NSPasteboard::generalPasteboard();
    // SAFETY: AppKit exports this immutable process-lifetime string type.
    #[expect(
        unsafe_code,
        reason = "audited immutable AppKit pasteboard type constant"
    )]
    let text_type = unsafe { NSPasteboardTypeString };
    pasteboard.clearContents();
    if pasteboard.setString_forType(&NSString::from_str(command), text_type) {
        Ok(())
    } else {
        Err(Fault::new(
            "OcrSetupGuidance",
            "Could not copy the installation command",
        ))
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "opening a validated fixed HTTPS catalog URL with the system handler"
)]
fn open_link(url: &str) -> Result<(), Fault> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    // SAFETY: Both strings are NUL-terminated and live through the synchronous call;
    // no executable, arguments, or working directory is supplied.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize > 32 {
        Ok(())
    } else {
        Err(Fault::new(
            "OcrSetupGuidance",
            "Could not open the catalog link",
        ))
    }
}

#[cfg(windows)]
fn copy_command(command: &str) -> Result<(), Fault> {
    crate::windows_shell::copy_text(command)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_link(_url: &str) -> Result<(), Fault> {
    Err(Fault::new(
        "OcrSetupPlatform",
        "Native setup guidance requires macOS or Windows",
    ))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn copy_command(_command: &str) -> Result<(), Fault> {
    Err(Fault::new(
        "OcrSetupPlatform",
        "Native clipboard access requires macOS or Windows",
    ))
}
