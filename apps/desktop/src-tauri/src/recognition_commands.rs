use super::{Backend, background};
use mado_mata_desktop::application::{
    AuthoringRef, RecognitionCopy, RecognitionSaved, RecognitionTrial, RecognitionView,
};
use mado_runtime_comparison::model::Fault;
use mado_runtime_comparison::recognition::{RecognitionDocument, SnippetKind};
use std::path::Path;
use tauri::{Emitter, Manager};

#[tauri::command]
pub async fn native_discover(
    owner: AuthoringRef,
    revision: String,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeSelectionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.native_discover(&owner, &revision, None)).await
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
    cache_original: bool,
    app: tauri::AppHandle,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeCaptureResult, Fault> {
    let application = state.bootstrap.application()?;
    // Cache availability cannot fail image acceptance. Never resolve it when OFF.
    let cache = cache_original.then(|| crate::cache_commands::cache(&app));
    background(move || {
        application.native_capture(
            &owner,
            &revision,
            generation,
            capture_id.as_deref(),
            document_revision,
            cache,
        )
    })
    .await
}

#[tauri::command]
pub async fn native_release_selection(
    owner: AuthoringRef,
    revision: String,
    state: tauri::State<'_, Backend>,
) -> Result<mado_mata_desktop::application::NativeSelectionView, Fault> {
    let application = state.bootstrap.application()?;
    background(move || application.native_release_selection(&owner, &revision)).await
}

pub const PREVIEW_WINDOW: &str = "recognition-preview";

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
    if window.label() != PREVIEW_WINDOW {
        return Err(Fault::new(
            "RecognitionPreview",
            "Only the preview window may receive image pixels",
        ));
    }
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
    state: tauri::State<'_, Backend>,
) -> Result<RecognitionSaved, Fault> {
    let application = state.bootstrap.application()?;
    background(move || {
        application.recognition_save(&owner, &revision, document_revision, &crop_ids, &capture_id)
    })
    .await
}

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
    let worker = application.clone();
    let expected_owner = owner.clone();
    let expected_revision = revision.clone();
    let copied = background(move || {
        worker.recognition_copy(
            &expected_owner,
            &expected_revision,
            document_revision,
            &definition_ids,
            mode,
            &capture_id,
        )
    })
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

#[tauri::command]
pub async fn recognition_open_preview(
    owner: AuthoringRef,
    revision: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    let application = state.bootstrap.application()?;
    let expected_owner = owner.clone();
    background(move || application.recognition_view(&owner, &revision)).await?;
    {
        let mut held = state
            .preview_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.as_ref().is_some_and(|held| *held != expected_owner) {
            return Err(Fault::new(
                "StaleRecognition",
                "Close the previous owner's preview before opening this one",
            ));
        }
        *held = Some(expected_owner);
    }
    let opened = (|| {
        if let Some(window) = app.get_webview_window(PREVIEW_WINDOW) {
            window.show().map_err(preview_fault)?;
            window.set_focus().map_err(preview_fault)?;
            app.emit_to("main", "recognition-preview-ready", ())
                .map_err(preview_fault)?;
            return Ok(());
        }
        tauri::WebviewWindowBuilder::new(
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
        Ok(())
    })();
    if opened.is_err() && app.get_webview_window(PREVIEW_WINDOW).is_none() {
        *state
            .preview_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
    opened
}

#[tauri::command]
pub fn recognition_close_preview(
    owner: AuthoringRef,
    app: tauri::AppHandle,
    state: tauri::State<'_, Backend>,
) -> Result<(), Fault> {
    if state
        .preview_owner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        != Some(&owner)
    {
        return Err(Fault::new(
            "StaleRecognition",
            "Preview belongs to another Edit owner",
        ));
    }
    if let Some(window) = app.get_webview_window(PREVIEW_WINDOW) {
        window.close().map_err(preview_fault)?;
    }
    Ok(())
}

fn preview_fault(error: tauri::Error) -> Fault {
    Fault::new("RecognitionPreview", error.to_string())
}
