use super::*;

fn request(zones: usize) -> WireRequest {
    WireRequest {
        identity: TrialIdentity {
            owner: "owner".into(),
            revision: "revision".into(),
            frame: "frame".into(),
            frame_revision: 1,
            content_revision: 1,
            zones_revision: 1,
            configuration_revision: "config".into(),
        },
        width: 10,
        height: 10,
        selection: WireSelection::Ocr {
            zones: (0..zones)
                .map(|index| OcrZone {
                    id: format!("zone-{index}"),
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 10,
                        height: 10,
                    },
                })
                .collect(),
        },
    }
}

#[test]
fn selection_is_nonempty_distinct_and_bounded_by_received_capability() {
    assert_eq!(
        request(0).validate(Some(3)).unwrap_err().category,
        "RecognitionRequest"
    );
    assert_eq!(request(3).validate(Some(3)).unwrap(), 400);
    assert_eq!(
        request(4).validate(Some(3)).unwrap_err().category,
        "RecognitionRequest"
    );
    let mut repeated = request(2);
    let WireSelection::Ocr { zones } = &mut repeated.selection else {
        unreachable!()
    };
    zones[1].id = zones[0].id.clone();
    assert_eq!(
        repeated.validate(Some(3)).unwrap_err().category,
        "RecognitionRequest"
    );
}

#[cfg(feature = "engine")]
#[test]
fn facade_group_limit_is_authoritative_and_never_hidden_batched() {
    let cap = capabilities().unwrap();
    assert_eq!(
        request(cap.max_ocr_zones)
            .validate(Some(mado_pilot::MAX_OCR_ZONES))
            .unwrap(),
        400
    );
    assert_eq!(
        request(cap.max_ocr_zones + 1)
            .validate(Some(mado_pilot::MAX_OCR_ZONES))
            .unwrap_err()
            .category,
        "RecognitionRequest"
    );
}

#[test]
fn rectangles_are_rejected_instead_of_clipped_or_wrapped() {
    let mut input = request(1);
    let WireSelection::Ocr { zones } = &mut input.selection else {
        unreachable!()
    };
    zones[0].rect = PixelRect {
        x: u32::MAX,
        y: 0,
        width: 2,
        height: 1,
    };
    assert_eq!(
        input.validate(Some(3)).unwrap_err().category,
        "RecognitionRequest"
    );
}

#[test]
fn diagnostic_overflow_never_turns_into_a_truncated_success() {
    let output = json!({"kind":"ocr","zones":[{"id":"zone","outcome":"recognized","regions":[
        {"text":"unexpected observation","confidence":0.25,"bounds":{"x":1,"y":2,"width":3,"height":4}}
    ]}]});
    assert_eq!(
        bounded_diagnostics(output.clone(), DIAGNOSTIC_REGIONS).unwrap(),
        output
    );
    assert_eq!(
        bounded_diagnostics(output, DIAGNOSTIC_REGIONS + 1)
            .unwrap_err()
            .category,
        "RecognitionOutputLimit"
    );
    assert_eq!(
        bounded_diagnostics(json!({"text":"界".repeat(DIAGNOSTIC_BYTES)}), 1)
            .unwrap_err()
            .category,
        "RecognitionOutputLimit"
    );
}

#[test]
fn trial_input_rejects_expectation_filters_and_executable_authority() {
    let mut zone =
        json!({"id":"zone","rect":{"x":0,"y":0,"width":1,"height":1},"expected":"secret"});
    assert!(serde_json::from_value::<OcrZone>(zone.clone()).is_err());
    zone.as_object_mut().unwrap().remove("expected");
    zone["executable"] = json!("untrusted-command");
    assert!(serde_json::from_value::<OcrZone>(zone).is_err());
}

#[test]
fn template_search_must_fit_the_pattern_without_conflating_policy_and_output_limits() {
    let mut input = request(1);
    input.selection = WireSelection::Template {
        id: "zone".into(),
        search: PixelRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        },
        png_bytes: 1,
        template_id: "recognition.zone".into(),
        template_path: "templates/zone.png".into(),
        manifest: json!({"templates":[{"id":"recognition.zone","path":"templates/zone.png",
            "width":3,"height":1,"match_defaults":{"min_score":0.8,"max_results":1}}]})
        .to_string(),
    };
    assert_eq!(
        input.validate(Some(3)).unwrap_err().category,
        "RecognitionRequest"
    );
    let WireSelection::Template {
        search, manifest, ..
    } = &mut input.selection
    else {
        unreachable!()
    };
    search.width = 3;
    let mut data: Value = serde_json::from_str(manifest).unwrap();
    data["templates"][0]["match_defaults"]["max_results"] = json!(257);
    *manifest = data.to_string();
    assert_eq!(input.validate(Some(3)).unwrap(), 400);
    let WireSelection::Template { manifest, .. } = &mut input.selection else {
        unreachable!()
    };
    data["templates"][0]["match_defaults"]["max_results"] = json!(u64::from(u32::MAX) + 1);
    *manifest = data.to_string();
    assert_eq!(
        input.validate(Some(3)).unwrap_err().category,
        "RecognitionRequest"
    );
}
