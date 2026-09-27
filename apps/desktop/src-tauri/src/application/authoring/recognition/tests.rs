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
        self.app()
            .recognition_load(&self.view.owner, &self.view.revision, &self.source)
            .unwrap()
    }
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
        )
        .unwrap();
    assert!(!updated.frame.as_ref().unwrap().confirmed);
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &editor.view.revision,
            &frame_id,
            updated.document_revision,
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
        )
        .unwrap();
    let new_revision = saved.mutation.committed_revision;
    let saved_view = saved.recognition.unwrap();
    assert_eq!(saved_view.document.as_ref().unwrap().definitions.len(), 9);
    let candidate = app
        .publisher
        .open(Path::new(&editor.view.package_path))
        .unwrap();
    let crop = candidate.recognition_crop("zone0").unwrap().unwrap();
    let decoded = images::decode_png(&crop, ImageKind::Crop).unwrap();
    assert_eq!((decoded.width, decoded.height), (10, 8));
    assert_eq!(&decoded.rgba[..4], &[7, 7, 17, 255]);
    assert_eq!(&decoded.rgba[decoded.rgba.len() - 4..], &[16, 14, 17, 255]);
    assert!(candidate.recognition_crop("zone1").unwrap().is_none());
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
    app.recognition_preview(&editor.view.owner, &frame_id)
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
        )
        .err()
        .unwrap();
    assert_eq!(refusal.category, "StaleRecognition");
    fs::write(&editor.source, b"not a PNG").unwrap();
    assert!(
        editor
            .app()
            .recognition_load(&editor.view.owner, &editor.view.revision, &editor.source)
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
                SnippetKind::OcrRecognize
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
        )
        .unwrap();
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &editor.view.revision,
            &updated.frame.as_ref().unwrap().id,
            updated.document_revision,
        )
        .unwrap();
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &editor.view.revision,
            confirmed.document_revision,
            &[],
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
    )
    .unwrap();
    let replacement = app
        .recognition_load(&editor.view.owner, &revision, &editor.source)
        .unwrap();
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
    let next = app
        .recognition_load(&reopened.owner, &reopened.revision, &editor.source)
        .unwrap();
    assert!(next.frame.as_ref().unwrap().confirmed);
    assert_eq!(next.document, Some(saved_document.clone()));
    let setup = app
        .recognition_copy(
            &reopened.owner,
            &reopened.revision,
            next.document_revision,
            &[],
            SnippetKind::GameContent,
        )
        .unwrap();
    assert_eq!(setup.basis, saved_document.basis);
    assert!(
        !setup.verified,
        "setup geometry is never recognition evidence"
    );

    // Even when the old content rectangle fits, different dimensions are only
    // an editable proposal. Repeated loading and Save cannot silently confirm it.
    let resized = DecodedImage::from_rgba(40, 24, vec![255; 40 * 24 * 4]).unwrap();
    let png = images::encode_crop(&resized, [0, 0, 40, 24]).unwrap();
    fs::write(&editor.source, png.as_bytes()).unwrap();
    for _ in 0..2 {
        let next = app
            .recognition_load(&reopened.owner, &reopened.revision, &editor.source)
            .unwrap();
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
            )
            .is_err(),
            "unconfirmed geometry is never published as setup"
        );
        assert!(
            app.recognition_save(
                &reopened.owner,
                &reopened.revision,
                next.document_revision,
                &[],
            )
            .is_err()
        );
    }
    fs::write(&editor.source, b"not a PNG").unwrap();
    assert!(
        app.recognition_load(&reopened.owner, &reopened.revision, &editor.source)
            .is_err()
    );
    assert!(
        app.recognition_view(&reopened.owner, &reopened.revision)
            .unwrap()
            .frame
            .is_none()
    );
    fs::write(&editor.source, png.as_bytes()).unwrap();
    let restored = app
        .recognition_load(&reopened.owner, &reopened.revision, &editor.source)
        .unwrap();
    assert!(
        !restored.frame.as_ref().unwrap().confirmed,
        "failed loads do not grant geometry authority"
    );
    let current = app
        .recognition_view(&reopened.owner, &reopened.revision)
        .unwrap();
    app.recognition_confirm(
        &reopened.owner,
        &reopened.revision,
        &current.frame.as_ref().unwrap().id,
        current.document_revision,
    )
    .unwrap();
    let next = app
        .recognition_load(&reopened.owner, &reopened.revision, &editor.source)
        .unwrap();
    assert!(next.frame.as_ref().unwrap().confirmed);
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
        )
        .unwrap();
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &editor.view.revision,
            &frame_id,
            updated.document_revision,
        )
        .unwrap();
    let saved = app
        .recognition_save(
            &editor.view.owner,
            &editor.view.revision,
            confirmed.document_revision,
            &["saved".into()],
        )
        .unwrap();
    let revision = saved.mutation.committed_revision;
    let saved_document = saved.recognition.unwrap().document.unwrap();
    assert!(saved_document.definitions[0].saved.is_some());

    let replacement = DecodedImage::from_rgba(16, 12, vec![255; 16 * 12 * 4]).unwrap();
    let png = images::encode_crop(&replacement, [0, 0, 16, 12]).unwrap();
    fs::write(&editor.source, png.as_bytes()).unwrap();
    let loaded = app
        .recognition_load(&editor.view.owner, &revision, &editor.source)
        .unwrap();
    let replaced_frame = loaded.frame.as_ref().unwrap();
    let replaced_id = replaced_frame.id.clone();
    assert_eq!((replaced_frame.width, replaced_frame.height), (16, 12));
    assert!(!replaced_frame.confirmed);
    let mut draft = loaded.document.unwrap();
    assert_eq!(
        draft.basis.content,
        PixelRect {
            x: 0,
            y: 0,
            width: 16,
            height: 12
        }
    );
    draft.definitions[0].name = "unsaved replacement".into();
    draft.definitions[0].revision += 1;
    draft.definitions.push(zone("unsaved"));
    app.recognition_update(
        &editor.view.owner,
        &revision,
        draft,
        Some(&replaced_id),
        loaded.document_revision,
    )
    .unwrap();
    app.recognition_preview(&editor.view.owner, &replaced_id)
        .unwrap();

    let discarded = app
        .recognition_discard(&editor.view.owner, &revision)
        .unwrap();
    assert_eq!(discarded.document.as_ref(), Some(&saved_document));
    assert_eq!(discarded.saved_document.as_ref(), Some(&saved_document));
    assert!(
        discarded.frame.is_none(),
        "restored metadata cannot describe the replacement image"
    );
    assert_eq!(
        app.recognition_preview(&editor.view.owner, &replaced_id)
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
        )
        .err()
        .expect("Discard must not leave incompatible geometry available for Copy");
    assert_eq!(refusal.category, "RecognitionInput");

    let loaded = app
        .recognition_load(&editor.view.owner, &revision, &editor.source)
        .unwrap();
    let frame = loaded.frame.as_ref().unwrap();
    assert_ne!(frame.id, replaced_id);
    assert!(!frame.confirmed);
    let confirmed = app
        .recognition_confirm(
            &editor.view.owner,
            &revision,
            &frame.id,
            loaded.document_revision,
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
        )
        .unwrap();
    assert_eq!(copied.basis, edited_document.basis);
    assert_eq!(
        app.publisher
            .open(Path::new(&editor.view.package_path))
            .unwrap()
            .recognition()
            .unwrap(),
        Some(saved_document)
    );
}

#[test]
fn preview_keeps_frame_pixels_after_source_save_while_command_admission_is_busy() {
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

    // An admitted command does not own these immutable, owner/frame-bound pixels.
    let preview = {
        let _command = lock(&app.commands);
        app.recognition_preview(&editor.view.owner, &frame_id)
            .unwrap()
    };
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
        )
        .unwrap();
    let copy = |ids: &[String], mode| {
        app.recognition_copy(
            &editor.view.owner,
            &editor.view.revision,
            current.document_revision,
            ids,
            mode,
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
        )
        .unwrap();
    let saved = app
        .recognition_save(
            owner,
            &editor.view.revision,
            updated.document_revision,
            &["pattern".into()],
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
            )
            .unwrap();
        app.recognition_copy(
            owner,
            &revision,
            view.document_revision,
            &ids,
            SnippetKind::TemplateRecognize,
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
        )
        .unwrap();
    let saved = app
        .recognition_save(owner, &editor.view.revision, updated.document_revision, &[])
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
