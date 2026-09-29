use super::{
    MAX_DEFINITIONS, MAX_METADATA_BYTES, RecognitionDefinition, RecognitionDocument, bounded_json,
    invalid, write_bounded,
};
use crate::model::Fault;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PACKAGE_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureDocument {
    pub capture_id: String,
    pub document: RecognitionDocument,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecognitionPackage {
    pub version: u32,
    pub captures: Vec<CaptureDocument>,
}

impl Default for RecognitionPackage {
    fn default() -> Self {
        Self {
            version: PACKAGE_VERSION,
            captures: Vec::new(),
        }
    }
}

impl RecognitionPackage {
    pub fn documents(&self) -> impl Iterator<Item = &RecognitionDocument> {
        self.captures.iter().map(|capture| &capture.document)
    }

    pub fn definitions(&self) -> impl Iterator<Item = &RecognitionDefinition> {
        self.documents().flat_map(|document| &document.definitions)
    }

    pub fn document(&self, capture_id: &str) -> Result<&RecognitionDocument, Fault> {
        self.captures
            .iter()
            .find(|capture| capture.capture_id == capture_id)
            .map(|capture| &capture.document)
            .ok_or_else(|| invalid("capture is missing"))
    }

    pub fn document_mut(&mut self, capture_id: &str) -> Result<&mut RecognitionDocument, Fault> {
        self.captures
            .iter_mut()
            .find(|capture| capture.capture_id == capture_id)
            .map(|capture| &mut capture.document)
            .ok_or_else(|| invalid("capture is missing"))
    }

    pub fn validate(&self) -> Result<(), Fault> {
        if self.version != PACKAGE_VERSION {
            return Err(Fault::new(
                "RecognitionVersion",
                "unsupported recognition package version",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut definitions = 0_usize;
        for capture in &self.captures {
            capture
                .capture_id
                .parse::<xid::Id>()
                .map_err(|_| invalid("capture identity requires a canonical XID"))?;
            if !ids.insert(&capture.capture_id) {
                return Err(invalid("capture identities must be distinct"));
            }
            capture.document.validate_fields()?;
            definitions = definitions
                .checked_add(capture.document.definitions.len())
                .filter(|count| *count <= MAX_DEFINITIONS)
                .ok_or_else(|| invalid("aggregate recognition definition limit exceeded"))?;
        }
        write_bounded(self, std::io::sink())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Fault> {
        self.validate()?;
        bounded_json(self)
    }
}

/// A legacy read is data, not a migration or an identity allocation. Only explicit
/// authoring Save persists the host-assigned capture namespace.
#[derive(Clone, Debug, PartialEq)]
pub enum RecognitionMetadata {
    Legacy(RecognitionDocument),
    Captures(RecognitionPackage),
}

impl RecognitionMetadata {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Fault> {
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(invalid("recognition metadata byte limit exceeded"));
        }
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let version: Version =
            serde_json::from_slice(bytes).map_err(|_| invalid("malformed recognition metadata"))?;
        match version.version {
            1 => RecognitionDocument::from_bytes(bytes).map(Self::Legacy),
            PACKAGE_VERSION => {
                let package: RecognitionPackage = serde_json::from_slice(bytes)
                    .map_err(|_| invalid("malformed recognition capture metadata"))?;
                package.validate()?;
                Ok(Self::Captures(package))
            }
            _ => Err(Fault::new(
                "RecognitionVersion",
                "unsupported recognition package version",
            )),
        }
    }

    pub fn documents(&self) -> impl Iterator<Item = &RecognitionDocument> {
        let (legacy, captures) = match self {
            Self::Legacy(document) => (Some(document), &[][..]),
            Self::Captures(package) => (None, package.captures.as_slice()),
        };
        legacy
            .into_iter()
            .chain(captures.iter().map(|capture| &capture.document))
    }

    pub fn definitions(&self) -> impl Iterator<Item = &RecognitionDefinition> {
        self.documents().flat_map(|document| &document.definitions)
    }

    /// Legacy metadata has exactly one document; its transient namespace is bound
    /// by the authoring owner, never inferred from dimensions or Region IDs.
    pub fn document(&self, capture_id: &str) -> Result<&RecognitionDocument, Fault> {
        match self {
            Self::Legacy(document) => Ok(document),
            Self::Captures(package) => package.document(capture_id),
        }
    }
}
