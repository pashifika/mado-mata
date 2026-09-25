use block2::RcBlock;
use mado_mata_desktop::application::TargetPickerGuard;
use mado_runtime_comparison::model::Fault;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSModalResponse, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel, NSWindow,
};
use objc2_foundation::NSArray;
use objc2_uniform_type_identifiers::UTTypeApplicationBundle;
use std::cell::RefCell;
use std::path::Path;

pub async fn choose(
    window: tauri::WebviewWindow,
    guard: TargetPickerGuard,
) -> Result<Option<String>, Fault> {
    let (send, mut receive) = tauri::async_runtime::channel(1);
    let native_window = window.clone();
    window
        .run_on_main_thread(move || {
            let failed = send.clone();
            let result = (|| {
                let mtm = MainThreadMarker::new().ok_or_else(picker_failed)?;
                let pointer = native_window.ns_window().map_err(|_| picker_failed())?;
                if pointer.is_null() {
                    return Err(picker_failed());
                }
                // SAFETY: Tauri owns this live NSWindow; access stays on its main thread.
                let parent = unsafe { &*pointer.cast::<NSWindow>() };
                let panel = NSOpenPanel::openPanel(mtm);
                panel.setCanChooseFiles(true);
                panel.setCanChooseDirectories(false);
                panel.setAllowsMultipleSelection(false);
                panel.setTreatsFilePackagesAsDirectories(false);
                panel.setCanResolveUbiquitousConflicts(false);
                panel.setCanDownloadUbiquitousContents(false);
                // SAFETY: The system framework exports an immutable, process-lifetime UTI.
                let application_type = unsafe { UTTypeApplicationBundle };
                panel.setAllowedContentTypes(&NSArray::from_slice(&[application_type]));
                let selected_panel = panel.clone();
                let completion = RefCell::new(Some((guard, send)));
                let handler = RcBlock::new(move |response: NSModalResponse| {
                    // Dismiss before releasing admission, so a new run never starts behind the sheet.
                    selected_panel.orderOut(None);
                    let Some((guard, send)) = completion.borrow_mut().take() else {
                        return;
                    };
                    let result = if response == NSModalResponseCancel {
                        Ok(None)
                    } else if response == NSModalResponseOK {
                        guard.validate().and_then(|()| {
                            let url = selected_panel.URL().ok_or_else(picker_failed)?;
                            if !url.isFileURL() {
                                return Err(picker_failed());
                            }
                            let path = url.path().ok_or_else(picker_failed)?.to_string();
                            if path.len() > 4096
                                || !Path::new(&path).is_absolute()
                                || path.chars().any(char::is_control)
                                || !Path::new(&path)
                                    .extension()
                                    .and_then(|value| value.to_str())
                                    .is_some_and(|value| value.eq_ignore_ascii_case("app"))
                            {
                                return Err(picker_failed());
                            }
                            Ok(Some(path))
                        })
                    } else {
                        Err(picker_failed())
                    };
                    drop(guard);
                    let _ = send.try_send(result);
                });
                panel.beginSheetModalForWindow_completionHandler(parent, &handler);
                Ok::<(), Fault>(())
            })();
            if let Err(error) = result {
                let _ = failed.try_send(Err(error));
            }
        })
        .map_err(|_| picker_failed())?;
    receive.recv().await.ok_or_else(picker_failed)?
}

fn picker_failed() -> Fault {
    Fault::new("TargetPicker", "Application selection could not complete")
}
