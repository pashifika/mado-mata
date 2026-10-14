use super::{Backend, background};
use mado_mata_desktop::application::{
    AuthoringRef, DistributionReview, InspectionOutcome, WorkspaceRef,
};
use mado_mata_desktop::author_signing::SigningInfo;
use mado_mata_desktop::distribution::{InstalledRelease, Publication};
use mado_runtime_comparison::model::Fault;
use std::path::Path;

fn require_main(window: &tauri::WebviewWindow) -> Result<(), Fault> {
    if window.label() == super::collaboration_commands::MAIN_WINDOW {
        Ok(())
    } else {
        Err(Fault::new(
            "PackageWindow",
            "Package transfer and signing require the main window",
        ))
    }
}

#[tauri::command]
pub async fn package_export_prepare(
    owner: AuthoringRef,
    revision: String,
    destination: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<DistributionReview, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || {
        application.package_export_prepare(&owner, &revision, Path::new(&destination))
    })
    .await
}

#[tauri::command]
pub async fn package_export_publish(
    owner: AuthoringRef,
    revision: String,
    token: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Publication, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || application.package_export_publish(&owner, &revision, &token)).await
}

#[tauri::command]
pub async fn package_import_prepare(
    workspace: WorkspaceRef,
    path: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<DistributionReview, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || application.package_import_prepare(&workspace, Path::new(&path))).await
}

#[tauri::command]
pub async fn package_import_publish(
    workspace: WorkspaceRef,
    token: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Publication, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || application.package_import_publish(&workspace, &token)).await
}

#[tauri::command]
pub fn package_distribution_cancel(
    workspace: WorkspaceRef,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<bool, Fault> {
    require_main(&window)?;
    state
        .bootstrap
        .running_application()?
        .package_distribution_cancel(&workspace)
}

#[tauri::command]
pub async fn package_installed_list(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Vec<InstalledRelease>, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || application.package_installed_list()).await
}

#[tauri::command]
pub async fn inspect_installed(
    workspace: WorkspaceRef,
    package_id: String,
    artifact_id: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<InspectionOutcome, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || application.inspect_installed(&workspace, &package_id, &artifact_id)).await
}

#[tauri::command]
pub async fn author_signing_status(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Option<SigningInfo>, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || application.author_signing_status()).await
}

#[tauri::command]
pub async fn author_signing_enroll(
    workspace: WorkspaceRef,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
    app: tauri::AppHandle,
) -> Result<SigningInfo, Fault> {
    require_main(&window)?;
    let application = state.bootstrap.application()?;
    background(move || {
        application.author_signing_enroll(&workspace, move |url| {
            let url = url.to_owned();
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let scheduled = pending.clone();
            app.run_on_main_thread(move || {
                if scheduled.swap(false, std::sync::atomic::Ordering::AcqRel) {
                    let _ = send.send(open_browser(&url));
                }
            })
            .map_err(|_| Fault::new("SigningBrowser", "Could not schedule the external browser"))?;
            let result = receive.recv_timeout(std::time::Duration::from_secs(10));
            pending.store(false, std::sync::atomic::Ordering::Release);
            result
                .map_err(|_| Fault::new("SigningBrowser", "The external browser did not respond"))?
        })
    })
    .await
}

#[tauri::command]
pub async fn package_pick(
    workspace: WorkspaceRef,
    exporting: bool,
    suggested_name: Option<String>,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Option<String>, Fault> {
    require_main(&window)?;
    if suggested_name.as_ref().is_some_and(|name| {
        name.len() > 256 || name.contains(['/', '\\']) || name.chars().any(char::is_control)
    }) {
        return Err(Fault::new(
            "PackagePicker",
            "The suggested filename is not a bounded basename",
        ));
    }
    let application = state.bootstrap.application()?;
    let guard =
        background(move || application.reserve_distribution_picker(&workspace, exporting)).await?;
    #[cfg(target_os = "macos")]
    {
        super::picker::choose_package(window, guard, exporting, suggested_name).await
    }
    #[cfg(windows)]
    {
        super::windows_shell::choose_package(window, guard, exporting, suggested_name).await
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (window, guard, exporting, suggested_name);
        Err(Fault::new(
            "PackagePicker",
            "Package file selection requires macOS or Windows",
        ))
    }
}

#[cfg(target_os = "macos")]
fn open_browser(url: &str) -> Result<(), Fault> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};
    let url = NSURL::URLWithString(&NSString::from_str(url))
        .ok_or_else(|| Fault::new("SigningBrowser", "Invalid authorization URL"))?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(Fault::new(
            "SigningBrowser",
            "Could not open the external browser",
        ))
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "opening the library-generated Google authorization URL with the system handler"
)]
fn open_browser(url: &str) -> Result<(), Fault> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    // SAFETY: The library-generated URL and verb are NUL-terminated and live through
    // the synchronous call. No executable, arguments, or working directory is supplied.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            windows_sys::core::w!("open"),
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
            "SigningBrowser",
            "Could not open the external browser",
        ))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_browser(_url: &str) -> Result<(), Fault> {
    Err(Fault::new(
        "SigningBrowser",
        "External-browser enrollment requires macOS or Windows",
    ))
}
