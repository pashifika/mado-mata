use block2::RcBlock;
use mado_mata_desktop::application::{RecognitionPickerGuard, TargetPickerGuard};
use mado_runtime_comparison::model::Fault;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSModalResponse, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel, NSWindow,
};
use objc2_foundation::NSArray;
use objc2_uniform_type_identifiers::{UTTypeApplicationBundle, UTTypePNG};
use std::cell::RefCell;
use std::path::Path;

#[derive(Clone, Copy)]
enum SelectionKind {
    Application,
    Png,
    Directory,
}

pub async fn choose(
    window: tauri::WebviewWindow,
    guard: TargetPickerGuard,
) -> Result<Option<String>, Fault> {
    choose_path(window, SelectionKind::Application, move || guard.validate()).await
}

pub async fn choose_png(
    window: tauri::WebviewWindow,
    guard: RecognitionPickerGuard,
) -> Result<Option<String>, Fault> {
    choose_path(window, SelectionKind::Png, move || guard.validate()).await
}

pub async fn choose_directory(window: tauri::WebviewWindow) -> Result<Option<String>, Fault> {
    choose_path(window, SelectionKind::Directory, || Ok(())).await
}

pub async fn choose_package(
    window: tauri::WebviewWindow,
    guard: mado_mata_desktop::application::DistributionPickerGuard,
    exporting: bool,
    suggested_name: Option<String>,
) -> Result<Option<String>, Fault> {
    use objc2_app_kit::NSSavePanel;
    use objc2_foundation::NSString;
    use objc2_uniform_type_identifiers::UTType;
    let failed = || Fault::new("PackagePicker", "Package file selection could not complete");
    let (send, mut receive) = tauri::async_runtime::channel(1);
    let native_window = window.clone();
    window.run_on_main_thread(move || {
        let failure = send.clone();
        let result = (|| {
            guard.validate()?;
            let mtm = MainThreadMarker::new().ok_or_else(failed)?;
            let pointer = native_window.ns_window().map_err(|_| failed())?;
            if pointer.is_null() { return Err(failed()); }
            // SAFETY: Tauri retains this non-null NSWindow through the main-thread sheet call.
            #[expect(unsafe_code, reason = "audited Tauri NSWindow borrow on the main thread")]
            let parent = unsafe { &*pointer.cast::<NSWindow>() };
            let panel = if exporting {
                NSSavePanel::savePanel(mtm)
            } else {
                let panel = NSOpenPanel::openPanel(mtm);
                panel.setCanChooseFiles(true);
                panel.setCanChooseDirectories(false);
                panel.setAllowsMultipleSelection(false);
                panel.setCanResolveUbiquitousConflicts(false);
                panel.setCanDownloadUbiquitousContents(false);
                panel.into_super()
            };
            let kind = UTType::typeWithFilenameExtension(&NSString::from_str("mmpkg")).ok_or_else(failed)?;
            panel.setAllowedContentTypes(&NSArray::from_slice(&[&*kind]));
            panel.setAllowsOtherFileTypes(false);
            panel.setCanCreateDirectories(false);
            panel.setTreatsFilePackagesAsDirectories(false);
            if exporting {
                panel.setExtensionHidden(false);
                // Set the filename after type restrictions to avoid a repeated extension.
                if let Some(name) = suggested_name {
                    panel.setNameFieldStringValue(&NSString::from_str(&name));
                }
            }
            let selected = panel.clone();
            let completion = RefCell::new(Some((guard, send)));
            let handler = RcBlock::new(move |response: NSModalResponse| {
                selected.orderOut(None);
                let Some((guard, send)) = completion.borrow_mut().take() else { return };
                let result = if response == NSModalResponseCancel {
                    Ok(None)
                } else if response == NSModalResponseOK {
                    guard.validate().and_then(|()| {
                        let url = selected.URL().ok_or_else(failed)?;
                        if !url.isFileURL() { return Err(failed()); }
                        let path = url.path().ok_or_else(failed)?.to_string();
                        if path.len() > 4096 || !Path::new(&path).is_absolute() || path.chars().any(char::is_control)
                            || !Path::new(&path).extension().and_then(|part| part.to_str()).is_some_and(|part| part.eq_ignore_ascii_case("mmpkg")) {
                            return Err(failed());
                        }
                        Ok(Some(path))
                    })
                } else { Err(failed()) };
                let _ = send.try_send(result);
            });
            panel.beginSheetModalForWindow_completionHandler(parent, &handler);
            Ok::<(), Fault>(())
        })();
        if let Err(error) = result { let _ = failure.try_send(Err(error)); }
    }).map_err(|_| failed())?;
    receive.recv().await.ok_or_else(failed)?
}

async fn choose_path(
    window: tauri::WebviewWindow,
    kind: SelectionKind,
    validate: impl FnOnce() -> Result<(), Fault> + Send + 'static,
) -> Result<Option<String>, Fault> {
    let picker_failed = move || selection_failed(kind);
    let directory = matches!(kind, SelectionKind::Directory);
    let png = matches!(kind, SelectionKind::Png);
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
                // SAFETY: ns_window returned a non-null NSWindow owned by Tauri; native_window
                // stays live through the sheet call, and this closure runs on the main thread.
                #[expect(
                    unsafe_code,
                    reason = "audited Tauri NSWindow borrow on the main thread"
                )]
                let parent = unsafe { &*pointer.cast::<NSWindow>() };
                let panel = NSOpenPanel::openPanel(mtm);
                panel.setCanChooseFiles(!directory);
                panel.setCanChooseDirectories(directory);
                panel.setAllowsMultipleSelection(false);
                panel.setTreatsFilePackagesAsDirectories(false);
                panel.setCanResolveUbiquitousConflicts(false);
                panel.setCanDownloadUbiquitousContents(false);
                if directory {
                    panel.setAllowedContentTypes(&NSArray::new());
                } else {
                    // SAFETY: Immutable process-lifetime system UTI, on the main thread.
                    #[expect(
                        unsafe_code,
                        reason = "audited immutable system content-type constant"
                    )]
                    let content_type = unsafe {
                        if png {
                            UTTypePNG
                        } else {
                            UTTypeApplicationBundle
                        }
                    };
                    panel.setAllowedContentTypes(&NSArray::from_slice(&[content_type]));
                }
                let selected_panel = panel.clone();
                let completion = RefCell::new(Some((validate, send)));
                let handler = RcBlock::new(move |response: NSModalResponse| {
                    // Dismiss before releasing admission, so a new run never starts behind the sheet.
                    selected_panel.orderOut(None);
                    let Some((validate, send)) = completion.borrow_mut().take() else {
                        return;
                    };
                    let result = if response == NSModalResponseCancel {
                        Ok(None)
                    } else if response == NSModalResponseOK {
                        validate().and_then(|()| {
                            let url = selected_panel.URL().ok_or_else(picker_failed)?;
                            if !url.isFileURL() {
                                return Err(picker_failed());
                            }
                            let path = url.path().ok_or_else(picker_failed)?.to_string();
                            if path.len() > 4096
                                || !Path::new(&path).is_absolute()
                                || path.chars().any(char::is_control)
                                || (!directory
                                    && !Path::new(&path)
                                        .extension()
                                        .and_then(|value| value.to_str())
                                        .is_some_and(|value| {
                                            value.eq_ignore_ascii_case(if png {
                                                "png"
                                            } else {
                                                "app"
                                            })
                                        }))
                            {
                                return Err(picker_failed());
                            }
                            Ok(Some(path))
                        })
                    } else {
                        Err(picker_failed())
                    };
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

fn selection_failed(kind: SelectionKind) -> Fault {
    match kind {
        SelectionKind::Png => Fault::new(
            "RecognitionPicker",
            "Saved PNG selection could not complete",
        ),
        SelectionKind::Application => {
            Fault::new("TargetPicker", "Application selection could not complete")
        }
        SelectionKind::Directory => Fault::new(
            "OcrSetupPicker",
            "Installation folder selection could not complete",
        ),
    }
}
