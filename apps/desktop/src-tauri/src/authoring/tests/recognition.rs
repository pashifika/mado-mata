use super::*;
use mado_runtime_comparison::images::{DecodedImage, PayloadBytes, encode_crop};
use mado_runtime_comparison::recognition::{
    AUTHORING_ASSET, CaptureDocument, ENGINE_MANIFEST_ASSET, GeometryBasis, NormalizedRect,
    PixelRect, RecognitionDefinition, RecognitionDocument, RecognitionKind, RecognitionPackage,
    TEMPLATE_MAPS_ASSET, TemplateRights, TemplateSettings, merge_effective_template_maps,
    png_digest,
};
use std::collections::BTreeMap;

const CAPTURE: &str = "00000000000000000000";

fn capture_package(document: RecognitionDocument) -> RecognitionPackage {
    RecognitionPackage {
        captures: vec![CaptureDocument {
            capture_id: CAPTURE.into(),
            document,
        }],
        ..RecognitionPackage::default()
    }
}

fn saved_document(candidate: &Candidate) -> RecognitionDocument {
    candidate
        .recognition()
        .unwrap()
        .unwrap()
        .document(CAPTURE)
        .unwrap()
        .clone()
}

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
            RecognitionSave {
                package: capture_package(document),
                capture_id: CAPTURE.into(),
                crops,
            },
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
    assert_eq!(saved_document(&reopened), metadata);
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
    let metadata = saved_document(&saved);
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
    assert_eq!(saved_document(&renamed), metadata);
    assert!(!renamed.root().join(path).exists());
    assert_eq!(
        png_digest(
            &renamed
                .recognition_crop(CAPTURE, "region")
                .unwrap()
                .unwrap()
        ),
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
    let mut metadata = saved_document(&saved);
    let old = metadata.definitions[0].saved.clone().unwrap();
    metadata.definitions.push(RecognitionDefinition {
        id: "shared".into(),
        ..metadata.definitions[0].clone()
    });
    let shared = save(&fixture, &saved, metadata, Vec::new());
    let mut metadata = saved_document(&shared);
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
    let metadata = saved_document(&replaced);
    assert_eq!(
        saved_path(&replaced, "shared").1,
        "recognition/crops/0001.png"
    );
    assert_eq!(
        saved_path(&replaced, "region").1,
        "recognition/crops/0002.png"
    );
    assert_eq!(metadata.definitions[1].saved.as_ref(), Some(&old));
    assert_ne!(
        metadata.definitions[0].saved.as_ref().unwrap().asset,
        old.asset
    );
    assert_eq!(
        png_digest(
            &replaced
                .recognition_crop(CAPTURE, "shared")
                .unwrap()
                .unwrap()
        ),
        old.sha256
    );
    assert_ne!(
        png_digest(
            &replaced
                .recognition_crop(CAPTURE, "region")
                .unwrap()
                .unwrap()
        ),
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
                    package: capture_package(forged),
                    capture_id: CAPTURE.into(),
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
    let mut metadata = document(512);
    metadata.definitions.push(RecognitionDefinition {
        id: "second".into(),
        ..metadata.definitions[0].clone()
    });
    let edit = Edit::Recognition(RecognitionSave {
        package: capture_package(metadata),
        capture_id: CAPTURE.into(),
        crops: vec![
            SelectedCrop {
                definition_id: "region".into(),
                png: png.clone(),
            },
            SelectedCrop {
                definition_id: "second".into(),
                png,
            },
        ],
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
    let metadata = saved_document(&recovered);
    assert_eq!(
        saved_path(&recovered, "region").1,
        "recognition/crops/0001.png"
    );
    assert_eq!(
        saved_path(&recovered, "second").1,
        "recognition/crops/0002.png"
    );
    assert_ne!(
        saved_path(&recovered, "region").0,
        saved_path(&recovered, "second").0
    );
    assert_eq!(
        png_digest(
            &recovered
                .recognition_crop(CAPTURE, "second")
                .unwrap()
                .unwrap()
        ),
        expected
    );
    assert_eq!(
        metadata.definitions[0].saved.as_ref().unwrap().sha256,
        expected
    );
    assert_eq!(
        png_digest(
            &recovered
                .recognition_crop(CAPTURE, "region")
                .unwrap()
                .unwrap()
        ),
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
                    package: capture_package(document(512)),
                    capture_id: CAPTURE.into(),
                    crops: Vec::new()
                }
            )
            .is_err()
    );
}

fn add_png(fixture: &Fixture, candidate: &Candidate, id: &str, path: &str) -> Candidate {
    fixture.catalog(
        candidate,
        CatalogEdit::Add {
            path: path.into(),
            file_kind: CatalogFileKind::Asset,
            text: None,
            bytes: Some(crop(2, vec![10; 16]).to_vec()),
            id: Some(id.into()),
            module: None,
            format: Some("png".into()),
            width: Some(2),
            height: Some(2),
        },
    )
}

fn saved_path(candidate: &Candidate, definition_id: &str) -> (String, String) {
    let document = saved_document(&candidate);
    let asset = document
        .definition(definition_id)
        .unwrap()
        .saved
        .as_ref()
        .unwrap()
        .asset
        .clone();
    let manifest = candidate.draft.manifest().unwrap();
    let path = manifest["assets"][&asset]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    (asset, path)
}

#[test]
fn crop_batches_use_numeric_inventory_maximum_and_submission_order() {
    for (paths, first) in [
        (vec![], 1),
        (
            vec![
                "recognition/crops/named.png",
                "recognition/crops/nested/9000.png",
            ],
            1,
        ),
        (
            vec![
                "recognition/crops/0001.png",
                "recognition/crops/0007.png",
                "recognition/crops/0010.png",
            ],
            11,
        ),
        (
            vec!["recognition/crops/9999.png", "recognition/crops/10000.PNG"],
            10001,
        ),
    ] {
        let fixture = Fixture::new();
        let mut original = fixture.package();
        for (index, path) in paths.iter().enumerate() {
            original = add_png(&fixture, &original, &format!("unrelated{index}"), path);
        }
        let before = original.draft.files().clone();
        let mut metadata = document(2);
        metadata.definitions.push(RecognitionDefinition {
            id: "second".into(),
            ..metadata.definitions[0].clone()
        });
        let saved = save(
            &fixture,
            &original,
            metadata,
            vec![
                SelectedCrop {
                    definition_id: "second".into(),
                    png: crop(2, vec![20; 16]),
                },
                SelectedCrop {
                    definition_id: "region".into(),
                    png: crop(2, vec![30; 16]),
                },
            ],
        );
        let (second_id, second_path) = saved_path(&saved, "second");
        let (first_id, first_path) = saved_path(&saved, "region");
        crate::storage::validate_id(&second_id).unwrap();
        crate::storage::validate_id(&first_id).unwrap();
        assert_ne!(second_id, first_id);
        assert_eq!(second_path, format!("recognition/crops/{first:04}.png"));
        assert_eq!(
            first_path,
            format!("recognition/crops/{:04}.png", first + 1)
        );
        for path in paths {
            assert_eq!(saved.draft.files()[path], before[path]);
        }
        let reopened = fixture.publisher.open(saved.root()).unwrap();
        assert_eq!(saved_path(&reopened, "region"), (first_id, first_path));
        reopened.validate().unwrap();
    }
}

#[test]
fn renamed_and_deleted_highest_crops_reuse_paths_but_never_asset_identity() {
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
    let (original_id, original_path) = saved_path(&saved, "region");
    assert_eq!(original_path, "recognition/crops/0001.png");
    let renamed = fixture.catalog(
        &saved,
        CatalogEdit::Rename {
            path: original_path.clone(),
            destination: "recognition/crops/named.png".into(),
        },
    );
    let mut metadata = saved_document(&renamed);
    metadata.definitions.push(RecognitionDefinition {
        id: "second".into(),
        saved: None,
        ..metadata.definitions[0].clone()
    });
    let updated = save(
        &fixture,
        &renamed,
        metadata,
        vec![
            SelectedCrop {
                definition_id: "region".into(),
                png: crop(2, vec![20; 16]),
            },
            SelectedCrop {
                definition_id: "second".into(),
                png: crop(2, vec![30; 16]),
            },
        ],
    );
    assert_eq!(
        saved_path(&updated, "region"),
        (original_id.clone(), "recognition/crops/named.png".into())
    );
    let (second_id, second_path) = saved_path(&updated, "second");
    assert_ne!(second_id, original_id);
    assert_eq!(second_path, original_path);
    let mut metadata = saved_document(&updated);
    metadata.definitions.retain(|item| item.id != "second");
    let deleted = save(&fixture, &updated, metadata, Vec::new());
    assert!(!deleted.root().join(&second_path).exists());
    let mut metadata = saved_document(&deleted);
    metadata.definitions.push(RecognitionDefinition {
        id: "second".into(),
        saved: None,
        ..metadata.definitions[0].clone()
    });
    let recreated = save(
        &fixture,
        &deleted,
        metadata,
        vec![SelectedCrop {
            definition_id: "second".into(),
            png: crop(2, vec![40; 16]),
        }],
    );
    let (recreated_id, recreated_path) = saved_path(&recreated, "second");
    assert_ne!(recreated_id, second_id);
    assert_eq!(recreated_path, original_path);
    assert_eq!(
        saved_path(&recreated, "region"),
        (original_id, "recognition/crops/named.png".into())
    );
    recreated.validate().unwrap();
}

#[test]
fn overflowing_numeric_names_refuse_without_publishing_any_batch_bytes() {
    for stem in ["18446744073709551615", "18446744073709551616"] {
        let fixture = Fixture::new();
        let original = fixture.package();
        let original = add_png(
            &fixture,
            &original,
            "unrelated",
            &format!("recognition/crops/{stem}.png"),
        );
        let fault = fixture
            .publisher
            .publish_recognition(
                &original,
                original.revision(),
                RecognitionSave {
                    package: capture_package(document(2)),
                    capture_id: CAPTURE.into(),
                    crops: vec![SelectedCrop {
                        definition_id: "region".into(),
                        png: crop(2, vec![20; 16]),
                    }],
                },
            )
            .unwrap_err();
        assert_eq!(fault.category, "RecognitionSave");
        let reopened = fixture.publisher.open(original.root()).unwrap();
        assert_eq!(reopened.revision(), original.revision());
        assert_eq!(reopened.draft.files(), original.draft.files());
        assert!(reopened.recognition().unwrap().is_none());
    }
}

#[test]
fn stale_inventory_refuses_then_explicit_save_recomputes_from_new_revision() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let changed = add_png(
        &fixture,
        &original,
        "unrelated",
        "recognition/crops/0042.png",
    );
    let make_save = || RecognitionSave {
        package: capture_package(document(2)),
        capture_id: CAPTURE.into(),
        crops: vec![SelectedCrop {
            definition_id: "region".into(),
            png: crop(2, vec![20; 16]),
        }],
    };
    let fault = fixture
        .publisher
        .publish_recognition(&original, original.revision(), make_save())
        .unwrap_err();
    assert_eq!(fault.category, "AuthoringConflict");
    assert_eq!(
        fixture.publisher.open(original.root()).unwrap().revision(),
        changed.revision()
    );
    let saved = fixture
        .publisher
        .publish_recognition(&changed, changed.revision(), make_save())
        .unwrap()
        .candidate
        .unwrap();
    assert_eq!(saved_path(&saved, "region").1, "recognition/crops/0043.png");
    assert_eq!(
        saved.draft.files()["recognition/crops/0042.png"],
        changed.draft.files()["recognition/crops/0042.png"]
    );
}

#[test]
fn different_captures_fork_shared_crops_and_keep_package_wide_numbering() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let mut a = document(2);
    a.definitions[0].id = "r1".into();
    let first = save(
        &fixture,
        &original,
        a,
        vec![SelectedCrop {
            definition_id: "r1".into(),
            png: crop(2, vec![10; 16]),
        }],
    );
    let mut package = capture_package(saved_document(&first));
    let old = package.captures[0].document.definitions[0]
        .saved
        .clone()
        .unwrap();
    let mut b = package.captures[0].document.clone();
    b.basis.frame_width = 3;
    b.basis.frame_height = 3;
    b.basis.content.width = 3;
    b.basis.content.height = 3;
    let b_id = "0000000000000000000g";
    package.captures.push(CaptureDocument {
        capture_id: b_id.into(),
        document: b,
    });
    let second = fixture
        .publisher
        .publish_recognition(
            &first,
            first.revision(),
            RecognitionSave {
                package,
                capture_id: b_id.into(),
                crops: vec![SelectedCrop {
                    definition_id: "r1".into(),
                    png: crop(3, vec![20; 36]),
                }],
            },
        )
        .unwrap()
        .candidate
        .unwrap();
    let metadata = second.recognition().unwrap().unwrap();
    let a = metadata.document(CAPTURE).unwrap();
    let b = metadata.document(b_id).unwrap();
    assert_eq!(a.definitions[0].saved.as_ref(), Some(&old));
    assert_ne!(a.basis, b.basis);
    let fork = b.definitions[0].saved.as_ref().unwrap();
    assert_ne!(fork.asset, old.asset);
    let manifest = second.draft.manifest().unwrap();
    assert_eq!(
        manifest["assets"][&old.asset]["path"],
        "recognition/crops/0001.png"
    );
    assert_eq!(
        manifest["assets"][&fork.asset]["path"],
        "recognition/crops/0002.png"
    );
    assert_eq!(
        png_digest(&second.recognition_crop(CAPTURE, "r1").unwrap().unwrap()),
        old.sha256
    );
    assert_eq!(
        png_digest(&second.recognition_crop(b_id, "r1").unwrap().unwrap()),
        fork.sha256
    );
    let incomplete = fixture.publisher.publish_recognition(
        &second,
        second.revision(),
        RecognitionSave {
            package: capture_package(a.clone()),
            capture_id: CAPTURE.into(),
            crops: Vec::new(),
        },
    );
    assert!(
        incomplete.is_err(),
        "a selected-capture save cannot erase another capture"
    );
    assert_eq!(
        fixture.publisher.open(second.root()).unwrap().revision(),
        second.revision()
    );
    second.validate().unwrap();
}

#[test]
fn explicit_legacy_migration_preserves_references_and_recovers_one_complete_revision() {
    let fixture = Fixture::new();
    let original = fixture.package();
    let mut template = document(2);
    template.definitions[0].kind = RecognitionKind::Template;
    template.definitions[0].template = Some(TemplateSettings {
        search_region: template.definitions[0].region,
        threshold: 0.9,
        max_results: 8,
    });
    template.template_rights = Some(TemplateRights {
        license: "CC0-1.0".into(),
        created_by: "migration fixture".into(),
        created_for: None,
        reviewed: true,
    });
    let first = save(
        &fixture,
        &original,
        template,
        vec![SelectedCrop {
            definition_id: "region".into(),
            png: crop(2, vec![30; 16]),
        }],
    );
    let document = saved_document(&first);
    let legacy = document.to_bytes().unwrap();
    let path = first.root().join("recognition/authoring.json");
    fs::write(&path, &legacy).unwrap();
    let original = fixture.publisher.open(first.root()).unwrap();
    assert!(matches!(
        original.recognition().unwrap(),
        Some(mado_runtime_comparison::recognition::RecognitionMetadata::Legacy(_))
    ));
    assert_eq!(
        fs::read(&path).unwrap(),
        legacy,
        "opening must not migrate bytes"
    );
    let before = original.draft.files().clone();
    let interrupted = fixture.publisher.publish_with(
        &original,
        original.revision(),
        Edit::Recognition(RecognitionSave {
            package: capture_package(document.clone()),
            capture_id: CAPTURE.into(),
            crops: Vec::new(),
        }),
        |_| {
            Err(Fault::new(
                "Interrupted",
                "interrupt capture metadata migration",
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
    assert_eq!(saved_document(&recovered), document);
    assert!(matches!(
        recovered.recognition().unwrap(),
        Some(mado_runtime_comparison::recognition::RecognitionMetadata::Captures(_))
    ));
    for (path, bytes) in before
        .iter()
        .filter(|(path, _)| path.as_str() != "recognition/authoring.json")
    {
        assert_eq!(
            &recovered.draft.files()[path],
            bytes,
            "migration changed {path}"
        );
    }
    recovered.validate().unwrap();
}
