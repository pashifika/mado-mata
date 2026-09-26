use super::*;
use crate::application::test_support::{Fixture, view_ref};
use mado_runtime_comparison::recognition::{NormalizedRect, RecognitionDefinition};
use std::fs;

struct Editor {
    fixture: Fixture,
    view: super::super::AuthoringView,
    source: std::path::PathBuf,
}

impl Editor {
    fn new() -> Self {
        let fixture = Fixture::new();
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
    let copied = app
        .recognition_copy(
            &editor.view.owner,
            &editor.view.revision,
            confirmed.document_revision,
            "zone0",
            SnippetKind::OcrRecognize,
        )
        .unwrap();
    assert!(!copied.verified);
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
    for mode in [SnippetKind::OcrRecognize, SnippetKind::OcrWait] {
        let refusal = app
            .recognition_copy(
                &reopened.owner,
                &reopened.revision,
                restored.document_revision,
                "zone0",
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
                "retained",
                SnippetKind::OcrRecognize
            )
            .is_err()
    );
}

#[test]
fn reloaded_geometry_requires_confirmation_even_when_dimensions_match() {
    let editor = Editor::new();
    let first = editor.load();
    assert!(first.frame.unwrap().confirmed);
    let replacement = editor.load();
    let frame = replacement.frame.unwrap();
    assert!(!frame.confirmed);
    let confirmed = editor
        .app()
        .recognition_confirm(
            &editor.view.owner,
            &editor.view.revision,
            &frame.id,
            replacement.document_revision,
        )
        .unwrap();
    assert!(confirmed.frame.unwrap().confirmed);
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
            "saved",
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
    let copied = app
        .recognition_copy(
            &editor.view.owner,
            &revision,
            edited.document_revision,
            "new",
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
