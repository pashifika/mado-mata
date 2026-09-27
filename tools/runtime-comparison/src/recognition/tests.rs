use super::*;
use crate::images::{DecodedImage, PayloadBytes, encode_crop};
use crate::inventory::Inventory;
use crate::model::Plan;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

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
    let json_bytes = |value: Value| PayloadBytes::new(serde_json::to_vec(&value).unwrap()).unwrap();
    package(
        declarations,
        "export function readiness(): MadoReady { return \"Ready\"; }\nexport function workflow(): void {}\n".into(),
        [
            ("recognition/authoring.json".to_owned(), PayloadBytes::new(document.to_bytes().unwrap()).unwrap()),
            ("recognition/maps.json".to_owned(), json_bytes(serde_json::to_value(maps).unwrap())),
            ("recognition/engine.json".to_owned(), PayloadBytes::new(engine).unwrap()),
            ("recognition/pattern.png".to_owned(), png),
        ],
    )
}

fn package(
    declarations: Value,
    main: String,
    assets: impl IntoIterator<Item = (String, PayloadBytes)>,
) -> Inventory {
    let manifest = json!({
        "version":1,"package_id":"sample","runtime":"typescript","sdk":"mado-host-v1","entry_contract":"ready-string-v1",
        "entries":{"readiness":{"module":"main.ts","function":"readiness"},"workflow":{"module":"main.ts","function":"workflow"}},
        "sources":["main.ts"],"schema":"schema.json","profiles":{"default":"profile.json"},"assets":declarations,"source_maps":{},"dependencies":{}
    });
    let json_bytes = |value: Value| PayloadBytes::new(serde_json::to_vec(&value).unwrap()).unwrap();
    let mut files = BTreeMap::from([
        ("package.json".into(), json_bytes(manifest)),
        (
            "main.ts".into(),
            PayloadBytes::new(main.into_bytes()).unwrap(),
        ),
        (
            "schema.json".into(),
            json_bytes(
                json!({"version":1,"type":"object","additionalProperties":false,"required":[],"properties":{}}),
            ),
        ),
        (
            "profile.json".into(),
            json_bytes(json!({"package_id":"sample","schema_version":1,"options":{}})),
        ),
    ]);
    files.extend(assets);
    PackageDraft::from_files(files, &plan().limits)
        .unwrap()
        .validate()
        .unwrap()
}

fn plan() -> Plan {
    serde_json::from_str(include_str!("../../fixtures/manual-plan.json")).unwrap()
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
fn reference_text_survives_template_persistence_and_kind_roundtrip() {
    let (mut document, _) = template_document();
    let text = "Author reference: 日本語\n\"quoted\"";
    document.definitions[0].expected = Some(text.into());
    let mut reopened = RecognitionDocument::from_bytes(&document.to_bytes().unwrap()).unwrap();
    assert_eq!(reopened.definitions[0].expected.as_deref(), Some(text));
    reopened.definitions[0].kind = RecognitionKind::Ocr;
    reopened.definitions[0].template = None;
    let restored = RecognitionDocument::from_bytes(&reopened.to_bytes().unwrap()).unwrap();
    assert_eq!(restored.definitions[0].expected.as_deref(), Some(text));
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

const SDK: &str = "mado-host-v1";

fn setup_source(document: &RecognitionDocument) -> String {
    generate_snippet(
        document,
        &[],
        SnippetKind::GameContent,
        SDK,
        true,
        false,
        None,
    )
    .unwrap()
}

fn grouped_source(document: &RecognitionDocument, ids: &[&str], maximum: usize) -> String {
    let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    generate_snippet(
        document,
        &ids,
        SnippetKind::OcrRecognize,
        SDK,
        true,
        false,
        Some(maximum),
    )
    .unwrap()
}

/// Two OCR zones on a black-barred 640x480 frame: `label` covers the controlled
/// host's fixture text at (100, 80, 40, 20) and `miss` does not.
fn scene() -> RecognitionDocument {
    let mut document = document();
    document.basis = GeometryBasis {
        frame_width: 640,
        frame_height: 480,
        content: PixelRect {
            x: 40,
            y: 20,
            width: 560,
            height: 440,
        },
    };
    document.definitions[0].expected = Some("reference only".into());
    let mut miss = document.definitions[0].clone();
    miss.id = "miss".into();
    miss.name = "Miss".into();
    miss.expected = None;
    miss.region = NormalizedRect {
        u0: 0.5,
        v0: 0.5,
        u1: 1.0,
        v1: 1.0,
    };
    document.definitions.push(miss);
    document
}

// A resource-owning SDK protocol harness, not an OCR oracle: it records every host
// call and answers each requested zone with the scenario's outcome.
const HARNESS: &str = r#"
    const held = new Set();
    const calls = [];
    let failure = null;
    const host = { call(method, args) {
        calls.push([method, args]);
        if (method === 'observe') { held.add('observation'); return { id: 'observation', width: 640, height: 480 }; }
        if (method === 'scan_ocr_zones') {
            if (scenario === 'scan-error') throw { category: 'Backend' };
            return { kind: 'ocr', observation: args.observation, text_contract: 'engine', zones: args.zones.map(zone => ({
                id: zone.id, outcome: scenario === 'no-match' ? 'no_match' : 'recognized', regions: [] })) };
        }
        if (method === 'release') {
            if (!held.delete(args.id)) throw { category: 'InvalidHandle' };
            return { released: true };
        }
        throw new Error(`unsupported SDK method ${method}`);
    }};
    const canonical = value => JSON.stringify(value, (_, item) => item !== null && typeof item === 'object' && !Array.isArray(item)
        ? Object.fromEntries(Object.keys(item).sort().map(key => [key, item[key]])) : item);
"#;

fn run_harness(scenario: &str, source: &str, inspect: impl FnOnce(&rquickjs::Ctx<'_>)) {
    let runtime = rquickjs::Runtime::new().unwrap();
    let context = rquickjs::Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.globals().set("scenario", scenario).unwrap();
        ctx.eval::<(), _>(HARNESS).unwrap();
        ctx.eval::<(), _>(format!(
            "try {{\n{source}\n}} catch (error) {{ failure = error.category || error.message; }}"
        ))
        .unwrap();
        inspect(&ctx);
    });
}

#[test]
fn grouped_ocr_copy_sends_every_zone_in_one_request_and_releases_only_the_observation() {
    let document = scene();
    // Caller order is the request order, independent of document order.
    let source = format!(
        "{}{}",
        setup_source(&document),
        grouped_source(&document, &["miss", "label"], 2)
    );
    let expected = json!([
        ["observe", {}],
        ["scan_ocr_zones", {
            "observation": {"id": "observation", "width": 640, "height": 480},
            "basis": document.basis,
            "zones": [
                {"id": "miss", "region": document.definitions[1].region},
                {"id": "label", "region": document.definitions[0].region},
            ],
        }],
        ["release", {"id": "observation"}],
    ])
    .to_string();
    for scenario in ["recognized", "no-match", "scan-error"] {
        run_harness(scenario, &source, |ctx| {
            ctx.globals().set("expected", expected.as_str()).unwrap();
            assert!(
                ctx.eval::<bool, _>("canonical(calls) === canonical(JSON.parse(expected))")
                    .unwrap(),
                "{scenario}: {}",
                ctx.eval::<String, _>("canonical(calls)").unwrap()
            );
            assert_eq!(
                ctx.eval::<String, _>("JSON.stringify(Array.from(held))")
                    .unwrap(),
                "[]",
                "{scenario}"
            );
            let failure: Option<String> = ctx.eval("failure").unwrap();
            assert_eq!(
                failure.as_deref(),
                (scenario == "scan-error").then_some("Backend"),
                "{scenario}"
            );
        });
    }
}

#[test]
fn copied_names_and_reference_text_stay_inert_comments_and_regions_keep_exact_bits() {
    let mut document = scene();
    let hostile: String = ['\n', '\r', '\u{2028}', '\u{2029}']
        .into_iter()
        .map(|separator| {
            format!("\"*/\\{separator}host.call(\"log\", {{ message: \"leaked\" }});{separator}")
        })
        .collect();
    document.definitions[0].name = format!("日本語{hostile}");
    document.definitions[0].expected = Some(hostile);
    let edges = [1.0 / 3.0, 1.0 / 7.0, 2.0 / 3.0, 5.0 / 7.0]
        .map(|edge: f64| f64::from_bits(edge.to_bits() + 1));
    document.definitions[0].region = NormalizedRect {
        u0: edges[0],
        v0: edges[1],
        u1: edges[2],
        v1: edges[3],
    };
    document.validate().unwrap();
    let source = format!(
        "{}{}",
        setup_source(&document),
        grouped_source(&document, &["label"], 1)
    );
    run_harness("recognized", &source, |ctx| {
        let failure: Option<String> = ctx.eval("failure").unwrap();
        assert_eq!(failure, None);
        assert_eq!(
            ctx.eval::<String, _>("calls.map(([method]) => method).join()")
                .unwrap(),
            "observe,scan_ocr_zones,release"
        );
        for (name, edge) in ["u0", "v0", "u1", "v1"].into_iter().zip(edges) {
            let sent: f64 = ctx
                .eval(format!("calls[1][1].zones[0].region.{name}"))
                .unwrap();
            assert_eq!(sent.to_bits(), edge.to_bits(), "{name}");
        }
    });
}

#[test]
fn copied_setup_and_grouped_ocr_compile_and_run_one_request_on_the_controlled_host() {
    let document = scene();
    let main = format!(
        "{}export function readiness(): MadoReady {{ return \"Ready\"; }}\nexport function workflow(): void {{\n{}}}\n",
        setup_source(&document),
        grouped_source(&document, &["label", "miss"], 2)
    );
    let compiled = Arc::new(
        crate::typescript::compile(&package(json!({}), main, []), &plan().limits).unwrap(),
    );
    for scenario in ["success", "no-match", "backend-failure"] {
        let plan = Plan {
            scenario: scenario.into(),
            profile: "default".into(),
            ..plan()
        };
        let options = crate::host::resolve_options(
            &compiled.schema,
            &compiled.profiles["default"],
            &compiled.package_id,
        )
        .unwrap();
        let control = Arc::new(crate::model::Control::new(&plan.limits));
        let host = crate::host::Host::new(plan, options, compiled.assets.clone(), control).unwrap();
        let outcome = crate::javascript::run(compiled.clone(), host.clone());
        let facts = host.snapshot();
        let calls: BTreeMap<&str, u64> = facts["operation_metrics"]
            .as_object()
            .unwrap()
            .iter()
            .filter_map(|(name, metrics)| {
                let count = metrics["count"].as_u64().unwrap();
                (count > 0).then_some((name.as_str(), count))
            })
            .collect();
        assert_eq!(calls.get("scan_ocr_zones"), Some(&1), "{scenario}");
        assert!(
            calls
                .keys()
                .all(|name| matches!(*name, "observe" | "scan_ocr_zones" | "release")),
            "{calls:?}"
        );
        if scenario == "backend-failure" {
            // The host fault halts the Script; host teardown owns what it still holds.
            assert_eq!(outcome.unwrap_err().category, "Backend");
            assert_eq!(facts["operation_metrics"]["scan_ocr_zones"]["failures"], 1);
        } else {
            outcome.unwrap();
            // Readiness also observes and releases; only balanced ownership matters here.
            assert_eq!(calls.get("observe"), calls.get("release"), "{scenario}");
            assert_eq!(facts["live_handles"], 0, "{scenario}");
            assert_eq!(facts["attempt_owners"], 0, "{scenario}");
        }
        assert_eq!(host.finish()["clean"], true, "{scenario}");
        assert_eq!(host.snapshot()["live_handles"], 0, "{scenario}");
    }
}

#[test]
fn copied_template_releases_match_and_no_match_and_every_purpose_needs_confirmed_geometry() {
    let (template, _) = template_document();
    let ocr = document();
    for (document, ids, kind) in [
        (&ocr, vec![], SnippetKind::GameContent),
        (&ocr, vec!["label".to_owned()], SnippetKind::OcrRecognize),
        (
            &template,
            vec!["label".to_owned()],
            SnippetKind::TemplateRecognize,
        ),
    ] {
        assert!(generate_snippet(document, &ids, kind, SDK, false, true, Some(8)).is_err());
    }
    let snippet = generate_snippet(
        &template,
        &["label".into()],
        SnippetKind::TemplateRecognize,
        SDK,
        true,
        true,
        None,
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
                        if (args.kind !== 'template' || args.asset !== 'recognition_crop_label' || 'expected' in args) throw new Error('unexpected template request');
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
    ocr.expected = Some("author's Script reference".into());
    document.definitions.push(ocr);
    let mut second = document.definitions[1].clone();
    second.id = "ocr_second".into();
    second.expected = None;
    document.definitions.push(second);
    let mut inventory = inventory(&document, png);
    let setup = setup_source(&document);
    let grouped = grouped_source(&document, &["ocr_second", "ocr"], 2);
    let template = generate_snippet(
        &document,
        &["label".into()],
        SnippetKind::TemplateRecognize,
        SDK,
        true,
        true,
        None,
    )
    .unwrap();
    // Setup is module-scope data reused by every grouped block pasted after it.
    inventory.sources.insert("main.ts".into(), format!("{setup}export function readiness(): MadoReady {{return \"Ready\";}}\nexport function workflow(): void {{\n{grouped}{template}}}\n"));
    inventory.refresh_identity().unwrap();
    let compiled = crate::typescript::compile(&inventory, &plan().limits).unwrap();
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

#[test]
fn copy_validates_the_whole_request_and_engine_limit_before_returning_source() {
    let mut document = scene();
    let ids = vec!["label".to_owned(), "miss".to_owned()];
    let grouped = |document: &RecognitionDocument, ids: &[String], confirmed, maximum| {
        generate_snippet(
            document,
            ids,
            SnippetKind::OcrRecognize,
            SDK,
            confirmed,
            false,
            maximum,
        )
    };
    assert!(grouped(&document, &ids, true, Some(2)).is_ok());
    // Reference text is optional Script context, never a Copy requirement.
    document.definitions[0].expected = None;
    assert!(grouped(&document, &ids, true, Some(2)).is_ok());
    for selection in [
        vec![],
        vec!["label".to_owned(), "label".to_owned()],
        vec!["label".to_owned(), "missing".to_owned()],
    ] {
        assert!(grouped(&document, &selection, true, Some(8)).is_err());
    }
    assert!(
        grouped(&document, &ids, true, None).is_err(),
        "an unknown engine limit has no fallback"
    );
    assert!(
        grouped(&document, &ids, true, Some(1)).is_err(),
        "an oversized group is refused, never split"
    );
    assert!(grouped(&document, &ids, false, Some(2)).is_err());
    assert!(
        generate_snippet(
            &document,
            &ids,
            SnippetKind::OcrRecognize,
            "mado-host-v0",
            true,
            false,
            Some(2)
        )
        .is_err()
    );
    // Setup needs neither a selection nor the engine limit, and never names definitions.
    let setup = |ids: &[String]| {
        generate_snippet(
            &document,
            ids,
            SnippetKind::GameContent,
            SDK,
            true,
            false,
            None,
        )
    };
    assert!(setup(&[]).is_ok());
    assert!(setup(&ids[..1]).is_err());
    // The grouped request carries OCR only; a template stays a single saved-alias Copy.
    document.definitions[1].kind = RecognitionKind::Template;
    document.definitions[1].template = Some(TemplateSettings {
        search_region: NormalizedRect {
            u0: 0.0,
            v0: 0.0,
            u1: 1.0,
            v1: 1.0,
        },
        threshold: 0.9,
        max_results: 8,
    });
    document.validate().unwrap();
    assert!(grouped(&document, &ids, true, Some(2)).is_err());
    let template = |ids: &[String], saved| {
        generate_snippet(
            &document,
            ids,
            SnippetKind::TemplateRecognize,
            SDK,
            true,
            saved,
            None,
        )
    };
    assert!(template(&ids, true).is_err());
    assert!(template(&ids[1..], false).is_err());
    assert!(
        template(&ids[..1], true).is_err(),
        "an OCR definition is not a template"
    );
    // Invalid geometry anywhere in the document refuses the whole Copy.
    document.definitions[1].region.u1 = 2.0;
    assert!(grouped(&document, &ids[..1], true, Some(2)).is_err());
}
