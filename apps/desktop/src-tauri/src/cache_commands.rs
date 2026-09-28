use super::{Backend, background};
use mado_mata_desktop::application::{AuthoringRef, RecognitionView};
use mado_mata_desktop::capture_cache::CacheInfo;
use mado_runtime_comparison::model::Fault;
use std::path::Path;

#[tauri::command]
pub async fn capture_cache_info(state: tauri::State<'_, Backend>) -> Result<CacheInfo, Fault> {
    let application = state.bootstrap.application()?;
    background(move || Ok(application.capture_cache()?.info())).await
}

#[tauri::command]
pub async fn capture_cache_open(state: tauri::State<'_, Backend>) -> Result<(), Fault> {
    let application = state.bootstrap.application()?;
    background(move || open_folder(&application.capture_cache()?.folder_for_open()?)).await
}

#[tauri::command]
pub async fn recognition_load_cached(
    owner: AuthoringRef,
    revision: String,
    capture_id: String,
    document_revision: u64,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        let cache = application.capture_cache()?;
        application.recognition_load_cached(
            &owner,
            &revision,
            &cache,
            &capture_id,
            document_revision,
        )
    })
    .await
}

#[cfg(target_os = "macos")]
fn open_folder(path: &Path) -> Result<(), Fault> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg("--")
        .arg(path)
        .status()
        .map_err(|_| {
            Fault::new(
                "CaptureCache",
                "Finder could not be launched for the managed cache",
            )
        })?;
    if !status.success() {
        return Err(Fault::new(
            "CaptureCache",
            "Finder refused the managed cache folder",
        ));
    }
    Ok(())
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "ShellExecuteW receives only a validated managed folder and live UTF-16 buffers"
)]
fn open_folder(path: &Path) -> Result<(), Fault> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(Fault::new(
            "CaptureCache",
            "managed folder contains an invalid path component",
        ));
    }
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    // SAFETY: NUL-terminated arguments remain alive for this synchronous call; optional pointers are null.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err(Fault::new(
            "CaptureCache",
            "Explorer refused the managed cache folder",
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_folder(_path: &Path) -> Result<(), Fault> {
    Err(Fault::new(
        "CaptureCache",
        "opening the managed cache requires macOS or Windows",
    ))
}
