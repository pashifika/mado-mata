use mado_mata_desktop::application::RecognitionPickerGuard;
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
enum Selection {
    Png,
    Executable,
}

impl Selection {
    fn category(self) -> &'static str {
        match self {
            Self::Png => "RecognitionPicker",
            Self::Executable => "AuthoringExecutable",
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
    choose_file(window, guard, Selection::Png).await
}

/// Selection is only a path, never evidence of a running process or capture authority.
pub async fn choose_executable(
    window: tauri::WebviewWindow,
    guard: RecognitionPickerGuard,
    path: Option<String>,
) -> Result<Option<String>, Fault> {
    if let Some(path) = path {
        return super::background(move || {
            guard.validate()?;
            let path = selected_path(&path, Selection::Executable)?;
            guard.validate()?;
            Ok(Some(path))
        })
        .await;
    }
    choose_file(window, guard, Selection::Executable).await
}

async fn choose_file(
    window: tauri::WebviewWindow,
    guard: RecognitionPickerGuard,
    selection: Selection,
) -> Result<Option<String>, Fault> {
    let (send, mut receive) = tauri::async_runtime::channel(1);
    let parent = window.clone();
    window
        .run_on_main_thread(move || {
            let result = (|| {
                guard.validate()?;
                let hwnd = parent
                    .hwnd()
                    .map_err(|error| Fault::new(selection.category(), error.to_string()))?;
                open_file(hwnd.0 as HWND, selection)
            })();
            let _ = send.try_send((result, guard));
        })
        .map_err(|error| Fault::new(selection.category(), error.to_string()))?;
    let (selected, guard) = receive
        .recv()
        .await
        .ok_or_else(|| Fault::new(selection.category(), "File selection did not complete"))?;
    let selected = selected?;
    super::background(move || {
        // The dialog pumps messages; filesystem access must not block the event loop.
        guard.validate()?;
        let selected = selected
            .map(|path| selected_path(&path, selection))
            .transpose()?;
        guard.validate()?;
        Ok(selected)
    })
    .await
}

#[expect(unsafe_code, reason = "audited owner-modal Win32 common file dialog")]
fn open_file(owner: HWND, selection: Selection) -> Result<Option<String>, Fault> {
    let filter = match selection {
        Selection::Png => windows_sys::core::w!("PNG image (*.png)\0*.png\0"),
        Selection::Executable => windows_sys::core::w!("Application (*.exe)\0*.exe\0"),
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
                selection.category(),
                format!("File dialog failed ({error:#x})"),
            ))
        };
    }
    let length = path
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| Fault::new(selection.category(), "Selected path exceeds its bound"))?;
    String::from_utf16(&path[..length])
        .map(Some)
        .map_err(|_| Fault::new(selection.category(), "Selected path is not valid Unicode"))
}

fn selected_path(value: &str, selection: Selection) -> Result<String, Fault> {
    let path = Path::new(value);
    if value.len() > MAX_PATH_BYTES
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || !path
            .extension()
            .and_then(|part| part.to_str())
            .is_some_and(|part| part.eq_ignore_ascii_case(selection.extension()))
    {
        return Err(Fault::new(
            selection.category(),
            "Select a bounded absolute path with the required extension",
        ));
    }
    let io = |error: std::io::Error| Fault::new(selection.category(), error.to_string());
    let metadata = std::fs::symlink_metadata(path).map_err(io)?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(Fault::new(
            selection.category(),
            "Selected file must be regular, not a reparse point",
        ));
    }
    if matches!(selection, Selection::Executable) {
        for ancestor in path.ancestors().skip(1) {
            if std::fs::symlink_metadata(ancestor)
                .map_err(io)?
                .file_attributes()
                & FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return Err(Fault::new(
                    selection.category(),
                    "Executable path crosses a reparse point",
                ));
            }
        }
    }
    // The PNG loader and native target resolver independently revalidate identity at use time.
    Ok(value.to_owned())
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
    fn executable_selection_rejects_relative_wrong_extension_and_control_paths() {
        for path in [
            "game.exe",
            "C:game.exe",
            "C:\\game.cmd",
            "C:\\game.exe\n",
            "C:\\game.exe\0",
        ] {
            assert_eq!(
                selected_path(path, Selection::Executable)
                    .unwrap_err()
                    .category,
                "AuthoringExecutable"
            );
        }
    }

    #[test]
    fn executable_selection_preserves_literal_unicode_and_shell_metacharacters() {
        let root = std::env::temp_dir().join(format!(
            "mado-windows-path-{}",
            mado_mata_desktop::storage::new_id().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("ゲーム & literal.exe");
        std::fs::write(
            &path,
            b"selection does not launch or authorize an executable",
        )
        .unwrap();
        let text = path.to_str().unwrap();
        let result = selected_path(text, Selection::Executable);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();
        assert_eq!(result.unwrap(), text);
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
