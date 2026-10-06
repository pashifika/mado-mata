use super::{RecognitionDocument, RecognitionKind, invalid};
use crate::model::Fault;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnippetKind {
    GameContent,
    OcrRecognize,
    TemplateRecognize,
}

/// Produces all requested source or an error; callers publish only the complete result.
/// `GameContent` is reusable geometry data, not an engine call or OCR evidence.
/// `OcrRecognize` scans every requested OCR definition in one bounded `scan_ocr_zones`
/// request against that setup; nothing is split, filtered or awaited.
pub fn generate_snippet(
    document: &RecognitionDocument,
    definition_ids: &[String],
    kind: SnippetKind,
    sdk: &str,
    geometry_confirmed: bool,
    template_saved_current: bool,
    max_ocr_zones: Option<usize>,
) -> Result<String, Fault> {
    document.validate()?;
    if sdk != "mado-host-v1" || !geometry_confirmed {
        return Err(invalid(
            "Copy requires the current SDK and explicitly confirmed geometry",
        ));
    }
    if definition_ids
        .iter()
        .enumerate()
        .any(|(index, id)| definition_ids[..index].contains(id))
    {
        return Err(invalid("Copy requires distinct definitions"));
    }
    match kind {
        SnippetKind::GameContent => game_content(document, definition_ids),
        SnippetKind::OcrRecognize => grouped_ocr(document, definition_ids, max_ocr_zones),
        SnippetKind::TemplateRecognize => {
            template(document, definition_ids, template_saved_current)
        }
    }
}

fn game_content(
    document: &RecognitionDocument,
    definition_ids: &[String],
) -> Result<String, Fault> {
    if !definition_ids.is_empty() {
        return Err(invalid("Game content setup Copy takes no definitions"));
    }
    let basis = document.basis;
    Ok(format!(
        "// Game content setup (mado-host-v1): capture-pixel geometry only, not a recognition result.\n// Paste once before grouped OCR snippets. Copy it again after Game content edits.\nconst recognitionBasis = {{\n  frame_width: {},\n  frame_height: {},\n  content: {{ x: {}, y: {}, width: {}, height: {} }},\n}};\n",
        basis.frame_width,
        basis.frame_height,
        basis.content.x,
        basis.content.y,
        basis.content.width,
        basis.content.height
    ))
}

fn grouped_ocr(
    document: &RecognitionDocument,
    definition_ids: &[String],
    max_ocr_zones: Option<usize>,
) -> Result<String, Fault> {
    let maximum = max_ocr_zones.ok_or_else(|| {
        invalid("grouped OCR Copy requires the engine's reported grouped-request limit")
    })?;
    if definition_ids.is_empty() || definition_ids.len() > maximum {
        return Err(invalid(
            "grouped OCR Copy requires one to the engine limit of definitions",
        ));
    }
    let mut zones = String::new();
    for id in definition_ids {
        let definition = document.definition(id)?;
        if definition.kind != RecognitionKind::Ocr {
            return Err(invalid("grouped OCR Copy accepts only OCR definitions"));
        }
        let region = definition.region;
        zones.push_str(&format!("        // {}", literal(&definition.name)?));
        if let Some(text) = definition
            .expected
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            zones.push_str(&format!("; reference text: {}", literal(text)?));
        }
        zones.push_str(&format!(
            "\n        {{ id: {}, region: {{ u0: {}, v0: {}, u1: {}, v1: {} }} }},\n",
            literal(id)?,
            number(region.u0)?,
            number(region.v0)?,
            number(region.u1)?,
            number(region.v1)?
        ));
    }
    Ok(format!(
        "{{\n  // Checked OCR regions in one scan_ocr_zones request (mado-host-v1), relative to recognitionBasis\n  // from the Game content setup. Reference text is Script-author context only: never sent, matched or awaited.\n  const observation = host.call(\"observe\", {{}});\n  try {{\n    const scan = host.call(\"scan_ocr_zones\", {{\n      observation,\n      basis: recognitionBasis,\n      zones: [\n{zones}      ],\n    }});\n    // Use scan here: scan.zones follow this order, each \"recognized\" or \"no_match\" with every engine region.\n    // The scan is a plain snapshot without a handle to release. No input or automatic text logging.\n  }} finally {{\n    host.call(\"release\", {{ id: observation.id }});\n  }}\n}}\n"
    ))
}

fn template(
    document: &RecognitionDocument,
    definition_ids: &[String],
    template_saved_current: bool,
) -> Result<String, Fault> {
    let [id] = definition_ids else {
        return Err(invalid("template Copy accepts exactly one definition"));
    };
    let definition = document.definition(id)?;
    if definition.kind != RecognitionKind::Template {
        return Err(invalid("Copy purpose and recognition kind disagree"));
    }
    if !template_saved_current {
        return Err(invalid(
            "template Copy requires current saved assets, defaults and maps",
        ));
    }
    let saved = definition
        .saved
        .as_ref()
        .ok_or_else(|| invalid("template Copy requires a saved pattern"))?;
    let settings = definition
        .template
        .as_ref()
        .ok_or_else(|| invalid("template settings are missing"))?;
    let roi = settings.search_region.map_to_pixels(&document.basis)?;
    let basis = document.basis;
    Ok(format!(
        "{{\n  // Capture-pixel basis: frame {}x{}, content ({}, {}, {}, {}).\n  // Equal dimensions do not prove equal content placement. Recopy after edits.\n  const observation = host.call(\"observe\", {{}});\n  try {{\n    if (observation.width !== {} || observation.height !== {}) {{\n      throw new Error(\"Recognition geometry basis changed\");\n    }}\n    const roi = {{ x: {}, y: {}, width: {}, height: {} }};\n    const result = host.call(\"recognize\", {{ observation, roi, kind: \"template\", asset: {} }});\n    try {{\n      // Use result (or null for no match) here; no input or automatic text logging.\n    }} finally {{\n      if (result !== null) {{\n        host.call(\"release\", {{ id: result.id }});\n      }}\n    }}\n  }} finally {{\n    host.call(\"release\", {{ id: observation.id }});\n  }}\n}}\n",
        basis.frame_width,
        basis.frame_height,
        basis.content.x,
        basis.content.y,
        basis.content.width,
        basis.content.height,
        basis.frame_width,
        basis.frame_height,
        roi.x,
        roi.y,
        roi.width,
        roi.height,
        literal(&saved.asset)?
    ))
}

/// Shortest round-trip decimal, so the Script sends the exact stored normalized edge.
fn number(value: f64) -> Result<String, Fault> {
    serde_json::to_string(&value).map_err(|_| invalid("Script number encoding failed"))
}

/// A JSON string with JavaScript line separators escaped, safe in expressions and `//` comments.
fn literal(value: &str) -> Result<String, Fault> {
    serde_json::to_string(value)
        .map(|text| {
            text.replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029")
        })
        .map_err(|_| invalid("Script string encoding failed"))
}
