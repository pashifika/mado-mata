//! Script-free saved-frame recognition contracts shared by desktop and its owned child.

use crate::images::{DecodedImage, ImageKind, PayloadBytes, checked_rgba_bytes};
use crate::model::{Fault, encode_bounded};
use crate::recognition::PixelRect;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

#[cfg(feature = "engine")]
mod engine;
#[cfg(feature = "engine")]
pub(crate) use engine::execute;

pub const DIAGNOSTIC_BYTES: usize = 256 * 1024;
pub const DIAGNOSTIC_REGIONS: usize = 256;
pub const DURATION_MS: u64 = 30_000;
pub const CLEANUP_MS: u64 = 1000;
pub const CONTAINMENT_MS: u64 = 2000;
pub const TEXT_CONTRACT: &str =
    "facade-nfc-unicode-trimmed-no-additional-application-normalization";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrialIdentity {
    pub owner: String,
    pub revision: String,
    pub frame: String,
    pub frame_revision: u64,
    pub content_revision: u64,
    pub zones_revision: u64,
    pub configuration_revision: String,
}

pub struct TrialFrame {
    pub image: Arc<DecodedImage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrZone {
    pub id: String,
    pub rect: PixelRect,
}

pub struct TemplateInput {
    pub id: String,
    pub search: PixelRect,
    pub manifest: Vec<u8>,
    pub png: PayloadBytes,
    pub template_id: String,
    pub template_path: String,
}

pub enum TrialSelection {
    Ocr { zones: Vec<OcrZone> },
    Template { template: TemplateInput },
}

pub struct TrialRequest {
    pub identity: TrialIdentity,
    pub frame: TrialFrame,
    pub selection: TrialSelection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub version: u32,
    pub engine_revision: String,
    pub max_ocr_zones: usize,
    pub diagnostic_regions: usize,
    pub diagnostic_bytes: usize,
    pub text_contract: String,
}

pub fn capabilities() -> Result<Capabilities, Fault> {
    #[cfg(feature = "engine")]
    return Ok(Capabilities {
        version: 1,
        engine_revision: crate::model::ENGINE_REVISION.into(),
        max_ocr_zones: mado_pilot::MAX_OCR_ZONES,
        diagnostic_regions: DIAGNOSTIC_REGIONS,
        diagnostic_bytes: DIAGNOSTIC_BYTES,
        text_contract: TEXT_CONTRACT.into(),
    });
    #[cfg(not(feature = "engine"))]
    Err(Fault::new(
        "EngineUnavailable",
        "recognition requires the fixed engine-enabled runner artifact",
    ))
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WireSelection {
    Ocr {
        zones: Vec<OcrZone>,
    },
    Template {
        id: String,
        search: PixelRect,
        manifest: String,
        png_bytes: usize,
        template_id: String,
        template_path: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireRequest {
    pub identity: TrialIdentity,
    pub width: u32,
    pub height: u32,
    pub selection: WireSelection,
}

impl WireRequest {
    pub(crate) fn validate(&self, max_zones: Option<usize>) -> Result<usize, Fault> {
        encode_bounded(self, DIAGNOSTIC_BYTES)?;
        for value in [
            &self.identity.owner,
            &self.identity.revision,
            &self.identity.frame,
            &self.identity.configuration_revision,
        ] {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(invalid("invalid captured operation identity"));
            }
        }
        let frame_bytes = checked_rgba_bytes(self.width, self.height, ImageKind::Input)?;
        match &self.selection {
            WireSelection::Ocr { zones } => {
                // The host does not maintain its own copy of the facade's bound.
                if zones.is_empty()
                    || zones.len() > 256
                    || max_zones.is_some_and(|bound| zones.len() > bound)
                {
                    return Err(invalid(
                        "OCR selection exceeds the grouped request capability",
                    ));
                }
                let mut ids = BTreeSet::new();
                for zone in zones {
                    validate_id(&zone.id)?;
                    if !ids.insert(&zone.id) {
                        return Err(invalid("selected zone IDs must be distinct"));
                    }
                    validate_rect(zone.rect, self.width, self.height)?;
                }
            }
            WireSelection::Template {
                id,
                search,
                manifest,
                png_bytes,
                template_id,
                template_path,
            } => {
                validate_id(id)?;
                validate_id(template_id)?;
                validate_rect(*search, self.width, self.height)?;
                if *png_bytes == 0
                    || *png_bytes > crate::images::CROP_MAX_BYTES
                    || manifest.len() > DIAGNOSTIC_BYTES
                    || manifest.is_empty()
                    || template_path.len() > 256
                    || !template_path.starts_with("templates/")
                    || template_path
                        .split('/')
                        .any(|part| part.is_empty() || part == ".." || part == ".")
                    || template_path.contains('\\')
                    || template_path.chars().any(char::is_control)
                {
                    return Err(invalid("invalid bounded template payload"));
                }
                let manifest: Value = serde_json::from_str(manifest)
                    .map_err(|_| invalid("template manifest is not valid JSON"))?;
                let entries = manifest["templates"].as_array().ok_or_else(|| {
                    invalid("template manifest must declare the selected template")
                })?;
                if entries.len() != 1
                    || entries[0]["id"] != *template_id
                    || entries[0]["path"] != *template_path
                {
                    return Err(invalid(
                        "trial manifest must contain exactly the selected template",
                    ));
                }
                let entry = &entries[0];
                let width = entry["width"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| invalid("invalid template width"))?;
                let height = entry["height"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| invalid("invalid template height"))?;
                checked_rgba_bytes(width, height, ImageKind::Crop)?;
                if width > search.width || height > search.height {
                    return Err(invalid(
                        "template search ROI must contain room for the pattern",
                    ));
                }
                let defaults = &entry["match_defaults"];
                if defaults["min_score"]
                    .as_f64()
                    .is_none_or(|score| !(0.0..=1.0).contains(&score))
                    || defaults["max_results"]
                        .as_u64()
                        .is_none_or(|count| count == 0 || count > u64::from(u32::MAX))
                {
                    return Err(invalid("template defaults exceed supported trial bounds"));
                }
            }
        }
        Ok(frame_bytes)
    }

    pub(crate) fn png_bytes(&self) -> usize {
        match &self.selection {
            WireSelection::Ocr { .. } => 0,
            WireSelection::Template { png_bytes, .. } => *png_bytes,
        }
    }
}

fn validate_id(id: &str) -> Result<(), Fault> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(invalid("invalid selected definition identity"));
    }
    Ok(())
}

fn validate_rect(rect: PixelRect, width: u32, height: u32) -> Result<(), Fault> {
    if rect.width == 0
        || rect.height == 0
        || rect.x.checked_add(rect.width).is_none_or(|end| end > width)
        || rect
            .y
            .checked_add(rect.height)
            .is_none_or(|end| end > height)
    {
        return Err(invalid(
            "selected rectangle is empty or outside the immutable frame",
        ));
    }
    Ok(())
}

pub(crate) fn invalid(message: &str) -> Fault {
    Fault::new("RecognitionRequest", message)
}

#[cfg(any(feature = "engine", test))]
pub(crate) fn bounded_diagnostics(value: Value, regions: usize) -> Result<Value, Fault> {
    if regions > DIAGNOSTIC_REGIONS || encode_bounded(&value, DIAGNOSTIC_BYTES).is_err() {
        return Err(Fault::new(
            "RecognitionOutputLimit",
            "recognition diagnostics exceed 256 regions or 256 KiB",
        ));
    }
    Ok(value)
}

#[cfg(not(feature = "engine"))]
pub(crate) fn execute(
    _: WireRequest,
    _: Vec<u8>,
    _: Vec<u8>,
    _: Value,
    _: Arc<crate::model::Control>,
    _: impl FnOnce(&Result<Value, Fault>),
) -> (Result<Value, Fault>, Value) {
    (
        Err(Fault::new(
            "EngineUnavailable",
            "recognition requires the engine-enabled child",
        )),
        json!({"clean":true,"status":"CleanupFinished","session_closed":true}),
    )
}

#[cfg(test)]
mod tests;
