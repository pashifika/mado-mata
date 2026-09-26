use super::*;
use crate::images::{DecodedImage, PayloadBytes, encode_crop};
use crate::inventory::Inventory;
use crate::model::Plan;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn document() -> RecognitionDocument {
    RecognitionDocument {
        version: 1,
        rounding: 1,
        basis: GeometryBasis {
            frame_width: 8,
            frame_height: 8,
            content: PixelRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
        },
        definitions: vec![RecognitionDefinition {
            id: "label".into(),
            name: "Label".into(),
            revision: 1,
            kind: RecognitionKind::Ocr,
            region: NormalizedRect {
                u0: 0.0,
                v0: 0.0,
                u1: 0.25,
                v1: 0.25,
            },
            expected: None,
            template: None,
            saved: None,
        }],
        template_rights: None,
    }
}

fn png() -> PayloadBytes {
    let image = DecodedImage::from_rgba(2, 2, vec![128; 16]).unwrap();
    let (bytes, reservation) = encode_crop(&image, [0, 0, 2, 2]).unwrap().into_parts();
    PayloadBytes::from_reserved(bytes, reservation).unwrap()
}

fn template_document() -> (RecognitionDocument, PayloadBytes) {
    let mut document = document();
    let bytes = png();
    document.definitions[0].kind = RecognitionKind::Template;
    document.definitions[0].saved =
        Some(SavedCrop::from_png("recognition_crop_label".into(), &bytes).unwrap());
    document.definitions[0].template = Some(TemplateSettings {
        search_region: NormalizedRect {
            u0: 0.0,
            v0: 0.0,
            u1: 1.0,
            v1: 1.0,
        },
        threshold: 0.9,
        max_results: 8,
    });
    document.template_rights = Some(TemplateRights {
        license: "CC0-1.0".into(),
        created_by: "regression test".into(),
        created_for: None,
        reviewed: true,
    });
    (document, bytes)
}

fn inventory(document: &RecognitionDocument, png: PayloadBytes) -> Inventory {
    let saved = document.definitions[0].saved.as_ref().unwrap();
    let (maps, engine) = build_template_assets(document, "sample").unwrap().unwrap();
    let declarations = json!({
        (AUTHORING_ASSET):{"path":"recognition/authoring.json","format":"json","width":0,"height":0},
        (TEMPLATE_MAPS_ASSET):{"path":"recognition/maps.json","format":"json","width":0,"height":0},
        (ENGINE_MANIFEST_ASSET):{"path":"recognition/engine.json","format":"json","width":0,"height":0},
        (saved.asset.clone()):{"path":"recognition/pattern.png","format":"png","width":2,"height":2}
    });
    let manifest = json!({
        "version":1,"package_id":"sample","runtime":"typescript","sdk":"mado-host-v1","entry_contract":"ready-string-v1",
        "entries":{"readiness":{"module":"main.ts","function":"readiness"},"workflow":{"module":"main.ts","function":"workflow"}},
        "sources":["main.ts"],"schema":"schema.json","profiles":{"default":"profile.json"},"assets":declarations,"source_maps":{},"dependencies":{}
    });
    let json_bytes = |value: Value| PayloadBytes::new(serde_json::to_vec(&value).unwrap()).unwrap();
    let files = BTreeMap::from([
        ("package.json".into(), json_bytes(manifest)),
        ("main.ts".into(), PayloadBytes::new(b"export function readiness(): MadoReady { return \"Ready\"; }\nexport function workflow(): void {}\n".to_vec()).unwrap()),
        ("schema.json".into(), json_bytes(json!({"version":1,"type":"object","additionalProperties":false,"required":[],"properties":{}}))),
        ("profile.json".into(), json_bytes(json!({"package_id":"sample","schema_version":1,"options":{}}))),
        ("recognition/authoring.json".into(), PayloadBytes::new(document.to_bytes().unwrap()).unwrap()),
        ("recognition/maps.json".into(), json_bytes(serde_json::to_value(maps).unwrap())),
        ("recognition/engine.json".into(), PayloadBytes::new(engine).unwrap()),
        ("recognition/pattern.png".into(), png),
    ]);
    let plan: Plan = serde_json::from_str(include_str!("../../fixtures/manual-plan.json")).unwrap();
    PackageDraft::from_files(files, &plan.limits)
        .unwrap()
        .validate()
        .unwrap()
}

#[test]
fn normalized_regions_preserve_black_bar_offsets_and_cover_fractional_edges() {
    let basis = GeometryBasis {
        frame_width: 100,
        frame_height: 80,
        content: PixelRect {
            x: 7,
            y: 11,
            width: 83,
            height: 61,
        },
    };
    let region = NormalizedRect {
        u0: 0.1,
        v0: 0.2,
        u1: 0.9,
        v1: 1.0,
    };
    assert_eq!(
        region.map_to_pixels(&basis).unwrap(),
        PixelRect {
            x: 15,
            y: 23,
            width: 67,
            height: 49
        }
    );
    for bad in [
        NormalizedRect {
            u0: f64::NAN,
            ..region
        },
        NormalizedRect {
            v1: f64::INFINITY,
            ..region
        },
        NormalizedRect {
            u0: -0.01,
            ..region
        },
        NormalizedRect { u1: 1.01, ..region },
        NormalizedRect {
            u1: region.u0,
            ..region
        },
    ] {
        assert!(bad.map_to_pixels(&basis).is_err());
    }
    assert!(
        PixelRect {
            x: u32::MAX,
            y: 0,
            width: 1,
            height: 1
        }
        .validate_in(u32::MAX, 1)
        .is_err()
    );
    assert!(
        GeometryBasis {
            content: PixelRect {
                x: 80,
                width: 30,
                ..basis.content
            },
            ..basis
        }
        .validate()
        .is_err()
    );
}

#[test]
fn metadata_versions_budgets_and_normalized_float_identity_are_preserved() {
    let mut document = document();
    document.definitions[0].region.u0 = f64::from_bits((1.0_f64 / 8.0).to_bits() + 1);
    let roundtrip = RecognitionDocument::from_bytes(&document.to_bytes().unwrap()).unwrap();
    assert_eq!(roundtrip, document);
    let original = document.definitions[0].clone();
    document.definitions = (0..9)
        .map(|index| RecognitionDefinition {
            id: format!("zone{index}"),
            ..original.clone()
        })
        .collect();
    assert_eq!(
        RecognitionDocument::from_bytes(&document.to_bytes().unwrap()).unwrap(),
        document
    );
    document.rounding = 2;
    assert_eq!(
        document.validate().unwrap_err().category,
        "RecognitionVersion"
    );
    document.rounding = 1;
    document.version = 2;
    assert_eq!(
        document.validate().unwrap_err().category,
        "RecognitionVersion"
    );
    let mut future = serde_json::to_value(&document).unwrap();
    future["future_field"] = json!({"coordinate_system":"future"});
    let future_bytes = serde_json::to_vec(&future).unwrap();
    assert_eq!(
        RecognitionDocument::from_bytes(&future_bytes)
            .unwrap_err()
            .category,
        "RecognitionVersion"
    );
    document.version = 1;
    document.definitions = (0..MAX_DEFINITIONS)
        .map(|index| RecognitionDefinition {
            id: format!("zone{index}"),
            expected: Some("x".repeat(MAX_EXPECTED_BYTES)),
            ..original.clone()
        })
        .collect();
    assert!(document.validate().is_err());
    assert!(document.to_bytes().is_err());
    document.definitions = vec![original];
    document.definitions[0].expected = Some("x".repeat(MAX_EXPECTED_BYTES + 1));
    assert!(document.validate().is_err());
}

#[test]
fn declared_templates_refuse_missing_or_stale_references_and_merge_atomically() {
    let (document, png) = template_document();
    let mut inventory = inventory(&document, png);
    assert_eq!(
        validate_inventory(&inventory).unwrap(),
        Some(document.clone())
    );
    let saved = document.definitions[0].saved.as_ref().unwrap();
    let mut entries = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    merge_effective_template_maps(&inventory, &mut entries, &mut aliases).unwrap();
    assert_eq!(
        aliases[&saved.asset],
        format!("recognition.{}", saved.asset)
    );
    let compatible = (entries.clone(), aliases.clone());
    merge_effective_template_maps(&inventory, &mut entries, &mut aliases).unwrap();
    assert_eq!((entries.clone(), aliases.clone()), compatible);
    aliases.insert(saved.asset.clone(), "another-template".into());
    let before = (entries.clone(), aliases.clone());
    assert!(merge_effective_template_maps(&inventory, &mut entries, &mut aliases).is_err());
    assert_eq!((entries, aliases), before);
    let removed = inventory.assets.remove(&saved.asset).unwrap();
    assert!(validate_inventory(&inventory).is_err());
    inventory.assets.insert(saved.asset.clone(), removed);
    let mut stale = document.clone();
    stale.definitions[0].saved.as_mut().unwrap().sha256 = "0".repeat(64);
    inventory.assets.insert(
        AUTHORING_ASSET.into(),
        PayloadBytes::new(stale.to_bytes().unwrap()).unwrap(),
    );
    assert!(validate_inventory(&inventory).is_err());
    stale = document.clone();
    stale.definitions[0].saved.as_mut().unwrap().width = 1;
    inventory.assets.insert(
        AUTHORING_ASSET.into(),
        PayloadBytes::new(stale.to_bytes().unwrap()).unwrap(),
    );
    assert!(validate_inventory(&inventory).is_err());
    let mut unreviewed = document;
    unreviewed.template_rights.as_mut().unwrap().reviewed = false;
    assert!(build_template_assets(&unreviewed, "sample").is_err());
}

#[test]
fn saved_samples_use_the_entire_crop_without_remapping_the_source_zone() {
    let png = png();
    let mut document = document();
    document.definitions[0].saved = Some(SavedCrop::from_png("sample".into(), &png).unwrap());
    let saved = document.definitions[0].saved.as_ref().unwrap();
    assert_eq!(
        saved.sample_rect(),
        PixelRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2
        }
    );
    let region = document.definitions[0].region;
    document.basis.content = PixelRect {
        x: 2,
        y: 2,
        width: 4,
        height: 4,
    };
    assert_eq!(
        document.definitions[0]
            .saved
            .as_ref()
            .unwrap()
            .sample_rect(),
        PixelRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2
        }
    );
    assert_eq!(
        region.map_to_pixels(&document.basis).unwrap(),
        PixelRect {
            x: 2,
            y: 2,
            width: 1,
            height: 1
        }
    );
}

#[test]
fn copied_wait_preserves_script_text_and_releases_transferred_observations_on_every_exit() {
    let mut document = document();
    let expected = "日本語\"\\\n\r\t\u{2028}\u{2029}";
    document.definitions[0].expected = Some(expected.into());
    let snippet = generate_snippet(
        &document,
        "label",
        SnippetKind::OcrWait,
        "mado-host-v1",
        true,
        false,
        1000,
    )
    .unwrap();
    // This is a resource-owning SDK protocol harness, not an OCR oracle. Each
    // scenario represents a distinct handle transfer/error boundary.
    for scenario in [
        "success",
        "shared",
        "timeout",
        "retired-timeout",
        "query-error",
        "geometry",
        "result-release-error",
    ] {
        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        context.with(|ctx| {
            ctx.globals().set("scenario", scenario).unwrap();
            ctx.eval::<(), _>(r#"
                const held = new Set();
                let submittedExpected = null;
                let failure = null;
                const initial = { id: 'initial', width: 8, height: 8 };
                const host = { call(method, args) {
                    if (method === 'observe') { held.add(initial.id); return initial; }
                    if (method === 'query') {
                        submittedExpected = args.expected;
                        if (scenario === 'query-error') throw { category: 'QueryError' };
                        held.add('query'); return { id: 'query' };
                    }
                    if (method === 'query_wait') {
                        if (scenario === 'timeout' || scenario === 'retired-timeout') {
                            if (scenario === 'retired-timeout') held.delete('query');
                            throw { category: 'Timeout' };
                        }
                        if (scenario === 'shared') return { id: 'query', observation: { id: 'query', width: 8, height: 8 } };
                        held.add('result'); held.add('wait-observation');
                        return { id: 'result', observation: { id: 'wait-observation', width: scenario === 'geometry' ? 9 : 8, height: 8 } };
                    }
                    if (method === 'release') {
                        if (!held.delete(args.id)) throw { category: 'InvalidHandle' };
                        if (scenario === 'result-release-error' && args.id === 'result') throw { category: 'ReleaseError' };
                        return { released: true };
                    }
                    throw new Error('unrecognized SDK method');
                }};
            "#).unwrap();
            ctx.eval::<(), _>(format!("try {{ {snippet} }} catch (error) {{ failure = error.category || error.message; }}")).unwrap();
            assert_eq!(ctx.eval::<String, _>("JSON.stringify(Array.from(held))").unwrap(), "[]", "{scenario}");
            assert_eq!(ctx.eval::<String, _>("submittedExpected").unwrap(), expected);
            let failed: bool = ctx.eval("failure !== null").unwrap();
            assert_eq!(failed, !matches!(scenario, "success" | "shared"), "{scenario}");
            if matches!(scenario, "timeout" | "retired-timeout") {
                assert_eq!(ctx.eval::<String, _>("failure").unwrap(), "Timeout");
            }
        });
    }
}

#[test]
fn copied_diagnostics_release_match_and_no_match_and_refuse_unconfirmed_geometry() {
    let document = document();
    assert!(
        generate_snippet(
            &document,
            "label",
            SnippetKind::OcrRecognize,
            "mado-host-v1",
            false,
            false,
            1000
        )
        .is_err()
    );
    let snippet = generate_snippet(
        &document,
        "label",
        SnippetKind::OcrRecognize,
        "mado-host-v1",
        true,
        false,
        1000,
    )
    .unwrap();
    for scenario in ["match", "no-match", "recognition-error", "geometry"] {
        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        context.with(|ctx| {
            ctx.globals().set("scenario", scenario).unwrap();
            ctx.eval::<(), _>(r#"
                const held = new Set(); let recognized = 0;
                const host = { call(method, args) {
                    if (method === 'observe') { held.add('observation'); return {id:'observation',width:scenario==='geometry'?9:8,height:8}; }
                    if (method === 'recognize') {
                        recognized++;
                        if ('expected' in args) throw new Error('unsupported expected argument');
                        if (scenario === 'recognition-error') throw new Error('recognition failed');
                        if (scenario === 'no-match') return null;
                        held.add('result'); return {id:'result'};
                    }
                    if (method === 'release') { if (!held.delete(args.id)) throw new Error('unknown handle'); return {released:true}; }
                    throw new Error('unsupported method');
                }};
            "#).unwrap();
            ctx.eval::<(), _>(format!("try {{ {snippet} }} catch (_) {{}}" )).unwrap();
            assert_eq!(ctx.eval::<String, _>("JSON.stringify(Array.from(held))").unwrap(), "[]", "{scenario}");
            assert_eq!(ctx.eval::<u32, _>("recognized").unwrap(), u32::from(scenario != "geometry"));
        });
    }
}

#[test]
fn all_copy_purposes_typecheck_against_the_actual_sdk_and_template_alias_resolves() {
    let (mut document, png) = template_document();
    let mut ocr = super::tests::document().definitions.remove(0);
    ocr.id = "ocr".into();
    ocr.expected = Some("author's Script criterion".into());
    document.definitions.push(ocr);
    let mut inventory = inventory(&document, png);
    let mut body = String::new();
    for (id, kind) in [
        ("ocr", SnippetKind::OcrRecognize),
        ("ocr", SnippetKind::OcrWait),
        ("label", SnippetKind::TemplateRecognize),
    ] {
        body.push_str(
            &generate_snippet(&document, id, kind, "mado-host-v1", true, true, 1000).unwrap(),
        );
    }
    inventory.sources.insert("main.ts".into(), format!("export function readiness(): MadoReady {{return \"Ready\";}}\nexport function workflow(): void {{\n{body}\n}}"));
    inventory.refresh_identity().unwrap();
    let plan: Plan = serde_json::from_str(include_str!("../../fixtures/manual-plan.json")).unwrap();
    let compiled = crate::typescript::compile(&inventory, &plan.limits).unwrap();
    let mut entries = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    merge_effective_template_maps(&compiled, &mut entries, &mut aliases).unwrap();
    let asset = &document.definitions[0].saved.as_ref().unwrap().asset;
    let engine: Value =
        serde_json::from_slice(&compiled.assets[&entries[ENGINE_MANIFEST_PATH]]).unwrap();
    let template = engine["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"].as_str() == Some(aliases[asset].as_str()))
        .unwrap();
    assert_eq!(entries[template["path"].as_str().unwrap()], *asset);
    assert_eq!(
        template["content"]["value"],
        png_digest(&compiled.assets[asset])
    );
}
