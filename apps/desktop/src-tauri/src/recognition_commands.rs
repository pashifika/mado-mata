use super::{Backend, background};
use mado_mata_desktop::application::{
    AuthoringRef, RecognitionCopy, RecognitionPixel, RecognitionSaved, RecognitionTrial,
    RecognitionView,
};
use mado_runtime_comparison::model::Fault;
use mado_runtime_comparison::recognition::{RecognitionDocument, SnippetKind};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tauri::{Emitter, Manager};

#[tauri::command]
pub async fn native_discover(
    owner: AuthoringRef,
    revision: String,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeSelectionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.native_discover(&owner, &revision)).await
}

#[tauri::command]
pub async fn native_start(
    owner: AuthoringRef,
    revision: String,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeSelectionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.native_start(&owner, &revision)).await
}

#[tauri::command]
pub async fn native_reset_target(
    owner: AuthoringRef,
    revision: String,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeSelectionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.native_reset_target(&owner, &revision)).await
}

#[tauri::command]
pub async fn native_select_candidate(
    owner: AuthoringRef,
    revision: String,
    generation: u64,
    candidate_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeSelectionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.native_select_candidate(&owner, &revision, generation, &candidate_id)
    })
    .await
}

#[tauri::command]
pub async fn native_capture(
    owner: AuthoringRef,
    revision: String,
    generation: u64,
    capture_id: Option<String>,
    document_revision: u64,
    new_capture: bool,
    crop_ids: Vec<String>,
    crop_sources: std::collections::BTreeMap<String, String>,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeCaptureResult, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        // The host setting is sampled at command admission, never supplied by the renderer.
        // Cache availability cannot fail image acceptance. Never resolve it when OFF.
        let cache = application
            .settings()?
            .capture_cache_enabled
            .then(|| application.capture_cache());
        application.native_capture(
            &owner,
            &revision,
            generation,
            capture_id.as_deref(),
            document_revision,
            new_capture,
            &crop_ids,
            &crop_sources,
            cache,
        )
    })
    .await
}

#[tauri::command]
pub async fn native_release_selection(
    owner: AuthoringRef,
    revision: String,
    generation: u64,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeSelectionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.native_release_selection(&owner, &revision, generation)).await
}

pub const PREVIEW_WINDOW: &str = "recognition-preview";

fn require_preview_window(label: &str) -> Result<(), Fault> {
    if label != PREVIEW_WINDOW {
        return Err(Fault::new(
            "RecognitionPreview",
            "Only the preview window may receive image pixels",
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn recognition_view(
    owner: AuthoringRef,
    revision: String,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.recognition_view(&owner, &revision)).await
}

#[tauri::command]
pub async fn recognition_capabilities(
    owner: AuthoringRef,
    revision: String,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.recognition_capabilities(&owner, &revision)).await
}

#[tauri::command]
pub async fn recognition_pick(
    owner: AuthoringRef,
    revision: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Option<String>, Fault> {
    let application = state.bootstrap.application()?;
    let guard = background(move || application.begin_recognition_picker(&owner, &revision)).await?;
    #[cfg(target_os = "macos")]
    {
        crate::picker::choose_png(window, guard).await
    }
    #[cfg(windows)]
    {
        crate::windows_shell::choose_png(window, guard).await
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (window, guard);
        Err(Fault::new(
            "RecognitionPlatform",
            "PNG selection requires macOS or Windows",
        ))
    }
}

#[tauri::command]
pub async fn recognition_load(
    owner: AuthoringRef,
    revision: String,
    path: String,
    capture_id: Option<String>,
    document_revision: u64,
    new_capture: bool,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_load(
            &owner,
            &revision,
            Path::new(&path),
            capture_id.as_deref(),
            document_revision,
            new_capture,
        )
    })
    .await
}

#[tauri::command]
pub async fn recognition_select(
    owner: AuthoringRef,
    revision: String,
    capture_id: Option<String>,
    document_revision: u64,
    selected_capture_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_select(
            &owner,
            &revision,
            capture_id.as_deref(),
            document_revision,
            &selected_capture_id,
        )
    })
    .await
}

#[tauri::command]
pub async fn recognition_preview(
    owner: AuthoringRef,
    frame_id: String,
    capture_id: String,
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, Backend>,
) -> Result<tauri::ipc::Response, Fault> {
    require_preview_window(window.label())?;
    let application = state.bootstrap.application()?;
    let worker = application.clone();
    let expected = owner.clone();
    let bytes =
        background(move || worker.recognition_preview(&expected, &frame_id, &capture_id)).await?;
    if app.get_webview_window(PREVIEW_WINDOW).is_none() {
        application.recognition_release_preview(&owner);
        return Err(Fault::new(
            "StaleRecognition",
            "Preview window closed during image preparation",
        ));
    }
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn recognition_pixel(
    owner: AuthoringRef,
    capture_id: String,
    frame_id: String,
    frame_revision: u64,
    x: u32,
    y: u32,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionPixel, Fault> {
    require_preview_window(window.label())?;
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_pixel(&owner, &capture_id, &frame_id, frame_revision, x, y)
    })
    .await
}

#[tauri::command]
pub async fn recognition_update(
    owner: AuthoringRef,
    revision: String,
    document: RecognitionDocument,
    frame_id: Option<String>,
    document_revision: u64,
    capture_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_update(
            &owner,
            &revision,
            document,
            frame_id.as_deref(),
            document_revision,
            &capture_id,
        )
    })
    .await
}

#[tauri::command]
pub async fn recognition_confirm(
    owner: AuthoringRef,
    revision: String,
    frame_id: String,
    document_revision: u64,
    capture_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_confirm(
            &owner,
            &revision,
            &frame_id,
            document_revision,
            &capture_id,
        )
    })
    .await
}

#[tauri::command]
pub async fn recognition_discard(
    owner: AuthoringRef,
    revision: String,
    capture_id: Option<String>,
    document_revision: u64,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_discard(&owner, &revision, capture_id.as_deref(), document_revision)
    })
    .await
}

#[tauri::command]
pub async fn recognition_trial(
    owner: AuthoringRef,
    revision: String,
    frame_id: Option<String>,
    document_revision: u64,
    selected_ids: Vec<String>,
    sample_id: Option<String>,
    capture_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionTrial, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_trial(
            &owner,
            &revision,
            frame_id.as_deref(),
            document_revision,
            &selected_ids,
            sample_id.as_deref(),
            &capture_id,
        )
    })
    .await
}

#[tauri::command]
pub async fn recognition_save(
    owner: AuthoringRef,
    revision: String,
    document_revision: u64,
    crop_ids: Vec<String>,
    capture_id: String,
    crop_sources: std::collections::BTreeMap<String, String>,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionSaved, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_save(
            &owner,
            &revision,
            document_revision,
            &crop_ids,
            &capture_id,
            &crop_sources,
        )
    })
    .await
}

/// The one source generation path for an explicit capture, purpose and ordered
/// definitions. It never reads UI selection or touches the clipboard.
async fn generate(
    application: Arc<mado_mata_desktop::application::Application>,
    owner: AuthoringRef,
    revision: String,
    document_revision: u64,
    definition_ids: Vec<String>,
    mode: SnippetKind,
    capture_id: String,
) -> Result<RecognitionCopy, Fault> {
    background(move || {
        application.recognition_copy(
            &owner,
            &revision,
            document_revision,
            &definition_ids,
            mode,
            &capture_id,
        )
    })
    .await
}

/// Agent retrieval: the same generation as Copy without clipboard publication.
#[tauri::command]
pub async fn recognition_generate(
    owner: AuthoringRef,
    revision: String,
    document_revision: u64,
    definition_ids: Vec<String>,
    mode: SnippetKind,
    capture_id: String,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionCopy, Fault> {
    let application = state.bootstrap.application()?;
    generate(
        application,
        owner,
        revision,
        document_revision,
        definition_ids,
        mode,
        capture_id,
    )
    .await
}

/// UI Copy: generates, then alone publishes the complete source to the clipboard.
#[tauri::command]
pub async fn recognition_copy(
    owner: AuthoringRef,
    revision: String,
    document_revision: u64,
    definition_ids: Vec<String>,
    mode: SnippetKind,
    capture_id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionCopy, Fault> {
    let application = state.bootstrap.application()?;
    let copied = generate(
        application.clone(),
        owner.clone(),
        revision.clone(),
        document_revision,
        definition_ids,
        mode,
        capture_id,
    )
    .await?;
    publish_clipboard(app, application, owner, revision, copied).await
}

#[cfg(target_os = "macos")]
async fn publish_clipboard(
    app: tauri::AppHandle,
    application: std::sync::Arc<mado_mata_desktop::application::Application>,
    owner: AuthoringRef,
    revision: String,
    copied: RecognitionCopy,
) -> Result<RecognitionCopy, Fault> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    let (send, mut receive) = tauri::async_runtime::channel(1);
    app.run_on_main_thread(move || {
        let result = application.recognition_publish_copy(&owner, &revision, copied, |source| {
            let pasteboard = NSPasteboard::generalPasteboard();
            let text = NSString::from_str(source);
            // SAFETY: AppKit exports this immutable process-lifetime string type.
            #[expect(
                unsafe_code,
                reason = "audited immutable AppKit pasteboard type constant"
            )]
            let text_type = unsafe { NSPasteboardTypeString };
            pasteboard.clearContents();
            if pasteboard.setString_forType(&text, text_type) {
                Ok(())
            } else {
                Err(Fault::new(
                    "RecognitionClipboard",
                    "Clipboard publication failed; package source was not changed",
                ))
            }
        });
        let _ = send.try_send(result);
    })
    .map_err(|_| {
        Fault::new(
            "RecognitionClipboard",
            "Clipboard publication could not be scheduled",
        )
    })?;
    receive.recv().await.ok_or_else(|| {
        Fault::new(
            "RecognitionClipboard",
            "Clipboard publication did not complete",
        )
    })?
}

#[cfg(windows)]
async fn publish_clipboard(
    _app: tauri::AppHandle,
    application: std::sync::Arc<mado_mata_desktop::application::Application>,
    owner: AuthoringRef,
    revision: String,
    copied: RecognitionCopy,
) -> Result<RecognitionCopy, Fault> {
    background(move || {
        application.recognition_publish_copy(
            &owner,
            &revision,
            copied,
            crate::windows_shell::copy_text,
        )
    })
    .await
}

#[cfg(not(any(target_os = "macos", windows)))]
async fn publish_clipboard(
    _app: tauri::AppHandle,
    _application: std::sync::Arc<mado_mata_desktop::application::Application>,
    _owner: AuthoringRef,
    _revision: String,
    _copied: RecognitionCopy,
) -> Result<RecognitionCopy, Fault> {
    Err(Fault::new(
        "RecognitionPlatform",
        "Native clipboard publication requires macOS or Windows",
    ))
}

pub(super) struct PreviewSession {
    owner: AuthoringRef,
    closing: AtomicBool,
    settled: AtomicBool,
    fence: std::sync::Mutex<(String, u64)>,
}

#[tauri::command]
pub async fn recognition_open_preview(
    owner: AuthoringRef,
    revision: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    let application = state.bootstrap.application()?;
    let expected_owner = owner.clone();
    let session = {
        let mut held = state
            .preview_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = held.as_ref() {
            if session.owner != owner || session.closing.load(Ordering::Acquire) {
                return Err(Fault::new(
                    "StaleRecognition",
                    "Wait for the previous preview to close",
                ));
            }
            session.clone()
        } else {
            let session = Arc::new(PreviewSession {
                owner: owner.clone(),
                closing: AtomicBool::new(false),
                settled: AtomicBool::new(false),
                fence: std::sync::Mutex::new((revision.clone(), 0)),
            });
            *held = Some(session.clone());
            session
        }
    };
    let worker = application.clone();
    let admission = app.clone();
    let admitted_session = session.clone();
    let opened = async {
        let fence = background(move || {
            worker.recognition_view(&owner, &revision)?;
            let backend = admission.state::<Backend>();
            let held = backend
                .preview_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !held
                .as_ref()
                .is_some_and(|held| Arc::ptr_eq(held, &admitted_session))
                || admitted_session.closing.load(Ordering::Acquire)
            {
                return Err(Fault::new(
                    "StaleRecognition",
                    "Preview ownership changed while opening",
                ));
            }
            worker.native_open_preview(&owner, &revision)?;
            worker.native_preview_fence(&owner)
        })
        .await?;
        *session
            .fence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fence;
        let held = state
            .preview_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !held
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, &session))
        {
            return Err(Fault::new(
                "StaleRecognition",
                "Preview ownership changed while opening",
            ));
        }
        if session.closing.load(Ordering::Acquire) {
            return Err(Fault::new(
                "NativeCaptureBusy",
                "Preview cleanup is in progress",
            ));
        }
        if let Some(window) = app.get_webview_window(PREVIEW_WINDOW) {
            window.show().map_err(preview_fault)?;
            window.set_focus().map_err(preview_fault)?;
            app.emit_to("main", "recognition-preview-ready", ())
                .map_err(preview_fault)?;
            return Ok(());
        }
        let window = tauri::WebviewWindowBuilder::new(
            &app,
            PREVIEW_WINDOW,
            tauri::WebviewUrl::App("index.html?surface=recognition-preview".into()),
        )
        .devtools(false)
        .title("MadoMata — Recognition preview")
        .inner_size(1100.0, 760.0)
        .min_inner_size(480.0, 320.0)
        .build()
        .map_err(preview_fault)?;
        let events = app.clone();
        let held = session.clone();
        let window_application = application.clone();
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let app = events.clone();
                let session = held.clone();
                tauri::async_runtime::spawn(async move {
                    let backend = app.state::<Backend>();
                    let result = match backend.bootstrap.running_application() {
                        Ok(application) => match application.native_preview_fence(&session.owner) {
                            Ok((revision, generation)) => {
                                close_preview(&app, &session, &revision, generation).await
                            }
                            Err(_) => {
                                let fence = session
                                    .fence
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .clone();
                                close_preview(&app, &session, &fence.0, fence.1).await
                            }
                        },
                        Err(error) => Err(error),
                    };
                    if let Err(error) = result {
                        preview_close_failed(&app, &session, &error);
                    }
                });
            }
            tauri::WindowEvent::Destroyed => preview_destroyed(&events, &held, &window_application),
            _ => {}
        });
        Ok(())
    }
    .await;
    if opened.is_err() && app.get_webview_window(PREVIEW_WINDOW).is_none() {
        let fence = {
            let held = state
                .preview_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !held
                .as_ref()
                .is_some_and(|held| Arc::ptr_eq(held, &session))
            {
                return opened;
            }
            session.closing.store(true, Ordering::Release);
            application.native_begin_preview_close(&expected_owner).ok()
        };
        if let Some((_, generation)) = fence {
            let cleanup = application.clone();
            let owner = expected_owner.clone();
            background(move || cleanup.native_finish_preview_close(&owner, generation)).await?;
        }
        application.recognition_release_preview(&expected_owner);
        clear_preview_session(&app, &session);
    }
    opened
}

fn clear_preview_session(app: &tauri::AppHandle, session: &Arc<PreviewSession>) {
    session.settled.store(true, Ordering::Release);
    let backend = app.state::<Backend>();
    let mut held = backend
        .preview_owner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.as_ref().is_some_and(|held| Arc::ptr_eq(held, session)) {
        *held = None;
        let fence = session
            .fence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = app.emit_to(
            "main",
            "recognition-preview-closed",
            serde_json::json!({
                "owner": session.owner, "revision": fence.0, "generation": fence.1,
            }),
        );
    }
}

fn preview_close_failed(app: &tauri::AppHandle, session: &Arc<PreviewSession>, error: &Fault) {
    let fence = session
        .fence
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let payload = serde_json::json!({
        "owner": session.owner, "revision": fence.0, "generation": fence.1, "error": error,
    });
    let _ = app.emit_to(PREVIEW_WINDOW, "recognition-preview-close-failed", &payload);
    let _ = app.emit_to("main", "recognition-preview-close-failed", &payload);
}

fn preview_destroyed(
    app: &tauri::AppHandle,
    session: &Arc<PreviewSession>,
    application: &Arc<mado_mata_desktop::application::Application>,
) {
    // Never take the shell mutex on the UI callback: an opening window may be
    // waiting for a UI dispatch while holding it. Completed old instances do not
    // touch current native admission or discover a replacement generation.
    session.closing.store(true, Ordering::Release);
    let fence = if session.settled.load(Ordering::Acquire) {
        None
    } else {
        let fence = application.native_begin_preview_close(&session.owner);
        if let Ok(fence) = &fence {
            *session
                .fence
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = fence.clone();
        }
        application.recognition_release_preview(&session.owner);
        Some(fence)
    };
    let app = app.clone();
    let session = session.clone();
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        let owner = session.owner.clone();
        let worker = application.clone();
        let result = match fence {
            Some(Ok((_, generation))) => {
                background(move || worker.native_finish_preview_close(&owner, generation))
                    .await
                    .map(Some)
            }
            Some(Err(error)) => Err(error),
            None => Ok(None),
        };
        match result {
            Ok(Some(selection)) => {
                *session
                    .fence
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    (selection.revision, selection.selection_generation);
            }
            Err(error) if application.native_preview_fence(&session.owner).is_ok() => {
                preview_close_failed(&app, &session, &error);
                return;
            }
            _ => {}
        }
        clear_preview_session(&app, &session);
    });
}

async fn close_preview(
    app: &tauri::AppHandle,
    session: &Arc<PreviewSession>,
    revision: &str,
    generation: u64,
) -> Result<(), Fault> {
    {
        let backend = app.state::<Backend>();
        let held = backend
            .preview_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !held.as_ref().is_some_and(|held| Arc::ptr_eq(held, session)) {
            return Err(Fault::new(
                "StaleRecognition",
                "Preview belongs to another Edit owner",
            ));
        }
        if session.closing.swap(true, Ordering::AcqRel) {
            return Err(Fault::new(
                "NativeCaptureBusy",
                "Preview cleanup is already in progress",
            ));
        }
    }
    *session
        .fence
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (revision.into(), generation);
    let application = match app.state::<Backend>().bootstrap.application() {
        Ok(application) => application,
        Err(error) => {
            session.closing.store(false, Ordering::Release);
            return Err(error);
        }
    };
    let worker = application.clone();
    let owner = session.owner.clone();
    let revision = revision.to_owned();
    let result =
        background(move || worker.native_close_preview(&owner, &revision, generation)).await;
    if let Err(error) = result {
        // An exited Edit owner has already settled its worker. Never substitute
        // the new owner's generation when closing that old preview surface.
        if application.native_preview_fence(&session.owner).is_ok() {
            session.closing.store(false, Ordering::Release);
            return Err(error);
        }
    }
    application.recognition_release_preview(&session.owner);
    session.settled.store(true, Ordering::Release);
    if let Some(window) = app.get_webview_window(PREVIEW_WINDOW) {
        if let Err(error) = window.destroy() {
            session.settled.store(false, Ordering::Release);
            session.closing.store(false, Ordering::Release);
            return Err(preview_fault(error));
        }
    }
    clear_preview_session(app, session);
    Ok(())
}

#[tauri::command]
pub async fn recognition_close_preview(
    owner: AuthoringRef,
    revision: String,
    generation: u64,
    app: tauri::AppHandle,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    let session = state
        .preview_owner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|session| session.owner == owner)
        .cloned()
        .ok_or_else(|| Fault::new("StaleRecognition", "Preview belongs to another Edit owner"))?;
    close_preview(&app, &session, &revision, generation).await
}

fn preview_fault(error: tauri::Error) -> Fault {
    Fault::new("RecognitionPreview", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn pixel_commands_require_the_exact_preview_window_label() {
        require_preview_window(PREVIEW_WINDOW).unwrap();
        for label in ["main", "", "recognition-preview-other"] {
            assert_eq!(
                require_preview_window(label).unwrap_err().category,
                "RecognitionPreview",
            );
        }
    }

    #[test]
    fn pixel_command_coordinates_reject_invalid_transport_numbers_without_coercion() {
        // Both coordinate arguments use u32. Tauri's CommandItem forwards JSON
        // values to this same deserializer; no native WebView is needed here.
        for raw in [
            "-1",
            "-0",
            "0.5",
            "1.0",
            "4294967296",
            "1e100",
            "null",
            "\"1\"",
            "true",
            "false",
            "[]",
            "{}",
        ] {
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert!(
                u32::deserialize(&value).is_err(),
                "a pixel coordinate must reject {raw} rather than floor, wrap or default",
            );
        }
        for expected in [0u32, 1, u32::MAX] {
            let value = serde_json::json!(expected);
            assert_eq!(u32::deserialize(&value).unwrap(), expected);
        }
    }

    #[test]
    fn pixel_command_frame_revision_rejects_invalid_transport_numbers() {
        for raw in [
            "-1",
            "-0",
            "0.5",
            "1.0",
            "18446744073709551616",
            "1e100",
            "null",
            "\"1\"",
            "true",
            "false",
            "[]",
            "{}",
        ] {
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert!(
                u64::deserialize(&value).is_err(),
                "frameRevision must reject {raw}",
            );
        }
        for expected in [0u64, 1, u64::MAX] {
            let value = serde_json::json!(expected);
            assert_eq!(u64::deserialize(&value).unwrap(), expected);
        }
    }
}
