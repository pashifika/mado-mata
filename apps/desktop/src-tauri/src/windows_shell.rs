use mado_mata_desktop::application::{RecognitionPickerGuard, TargetPickerGuard};
use mado_runtime_comparison::model::Fault;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, GetOpenFileNameW, OFN_DONTADDTORECENT, OFN_EXPLORER, OFN_FILEMUSTEXIST,
    OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_NODEREFERENCELINKS, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

const MAX_PATH_BYTES: usize = 4096;
const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy)]
enum FileKind {
    Png,
    Executable,
}

impl FileKind {
    fn category(self) -> &'static str {
        match self {
            Self::Png => "RecognitionPicker",
            Self::Executable => "TargetPicker",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Executable => "exe",
        }
    }
}

pub async fn choose_png(
    window: tauri::WebviewWindow,
    guard: RecognitionPickerGuard,
) -> Result<Option<String>, Fault> {
    choose_file(window, FileKind::Png, move || guard.validate()).await
}

pub async fn choose_executable(
    window: tauri::WebviewWindow,
    guard: TargetPickerGuard,
) -> Result<Option<String>, Fault> {
    choose_file(window, FileKind::Executable, move || guard.validate()).await
}

async fn choose_file(
    window: tauri::WebviewWindow,
    kind: FileKind,
    validate: impl Fn() -> Result<(), Fault> + Send + 'static,
) -> Result<Option<String>, Fault> {
    let (send, mut receive) = tauri::async_runtime::channel(1);
    let parent = window.clone();
    window
        .run_on_main_thread(move || {
            let result = (|| {
                validate()?;
                let hwnd = parent
                    .hwnd()
                    .map_err(|error| Fault::new(kind.category(), error.to_string()))?;
                open_file(hwnd.0 as HWND, kind)
            })();
            let _ = send.try_send((result, validate));
        })
        .map_err(|error| Fault::new(kind.category(), error.to_string()))?;
    let (selected, validate) = receive
        .recv()
        .await
        .ok_or_else(|| Fault::new(kind.category(), "File selection did not complete"))?;
    let selected = selected?;
    super::background(move || {
        // The dialog pumps messages; filesystem access must not block the event loop.
        validate()?;
        let selected = selected.map(|path| selected_path(path, kind)).transpose()?;
        validate()?;
        Ok(selected)
    })
    .await
}

pub async fn choose_directory(window: tauri::WebviewWindow) -> Result<Option<String>, Fault> {
    let (send, mut receive) = tauri::async_runtime::channel(1);
    let parent = window.clone();
    window
        .run_on_main_thread(move || {
            let result = parent
                .hwnd()
                .map_err(|error| Fault::new("OcrSetupPicker", error.to_string()))
                .and_then(|hwnd| open_directory(hwnd.0 as HWND));
            let _ = send.try_send(result);
        })
        .map_err(|error| Fault::new("OcrSetupPicker", error.to_string()))?;
    receive
        .recv()
        .await
        .ok_or_else(|| Fault::new("OcrSetupPicker", "Folder selection did not complete"))?
}

pub async fn choose_package(
    window: tauri::WebviewWindow,
    guard: mado_mata_desktop::application::DistributionPickerGuard,
    exporting: bool,
    suggested_name: Option<String>,
) -> Result<Option<String>, Fault> {
    let (send, mut receive) = tauri::async_runtime::channel(1);
    let parent = window.clone();
    window
        .run_on_main_thread(move || {
            let result = (|| {
                guard.validate()?;
                let hwnd = parent
                    .hwnd()
                    .map_err(|_| Fault::new("PackagePicker", "The parent window is unavailable"))?;
                let result = package_file(hwnd.0 as HWND, exporting, suggested_name.as_deref())?;
                guard.validate()?;
                Ok(result)
            })();
            let _ = send.try_send(result);
        })
        .map_err(|_| Fault::new("PackagePicker", "Could not schedule package file selection"))?;
    receive
        .recv()
        .await
        .ok_or_else(|| Fault::new("PackagePicker", "Package file selection did not complete"))?
}

#[expect(unsafe_code, reason = "audited owner-modal Win32 package file dialog")]
fn package_file(
    owner: HWND,
    exporting: bool,
    suggested: Option<&str>,
) -> Result<Option<String>, Fault> {
    use windows_sys::Win32::UI::Controls::Dialogs::GetSaveFileNameW;
    let mut path = [0_u16; MAX_PATH_BYTES + 1];
    if let Some(name) = suggested {
        for (destination, unit) in path.iter_mut().zip(name.encode_utf16()) {
            *destination = unit;
        }
    }
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: windows_sys::core::w!("MadoMata package (*.mmpkg)\0*.mmpkg\0"),
        nFilterIndex: 1,
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
        lpstrDefExt: windows_sys::core::w!("mmpkg"),
        Flags: OFN_EXPLORER
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | OFN_DONTADDTORECENT
            | OFN_HIDEREADONLY
            | OFN_NODEREFERENCELINKS
            | if exporting { 0 } else { OFN_FILEMUSTEXIST },
        ..Default::default()
    };
    // SAFETY: The owner and every UTF-16 buffer remain live through this main-thread modal call.
    let accepted = unsafe {
        if exporting {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    };
    if accepted == 0 {
        // SAFETY: Read this thread's common-dialog error immediately after failure.
        let error = unsafe { CommDlgExtendedError() };
        return if error == 0 {
            Ok(None)
        } else {
            Err(Fault::new(
                "PackagePicker",
                format!("File dialog failed ({error:#x})"),
            ))
        };
    }
    let length = path
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| Fault::new("PackagePicker", "Selected path exceeds its bound"))?;
    let path = String::from_utf16(&path[..length])
        .map_err(|_| Fault::new("PackagePicker", "Selected path is not valid Unicode"))?;
    if path.len() > MAX_PATH_BYTES
        || !Path::new(&path).is_absolute()
        || path.chars().any(char::is_control)
        || !Path::new(&path)
            .extension()
            .and_then(|part| part.to_str())
            .is_some_and(|part| part.eq_ignore_ascii_case("mmpkg"))
    {
        return Err(Fault::new(
            "PackagePicker",
            "Select a bounded absolute .mmpkg path",
        ));
    }
    Ok(Some(path))
}

#[expect(
    unsafe_code,
    reason = "audited owner-modal Windows folder selection and COM lifetime"
)]
fn open_directory(owner: HWND) -> Result<Option<String>, Fault> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{
        BIF_EDITBOX, BIF_NEWDIALOGSTYLE, BIF_NONEWFOLDERBUTTON, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
        SHBrowseForFolderW, SHGetPathFromIDListEx,
    };
    // SAFETY: Runs on the window thread; each successful COM initialization is balanced below.
    let initialized = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if initialized < 0 {
        return Err(Fault::new(
            "OcrSetupPicker",
            "Folder selection requires the UI COM apartment",
        ));
    }
    let result = (|| {
        let mut display = [0_u16; 260];
        let dialog = BROWSEINFOW {
            hwndOwner: owner,
            pszDisplayName: display.as_mut_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS
                | BIF_NEWDIALOGSTYLE
                | BIF_EDITBOX
                | BIF_NONEWFOLDERBUTTON,
            ..Default::default()
        };
        // SAFETY: Tauri retains the owner and buffers through the synchronous modal call.
        let selected = unsafe { SHBrowseForFolderW(&dialog) };
        if selected.is_null() {
            return Ok(None);
        }
        let mut path = [0_u16; MAX_PATH_BYTES + 1];
        // SAFETY: selected is the owned shell PIDL; the output buffer has the stated capacity.
        let converted =
            unsafe { SHGetPathFromIDListEx(selected, path.as_mut_ptr(), path.len() as u32, 0) };
        // SAFETY: Release the shell-allocated PIDL exactly once after its last use.
        unsafe { CoTaskMemFree(selected.cast()) };
        if converted == 0 {
            return Err(Fault::new(
                "OcrSetupPicker",
                "Select a filesystem folder within the path limit",
            ));
        }
        let length = path.iter().position(|unit| *unit == 0).ok_or_else(|| {
            Fault::new("OcrSetupPicker", "Selected folder path exceeds its bound")
        })?;
        let path = String::from_utf16(&path[..length]).map_err(|_| {
            Fault::new(
                "OcrSetupPicker",
                "Selected folder path is not valid Unicode",
            )
        })?;
        if path.len() > MAX_PATH_BYTES
            || !Path::new(&path).is_absolute()
            || path.chars().any(char::is_control)
        {
            return Err(Fault::new(
                "OcrSetupPicker",
                "Select a bounded absolute folder path",
            ));
        }
        // Resource inspection independently checks this folder before reading any libraries.
        Ok(Some(path))
    })();
    // SAFETY: Balance this function's successful initialization, including S_FALSE.
    unsafe { CoUninitialize() };
    result
}

#[expect(unsafe_code, reason = "audited owner-modal Win32 common file dialog")]
fn open_file(owner: HWND, kind: FileKind) -> Result<Option<String>, Fault> {
    let filter = match kind {
        FileKind::Png => windows_sys::core::w!("PNG image (*.png)\0*.png\0"),
        FileKind::Executable => windows_sys::core::w!("Windows executable (*.exe)\0*.exe\0"),
    };
    let mut path = [0_u16; MAX_PATH_BYTES + 1];
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: filter,
        nFilterIndex: 1,
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
        Flags: OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | OFN_DONTADDTORECENT
            | OFN_HIDEREADONLY
            | OFN_NODEREFERENCELINKS,
        ..Default::default()
    };
    // SAFETY: Tauri retains the owner on this thread; all buffers outlive the modal call.
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        // SAFETY: Read this thread's common-dialog error immediately after failure.
        let error = unsafe { CommDlgExtendedError() };
        return if error == 0 {
            Ok(None)
        } else {
            Err(Fault::new(
                kind.category(),
                format!("File dialog failed ({error:#x})"),
            ))
        };
    }
    let length = path
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| Fault::new(kind.category(), "Selected path exceeds its bound"))?;
    String::from_utf16(&path[..length])
        .map(Some)
        .map_err(|_| Fault::new(kind.category(), "Selected path is not valid Unicode"))
}

fn selected_path(value: String, kind: FileKind) -> Result<String, Fault> {
    let path = Path::new(&value);
    if value.len() > MAX_PATH_BYTES
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || !path
            .extension()
            .and_then(|part| part.to_str())
            .is_some_and(|part| part.eq_ignore_ascii_case(kind.extension()))
    {
        return Err(Fault::new(
            kind.category(),
            "Select a bounded absolute path with the required extension",
        ));
    }
    let io = |error: std::io::Error| Fault::new(kind.category(), error.to_string());
    let metadata = std::fs::symlink_metadata(path).map_err(io)?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(Fault::new(
            kind.category(),
            "Selected file must be regular, not a reparse point",
        ));
    }
    // The consuming PNG loader or target metadata check revalidates at use time.
    Ok(value)
}

struct ClipboardWindow(HWND);

impl Drop for ClipboardWindow {
    #[expect(
        unsafe_code,
        reason = "releasing the hidden clipboard owner on its creating thread"
    )]
    fn drop(&mut self) {
        // SAFETY: This private window is owned by this scope and never leaves its thread.
        unsafe { DestroyWindow(self.0) };
    }
}

struct Clipboard;

impl Drop for Clipboard {
    #[expect(
        unsafe_code,
        reason = "closing this thread's exclusively opened clipboard"
    )]
    fn drop(&mut self) {
        // SAFETY: Constructed only after OpenClipboard succeeds, on the same thread.
        unsafe { CloseClipboard() };
    }
}

struct ClipboardMemory(HGLOBAL);

impl Drop for ClipboardMemory {
    #[expect(
        unsafe_code,
        reason = "releasing unpublished movable clipboard allocation"
    )]
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Non-null means ownership was not transferred to SetClipboardData.
            unsafe { GlobalFree(self.0) };
        }
    }
}

#[expect(
    unsafe_code,
    reason = "audited Win32 Unicode clipboard ownership transfer"
)]
pub fn copy_text(text: &str) -> Result<(), Fault> {
    let failed = || {
        Fault::new(
            "RecognitionClipboard",
            std::io::Error::last_os_error().to_string(),
        )
    };
    if text.len() > MAX_CLIPBOARD_BYTES || text.contains('\0') {
        return Err(Fault::new(
            "RecognitionClipboard",
            "Clipboard text exceeds its bound or contains NUL",
        ));
    }
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: STATIC is a system class; this invisible message-only window has no borrowed state.
    let window = ClipboardWindow(unsafe {
        CreateWindowExW(
            0,
            windows_sys::core::w!("STATIC"),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    });
    if window.0.is_null() {
        return Err(failed());
    }
    // SAFETY: The allocation is bounded and remains ours until publication succeeds.
    let mut memory = ClipboardMemory(unsafe { GlobalAlloc(GMEM_MOVEABLE, text.len() * 2) });
    if memory.0.is_null() {
        return Err(failed());
    }
    // SAFETY: The live allocation is movable global memory and has not been transferred.
    let pointer = unsafe { GlobalLock(memory.0) }.cast::<u16>();
    if pointer.is_null() {
        return Err(failed());
    }
    // SAFETY: Destination has exactly the required size, alignment, and no aliases with text.
    unsafe {
        pointer.copy_from_nonoverlapping(text.as_ptr(), text.len());
        GlobalUnlock(memory.0);
    }
    // SAFETY: A non-null window is needed so EmptyClipboard does not establish a null owner.
    if unsafe { OpenClipboard(window.0) } == 0 {
        return Err(failed());
    }
    let _clipboard = Clipboard;
    // SAFETY: The current thread owns the open clipboard and supplies initialized UTF-16 plus NUL.
    if unsafe { EmptyClipboard() } == 0
        || unsafe { SetClipboardData(u32::from(CF_UNICODETEXT), memory.0) }.is_null()
    {
        return Err(failed());
    }
    memory.0 = std::ptr::null_mut();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_selection_rejects_missing_files_directories_and_other_extensions() {
        let executable = std::env::current_exe()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            selected_path(executable.clone(), FileKind::Executable).unwrap(),
            executable
        );
        assert!(selected_path(executable.clone(), FileKind::Png).is_err());
        let directory = std::env::temp_dir().join(format!("picker-{}.exe", xid::new()));
        std::fs::create_dir(&directory).unwrap();
        let value = directory.to_str().unwrap().to_owned();
        assert_eq!(
            selected_path(value.clone(), FileKind::Executable)
                .unwrap_err()
                .category,
            "TargetPicker"
        );
        std::fs::remove_dir(&directory).unwrap();
        assert!(selected_path(value, FileKind::Executable).is_err());
        for value in [
            "relative.exe",
            "C:\\invalid\\name.exe\n",
            "C:\\file.lnk",
            "C:\\file.txt",
        ] {
            assert_eq!(
                selected_path(value.into(), FileKind::Executable)
                    .unwrap_err()
                    .category,
                "TargetPicker"
            );
        }
    }

    #[test]
    fn clipboard_refuses_oversized_or_embedded_nul_before_touching_system_clipboard() {
        assert_eq!(
            copy_text("a\0b").unwrap_err().category,
            "RecognitionClipboard"
        );
        assert_eq!(
            copy_text(&"x".repeat(MAX_CLIPBOARD_BYTES + 1))
                .unwrap_err()
                .category,
            "RecognitionClipboard"
        );
    }
}
