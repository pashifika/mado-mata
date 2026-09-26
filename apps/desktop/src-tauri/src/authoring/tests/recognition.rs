use super::*;
use mado_runtime_comparison::images::{DecodedImage, PayloadBytes, encode_crop};
use mado_runtime_comparison::recognition::{
    AUTHORING_ASSET, ENGINE_MANIFEST_ASSET, GeometryBasis, NormalizedRect, PixelRect,
    RecognitionDefinition, RecognitionDocument, RecognitionKind, TEMPLATE_MAPS_ASSET,
    TemplateRights, TemplateSettings, merge_effective_template_maps, png_digest,
};
use std::collections::BTreeMap;

fn document(side: u32) -> RecognitionDocument {
    RecognitionDocument {
        version: 1,
        rounding: 1,
        basis: GeometryBasis {
            frame_width: side,
            frame_height: side,
            content: PixelRect {
                x: 0,
                y: 0,
                width: side,
                height: side,
            },
        },
        definitions: vec![RecognitionDefinition {
            id: "region".into(),
            name: "Region".into(),
            revision: 1,
            kind: RecognitionKind::Ocr,
            region: NormalizedRect {
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
            },
            expected: None,
            template: None,
            saved: None,
        }],
        template_rights: None,
    }
}

fn crop(side: u32, pixels: Vec<u8>) -> PayloadBytes {
    let frame = DecodedImage::from_rgba(side, side, pixels).unwrap();
    let (bytes, reservation) = encode_crop(&frame, [0, 0, side, side])
        .unwrap()
        .into_parts();
    PayloadBytes::from_reserved(bytes, reservation).unwrap()
}

fn save(
    fixture: &Fixture,
    candidate: &Candidate,
    document: RecognitionDocument,
    crops: Vec<SelectedCrop>,
) -> Candidate {
    fixture
        .publisher
        .publish_recognition(
            candidate,
            candidate.revision(),
            RecognitionSave { document, crops },
        )
        .unwrap()
        .candidate
        .unwrap()
}

#[test]
fn metadata_only_save_reopens_more_than_one_scan_of_definitions_without_repairing_source() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let invalid_source = "export function workflow( {";
    let source = fixture.save(&original, "main.ts", invalid_source);
    let source = fixture.save(&source, "schema.json", "{");
    let mut metadata = document(8);
    let definition = metadata.definitions[0].clone();
    metadata.definitions = (0..9)
        .map(|index| RecognitionDefinition {
            id: format!("region{index}"),
            ..definition.clone()
        })
        .collect();
    let saved = save(&fixture, &source, metadata.clone(), Vec::new());
    let reopened = fixture.publisher.open(saved.root()).unwrap();
    assert_eq!(reopened.recognition().unwrap(), Some(metadata));
    assert_eq!(
        fs::read_to_string(reopened.root().join("main.ts")).unwrap(),
        invalid_source
    );
    assert_eq!(
        fs::read_to_string(reopened.root().join("schema.json")).unwrap(),
        "{"
    );
    assert!(reopened.validate().is_err());
    assert_eq!(
        reopened
            .draft
            .files()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "main.ts",
            "package.json",
            "profiles/default.json",
            "recognition/authoring.json",
            "schema.json"
        ]
    );
}

#[test]
fn crop_only_template_save_derives_identity_renames_coherently_and_refuses_referenced_removal() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let mut metadata = document(2);
    metadata.definitions[0].kind = RecognitionKind::Template;
    metadata.definitions[0].template = Some(TemplateSettings {
        search_region: metadata.definitions[0].region,
        threshold: 0.9,
        max_results: 8,
    });
    metadata.template_rights = Some(TemplateRights {
        license: "CC0-1.0".into(),
        created_by: "regression test".into(),
        created_for: None,
        reviewed: true,
    });
    let png = crop(2, vec![128; 16]);
    let digest = png_digest(&png);
    let saved = save(
        &fixture,
        &original,
        metadata,
        vec![SelectedCrop {
            definition_id: "region".into(),
            png,
        }],
    );
    let metadata = saved.recognition().unwrap().unwrap();
    let reference = metadata.definitions[0].saved.as_ref().unwrap();
    assert_eq!(
        (&reference.sha256, reference.width, reference.height),
        (&digest, 2, 2)
    );
    let manifest = saved.draft.manifest().unwrap();
    let path = manifest["assets"][&reference.asset]["path"]
        .as_str()
        .unwrap();
    let renamed = fixture.catalog(
        &saved,
        CatalogEdit::Rename {
            path: path.into(),
            destination: "recognition/renamed.png".into(),
        },
    );
    assert_eq!(renamed.recognition().unwrap(), Some(metadata.clone()));
    assert!(!renamed.root().join(path).exists());
    assert_eq!(
        png_digest(&renamed.recognition_crop("region").unwrap().unwrap()),
        digest
    );
    let inventory = renamed.validate().unwrap();
    let mut entries = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    merge_effective_template_maps(&inventory, &mut entries, &mut aliases).unwrap();
    assert_eq!(
        aliases[&reference.asset],
        format!("recognition.{}", reference.asset)
    );
    assert!(
        fixture
            .publisher
            .publish(
                &renamed,
                renamed.revision(),
                Edit::Catalog(CatalogEdit::Remove {
                    path: "recognition/renamed.png".into()
                })
            )
            .is_err()
    );
    let mut empty = metadata;
    empty.definitions.clear();
    let deleted = save(&fixture, &renamed, empty, Vec::new());
    assert!(!deleted.root().join("recognition/renamed.png").exists());
    let manifest = deleted.draft.manifest().unwrap();
    assert!(manifest["assets"].get(TEMPLATE_MAPS_ASSET).is_none());
    assert!(manifest["assets"].get(ENGINE_MANIFEST_ASSET).is_none());
    assert_eq!(
        manifest["assets"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [AUTHORING_ASSET]
    );
}

#[test]
fn explicit_replacement_does_not_change_a_crop_retained_by_another_definition() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let saved = save(
        &fixture,
        &original,
        document(2),
        vec![SelectedCrop {
            definition_id: "region".into(),
            png: crop(2, vec![10; 16]),
        }],
    );
    let mut metadata = saved.recognition().unwrap().unwrap();
    let old = metadata.definitions[0].saved.clone().unwrap();
    metadata.definitions.push(RecognitionDefinition {
        id: "shared".into(),
        ..metadata.definitions[0].clone()
    });
    let shared = save(&fixture, &saved, metadata, Vec::new());
    let mut metadata = shared.recognition().unwrap().unwrap();
    metadata.definitions[0].revision = 2;
    let replaced = save(
        &fixture,
        &shared,
        metadata,
        vec![SelectedCrop {
            definition_id: "region".into(),
            png: crop(2, vec![90; 16]),
        }],
    );
    let metadata = replaced.recognition().unwrap().unwrap();
    assert_eq!(metadata.definitions[1].saved.as_ref(), Some(&old));
    assert_ne!(
        metadata.definitions[0].saved.as_ref().unwrap().asset,
        old.asset
    );
    assert_eq!(
        png_digest(&replaced.recognition_crop("shared").unwrap().unwrap()),
        old.sha256
    );
    assert_ne!(
        png_digest(&replaced.recognition_crop("region").unwrap().unwrap()),
        old.sha256
    );
    let mut forged = metadata;
    forged.definitions[0].saved.as_mut().unwrap().sha256 = "0".repeat(64);
    assert!(
        fixture
            .publisher
            .publish_recognition(
                &replaced,
                replaced.revision(),
                RecognitionSave {
                    document: forged,
                    crops: Vec::new()
                }
            )
            .is_err()
    );
    assert_eq!(
        fixture.publisher.open(replaced.root()).unwrap().revision(),
        replaced.revision()
    );
}

#[test]
fn image_sized_transaction_recovers_the_same_complete_revision_after_interruption() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let mut seed = 0x1842_9027_u32;
    let pixels = (0..512 * 512 * 4)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let png = crop(512, pixels);
    assert!(png.len() > MAX_BYTES);
    let expected = png_digest(&png);
    let edit = Edit::Recognition(RecognitionSave {
        document: document(512),
        crops: vec![SelectedCrop {
            definition_id: "region".into(),
            png,
        }],
    });
    let interrupted = fixture.publisher.publish_with(
        &original,
        original.revision(),
        edit,
        |_| {
            Err(Fault::new(
                "Interrupted",
                "stop after the first package mutation",
            ))
        },
        || Ok(()),
    );
    assert_eq!(
        interrupted.unwrap_err().category,
        "AuthoringRecoveryRequired"
    );
    assert!(fixture.publisher.open(original.root()).is_err());
    fixture.publisher.recover(original.root()).unwrap();
    let recovered = fixture.publisher.open(original.root()).unwrap();
    let metadata = recovered.recognition().unwrap().unwrap();
    assert_eq!(
        metadata.definitions[0].saved.as_ref().unwrap().sha256,
        expected
    );
    assert_eq!(
        png_digest(&recovered.recognition_crop("region").unwrap().unwrap()),
        expected
    );
    assert!(recovered.validate().is_ok());
    assert!(
        fixture
            .publisher
            .publish_recognition(
                &original,
                original.revision(),
                RecognitionSave {
                    document: document(512),
                    crops: Vec::new()
                }
            )
            .is_err()
    );
}
