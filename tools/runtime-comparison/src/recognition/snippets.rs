use super::{RecognitionDocument, RecognitionKind, invalid};
use crate::model::Fault;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnippetKind {
    OcrRecognize,
    OcrWait,
    TemplateRecognize,
}

/// Produces source only. The Application authenticates geometry/save freshness;
/// expected text is an optional Script query criterion, never a trial comparator.
pub fn generate_snippet(
    document: &RecognitionDocument,
    definition_id: &str,
    kind: SnippetKind,
    sdk: &str,
    geometry_confirmed: bool,
    template_saved_current: bool,
    timeout_ms: u64,
) -> Result<String, Fault> {
    document.validate()?;
    if sdk != "mado-host-v1" || !geometry_confirmed {
        return Err(invalid(
            "Copy requires the current SDK and explicitly confirmed geometry",
        ));
    }
    let definition = document.definition(definition_id)?;
    let (region, request) = match (kind, definition.kind) {
        (SnippetKind::OcrRecognize | SnippetKind::OcrWait, RecognitionKind::Ocr) => {
            (definition.region, "kind: \"ocr\"".to_owned())
        }
        (SnippetKind::TemplateRecognize, RecognitionKind::Template) => {
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
            (
                settings.search_region,
                format!("kind: \"template\", asset: {}", literal(&saved.asset)?),
            )
        }
        _ => return Err(invalid("Copy purpose and recognition kind disagree")),
    };
    let roi = region.map_to_pixels(&document.basis)?;
    let basis = document.basis;
    let mut source = format!(
        "{{\n  // Capture-pixel basis: frame {}x{}, content ({}, {}, {}, {}).\n  // Equal dimensions do not prove equal content placement. Recopy after edits.\n  const observation = host.call(\"observe\", {{}});\n  try {{\n    if (observation.width !== {} || observation.height !== {}) {{\n      throw new Error(\"Recognition geometry basis changed\");\n    }}\n    const roi = {{ x: {}, y: {}, width: {}, height: {} }};\n",
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
        roi.height
    );
    if kind == SnippetKind::OcrWait {
        let expected = definition
            .expected
            .as_ref()
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                invalid("query-wait Copy requires a nonempty Script expected-text criterion")
            })?;
        if timeout_ms == 0 || timeout_ms > 30_000 {
            return Err(invalid(
                "query-wait Copy requires a finite supported timeout",
            ));
        }
        source.push_str(&format!(
            "    const query = host.call(\"query\", {{ observation, roi, {request}, expected: {} }});\n    let result = null;\n    try {{\n      result = host.call(\"query_wait\", {{ id: query.id, timeout_ms: {timeout_ms} }});\n      try {{\n        if (result.observation.width !== {} || result.observation.height !== {}) {{\n          throw new Error(\"Recognition geometry basis changed\");\n        }}\n        // Use result here; no text is logged and no input is submitted.\n      }} finally {{\n        try {{\n          host.call(\"release\", {{ id: result.id }});\n        }} finally {{\n          if (result.observation.id !== observation.id && result.observation.id !== result.id && result.observation.id !== query.id) {{\n            host.call(\"release\", {{ id: result.observation.id }});\n          }}\n        }}\n      }}\n    }} finally {{\n      if (result === null || result.id !== query.id) {{\n        try {{\n          host.call(\"release\", {{ id: query.id }});\n        }} catch (error) {{\n          // The controlled host retires failed queries before returning timeout.\n          if (result !== null || typeof error !== \"object\" || error === null || !(\"category\" in error) || error.category !== \"InvalidHandle\") {{\n            throw error;\n          }}\n        }}\n      }}\n    }}\n",
            literal(expected)?, basis.frame_width, basis.frame_height
        ));
    } else {
        source.push_str(&format!(
            "    const result = host.call(\"recognize\", {{ observation, roi, {request} }});\n    try {{\n      // Use result (or null for no match) here; no input or automatic text logging.\n    }} finally {{\n      if (result !== null) {{\n        host.call(\"release\", {{ id: result.id }});\n      }}\n    }}\n"
        ));
    }
    source.push_str("  } finally {\n    host.call(\"release\", { id: observation.id });\n  }\n}\n");
    Ok(source)
}

fn literal(value: &str) -> Result<String, Fault> {
    serde_json::to_string(value)
        .map(|text| {
            text.replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029")
        })
        .map_err(|_| invalid("Script string encoding failed"))
}
