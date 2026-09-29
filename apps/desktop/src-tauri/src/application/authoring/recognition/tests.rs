use super::*;
use crate::application::test_support::{Fixture, view_ref};
use mado_runtime_comparison::recognition::{
    NormalizedRect, RecognitionDefinition, TemplateRights, TemplateSettings,
};
use std::fs;

struct Editor {
    fixture: Fixture,
    view: super::super::AuthoringView,
    source: std::path::PathBuf,
}

impl Editor {
    fn new() -> Self {
        Self::with_fixture(Fixture::new())
    }

    fn with_fixture(fixture: Fixture) -> Self {
        let workspace = fixture
            .application
            .create_workspace("recognition", "Recognition")
            .unwrap();
        let view = fixture
            .application
            .authoring_create(&view_ref(&workspace), "recognition-package")
            .unwrap();
        let source = fixture.root.join("selected.png");
        let mut pixels = Vec::with_capacity(32 * 24 * 4);
        for y in 0..24u8 {
            for x in 0..32u8 {
                pixels.extend_from_slice(&[x, y, 17, 255]);
            }
        }
        let image = DecodedImage::from_rgba(32, 24, pixels).unwrap();
        let png = images::encode_crop(&image, [0, 0, 32, 24]).unwrap();
        fs::write(&source, png.as_bytes()).unwrap();
        Self {
            fixture,
            view,
            source,
        }
    }
    fn app(&self) -> &Arc<Application> {
        &self.fixture.application
    }
    fn load(&self) -> RecognitionView {
        load_selected(
            self.app(),
            &self.view.owner,
            &self.view.revision,
            &self.source,
        )
        .unwrap()
    }
}

fn current_capture(app: &Application) -> String {
    lock(&app.workspaces)
        .authoring
        .as_ref()
        .unwrap()
        .recognition
        .capture_id
        .clone()
        .unwrap()
}

fn load_selected(
    app: &Application,
    owner: &AuthoringRef,
    revision: &str,
    path: &Path,
) -> Result<RecognitionView, Fault> {
    let view = app.recognition_view(owner, revision)?;
    app.recognition_load(
        owner,
        revision,
        path,
        view.capture_id.as_deref(),
        view.document_revision,
        false,
    )
}

/// Stands in for the fixed engine child's reported grouped-request bound; these
/// portable tests never start the engine.
fn grant_capability(app: &Application, maximum: usize) {
    lock(&app.workspaces)
        .authoring
        .as_mut()
        .unwrap()
        .recognition
        .max_ocr_zones = Some(maximum);
}

/// Retains a settled trial observing `zones`, shaped like the runner's clean envelope.
/// Tests inject it rather than granting native OCR authority.
fn retain_observed_trial(
    app: &Application,
    owner: &AuthoringRef,
    revision: &str,
    view: &RecognitionView,
    zones: &[&str],
) {
    let frame = view.frame.as_ref().unwrap();
    let controller = json!({
        "run": "observed-trial", "operation": "recognition_trial", "state": "terminal", "error": null,
        "result": {"version": 1, "operation": "recognition_trial", "primary": null,
            "cleanup": {"clean": true}, "child_reaped": true, "forced": false, "exit_code": 0,
            "result": {"kind": "ocr", "zones": zones.iter().map(|id| {
                json!({"id": id, "regions": [{"text": "trial observation"}]})
            }).collect::<Vec<_>>()}}
    });
    lock(&app.workspaces)
        .authoring
        .as_mut()
        .unwrap()
        .recognition
        .trial = Some(RecognitionTrial {
        owner: owner.clone(),
        revision: revision.to_owned(),
        capture_id: view.capture_id.clone().unwrap(),
        document_revision: view.document_revision,
        frame_id: Some(frame.id.clone()),
        frame_revision: frame.revision,
        configuration_revision: view.configuration_revision.clone(),
        sample_id: None,
        stale: false,
        controller: Arc::new(controller),
    });
}

fn zone(id: &str) -> RecognitionDefinition {
    RecognitionDefinition {
        id: id.into(),
        name: id.into(),
        revision: 1,
        kind: RecognitionKind::Ocr,
        region: NormalizedRect {
            u0: 0.25,
            v0: 0.25,
            u1: 0.75,
            v1: 0.75,
        },
        expected: Some("Script-owned query text".into()),
        template: None,
        saved: None,
    }
}

#[test]
fn cropped_save_reopens_without_original_frame_and_copy_does_not_edit_source() {
    let editor = Editor::new();
    let app = editor.app();
    let loaded = editor.load();
    let frame_id = loaded.frame.as_ref().unwrap().id.clone();
    let mut document = loaded.document.unwrap();
    document.basis.content = PixelRect {
        x: 2,
        y: 3,
        width: 20,
        height: 16,
    };
    document.definitions = (0..9).map(|index| zone(&format!("zone{index}"))).collect();
    let updated = app
        .recognition_update(
            &editor.view.owner,
            &editor.view.revision,
            document,
            Some(&frame_id),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    assert!(!updated.frame.as_ref().unwrap().confirmed);
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &editor.view.revision,
            &frame_id,
            updated.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let source_path = Path::new(&editor.view.package_path).join("main.ts");
    let source_before = fs::read(&source_path).unwrap();
    grant_capability(app, 8);
    let checked: Vec<String> = (0..8).map(|index| format!("zone{index}")).collect();
    for (ids, mode) in [
        (Vec::new(), SnippetKind::GameContent),
        (checked, SnippetKind::OcrRecognize),
    ] {
        let copied = app
            .recognition_copy(
                &editor.view.owner,
                &editor.view.revision,
                confirmed.document_revision,
                &ids,
                mode,
                &current_capture(editor.app()),
            )
            .unwrap();
        assert!(!copied.verified);
    }
    assert_eq!(fs::read(&source_path).unwrap(), source_before);
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &editor.view.revision,
            confirmed.document_revision,
            &["zone0".into()],
            &current_capture(editor.app()),
            &BTreeMap::from([("zone0".into(), frame_id.clone())]),
        )
        .unwrap();
    let new_revision = saved.mutation.committed_revision;
    let saved_view = saved.recognition.unwrap();
    assert_eq!(saved_view.document.as_ref().unwrap().definitions.len(), 9);
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let crop = candidate
        .recognition_crop(&current_capture(app), "zone0")
        .unwrap()
        .unwrap();
    let decoded = images::decode_png(&crop, ImageKind::Crop).unwrap();
    assert_eq!((decoded.width, decoded.height), (10, 8));
    assert_eq!(&decoded.rgba[..4], &[7, 7, 17, 255]);
    assert_eq!(&decoded.rgba[decoded.rgba.len() - 4..], &[16, 14, 17, 255]);
    assert!(
        candidate
            .recognition_crop(&current_capture(app), "zone1")
            .unwrap()
            .is_none()
    );
    let manifest: Value = serde_json::from_slice(
        &fs::read(Path::new(&editor.view.package_path).join("package.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["assets"].as_object().unwrap().len(),
        2,
        "only metadata and the explicitly selected crop are declared"
    );
    for (requested_frame, requested_revision, sample) in [
        (
            Some(frame_id.as_str()),
            saved_view.document_revision,
            Some("zone0"),
        ),
        (None, saved_view.document_revision + 1, Some("zone0")),
        (None, saved_view.document_revision, None),
        (Some("stale-frame"), saved_view.document_revision, None),
    ] {
        let refusal = app
            .recognition_trial(
                &editor.view.owner,
                &new_revision,
                requested_frame,
                requested_revision,
                &["zone0".into()],
                sample,
                &current_capture(editor.app()),
            )
            .err()
            .unwrap();
        assert_eq!(refusal.category, "StaleRecognition");
    }
    let recheck_sample = |owner: &AuthoringRef, revision: &str, view: &RecognitionView| {
        let trial = app
            .recognition_trial(
                owner,
                revision,
                None,
                view.document_revision,
                &["zone0".into()],
                Some("zone0"),
                &current_capture(editor.app()),
            )
            .unwrap();
        assert!(
            !trial.stale,
            "an unrelated loaded frame must not stale a sample"
        );
        assert_eq!(trial.controller["error"]["category"], "EnvironmentUnset");
        assert_eq!(
            trial.controller["error"]["context"]["cleanup"],
            json!({"clean": true, "child_started": false})
        );
        // This portable regression reaches the actual OCR prerequisite boundary,
        // not inference: it must not launch a child without a configured environment.
    };
    recheck_sample(&editor.view.owner, &new_revision, &saved_view);
    app.recognition_preview(
        &editor.view.owner,
        &frame_id,
        &current_capture(editor.app()),
    )
    .unwrap();
    app.recognition_release_preview(&editor.view.owner);
    assert_eq!(
        app.recognition_view(&editor.view.owner, &new_revision)
            .unwrap()
            .document,
        saved_view.document
    );
    let workspace = app.authoring_exit(&editor.view.owner).unwrap();
    fs::remove_file(&editor.source).unwrap();
    let reopened = app
        .authoring_open(&view_ref(&workspace), Path::new(&editor.view.package_path))
        .unwrap();
    let restored = app
        .recognition_view(&reopened.owner, &reopened.revision)
        .unwrap();
    assert!(restored.frame.is_none());
    assert_eq!(restored.document, saved_view.document);
    recheck_sample(&reopened.owner, &reopened.revision, &restored);
    for (ids, mode) in [
        (Vec::new(), SnippetKind::GameContent),
        (vec!["zone0".to_owned()], SnippetKind::OcrRecognize),
    ] {
        let refusal = app
            .recognition_copy(
                &reopened.owner,
                &reopened.revision,
                restored.document_revision,
                &ids,
                mode,
                &current_capture(editor.app()),
            )
            .err()
            .expect("saved coordinates and a crop do not confirm a loaded frame");
        assert_eq!(refusal.category, "RecognitionInput");
    }
    assert!(
        app.recognition_view(&editor.view.owner, &new_revision)
            .is_err()
    );
}

#[test]
fn failed_replacement_has_no_frame_and_stale_metadata_cannot_overwrite_newer_edit() {
    let editor = Editor::new();
    let loaded = editor.load();
    let frame_id = loaded.frame.as_ref().unwrap().id.clone();
    let mut document = loaded.document.unwrap();
    document.definitions.push(zone("retained"));
    let current = editor
        .app()
        .recognition_update(
            &editor.view.owner,
            &editor.view.revision,
            document.clone(),
            Some(&frame_id),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    document.definitions[0].name = "stale overwrite".into();
    let refusal = editor
        .app()
        .recognition_update(
            &editor.view.owner,
            &editor.view.revision,
            document,
            Some(&frame_id),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .err()
        .unwrap();
    assert_eq!(refusal.category, "StaleRecognition");
    fs::write(&editor.source, b"not a PNG").unwrap();
    assert!(
        load_selected(
            editor.app(),
            &editor.view.owner,
            &editor.view.revision,
            &editor.source
        )
        .is_err()
    );
    let failed = editor
        .app()
        .recognition_view(&editor.view.owner, &editor.view.revision)
        .unwrap();
    assert!(failed.frame.is_none());
    assert_eq!(failed.document, current.document);
    assert!(
        editor
            .app()
            .recognition_copy(
                &editor.view.owner,
                &editor.view.revision,
                failed.document_revision,
                &["retained".into()],
                SnippetKind::OcrRecognize,
                &current_capture(editor.app()),
            )
            .is_err()
    );
    assert!(
        editor
            .app()
            .recognition_save(
                &editor.view.owner,
                &editor.view.revision,
                failed.document_revision,
                &["retained".into()],
                &current_capture(editor.app()),
                &BTreeMap::from([("retained".into(), frame_id.clone())]),
            )
            .is_err(),
        "a confirmed basis cannot substitute for crop pixels"
    );
    let saved = editor
        .app()
        .recognition_save(
            &editor.view.owner,
            &editor.view.revision,
            failed.document_revision,
            &[],
            &current_capture(editor.app()),
            &BTreeMap::new(),
        )
        .unwrap()
        .recognition
        .unwrap();
    assert_eq!(saved.saved_document, current.document);
    assert!(saved.frame.is_none());
    assert!(saved.basis_confirmed);
}

#[test]
fn scene_images_reuse_confirmed_setup_and_definitions_across_save_and_reopen() {
    let editor = Editor::new();
    let app = editor.app();
    let loaded = editor.load();
    let mut document = loaded.document.unwrap();
    document.basis.content = PixelRect {
        x: 2,
        y: 3,
        width: 20,
        height: 16,
    };
    document.definitions.push(zone("saved"));
    let updated = app
        .recognition_update(
            &editor.view.owner,
            &editor.view.revision,
            document,
            loaded.frame.as_ref().map(|frame| frame.id.as_str()),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &editor.view.revision,
            &updated.frame.as_ref().unwrap().id,
            updated.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &editor.view.revision,
            confirmed.document_revision,
            &[],
            &current_capture(editor.app()),
            &BTreeMap::new(),
        )
        .unwrap();
    let revision = saved.mutation.committed_revision;
    let saved_view = saved.recognition.unwrap();
    let saved_document = saved_view.document.clone().unwrap();
    let mut draft = saved_document.clone();
    draft.definitions.push(zone("unsaved"));
    app.recognition_update(
        &editor.view.owner,
        &revision,
        draft.clone(),
        saved_view.frame.as_ref().map(|frame| frame.id.as_str()),
        saved_view.document_revision,
        &current_capture(editor.app()),
    )
    .unwrap();
    let replacement = load_selected(app, &editor.view.owner, &revision, &editor.source).unwrap();
    assert!(replacement.frame.as_ref().unwrap().confirmed);
    assert_ne!(
        replacement.frame.as_ref().unwrap().id,
        loaded.frame.as_ref().unwrap().id
    );
    assert_eq!(replacement.document, Some(draft));
    assert_eq!(replacement.saved_document.as_ref(), Some(&saved_document));
    // Reopening restores saved setup, without retaining the previous frame or the
    // unsaved definition that the explicit Edit exit discards.
    let workspace = app.authoring_exit(&editor.view.owner).unwrap();
    let reopened = app
        .authoring_open(&view_ref(&workspace), Path::new(&editor.view.package_path))
        .unwrap();
    let next = load_selected(app, &reopened.owner, &reopened.revision, &editor.source).unwrap();
    assert!(next.frame.as_ref().unwrap().confirmed);
    assert_eq!(next.document, Some(saved_document.clone()));
    let setup = app
        .recognition_copy(
            &reopened.owner,
            &reopened.revision,
            next.document_revision,
            &[],
            SnippetKind::GameContent,
            &current_capture(editor.app()),
        )
        .unwrap();
    assert_eq!(setup.basis, saved_document.basis);
    assert!(
        !setup.verified,
        "setup geometry is never recognition evidence"
    );

    // Different dimensions remain a proposal until explicitly confirmed.
    let resized = DecodedImage::from_rgba(40, 24, vec![255; 40 * 24 * 4]).unwrap();
    let png = images::encode_crop(&resized, [0, 0, 40, 24]).unwrap();
    fs::write(&editor.source, png.as_bytes()).unwrap();
    let next = load_selected(app, &reopened.owner, &reopened.revision, &editor.source).unwrap();
    assert!(!next.frame.as_ref().unwrap().confirmed);
    assert_eq!(
        next.document.as_ref().unwrap().basis.content,
        saved_document.basis.content
    );
    assert_eq!(
        next.document.as_ref().unwrap().definitions,
        saved_document.definitions
    );
    assert!(
        app.recognition_copy(
            &reopened.owner,
            &reopened.revision,
            next.document_revision,
            &[],
            SnippetKind::GameContent,
            &current_capture(editor.app()),
        )
        .is_err()
    );
    assert!(
        app.recognition_confirm(
            &reopened.owner,
            &reopened.revision,
            &next.frame.as_ref().unwrap().id,
            next.document_revision,
            &current_capture(editor.app()),
        )
        .is_err()
    );
    let mut resized_basis = next.document.clone().unwrap();
    resized_basis.basis.frame_width = 40;
    let next = app
        .recognition_update(
            &reopened.owner,
            &reopened.revision,
            resized_basis,
            Some(&next.frame.as_ref().unwrap().id),
            next.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    assert!(
        app.recognition_save(
            &reopened.owner,
            &reopened.revision,
            next.document_revision,
            &[],
            &current_capture(editor.app()),
            &BTreeMap::new(),
        )
        .is_err()
    );
    assert!(load_selected(app, &reopened.owner, &reopened.revision, &editor.source).is_err());
    let retained = app
        .recognition_view(&reopened.owner, &reopened.revision)
        .unwrap();
    assert_eq!(
        retained.frame.as_ref().unwrap().id,
        next.frame.as_ref().unwrap().id
    );
    app.recognition_confirm(
        &reopened.owner,
        &reopened.revision,
        next.frame.as_ref().unwrap().id.as_str(),
        next.document_revision,
        &current_capture(editor.app()),
    )
    .unwrap();
    fs::write(&editor.source, b"not a PNG").unwrap();
    assert!(load_selected(app, &reopened.owner, &reopened.revision, &editor.source).is_err());
    assert!(
        app.recognition_view(&reopened.owner, &reopened.revision)
            .unwrap()
            .frame
            .is_none()
    );
    fs::write(&editor.source, png.as_bytes()).unwrap();
    let restored = load_selected(app, &reopened.owner, &reopened.revision, &editor.source).unwrap();
    assert!(restored.frame.as_ref().unwrap().confirmed);
    assert_eq!(restored.document, next.document);
}

#[test]
fn discard_restores_saved_metadata_and_clears_a_different_dimension_frame() {
    let editor = Editor::new();
    let app = editor.app();
    let loaded = editor.load();
    let frame_id = loaded.frame.as_ref().unwrap().id.clone();
    let mut document = loaded.document.unwrap();
    document.basis.content = PixelRect {
        x: 2,
        y: 3,
        width: 20,
        height: 16,
    };
    document.definitions.push(zone("saved"));
    let updated = app
        .recognition_update(
            &editor.view.owner,
            &editor.view.revision,
            document,
            Some(&frame_id),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &editor.view.revision,
            &frame_id,
            updated.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &editor.view.revision,
            confirmed.document_revision,
            &["saved".into()],
            &current_capture(editor.app()),
            &BTreeMap::from([("saved".into(), frame_id.clone())]),
        )
        .unwrap();
    let revision = saved.mutation.committed_revision;
    let saved_document = saved.recognition.unwrap().document.unwrap();
    assert!(saved_document.definitions[0].saved.is_some());

    let replacement = DecodedImage::from_rgba(16, 12, vec![255; 16 * 12 * 4]).unwrap();
    let png = images::encode_crop(&replacement, [0, 0, 16, 12]).unwrap();
    fs::write(&editor.source, png.as_bytes()).unwrap();
    let loaded = load_selected(app, &editor.view.owner, &revision, &editor.source).unwrap();
    let replaced_frame = loaded.frame.as_ref().unwrap();
    let replaced_id = replaced_frame.id.clone();
    assert_eq!((replaced_frame.width, replaced_frame.height), (16, 12));
    assert!(!replaced_frame.confirmed);
    let mut draft = loaded.document.unwrap();
    assert_eq!(draft.basis, saved_document.basis);
    draft.basis = GeometryBasis {
        frame_width: 16,
        frame_height: 12,
        content: PixelRect {
            x: 0,
            y: 0,
            width: 16,
            height: 12,
        },
    };
    draft.definitions[0].name = "unsaved replacement".into();
    draft.definitions[0].revision += 1;
    draft.definitions.push(zone("unsaved"));
    app.recognition_update(
        &editor.view.owner,
        &revision,
        draft,
        Some(&replaced_id),
        loaded.document_revision,
        &current_capture(editor.app()),
    )
    .unwrap();
    app.recognition_preview(
        &editor.view.owner,
        &replaced_id,
        &current_capture(editor.app()),
    )
    .unwrap();

    let discarded = app
        .recognition_discard(
            &editor.view.owner,
            &revision,
            Some(&current_capture(editor.app())),
            app.recognition_view(&editor.view.owner, &revision)
                .unwrap()
                .document_revision,
        )
        .unwrap();
    assert_eq!(discarded.document.as_ref(), Some(&saved_document));
    assert_eq!(discarded.saved_document.as_ref(), Some(&saved_document));
    assert!(
        discarded.frame.is_none(),
        "restored metadata cannot describe the replacement image"
    );
    assert_eq!(
        app.recognition_preview(&editor.view.owner, &replaced_id, &current_capture(app))
            .unwrap_err()
            .category,
        "StaleRecognition"
    );
    let refusal = app
        .recognition_copy(
            &editor.view.owner,
            &revision,
            discarded.document_revision,
            &["saved".into()],
            SnippetKind::OcrRecognize,
            &current_capture(editor.app()),
        )
        .err()
        .expect("Discard must not leave incompatible geometry available for Copy");
    assert_eq!(refusal.category, "RecognitionInput");

    let loaded = load_selected(app, &editor.view.owner, &revision, &editor.source).unwrap();
    let frame = loaded.frame.as_ref().unwrap();
    assert_ne!(frame.id, replaced_id);
    assert!(!frame.confirmed);
    assert!(matches!(
        app.recognition_confirm(
            &editor.view.owner,
            &revision,
            &frame.id,
            loaded.document_revision,
            &current_capture(app),
        ),
        Err(error) if error.category == "StaleRecognition"
    ));
    let mut rebased = loaded.document.clone().unwrap();
    rebased.basis = GeometryBasis {
        frame_width: frame.width,
        frame_height: frame.height,
        content: PixelRect {
            x: 0,
            y: 0,
            width: frame.width,
            height: frame.height,
        },
    };
    let updated = app
        .recognition_update(
            &editor.view.owner,
            &revision,
            rebased,
            Some(&frame.id),
            loaded.document_revision,
            &current_capture(app),
        )
        .unwrap();
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &revision,
            &frame.id,
            updated.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    assert!(confirmed.frame.as_ref().unwrap().confirmed);
    let mut edited_document = confirmed.document.unwrap();
    assert_eq!(
        edited_document.basis,
        GeometryBasis {
            frame_width: 16,
            frame_height: 12,
            content: PixelRect {
                x: 0,
                y: 0,
                width: 16,
                height: 12
            },
        }
    );
    edited_document.definitions[0].name = "edited after discard".into();
    edited_document.definitions[0].revision += 1;
    edited_document.definitions.push(zone("new"));
    let edited = app
        .recognition_update(
            &editor.view.owner,
            &revision,
            edited_document.clone(),
            Some(&frame.id),
            confirmed.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    assert_eq!(edited.document.as_ref(), Some(&edited_document));
    assert_eq!(edited.saved_document.as_ref(), Some(&saved_document));
    assert!(edited.frame.as_ref().unwrap().confirmed);
    grant_capability(app, 8);
    let copied = app
        .recognition_copy(
            &editor.view.owner,
            &revision,
            edited.document_revision,
            &["new".into()],
            SnippetKind::OcrRecognize,
            &current_capture(editor.app()),
        )
        .unwrap();
    assert_eq!(copied.basis, edited_document.basis);
    assert_eq!(
        app.publisher
            .open(Path::new(&editor.view.package_path))
            .unwrap()
            .recognition()
            .unwrap()
            .map(|metadata| metadata.document(&current_capture(app)).unwrap().clone()),
        Some(saved_document)
    );
}

#[test]
fn preview_keeps_frame_pixels_after_source_save() {
    let editor = Editor::new();
    let app = editor.app();
    let loaded = editor.load();
    let frame_id = loaded.frame.as_ref().unwrap().id.clone();
    let expected = images::decode_png(&fs::read(&editor.source).unwrap(), ImageKind::Crop).unwrap();
    let mutation = app
        .authoring_save(
            &editor.view.owner,
            &editor.view.revision,
            "main.ts",
            "export const previewRevision = 1;".into(),
        )
        .unwrap();
    assert_ne!(mutation.committed_revision, editor.view.revision);

    let preview = app
        .recognition_preview(&editor.view.owner, &frame_id, &current_capture(app))
        .unwrap();
    let decoded = images::decode_png(&preview, ImageKind::Crop).unwrap();
    assert_eq!(
        (decoded.width, decoded.height),
        (expected.width, expected.height)
    );
    assert_eq!(decoded.rgba, expected.rgba);
    app.recognition_release_preview(&editor.view.owner);
}

#[test]
fn grouped_copy_uses_the_cached_engine_limit_and_verifies_every_checked_zone() {
    let editor = Editor::new();
    let app = editor.app();
    let loaded = editor.load();
    let mut document = loaded.document.unwrap();
    document.definitions = vec![zone("first"), zone("second"), zone("no-text")];
    document.definitions[2].expected = None;
    let current = app
        .recognition_update(
            &editor.view.owner,
            &editor.view.revision,
            document,
            loaded.frame.as_ref().map(|frame| frame.id.as_str()),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let copy = |ids: &[String], mode| {
        app.recognition_copy(
            &editor.view.owner,
            &editor.view.revision,
            current.document_revision,
            ids,
            mode,
            &current_capture(editor.app()),
        )
    };
    let ids: Vec<String> = vec!["first".into(), "second".into()];
    // Setup needs no checked zones and no engine report; grouped OCR has no fallback bound.
    assert!(!copy(&[], SnippetKind::GameContent).unwrap().verified);
    assert!(copy(&ids[..1], SnippetKind::GameContent).is_err());
    assert!(copy(&ids, SnippetKind::OcrRecognize).is_err());
    grant_capability(app, 2);
    for selection in [
        vec![],
        vec!["first".into(), "first".into()],
        vec!["first".into(), "missing".into()],
        vec!["first".into(), "second".into(), "no-text".into()],
    ] {
        assert!(copy(&selection, SnippetKind::OcrRecognize).is_err());
    }
    grant_capability(app, 1);
    assert!(
        copy(&ids, SnippetKind::OcrRecognize).is_err(),
        "the cached engine report bounds the one request; nothing is split"
    );
    grant_capability(app, 2);
    assert!(!copy(&ids, SnippetKind::OcrRecognize).unwrap().verified);
    // Reference text is optional Script context, not a Copy prerequisite.
    assert_eq!(
        copy(
            &["no-text".into(), "first".into()],
            SnippetKind::OcrRecognize
        )
        .unwrap()
        .definition_ids,
        vec!["no-text".to_owned(), "first".to_owned()]
    );

    // A settled trial is evidence only for the definitions actually observed.
    for observed in [vec!["first"], vec!["first", "second"]] {
        retain_observed_trial(
            app,
            &editor.view.owner,
            &editor.view.revision,
            &current,
            &observed,
        );
        let copied = copy(&ids, SnippetKind::OcrRecognize).unwrap();
        assert_eq!(copied.definition_ids, ids);
        assert_eq!(copied.verified, observed.len() == 2);
        assert!(
            !copy(&[], SnippetKind::GameContent).unwrap().verified,
            "a successful OCR trial never verifies setup geometry"
        );
    }
    let next = editor.load();
    assert!(next.frame.as_ref().unwrap().confirmed);
    assert!(next.trial.as_ref().unwrap().stale);
    assert!(
        !app.recognition_copy(
            &editor.view.owner,
            &editor.view.revision,
            next.document_revision,
            &ids,
            SnippetKind::OcrRecognize,
            &current_capture(app),
        )
        .unwrap()
        .verified
    );
    assert!(
        copy(&ids, SnippetKind::OcrRecognize).is_err(),
        "the previous document revision cannot publish"
    );
}

#[test]
fn copy_never_certifies_a_trial_captured_at_an_older_package_revision() {
    let editor = Editor::new();
    let app = editor.app();
    let owner = &editor.view.owner;
    let loaded = editor.load();
    let mut document = loaded.document.unwrap();
    document.definitions.push(zone("first"));
    let current = app
        .recognition_update(
            owner,
            &editor.view.revision,
            document,
            loaded.frame.as_ref().map(|frame| frame.id.as_str()),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    grant_capability(app, 1);
    retain_observed_trial(app, owner, &editor.view.revision, &current, &["first"]);
    let ids = vec!["first".to_owned()];
    let verified = |revision: &str| {
        app.recognition_copy(
            owner,
            revision,
            current.document_revision,
            &ids,
            SnippetKind::OcrRecognize,
            &current_capture(editor.app()),
        )
        .unwrap()
        .verified
    };
    assert!(verified(&editor.view.revision));
    // A source-only save keeps the recognition draft revision but stales the trial row.
    let saved = app
        .authoring_save(
            owner,
            &editor.view.revision,
            "main.ts",
            "export const copied = 1;".into(),
        )
        .unwrap();
    let view = app
        .recognition_view(owner, &saved.committed_revision)
        .unwrap();
    assert_eq!(view.document_revision, current.document_revision);
    assert!(view.trial.unwrap().stale);
    assert!(
        !verified(&saved.committed_revision),
        "the displayed stale row cannot certify a fresh Copy receipt"
    );
}

#[test]
fn template_copy_accepts_a_hand_restored_saved_definition_without_reusing_its_revision() {
    let editor = Editor::new();
    let app = editor.app();
    let owner = &editor.view.owner;
    let loaded = editor.load();
    let frame_id = loaded.frame.as_ref().unwrap().id.clone();
    let mut document = loaded.document.unwrap();
    document.definitions.push(RecognitionDefinition {
        kind: RecognitionKind::Template,
        expected: None,
        template: Some(TemplateSettings {
            search_region: NormalizedRect {
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
            },
            threshold: 0.9,
            max_results: 4,
        }),
        ..zone("pattern")
    });
    document.template_rights = Some(TemplateRights {
        license: "CC0-1.0".into(),
        created_by: "regression test".into(),
        created_for: None,
        reviewed: true,
    });
    let updated = app
        .recognition_update(
            owner,
            &editor.view.revision,
            document,
            Some(&frame_id),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let saved = app
        .recognition_save(
            owner,
            &editor.view.revision,
            updated.document_revision,
            &["pattern".into()],
            &current_capture(editor.app()),
            &BTreeMap::from([("pattern".into(), frame_id.clone())]),
        )
        .unwrap();
    let revision = saved.mutation.committed_revision;
    let mut view = saved.recognition.unwrap();
    let stored = view.saved_document.clone().unwrap();
    let ids = vec!["pattern".to_owned()];
    let mut edit = |name: &str| {
        let mut draft = view.document.clone().unwrap();
        draft.definitions[0].name = name.into();
        draft.definitions[0].revision += 1;
        view = app
            .recognition_update(
                owner,
                &revision,
                draft,
                Some(&frame_id),
                view.document_revision,
                &current_capture(editor.app()),
            )
            .unwrap();
        app.recognition_copy(
            owner,
            &revision,
            view.document_revision,
            &ids,
            SnippetKind::TemplateRecognize,
            &current_capture(editor.app()),
        )
    };
    assert!(
        edit("renamed").is_err(),
        "an unsaved name keeps template Copy blocked"
    );
    let restored = edit(&stored.definitions[0].name).expect("restored saved content is current");
    assert!(!restored.verified);
    let document = view.document.unwrap();
    assert!(
        document.definitions[0].revision > stored.definitions[0].revision,
        "the edit fence keeps advancing; restoring content never restores its revision"
    );
    assert_ne!(Some(document), view.saved_document);
}

/// `/usr/bin/true` is a real child that exits before authenticating as the engine runner.
/// The supervisor reaps it and reports incomplete cleanup beside the primary failure, with
/// no engine, capture, input or focus authority involved.
#[cfg(unix)]
#[test]
fn reaped_child_with_incomplete_cleanup_stays_failed_without_refusing_save_or_exit() {
    let editor = Editor::with_fixture(Fixture::with_engine(|_| {
        std::path::PathBuf::from("/usr/bin/true")
    }));
    let app = editor.app();
    let owner = &editor.view.owner;
    let failure = app
        .recognition_capabilities(owner, &editor.view.revision)
        .err()
        .expect("the unauthenticated child must fail");
    assert_eq!(failure.category, "EngineUnavailable");
    assert_eq!(failure.context["child_reaped"], true);
    assert_eq!(failure.context["cleanup"]["clean"], false);
    let controller = app.poll().controller;
    assert_eq!(controller["operation"], "recognition_capabilities");
    assert_eq!(
        controller["result"]["cleanup"]["status"], "IncompleteCleanup",
        "the failed cleanup outcome stays visible"
    );

    // Reaped ownership has settled: local drafts still save and Edit can be left.
    let loaded = editor.load();
    assert!(
        loaded.capabilities["max_ocr_zones"].is_null(),
        "a failed read never supplies a grouped limit"
    );
    let mut document = loaded.document.unwrap();
    document.definitions.push(zone("kept"));
    let updated = app
        .recognition_update(
            owner,
            &editor.view.revision,
            document,
            loaded.frame.as_ref().map(|frame| frame.id.as_str()),
            loaded.document_revision,
            &current_capture(editor.app()),
        )
        .unwrap();
    let saved = app
        .recognition_save(
            owner,
            &editor.view.revision,
            updated.document_revision,
            &[],
            &current_capture(editor.app()),
            &BTreeMap::new(),
        )
        .unwrap();
    app.authoring_save(
        owner,
        &saved.mutation.committed_revision,
        "main.ts",
        "export const reaped = 1;".into(),
    )
    .unwrap();
    app.authoring_exit(owner).unwrap();
}

#[cfg(unix)]
#[test]
fn shutdown_settles_an_unreturned_recognition_run_and_reports_its_cleanup_failure() {
    let editor = Editor::with_fixture(Fixture::with_engine(|_| {
        std::path::PathBuf::from("/usr/bin/true")
    }));
    let app = editor.app();
    let owner = &editor.view.owner;
    // Start the run as the capability command does, but leave its reply unsent, as when
    // confirmed close overtakes that command's settlement.
    let (command, mut state) = app.command_state().unwrap();
    let mut stop = lock(&app.authoring_stop);
    let run = app.runner.recognition_capabilities().unwrap();
    app.own_recognition_run(&mut state, owner, &run, &mut stop, None);
    drop(stop);
    drop(state);
    drop(command);
    // Wait for the reaped child through the runner itself, without the application collector.
    let terminal = crate::application::test_support::settled(app);
    assert_eq!(terminal.result.unwrap()["child_reaped"], true);

    app.prepare_close(Some(owner)).unwrap();
    let failure = app.shutdown().unwrap_err();
    assert_eq!(failure.category, "RecognitionCleanup");
    assert_eq!(failure.context["operation"], "recognition_capabilities");
    assert_eq!(failure.context["child_reaped"], true);
    assert_eq!(failure.context["cleanup"]["clean"], false);
    assert!(
        app.authoring_owner().is_none(),
        "reaped ownership has settled, so the lease is released"
    );
}

#[test]
fn only_unconfirmed_child_ownership_contains_the_edit_session() {
    // Terminal controllers as the runner produces them (`desktop/recognition.rs`
    // `preparation_fault`, `desktop/operation.rs` worker join, `runner/recognition.rs`).
    let started_without_reaping = json!({"operation": "recognition_trial", "result": null,
        "error": {"category": "Transport", "message": "owned recognition pipe unavailable",
            "context": {"operation": "recognition_trial", "stage": "preparation",
                "cause": {"cleanup": {"clean": false, "child_started": true}, "stage": "child_startup"},
                "cleanup": {"clean": false, "child_started": true}}}});
    let worker_panic = json!({"operation": "recognition_capabilities", "result": null,
        "error": {"category": "Controller", "message": "operation worker panicked; inspect containment evidence",
            "context": {"operation": "recognition_capabilities", "stage": "worker",
                "cleanup": {"clean": false, "child_started": null}}}});
    let reaped = |primary: &str| {
        json!({"operation": "recognition_trial", "error": null, "result": {
            "operation": "recognition_trial", "primary": {"category": primary, "message": "m", "context": null},
            "cleanup": {"clean": false, "status": "IncompleteCleanup", "child_cleanup": null},
            "child_reaped": true, "forced": true, "exit_code": null, "result": null}})
    };
    for unconfirmed in [started_without_reaping, worker_panic, reaped("Containment")] {
        assert_eq!(
            containment(&unconfirmed).unwrap().category,
            "RecognitionCleanup"
        );
    }
    let forced = reaped("Timeout");
    assert!(containment(&forced).is_none());
    assert_eq!(
        incomplete_cleanup(&forced).unwrap().context["child_reaped"],
        true
    );
    let clean_refusal = json!({"operation": "recognition_trial", "result": null,
        "error": {"category": "EnvironmentUnset", "message": "m",
            "context": {"cleanup": {"clean": true, "child_started": false}}}});
    assert!(containment(&clean_refusal).is_none());
    assert!(incomplete_cleanup(&clean_refusal).is_none());
}

#[test]
fn saved_images_add_select_save_and_reopen_independent_capture_namespaces() {
    let editor = Editor::new();
    let app = editor.app();
    let owner = &editor.view.owner;
    let a = editor.load();
    let a_id = a.capture_id.clone().unwrap();
    crate::storage::validate_id(&a_id).unwrap();
    let a_pixels = {
        let state = lock(&app.workspaces);
        Arc::downgrade(
            &state
                .authoring
                .as_ref()
                .unwrap()
                .recognition
                .frame
                .as_ref()
                .unwrap()
                .image,
        )
    };
    let mut a_document = a.document.clone().unwrap();
    a_document.definitions.push(zone("r1"));
    a_document.basis.content = PixelRect {
        x: 2,
        y: 3,
        width: 20,
        height: 16,
    };
    let a = app
        .recognition_update(
            owner,
            &editor.view.revision,
            a_document,
            a.frame.as_ref().map(|frame| frame.id.as_str()),
            a.document_revision,
            &a_id,
        )
        .unwrap();
    let a = app
        .recognition_confirm(
            owner,
            &editor.view.revision,
            &a.frame.as_ref().unwrap().id,
            a.document_revision,
            &a_id,
        )
        .unwrap();
    let a_saved = app
        .recognition_save(
            owner,
            &editor.view.revision,
            a.document_revision,
            &["r1".into()],
            &a_id,
            &BTreeMap::from([("r1".into(), a.frame.as_ref().unwrap().id.clone())]),
        )
        .unwrap();
    let revision = a_saved.mutation.committed_revision;
    let a = a_saved.recognition.unwrap();
    let a_document = a.document.clone().unwrap();

    let b_image = DecodedImage::from_rgba(32, 24, vec![90; 32 * 24 * 4]).unwrap();
    fs::write(
        &editor.source,
        images::encode_crop(&b_image, [0, 0, 32, 24])
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    drop(b_image);
    let b = app
        .recognition_load(
            owner,
            &revision,
            &editor.source,
            Some(&a_id),
            a.document_revision,
            true,
        )
        .unwrap();
    assert!(
        a_pixels.upgrade().is_none(),
        "adding B releases the only decoded A original"
    );
    let b_id = b.capture_id.clone().unwrap();
    crate::storage::validate_id(&b_id).unwrap();
    assert_ne!(a_id, b_id);
    let mut b_document = b.document.clone().unwrap();
    b_document.definitions.push(zone("r1"));
    let b = app
        .recognition_update(
            owner,
            &revision,
            b_document,
            b.frame.as_ref().map(|frame| frame.id.as_str()),
            b.document_revision,
            &b_id,
        )
        .unwrap();
    let b_pixels = {
        let state = lock(&app.workspaces);
        Arc::downgrade(
            &state
                .authoring
                .as_ref()
                .unwrap()
                .recognition
                .frame
                .as_ref()
                .unwrap()
                .image,
        )
    };
    let b_saved = app
        .recognition_save(
            owner,
            &revision,
            b.document_revision,
            &["r1".into()],
            &b_id,
            &BTreeMap::from([("r1".into(), b.frame.as_ref().unwrap().id.clone())]),
        )
        .unwrap();
    let revision = b_saved.mutation.committed_revision;
    let b = b_saved.recognition.unwrap();
    let b_document = b.document.clone().unwrap();
    assert_ne!(a_document.basis, b_document.basis);
    assert_ne!(
        a_document.definitions[0].saved,
        b_document.definitions[0].saved
    );

    // Even current B generations cannot authorize A's identically named Region.
    assert_eq!(
        app.recognition_update(
            owner,
            &revision,
            a_document.clone(),
            b.frame.as_ref().map(|frame| frame.id.as_str()),
            b.document_revision,
            &a_id
        )
        .err()
        .unwrap()
        .category,
        "StaleRecognition"
    );
    assert_eq!(
        app.recognition_save(
            owner,
            &revision,
            b.document_revision,
            &[],
            &a_id,
            &BTreeMap::new()
        )
        .err()
        .unwrap()
        .category,
        "StaleRecognition"
    );
    assert_eq!(
        app.recognition_copy(
            owner,
            &revision,
            b.document_revision,
            &[],
            SnippetKind::GameContent,
            &a_id
        )
        .err()
        .unwrap()
        .category,
        "StaleRecognition"
    );
    assert_eq!(
        app.recognition_preview(owner, &b.frame.as_ref().unwrap().id, &a_id)
            .err()
            .unwrap()
            .category,
        "StaleRecognition"
    );
    assert_eq!(
        app.recognition_trial(
            owner,
            &revision,
            None,
            b.document_revision,
            &["r1".into()],
            Some("r1"),
            &a_id
        )
        .err()
        .unwrap()
        .category,
        "StaleRecognition"
    );

    let selected = app
        .recognition_select(owner, &revision, Some(&b_id), b.document_revision, &a_id)
        .unwrap();
    assert!(
        b_pixels.upgrade().is_none(),
        "selecting metadata never retains a decoded history"
    );
    assert!(selected.frame.is_none());
    assert_eq!(selected.document.as_ref(), Some(&a_document));
    assert!(
        app.recognition_copy(
            owner,
            &revision,
            selected.document_revision,
            &[],
            SnippetKind::GameContent,
            &a_id
        )
        .is_err()
    );
    let sample = app
        .recognition_trial(
            owner,
            &revision,
            None,
            selected.document_revision,
            &["r1".into()],
            Some("r1"),
            &a_id,
        )
        .unwrap();
    assert_eq!(sample.capture_id, a_id);
    assert!(sample.frame_id.is_none());
    assert_eq!(sample.controller["error"]["category"], "EnvironmentUnset");
    let mut metadata_only = a_document.clone();
    metadata_only.definitions[0].name = "A without original pixels".into();
    let selected = app
        .recognition_update(
            owner,
            &revision,
            metadata_only.clone(),
            None,
            selected.document_revision,
            &a_id,
        )
        .unwrap();
    let saved = app
        .recognition_save(
            owner,
            &revision,
            selected.document_revision,
            &[],
            &a_id,
            &BTreeMap::new(),
        )
        .unwrap();
    let revision = saved.mutation.committed_revision;
    let saved = saved.recognition.unwrap();
    assert_eq!(
        saved
            .captures
            .iter()
            .find(|capture| capture.capture_id == b_id)
            .unwrap()
            .document,
        b_document
    );
    fs::write(&editor.source, b"corrupt selected image").unwrap();
    assert!(
        app.recognition_load(
            owner,
            &revision,
            &editor.source,
            Some(&a_id),
            saved.document_revision,
            false
        )
        .is_err()
    );
    let failed = app.recognition_view(owner, &revision).unwrap();
    assert!(failed.frame.is_none());
    assert_eq!(failed.document, Some(metadata_only.clone()));
    let historical = DecodedImage::from_rgba(32, 24, vec![70; 32 * 24 * 4]).unwrap();
    fs::write(
        &editor.source,
        images::encode_crop(&historical, [0, 0, 32, 24])
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    drop(historical);
    let reloaded = app
        .recognition_load(
            owner,
            &revision,
            &editor.source,
            Some(&a_id),
            failed.document_revision,
            false,
        )
        .unwrap();
    assert_eq!(reloaded.capture_id.as_deref(), Some(a_id.as_str()));
    assert_eq!(reloaded.document, Some(metadata_only.clone()));
    assert!(reloaded.frame.as_ref().unwrap().confirmed);
    assert_ne!(
        reloaded.frame.as_ref().unwrap().id,
        a.frame.as_ref().unwrap().id
    );

    let workspace = app.authoring_exit(owner).unwrap();
    let reopened = app
        .authoring_open(&view_ref(&workspace), Path::new(&editor.view.package_path))
        .unwrap();
    let restored = app
        .recognition_view(&reopened.owner, &reopened.revision)
        .unwrap();
    assert!(restored.frame.is_none());
    assert_eq!(restored.capture_id.as_deref(), Some(a_id.as_str()));
    assert_eq!(restored.document, Some(metadata_only));
    let restored_b = app
        .recognition_select(
            &reopened.owner,
            &reopened.revision,
            restored.capture_id.as_deref(),
            restored.document_revision,
            &b_id,
        )
        .unwrap();
    assert!(restored_b.frame.is_none());
    assert_eq!(restored_b.document, Some(b_document));
}

#[test]
fn accepted_image_seam_rejects_stale_publication_and_clipboard_after_selection() {
    let editor = Editor::new();
    let app = editor.app();
    let owner = &editor.view.owner;
    let empty = app.recognition_view(owner, &editor.view.revision).unwrap();
    let prepared = app
        .recognition_prepare_capture(
            owner,
            &editor.view.revision,
            None,
            empty.document_revision,
            true,
            &[],
            &BTreeMap::new(),
        )
        .unwrap();
    let image = DecodedImage::from_rgba(2, 2, vec![30; 16]).unwrap();
    let a = app
        .recognition_install_capture(owner, &editor.view.revision, None, prepared, image, true)
        .unwrap();
    let a_id = a.capture_id.clone().unwrap();
    assert!(!a.frame.as_ref().unwrap().confirmed);
    let a = app
        .recognition_confirm(
            owner,
            &editor.view.revision,
            &a.frame.as_ref().unwrap().id,
            a.document_revision,
            &a_id,
        )
        .unwrap();
    let copied = app
        .recognition_copy(
            owner,
            &editor.view.revision,
            a.document_revision,
            &[],
            SnippetKind::GameContent,
            &a_id,
        )
        .unwrap();
    let next = app
        .recognition_prepare_capture(
            owner,
            &editor.view.revision,
            Some(&a_id),
            a.document_revision,
            true,
            &[],
            &BTreeMap::new(),
        )
        .unwrap();
    let stale_image = DecodedImage::from_rgba(2, 2, vec![40; 16]).unwrap();
    assert_eq!(
        app.recognition_install_capture(
            owner,
            &editor.view.revision,
            None,
            prepared,
            stale_image,
            true
        )
        .err()
        .unwrap()
        .category,
        "StaleRecognition"
    );
    let b = app
        .recognition_install_capture(
            owner,
            &editor.view.revision,
            Some(&a_id),
            next,
            DecodedImage::from_rgba(2, 2, vec![50; 16]).unwrap(),
            true,
        )
        .unwrap();
    assert_ne!(
        b.capture_id.as_deref(),
        Some(a_id.as_str()),
        "equal dimensions still create another namespace"
    );
    let written = std::cell::Cell::new(false);
    let refusal = app.recognition_publish_copy(owner, &editor.view.revision, copied, |_| {
        written.set(true);
        Ok(())
    });
    assert_eq!(refusal.err().unwrap().category, "StaleRecognition");
    assert!(
        !written.get(),
        "a stale receipt never reaches the irreversible clipboard write"
    );
}

fn refresh_frame(
    editor: &Editor,
    view: &RecognitionView,
    acquire: impl FnOnce() -> DecodedImage,
    new_capture: bool,
    sources: &BTreeMap<String, String>,
) -> Result<RecognitionView, Fault> {
    let ids = sources.keys().cloned().collect::<Vec<_>>();
    let prepared = editor.app().recognition_prepare_capture(
        &editor.view.owner,
        &view.revision,
        view.capture_id.as_deref(),
        view.document_revision,
        new_capture,
        &ids,
        sources,
    )?;
    editor.app().recognition_install_capture(
        &editor.view.owner,
        &view.revision,
        view.capture_id.as_deref(),
        prepared,
        acquire(),
        new_capture,
    )
}

fn draft_zone(editor: &Editor) -> RecognitionView {
    let loaded = editor.load();
    let mut document = loaded.document.clone().unwrap();
    document.definitions.push(zone("pending"));
    editor
        .app()
        .recognition_update(
            &editor.view.owner,
            &editor.view.revision,
            document,
            Some(&loaded.frame.as_ref().unwrap().id),
            loaded.document_revision,
            loaded.capture_id.as_deref().unwrap(),
        )
        .unwrap()
}

#[test]
fn native_refresh_retains_draft_and_invalidates_evidence_without_confirming_edits() {
    let editor = Editor::new();
    let app = editor.app();
    let owner = &editor.view.owner;
    let view = draft_zone(&editor);
    let capture_id = view.capture_id.as_deref().unwrap();
    retain_observed_trial(app, owner, &view.revision, &view, &["pending"]);
    let copy = app
        .recognition_copy(
            owner,
            &view.revision,
            view.document_revision,
            &[],
            SnippetKind::GameContent,
            capture_id,
        )
        .unwrap();
    let updated = refresh_frame(
        &editor,
        &view,
        || DecodedImage::from_rgba(32, 24, vec![80; 32 * 24 * 4]).unwrap(),
        false,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(updated.capture_id, view.capture_id);
    assert_eq!(updated.document, view.document);
    assert_eq!(updated.saved_document, view.saved_document);
    assert!(updated.frame.as_ref().unwrap().confirmed);
    assert_ne!(
        updated.frame.as_ref().unwrap().id,
        view.frame.as_ref().unwrap().id
    );
    assert!(updated.trial.as_ref().unwrap().stale);
    let copied = std::cell::Cell::new(false);
    assert!(
        app.recognition_publish_copy(owner, &view.revision, copy, |_| {
            copied.set(true);
            Ok(())
        })
        .is_err()
    );
    assert!(!copied.get());

    let mut changed = updated.document.clone().unwrap();
    changed.basis.content.x = 2;
    changed.basis.content.width = 30;
    let unconfirmed = app
        .recognition_update(
            owner,
            &updated.revision,
            changed.clone(),
            Some(&updated.frame.as_ref().unwrap().id),
            updated.document_revision,
            capture_id,
        )
        .unwrap();
    let refreshed = refresh_frame(
        &editor,
        &unconfirmed,
        || DecodedImage::from_rgba(32, 24, vec![90; 32 * 24 * 4]).unwrap(),
        false,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(refreshed.capture_id, view.capture_id);
    assert_eq!(refreshed.document, Some(changed.clone()));
    assert!(!refreshed.basis_confirmed);
    assert!(!refreshed.frame.as_ref().unwrap().confirmed);
    let next = refresh_frame(
        &editor,
        &refreshed,
        || DecodedImage::from_rgba(32, 24, vec![100; 32 * 24 * 4]).unwrap(),
        true,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_ne!(next.capture_id, view.capture_id);
    assert!(next.document.as_ref().unwrap().definitions.is_empty());
    assert_eq!(
        next.captures
            .iter()
            .find(|capture| capture.capture_id == capture_id)
            .unwrap()
            .document,
        changed,
    );
}

#[test]
fn pending_crop_saves_original_pixels_after_later_frames_and_failed_acquisition() {
    let editor = Editor::new();
    let app = editor.app();
    let view = draft_zone(&editor);
    let capture_id = view.capture_id.as_deref().unwrap();
    let sources = BTreeMap::from([("pending".into(), view.frame.as_ref().unwrap().id.clone())]);
    let old_pixels = {
        let state = lock(&app.workspaces);
        Arc::downgrade(
            &state
                .authoring
                .as_ref()
                .unwrap()
                .recognition
                .frame
                .as_ref()
                .unwrap()
                .image,
        )
    };
    let later = refresh_frame(
        &editor,
        &view,
        || DecodedImage::from_rgba(32, 24, vec![80; 32 * 24 * 4]).unwrap(),
        false,
        &sources,
    )
    .unwrap();
    assert!(
        old_pixels.upgrade().is_none(),
        "only encoded selections retain old pixels"
    );
    assert_eq!(later.staged_crop_ids, ["pending"]);
    assert_eq!(later.document, view.document);
    let later = refresh_frame(
        &editor,
        &later,
        || DecodedImage::from_rgba(32, 24, vec![90; 32 * 24 * 4]).unwrap(),
        false,
        &sources,
    )
    .unwrap();
    assert!(
        app.publisher
            .open(Path::new(&editor.view.package_path))
            .unwrap()
            .recognition()
            .unwrap()
            .is_none(),
        "capture never publishes crops"
    );
    let prepared = app
        .recognition_prepare_capture(
            &editor.view.owner,
            &later.revision,
            Some(capture_id),
            later.document_revision,
            false,
            &["pending".into()],
            &sources,
        )
        .unwrap();
    // An acquisition that fails or is cancelled never calls installation.
    let failed = app
        .recognition_view(&editor.view.owner, &later.revision)
        .unwrap();
    assert_eq!(failed.document_revision, prepared);
    assert_eq!(failed.document, view.document);
    assert!(failed.frame.is_none());
    assert_eq!(failed.staged_crop_ids, ["pending"]);
    let source_path = Path::new(&editor.view.package_path).join("main.ts");
    let source_before = fs::read(&source_path).unwrap();
    fs::write(&source_path, b"export const externalChange = 1;").unwrap();
    let refused = app
        .recognition_save(
            &editor.view.owner,
            &failed.revision,
            failed.document_revision,
            &["pending".into()],
            capture_id,
            &sources,
        )
        .err()
        .unwrap();
    assert_eq!(refused.category, "AuthoringConflict");
    let retained = app
        .recognition_view(&editor.view.owner, &failed.revision)
        .unwrap();
    assert_eq!(retained.document_revision, failed.document_revision);
    assert_eq!(retained.document, failed.document);
    assert_eq!(retained.staged_crop_ids, ["pending"]);
    fs::write(&source_path, source_before).unwrap();
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &failed.revision,
            failed.document_revision,
            &["pending".into()],
            capture_id,
            &sources,
        )
        .unwrap();
    assert!(saved.recognition.unwrap().staged_crop_ids.is_empty());
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let png = candidate
        .recognition_crop(capture_id, "pending")
        .unwrap()
        .unwrap();
    let crop = images::decode_png(&png, ImageKind::Crop).unwrap();
    assert_eq!((crop.width, crop.height), (16, 12));
    for y in 0..12usize {
        for x in 0..16usize {
            assert_eq!(
                &crop.rgba[(y * 16 + x) * 4..(y * 16 + x + 1) * 4],
                &[(x + 8) as u8, (y + 6) as u8, 17, 255],
            );
        }
    }
}

#[test]
fn staged_crop_changes_refuse_old_sources_and_reselection_uses_new_pixels() {
    let editor = Editor::new();
    let app = editor.app();
    let view = draft_zone(&editor);
    let capture_id = view.capture_id.as_deref().unwrap();
    let sources = BTreeMap::from([("pending".into(), view.frame.as_ref().unwrap().id.clone())]);
    let later = refresh_frame(
        &editor,
        &view,
        || DecodedImage::from_rgba(32, 24, vec![80; 32 * 24 * 4]).unwrap(),
        false,
        &sources,
    )
    .unwrap();
    let mut changed = later.document.clone().unwrap();
    changed.definitions[0].revision += 1;
    changed.definitions[0].region.u0 = 0.0;
    let changed = app
        .recognition_update(
            &editor.view.owner,
            &later.revision,
            changed,
            Some(&later.frame.as_ref().unwrap().id),
            later.document_revision,
            capture_id,
        )
        .unwrap();
    assert_eq!(changed.stale_crop_ids, ["pending"]);
    assert!(
        app.recognition_save(
            &editor.view.owner,
            &changed.revision,
            changed.document_revision,
            &["pending".into()],
            capture_id,
            &sources,
        )
        .is_err()
    );
    assert!(
        app.recognition_prepare_capture(
            &editor.view.owner,
            &changed.revision,
            Some(capture_id),
            changed.document_revision,
            false,
            &["pending".into()],
            &sources,
        )
        .is_err()
    );
    let retained = app
        .recognition_view(&editor.view.owner, &changed.revision)
        .unwrap();
    assert_eq!(retained.document, changed.document);
    assert_eq!(retained.document_revision, changed.document_revision);
    assert_eq!(
        retained.frame.as_ref().unwrap().id,
        changed.frame.as_ref().unwrap().id
    );
    let reselected =
        BTreeMap::from([("pending".into(), changed.frame.as_ref().unwrap().id.clone())]);
    let newer = refresh_frame(
        &editor,
        &changed,
        || DecodedImage::from_rgba(32, 24, vec![90; 32 * 24 * 4]).unwrap(),
        false,
        &reselected,
    )
    .unwrap();
    app.recognition_save(
        &editor.view.owner,
        &newer.revision,
        newer.document_revision,
        &["pending".into()],
        capture_id,
        &reselected,
    )
    .unwrap();
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let png = candidate
        .recognition_crop(capture_id, "pending")
        .unwrap()
        .unwrap();
    let crop = images::decode_png(&png, ImageKind::Crop).unwrap();
    assert_eq!((crop.width, crop.height), (24, 12));
    assert!(crop.rgba.iter().all(|pixel| *pixel == 80));
}

#[test]
fn resized_refresh_keeps_old_geometry_and_pending_pixels_until_explicit_rebase() {
    let editor = Editor::new();
    let app = editor.app();
    let view = draft_zone(&editor);
    let capture_id = view.capture_id.as_deref().unwrap();
    let sources = BTreeMap::from([("pending".into(), view.frame.as_ref().unwrap().id.clone())]);
    let resized = refresh_frame(
        &editor,
        &view,
        || DecodedImage::from_rgba(16, 12, vec![90; 16 * 12 * 4]).unwrap(),
        false,
        &sources,
    )
    .unwrap();
    assert_eq!(resized.document, view.document);
    assert!(!resized.frame.as_ref().unwrap().confirmed);
    assert!(!resized.basis_confirmed);
    assert!(
        app.recognition_confirm(
            &editor.view.owner,
            &resized.revision,
            &resized.frame.as_ref().unwrap().id,
            resized.document_revision,
            capture_id,
        )
        .is_err()
    );
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &resized.revision,
            resized.document_revision,
            &["pending".into()],
            capture_id,
            &sources,
        )
        .unwrap();
    assert_eq!(
        saved.recognition.unwrap().saved_document.unwrap().basis,
        view.document.unwrap().basis,
    );
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let png = candidate
        .recognition_crop(capture_id, "pending")
        .unwrap()
        .unwrap();
    assert_eq!(
        &images::decode_png(&png, ImageKind::Crop).unwrap().rgba[..4],
        &[8, 6, 17, 255],
    );
}

#[test]
fn aggregate_pending_pixel_limit_refuses_refresh_before_releasing_original() {
    let editor = Editor::new();
    let app = editor.app();
    let empty = app
        .recognition_view(&editor.view.owner, &editor.view.revision)
        .unwrap();
    let capture = refresh_frame(
        &editor,
        &empty,
        || DecodedImage::from_rgba(2048, 2048, vec![17; 2048 * 2048 * 4]).unwrap(),
        true,
        &BTreeMap::new(),
    )
    .unwrap();
    let capture_id = capture.capture_id.as_deref().unwrap();
    let capture = app
        .recognition_confirm(
            &editor.view.owner,
            &capture.revision,
            &capture.frame.as_ref().unwrap().id,
            capture.document_revision,
            capture_id,
        )
        .unwrap();
    let mut document = capture.document.clone().unwrap();
    document.definitions = (0..9)
        .map(|id| {
            let mut definition = zone(&format!("region{id}"));
            definition.region = NormalizedRect {
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
            };
            definition
        })
        .collect();
    let sources = document
        .definitions
        .iter()
        .map(|definition| {
            (
                definition.id.clone(),
                capture.frame.as_ref().unwrap().id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let view = app
        .recognition_update(
            &editor.view.owner,
            &capture.revision,
            document,
            Some(&capture.frame.as_ref().unwrap().id),
            capture.document_revision,
            capture_id,
        )
        .unwrap();
    let ids = sources.keys().cloned().collect::<Vec<_>>();
    assert!(
        app.recognition_prepare_capture(
            &editor.view.owner,
            &view.revision,
            Some(capture_id),
            view.document_revision,
            false,
            &ids,
            &sources,
        )
        .is_err()
    );
    let retained = app
        .recognition_view(&editor.view.owner, &view.revision)
        .unwrap();
    assert_eq!(retained.document_revision, view.document_revision);
    assert_eq!(retained.document, view.document);
    assert_eq!(
        retained.frame.as_ref().unwrap().id,
        view.frame.as_ref().unwrap().id
    );
    assert!(retained.staged_crop_ids.is_empty());
}

#[test]
fn new_capture_keeps_old_pending_selection_and_never_overwrites_saved_crop() {
    let editor = Editor::new();
    let app = editor.app();
    let view = draft_zone(&editor);
    let capture_id = view.capture_id.clone().unwrap();
    let sources = BTreeMap::from([("pending".into(), view.frame.as_ref().unwrap().id.clone())]);
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &view.revision,
            view.document_revision,
            &["pending".into()],
            &capture_id,
            &sources,
        )
        .unwrap()
        .recognition
        .unwrap();
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let original = candidate
        .recognition_crop(&capture_id, "pending")
        .unwrap()
        .unwrap();
    let refreshed = refresh_frame(
        &editor,
        &saved,
        || DecodedImage::from_rgba(32, 24, vec![80; 32 * 24 * 4]).unwrap(),
        false,
        &BTreeMap::new(),
    )
    .unwrap();
    let pending = BTreeMap::from([(
        "pending".into(),
        refreshed.frame.as_ref().unwrap().id.clone(),
    )]);
    let next = refresh_frame(
        &editor,
        &refreshed,
        || DecodedImage::from_rgba(32, 24, vec![90; 32 * 24 * 4]).unwrap(),
        true,
        &pending,
    )
    .unwrap();
    assert_ne!(next.capture_id.as_deref(), Some(capture_id.as_str()));
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    assert_eq!(
        candidate
            .recognition_crop(&capture_id, "pending")
            .unwrap()
            .unwrap(),
        original,
    );
    let next = app
        .recognition_confirm(
            &editor.view.owner,
            &next.revision,
            &next.frame.as_ref().unwrap().id,
            next.document_revision,
            next.capture_id.as_deref().unwrap(),
        )
        .unwrap();
    let selected = app
        .recognition_select(
            &editor.view.owner,
            &next.revision,
            next.capture_id.as_deref(),
            next.document_revision,
            &capture_id,
        )
        .unwrap();
    assert!(selected.frame.is_none());
    assert_eq!(selected.staged_crop_ids, ["pending"]);
    app.recognition_save(
        &editor.view.owner,
        &selected.revision,
        selected.document_revision,
        &["pending".into()],
        &capture_id,
        &pending,
    )
    .unwrap();
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let png = candidate
        .recognition_crop(&capture_id, "pending")
        .unwrap()
        .unwrap();
    let crop = images::decode_png(&png, ImageKind::Crop).unwrap();
    assert!(crop.rgba.iter().all(|pixel| *pixel == 80));
}

fn assert_saved_original_crop(
    editor: &Editor,
    view: &RecognitionView,
    sources: &BTreeMap<String, String>,
) -> RecognitionView {
    let capture_id = view.capture_id.as_deref().unwrap();
    let saved = editor
        .app()
        .recognition_save(
            &editor.view.owner,
            &view.revision,
            view.document_revision,
            &["pending".into()],
            capture_id,
            sources,
        )
        .unwrap()
        .recognition
        .unwrap();
    let candidate = editor
        .app()
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let png = candidate
        .recognition_crop(capture_id, "pending")
        .unwrap()
        .unwrap();
    let crop = images::decode_png(&png, ImageKind::Crop).unwrap();
    assert_eq!((crop.width, crop.height), (16, 12));
    for y in 0..12usize {
        for x in 0..16usize {
            assert_eq!(
                &crop.rgba[(y * 16 + x) * 4..(y * 16 + x + 1) * 4],
                &[(x + 8) as u8, (y + 6) as u8, 17, 255],
            );
        }
    }
    saved
}

#[test]
fn refresh_review_metadata_edits_keep_staged_original_pixels() {
    let editor = Editor::new();
    let app = editor.app();
    let original = draft_zone(&editor);
    let sources = BTreeMap::from([(
        "pending".into(),
        original.frame.as_ref().unwrap().id.clone(),
    )]);
    let refreshed = refresh_frame(
        &editor,
        &original,
        || DecodedImage::from_rgba(32, 24, vec![80; 32 * 24 * 4]).unwrap(),
        false,
        &sources,
    )
    .unwrap();
    let mut document = refreshed.document.clone().unwrap();
    document.definitions[0].name = "Renamed after refresh".into();
    document.definitions[0].expected = Some("Updated script query".into());
    document.definitions[0].revision += 1;
    let updated = app
        .recognition_update(
            &editor.view.owner,
            &refreshed.revision,
            document.clone(),
            Some(&refreshed.frame.as_ref().unwrap().id),
            refreshed.document_revision,
            refreshed.capture_id.as_deref().unwrap(),
        )
        .unwrap();
    assert_eq!(updated.staged_crop_ids, ["pending"]);
    assert!(updated.stale_crop_ids.is_empty());
    assert_eq!(updated.staged_crop_sources, sources);
    let saved = assert_saved_original_crop(&editor, &updated, &sources);
    let stored = saved.saved_document.unwrap();
    assert_eq!(stored.basis, document.basis);
    assert_eq!(stored.definitions[0].name, document.definitions[0].name);
    assert_eq!(
        stored.definitions[0].expected,
        document.definitions[0].expected,
    );
}

#[test]
fn refresh_review_resized_frame_accepts_metadata_but_refuses_unmatched_basis_change() {
    let editor = Editor::new();
    let app = editor.app();
    let original = draft_zone(&editor);
    let sources = BTreeMap::from([(
        "pending".into(),
        original.frame.as_ref().unwrap().id.clone(),
    )]);
    let resized = refresh_frame(
        &editor,
        &original,
        || DecodedImage::from_rgba(16, 12, vec![90; 16 * 12 * 4]).unwrap(),
        false,
        &sources,
    )
    .unwrap();
    let capture_id = resized.capture_id.as_deref().unwrap();
    let frame_id = &resized.frame.as_ref().unwrap().id;
    let unchanged = app
        .recognition_update(
            &editor.view.owner,
            &resized.revision,
            resized.document.clone().unwrap(),
            Some(frame_id),
            resized.document_revision,
            capture_id,
        )
        .unwrap();
    assert_eq!(unchanged.document_revision, resized.document_revision);
    let mut document = unchanged.document.unwrap();
    document.definitions[0].name = "Historical selection".into();
    document.definitions[0].expected = Some("Still uses original pixels".into());
    document.definitions[0].revision += 1;
    let updated = app
        .recognition_update(
            &editor.view.owner,
            &resized.revision,
            document.clone(),
            Some(frame_id),
            resized.document_revision,
            capture_id,
        )
        .unwrap();
    assert_eq!(updated.document, Some(document.clone()));
    assert!(!updated.basis_confirmed);
    assert!(!updated.frame.as_ref().unwrap().confirmed);
    assert_eq!(updated.staged_crop_ids, ["pending"]);
    let mut unmatched = document.clone();
    unmatched.basis.content.x = 1;
    unmatched.basis.content.width = 31;
    let refused = app
        .recognition_update(
            &editor.view.owner,
            &updated.revision,
            unmatched,
            Some(frame_id),
            updated.document_revision,
            capture_id,
        )
        .err()
        .unwrap();
    assert_eq!(refused.category, "RecognitionInput");
    let retained = app
        .recognition_view(&editor.view.owner, &updated.revision)
        .unwrap();
    assert_eq!(retained.document_revision, updated.document_revision);
    assert_eq!(retained.document, updated.document);
    assert_eq!(retained.staged_crop_sources, sources);
    let saved = assert_saved_original_crop(&editor, &retained, &sources);
    assert_eq!(saved.saved_document.unwrap().basis, document.basis);
}

fn two_pending_captures(
    editor: &Editor,
) -> (
    RecognitionView,
    RecognitionView,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
) {
    let first = draft_zone(editor);
    let first_sources =
        BTreeMap::from([("pending".into(), first.frame.as_ref().unwrap().id.clone())]);
    let next = refresh_frame(
        editor,
        &first,
        || DecodedImage::from_rgba(32, 24, vec![80; 32 * 24 * 4]).unwrap(),
        true,
        &first_sources,
    )
    .unwrap();
    let next = editor
        .app()
        .recognition_confirm(
            &editor.view.owner,
            &next.revision,
            &next.frame.as_ref().unwrap().id,
            next.document_revision,
            next.capture_id.as_deref().unwrap(),
        )
        .unwrap();
    let mut document = next.document.clone().unwrap();
    document.definitions.push(zone("pending"));
    let next = editor
        .app()
        .recognition_update(
            &editor.view.owner,
            &next.revision,
            document,
            Some(&next.frame.as_ref().unwrap().id),
            next.document_revision,
            next.capture_id.as_deref().unwrap(),
        )
        .unwrap();
    let next_sources =
        BTreeMap::from([("pending".into(), next.frame.as_ref().unwrap().id.clone())]);
    let next = refresh_frame(
        editor,
        &next,
        || DecodedImage::from_rgba(32, 24, vec![90; 32 * 24 * 4]).unwrap(),
        false,
        &next_sources,
    )
    .unwrap();
    (first, next, first_sources, next_sources)
}

fn pending_crop_budget(editor: &Editor) -> BTreeMap<String, (usize, usize)> {
    let state = lock(&editor.app().workspaces);
    let mut budget = BTreeMap::new();
    for crop in &state.authoring.as_ref().unwrap().recognition.staged_crops {
        let held = budget.entry(crop.capture_id.clone()).or_insert((0, 0));
        held.0 += crop.png.len();
        held.1 += crop.decoded_bytes;
    }
    budget
}

#[test]
fn refresh_review_explicit_select_releases_only_abandoned_capture_stages() {
    let editor = Editor::new();
    let app = editor.app();
    let (first, next, first_sources, next_sources) = two_pending_captures(&editor);
    let first_id = first.capture_id.as_deref().unwrap();
    let next_id = next.capture_id.as_deref().unwrap();
    let held = pending_crop_budget(&editor);
    assert_eq!(
        held.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([first_id.to_owned(), next_id.to_owned(),])
    );
    assert_eq!(held[first_id].1, 16 * 12 * 4);
    assert_eq!(held[next_id].1, 16 * 12 * 4);
    assert_eq!(
        app.recognition_select(
            &editor.view.owner,
            &next.revision,
            Some(next_id),
            next.document_revision - 1,
            first_id,
        )
        .err()
        .unwrap()
        .category,
        "StaleRecognition",
    );
    assert_eq!(pending_crop_budget(&editor), held);
    let selected = app
        .recognition_select(
            &editor.view.owner,
            &next.revision,
            Some(next_id),
            next.document_revision,
            first_id,
        )
        .unwrap();
    assert_eq!(
        pending_crop_budget(&editor),
        BTreeMap::from([(first_id.to_owned(), held[first_id])]),
        "explicit switching releases only the abandoned capture's encoded and decoded budget",
    );
    assert_eq!(selected.staged_crop_sources, first_sources);
    let saved = assert_saved_original_crop(&editor, &selected, &first_sources);
    assert!(pending_crop_budget(&editor).is_empty());
    let abandoned = app
        .recognition_select(
            &editor.view.owner,
            &saved.revision,
            Some(first_id),
            saved.document_revision,
            next_id,
        )
        .unwrap();
    assert!(abandoned.staged_crop_sources.is_empty());
    assert_eq!(
        app.recognition_save(
            &editor.view.owner,
            &abandoned.revision,
            abandoned.document_revision,
            &["pending".into()],
            next_id,
            &next_sources,
        )
        .err()
        .unwrap()
        .category,
        "RecognitionInput",
    );
}

#[test]
fn refresh_review_explicit_load_releases_current_stages_even_when_read_fails() {
    for (new_capture, invalid_image) in [(false, false), (true, false), (false, true)] {
        let editor = Editor::new();
        let app = editor.app();
        let (first, next, first_sources, _) = two_pending_captures(&editor);
        let first_id = first.capture_id.as_deref().unwrap();
        let held = pending_crop_budget(&editor);
        let path = if invalid_image {
            let path = editor.fixture.root.join("invalid.png");
            fs::write(&path, b"not a PNG").unwrap();
            path
        } else {
            editor.source.clone()
        };
        assert_eq!(
            app.recognition_load(
                &editor.view.owner,
                &next.revision,
                &path,
                first.capture_id.as_deref(),
                next.document_revision,
                new_capture,
            )
            .err()
            .unwrap()
            .category,
            "StaleRecognition",
        );
        assert_eq!(pending_crop_budget(&editor), held);
        let result = app.recognition_load(
            &editor.view.owner,
            &next.revision,
            &path,
            next.capture_id.as_deref(),
            next.document_revision,
            new_capture,
        );
        let loaded = if invalid_image {
            assert!(result.is_err());
            let failed = app
                .recognition_view(&editor.view.owner, &next.revision)
                .unwrap();
            assert!(failed.frame.is_none());
            failed
        } else {
            let loaded = result.unwrap();
            let frame = loaded.frame.as_ref().unwrap();
            assert_eq!((frame.width, frame.height), (32, 24));
            assert_eq!(loaded.capture_id != next.capture_id, new_capture);
            loaded
        };
        assert_eq!(
            pending_crop_budget(&editor),
            BTreeMap::from([(first_id.to_owned(), held[first_id])]),
            "load must not charge abandoned pixels after its release boundary",
        );
        assert!(loaded.staged_crop_sources.is_empty());
        let selected = app
            .recognition_select(
                &editor.view.owner,
                &loaded.revision,
                loaded.capture_id.as_deref(),
                loaded.document_revision,
                first_id,
            )
            .unwrap();
        assert_eq!(selected.staged_crop_sources, first_sources);
        assert_saved_original_crop(&editor, &selected, &first_sources);
        assert!(pending_crop_budget(&editor).is_empty());
    }
}

/// The managed cache refuses linked ancestors, so this editor lives under the resolved temp root.
fn cached_editor() -> Editor {
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "mado-recognition-cache-{}",
        crate::storage::new_id().unwrap()
    ));
    crate::storage::Store::new(root.clone())
        .unwrap()
        .initialize(crate::application::test_support::preferences())
        .unwrap();
    let application = Application::new(
        root.clone(),
        root.join("runner-must-not-be-launched"),
        root.join("engine-must-not-be-launched"),
    )
    .unwrap();
    Editor::with_fixture(Fixture { root, application })
}

fn solid_png(value: u8) -> images::EncodedImage {
    let image = DecodedImage::from_rgba(32, 24, vec![value; 32 * 24 * 4]).unwrap();
    images::encode_crop(&image, [0, 0, 32, 24]).unwrap()
}

fn frame_pixels(app: &Application) -> Option<Vec<u8>> {
    lock(&app.workspaces)
        .authoring
        .as_ref()
        .unwrap()
        .recognition
        .frame
        .as_ref()
        .map(|frame| frame.image.rgba.clone())
}

#[test]
fn explicit_cached_reload_reports_historical_status_without_a_native_timestamp() {
    let editor = cached_editor();
    let app = editor.app();
    let owner = &editor.view.owner;
    let loaded = editor.load();
    let selected = loaded.frame.as_ref().unwrap();
    assert!(
        !selected.historical && selected.historical_capture_at_ms.is_none(),
        "an explicitly selected saved image is not historical evidence"
    );
    let capture_id = loaded.capture_id.clone().unwrap();
    let cache = app.capture_cache().unwrap();
    assert!(
        cache
            .persist(
                &editor.view.package_id,
                &capture_id,
                solid_png(17).as_bytes()
            )
            .cached
    );
    let saved = app
        .recognition_save(
            owner,
            &editor.view.revision,
            loaded.document_revision,
            &[],
            &capture_id,
            &BTreeMap::new(),
        )
        .unwrap();
    assert!(saved.recognition.is_some());
    let workspace = app.authoring_exit(owner).unwrap();
    let reopened = app
        .authoring_open(&view_ref(&workspace), Path::new(&editor.view.package_path))
        .unwrap();
    let restored = app
        .recognition_view(&reopened.owner, &reopened.revision)
        .unwrap();
    assert!(restored.frame.is_none());
    assert_eq!(restored.capture_id.as_deref(), Some(capture_id.as_str()));
    let reloaded = app
        .recognition_load_cached(
            &reopened.owner,
            &reopened.revision,
            &cache,
            &capture_id,
            restored.document_revision,
        )
        .unwrap();
    assert_eq!(reloaded.capture_id.as_deref(), Some(capture_id.as_str()));
    let frame = reloaded.frame.as_ref().unwrap();
    assert_eq!((frame.width, frame.height), (32, 24));
    assert!(
        frame.historical,
        "a cached reload must identify its pixels as historical evidence"
    );
    assert_eq!(
        frame.historical_capture_at_ms, None,
        "no native acquisition time exists for a cached reload"
    );
    assert_eq!(frame_pixels(app).as_deref(), Some(&[17; 32 * 24 * 4][..]));
    let selected_again = app
        .recognition_load(
            &reopened.owner,
            &reopened.revision,
            &editor.source,
            Some(&capture_id),
            reloaded.document_revision,
            false,
        )
        .unwrap();
    assert!(
        !selected_again.frame.as_ref().unwrap().historical,
        "an explicit saved-image load after a cached reload is not historical"
    );
}

#[cfg(unix)]
#[test]
fn cached_reload_never_follows_links_substituted_after_managed_validation() {
    use std::os::unix::fs::symlink;
    let editor = cached_editor();
    let app = editor.app();
    let owner = &editor.view.owner;
    let revision = &editor.view.revision;
    let loaded = editor.load();
    let capture_id = loaded.capture_id.clone().unwrap();
    let cache = app.capture_cache().unwrap();
    let managed = solid_png(17);
    let outside = editor.fixture.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let outside_png = outside.join(format!("{capture_id}.png"));
    fs::write(&outside_png, solid_png(200).as_bytes()).unwrap();
    let package_dir = editor
        .fixture
        .root
        .join("caches")
        .join(&editor.view.package_id);
    let moved = editor.fixture.root.join("caches").join("moved-package");
    let managed_png = package_dir.join(format!("{capture_id}.png"));
    for link_directory in [false, true] {
        assert!(
            cache
                .persist(&editor.view.package_id, &capture_id, managed.as_bytes())
                .cached
        );
        let view = app.recognition_view(owner, revision).unwrap();
        let result = app.load_recognition_source(
            owner,
            revision,
            Some(&capture_id),
            view.document_revision,
            false,
            |candidate| {
                let opened = cache
                    .open_image(candidate.package_id(), &capture_id)?
                    .expect("the managed original was persisted");
                // Substitution between managed validation and the actual read.
                if link_directory {
                    fs::rename(&package_dir, &moved).unwrap();
                    symlink(&outside, &package_dir).unwrap();
                } else {
                    fs::remove_file(&managed_png).unwrap();
                    symlink(&outside_png, &managed_png).unwrap();
                }
                Ok(ImageSource::Cached(opened))
            },
        );
        let pixels = frame_pixels(app);
        assert!(
            pixels.as_deref() != Some(&[200; 32 * 24 * 4][..]),
            "a link substituted after validation must never load outside pixels (directory link: {link_directory})"
        );
        assert!(
            pixels.as_deref() == Some(&[17; 32 * 24 * 4][..]),
            "the verified managed original is the frame's identity (directory link: {link_directory})"
        );
        result.unwrap();
        // At rest, the substituted layout is refused and leaves no frame.
        let view = app.recognition_view(owner, revision).unwrap();
        assert!(
            app.recognition_load_cached(
                owner,
                revision,
                &cache,
                &capture_id,
                view.document_revision
            )
            .is_err()
        );
        assert!(frame_pixels(app).is_none());
        if link_directory {
            fs::remove_file(&package_dir).unwrap();
            fs::rename(&moved, &package_dir).unwrap();
        }
        fs::remove_file(&managed_png).unwrap();
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires Windows Developer Mode or pre-authorized symbolic-link privilege"]
fn windows_cached_loader_refuses_reparse_substitution_and_preserves_verified_pixels() {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let editor = cached_editor();
    let app = editor.app();
    let owner = &editor.view.owner;
    let revision = &editor.view.revision;
    let loaded = editor.load();
    let capture_id = loaded.capture_id.clone().unwrap();
    let cache = app.capture_cache().unwrap();
    let managed = solid_png(17);
    let foreign = solid_png(200);
    let outside = editor.fixture.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let outside_png = outside.join(format!("{capture_id}.png"));
    fs::write(&outside_png, foreign.as_bytes()).unwrap();
    let cache_dir = editor.fixture.root.join("caches");
    let package_dir = cache_dir.join(&editor.view.package_id);
    let managed_png = package_dir.join(format!("{capture_id}.png"));
    assert!(
        cache
            .persist(&editor.view.package_id, &capture_id, managed.as_bytes())
            .cached
    );
    let view = app.recognition_view(owner, revision).unwrap();
    app.load_recognition_source(
        owner,
        revision,
        Some(&capture_id),
        view.document_revision,
        false,
        |candidate| {
            let opened = cache
                .open_image(candidate.package_id(), &capture_id)?
                .unwrap();
            let error = fs::remove_file(&managed_png).unwrap_err();
            assert!(matches!(error.raw_os_error(), Some(5 | 32)));
            let error = fs::rename(&managed_png, managed_png.with_extension("moved")).unwrap_err();
            assert!(matches!(error.raw_os_error(), Some(5 | 32)));
            Ok(ImageSource::Cached(opened))
        },
    )
    .unwrap();
    assert_eq!(frame_pixels(app).as_deref(), Some(&[17; 32 * 24 * 4][..]));

    // Once the read has released its handles, deliberately substitute each level.
    for target in [&managed_png, &package_dir, &cache_dir] {
        let moved = target.with_extension("original");
        fs::rename(target, &moved).unwrap();
        let directory = moved.is_dir();
        if directory {
            symlink_dir(&outside, target).unwrap();
        } else {
            symlink_file(&outside_png, target).unwrap();
        }
        let view = app.recognition_view(owner, revision).unwrap();
        let error = app
            .recognition_load_cached(owner, revision, &cache, &capture_id, view.document_revision)
            .err()
            .expect("cached loading must refuse the substituted reparse point");
        assert_eq!(error.category, "CaptureCache");
        assert!(frame_pixels(app).is_none());
        assert_eq!(fs::read(&outside_png).unwrap(), foreign.as_bytes());
        if directory {
            fs::remove_dir(target).unwrap();
        } else {
            fs::remove_file(target).unwrap();
        }
        fs::rename(&moved, target).unwrap();
    }
    let view = app.recognition_view(owner, revision).unwrap();
    app.recognition_load_cached(owner, revision, &cache, &capture_id, view.document_revision)
        .unwrap();
    assert_eq!(frame_pixels(app).as_deref(), Some(&[17; 32 * 24 * 4][..]));
}
