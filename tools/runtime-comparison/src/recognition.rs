//! Portable saved-image recognition metadata. This module never evaluates source,
//! reads arbitrary paths, acquires a target, or persists an original frame.
mod snippets;
mod templates;

#[cfg(test)]
mod tests;

pub use snippets::{SnippetKind, generate_snippet};
pub use templates::{
    TemplateMaps, build_template_assets, merge_effective_template_maps, validate_inventory,
    validate_package_assets,
};

use crate::images::{ImageKind, validate_png};
use crate::inventory::PackageDraft;
use crate::model::Fault;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const DOCUMENT_VERSION: u32 = 1;
pub const ROUNDING_VERSION: u32 = 1;
pub const MAX_DEFINITIONS: usize = 256;
pub const MAX_METADATA_BYTES: usize = 256 * 1024;
pub const MAX_EXPECTED_BYTES: usize = 4096;
pub const AUTHORING_ASSET: &str = "recognition_authoring";
pub const TEMPLATE_MAPS_ASSET: &str = "recognition_template_maps";
pub const ENGINE_MANIFEST_ASSET: &str = "recognition_engine_manifest";
pub const ENGINE_MANIFEST_PATH: &str = "madopilot-package.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn validate_in(&self, width: u32, height: u32) -> Result<(), Fault> {
        if self.width == 0
            || self.height == 0
            || self
                .x
                .checked_add(self.width)
                .is_none_or(|edge| edge > width)
            || self
                .y
                .checked_add(self.height)
                .is_none_or(|edge| edge > height)
        {
            return Err(invalid(
                "rectangle is empty, overflowing, or outside its basis",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeometryBasis {
    pub frame_width: u32,
    pub frame_height: u32,
    pub content: PixelRect,
}

impl GeometryBasis {
    pub fn validate(&self) -> Result<(), Fault> {
        let pixels = u64::from(self.frame_width) * u64::from(self.frame_height);
        if self.frame_width == 0
            || self.frame_height == 0
            || self.frame_width > crate::images::MAX_SIDE
            || self.frame_height > crate::images::MAX_SIDE
            || pixels > crate::images::INPUT_MAX_PIXELS as u64
        {
            return Err(invalid(
                "geometry basis exceeds the saved-frame image policy",
            ));
        }
        self.content
            .validate_in(self.frame_width, self.frame_height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRect {
    pub u0: f64,
    pub v0: f64,
    pub u1: f64,
    pub v1: f64,
}

impl NormalizedRect {
    pub fn map_to_pixels(&self, basis: &GeometryBasis) -> Result<PixelRect, Fault> {
        basis.validate()?;
        if [self.u0, self.v0, self.u1, self.v1]
            .iter()
            .any(|edge| !edge.is_finite() || !(0.0..=1.0).contains(edge))
            || self.u0 >= self.u1
            || self.v0 >= self.v1
        {
            return Err(invalid(
                "normalized region must have finite ordered edges within content",
            ));
        }
        let content = basis.content;
        // The validated image bound makes every conversion exact and non-overflowing.
        let left = (self.u0 * f64::from(content.width)).floor() as u32;
        let top = (self.v0 * f64::from(content.height)).floor() as u32;
        let right = (self.u1 * f64::from(content.width)).ceil() as u32;
        let bottom = (self.v1 * f64::from(content.height)).ceil() as u32;
        let local = PixelRect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        };
        local.validate_in(content.width, content.height)?;
        Ok(PixelRect {
            x: content.x + left,
            y: content.y + top,
            ..local
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionKind {
    Ocr,
    Template,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedCrop {
    pub asset: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

impl SavedCrop {
    /// Derives identity and dimensions from fully validated encoded crop bytes.
    pub fn from_png(asset: String, png: &[u8]) -> Result<Self, Fault> {
        PackageDraft::check_id(&asset)?;
        let info = validate_png(png, ImageKind::Crop)?;
        Ok(Self {
            asset,
            sha256: png_digest(png),
            width: info.width,
            height: info.height,
        })
    }

    pub fn sample_rect(&self) -> PixelRect {
        PixelRect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }

    fn validate(&self) -> Result<(), Fault> {
        PackageDraft::check_id(&self.asset)?;
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(invalid("saved crop requires a canonical SHA-256 digest"));
        }
        if self.width == 0
            || self.height == 0
            || self.width > crate::images::MAX_SIDE
            || self.height > crate::images::MAX_SIDE
            || u64::from(self.width) * u64::from(self.height)
                > crate::images::CROP_MAX_PIXELS as u64
        {
            return Err(invalid("saved crop dimensions exceed the crop policy"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateSettings {
    pub search_region: NormalizedRect,
    pub threshold: f64,
    pub max_results: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateRights {
    pub license: String,
    pub created_by: String,
    pub created_for: Option<String>,
    pub reviewed: bool,
}

impl TemplateRights {
    fn validate(&self) -> Result<(), Fault> {
        for text in [&self.license, &self.created_by]
            .into_iter()
            .chain(self.created_for.as_ref())
        {
            if text.trim().is_empty() || text.len() > MAX_EXPECTED_BYTES || text.contains('\0') {
                return Err(invalid(
                    "template license and provenance must be nonempty bounded text",
                ));
            }
        }
        if !self.reviewed {
            return Err(invalid(
                "saved templates require explicitly reviewed license and provenance",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecognitionDefinition {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub kind: RecognitionKind,
    pub region: NormalizedRect,
    pub expected: Option<String>,
    pub template: Option<TemplateSettings>,
    pub saved: Option<SavedCrop>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecognitionDocument {
    pub version: u32,
    pub rounding: u32,
    pub basis: GeometryBasis,
    pub definitions: Vec<RecognitionDefinition>,
    pub template_rights: Option<TemplateRights>,
}

impl RecognitionDocument {
    pub fn validate(&self) -> Result<(), Fault> {
        self.validate_fields()?;
        write_bounded(self, std::io::sink())?;
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), Fault> {
        if self.version != DOCUMENT_VERSION || self.rounding != ROUNDING_VERSION {
            return Err(Fault::new(
                "RecognitionVersion",
                "unsupported recognition document or rounding version",
            ));
        }
        self.basis.validate()?;
        if self.definitions.len() > MAX_DEFINITIONS {
            return Err(invalid("recognition definition limit exceeded"));
        }
        let mut ids = BTreeSet::new();
        let mut saved_template = false;
        for definition in &self.definitions {
            PackageDraft::check_id(&definition.id)?;
            if definition.id.len() > 64 || !ids.insert(definition.id.to_ascii_lowercase()) {
                return Err(invalid("definition IDs must be bounded and case-distinct"));
            }
            if definition.name.trim().is_empty()
                || definition.name.len() > 512
                || definition.name.contains('\0')
                || definition.revision > 9_007_199_254_740_991
                || definition
                    .expected
                    .as_ref()
                    .is_some_and(|text| text.len() > MAX_EXPECTED_BYTES)
            {
                return Err(invalid(
                    "definition name, revision, or expected text exceeds its bound",
                ));
            }
            let pattern = definition.region.map_to_pixels(&self.basis)?;
            if let Some(saved) = &definition.saved {
                saved.validate()?;
            }
            match (definition.kind, &definition.template) {
                (RecognitionKind::Ocr, None) => {}
                (RecognitionKind::Template, Some(settings)) => {
                    if definition.expected.is_some()
                        || !settings.threshold.is_finite()
                        || !(0.0..=1.0).contains(&settings.threshold)
                        || settings.max_results == 0
                    {
                        return Err(invalid(
                            "template requires supported match defaults and no OCR expectation",
                        ));
                    }
                    let search = settings.search_region.map_to_pixels(&self.basis)?;
                    let (width, height) = definition
                        .saved
                        .as_ref()
                        .map_or((pattern.width, pattern.height), |saved| {
                            (saved.width, saved.height)
                        });
                    if width > search.width || height > search.height {
                        return Err(invalid(
                            "template search region must have room for the pattern",
                        ));
                    }
                    saved_template |= definition.saved.is_some();
                }
                _ => return Err(invalid("recognition kind and template settings disagree")),
            }
        }
        if saved_template {
            self.template_rights
                .as_ref()
                .ok_or_else(|| invalid("saved templates require reviewed license and provenance"))?
                .validate()?;
        }
        if let Some(rights) = &self.template_rights {
            // Bound incomplete rights drafts as well, without treating them as reviewed.
            if rights.license.len() > MAX_EXPECTED_BYTES
                || rights.created_by.len() > MAX_EXPECTED_BYTES
                || rights
                    .created_for
                    .as_ref()
                    .is_some_and(|text| text.len() > MAX_EXPECTED_BYTES)
            {
                return Err(invalid("template rights exceed their metadata bound"));
            }
        }
        Ok(())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Fault> {
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(invalid("recognition metadata byte limit exceeded"));
        }
        #[derive(Deserialize)]
        struct Version {
            version: u32,
            rounding: u32,
        }
        let version: Version =
            serde_json::from_slice(bytes).map_err(|_| invalid("malformed recognition metadata"))?;
        if version.version != DOCUMENT_VERSION || version.rounding != ROUNDING_VERSION {
            return Err(Fault::new(
                "RecognitionVersion",
                "unsupported recognition document or rounding version",
            ));
        }
        let document: Self =
            serde_json::from_slice(bytes).map_err(|_| invalid("malformed recognition metadata"))?;
        document.validate_fields()?;
        Ok(document)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Fault> {
        self.validate_fields()?;
        bounded_json(self)
    }

    pub fn definition(&self, id: &str) -> Result<&RecognitionDefinition, Fault> {
        self.definitions
            .iter()
            .find(|definition| definition.id == id)
            .ok_or_else(|| invalid("recognition definition is missing"))
    }
}

pub fn png_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn bounded_json(value: &impl Serialize) -> Result<Vec<u8>, Fault> {
    let mut bytes = Vec::new();
    write_bounded(value, &mut bytes)?;
    Ok(bytes)
}

fn write_bounded(value: &impl Serialize, writer: impl std::io::Write) -> Result<(), Fault> {
    struct Bounded<W> {
        writer: W,
        written: usize,
    }
    impl<W: std::io::Write> std::io::Write for Bounded<W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_METADATA_BYTES.saturating_sub(self.written) {
                return Err(std::io::Error::other(
                    "recognition metadata byte limit exceeded",
                ));
            }
            self.writer.write_all(bytes)?;
            self.written += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }
    serde_json::to_writer(Bounded { writer, written: 0 }, value)
        .map_err(|_| invalid("recognition metadata cannot be encoded within its byte bound"))
}

fn invalid(message: &str) -> Fault {
    Fault::new("RecognitionMetadata", message)
}
