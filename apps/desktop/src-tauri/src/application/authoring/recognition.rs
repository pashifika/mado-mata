use super::{AuthoringMutation, AuthoringRef, OperationOwner};
use crate::application::{Application, MAX_SESSION_COUNTER, Workspaces, lock};
use crate::authoring::{Candidate, RecognitionSave, SelectedCrop};
use mado_runtime_comparison::images::{self, DecodedImage, ImageKind, PayloadReservation};
use mado_runtime_comparison::model::{Fault, identity};
use mado_runtime_comparison::recognition::{
    CaptureDocument, GeometryBasis, PixelRect, RecognitionDefinition, RecognitionDocument,
    RecognitionKind, RecognitionMetadata, RecognitionPackage, SavedCrop, SnippetKind,
    build_template_assets, generate_snippet,
};
use mado_runtime_comparison::recognition_trial::{
    OcrZone, TemplateInput, TrialFrame, TrialIdentity, TrialSelection,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

#[derive(Default)]
pub(super) struct RecognitionState {
    initialized: bool,
    source_revision: String,
    document: Option<RecognitionDocument>,
    saved_document: Option<RecognitionDocument>,
    package: RecognitionPackage,
    saved_package: RecognitionPackage,
    capture_id: Option<String>,
    confirmed_captures: BTreeSet<String>,
    migration_required: bool,
    revision: u64,
    basis_confirmed: bool,
    frame: Option<Frame>,
    next_frame: u64,
    max_ocr_zones: Option<usize>,
    trial: Option<RecognitionTrial>,
    run: Option<RecognitionRun>,
    preview_payload: Option<PayloadReservation>,
    preview_generation: u64,
    staged_crops: Vec<StagedCrop>,
    prepared_capture: Option<(u64, bool)>,
}

/// A child started by this lease. `collect` settles it in the critical section that
/// ends run ownership; its command then takes the settled controller to reply.
struct RecognitionRun {
    id: String,
    /// Captured trial inputs; `None` for a capability read.
    trial: Option<TrialTicket>,
    settled: Option<Arc<Value>>,
}

struct TrialTicket {
    owner: AuthoringRef,
    revision: String,
    capture_id: String,
    document_revision: u64,
    frame_id: Option<String>,
    frame_revision: u64,
    configuration_revision: String,
    sample_id: Option<String>,
}

impl TrialTicket {
    fn into_trial(self, stale: bool, controller: Arc<Value>) -> RecognitionTrial {
        RecognitionTrial {
            owner: self.owner,
            revision: self.revision,
            capture_id: self.capture_id,
            document_revision: self.document_revision,
            frame_id: self.frame_id,
            frame_revision: self.frame_revision,
            configuration_revision: self.configuration_revision,
            sample_id: self.sample_id,
            stale,
            controller,
        }
    }
}

struct Frame {
    id: String,
    revision: u64,
    image: Arc<DecodedImage>,
    native: Option<NativeFrameOrigin>,
    confirmed: bool,
}

/// Only selected regions survive an original's release, as bounded encoded pixels.
/// The input fingerprint excludes the later frame: these pixels belong to their selection.
struct StagedCrop {
    capture_id: String,
    definition_id: String,
    frame_id: String,
    fingerprint: String,
    png: images::PayloadBytes,
    decoded_bytes: usize,
}

fn crop_fingerprint(document: &RecognitionDocument, id: &str) -> Result<String, Fault> {
    identity(&(document.basis, document.definition(id)?))
}

fn check_crop_sources<'a>(
    crop_ids: &'a [String],
    crop_sources: &BTreeMap<String, String>,
) -> Result<BTreeSet<&'a String>, Fault> {
    let distinct: BTreeSet<_> = crop_ids.iter().collect();
    if distinct.len() != crop_ids.len()
        || crop_sources.len() != distinct.len()
        || crop_sources.keys().any(|id| !distinct.contains(id))
    {
        return Err(invalid(
            "Crop selection must have exactly one original frame per ID",
        ));
    }
    Ok(distinct)
}

/// Owns unpublished pixels/metadata, then the replaced values after the commit swap.
struct PreparedImage {
    capture: Option<CaptureDocument>,
    capture_id: Option<String>,
    document: Option<RecognitionDocument>,
    saved_document: Option<RecognitionDocument>,
    frame: Option<Frame>,
}

pub(super) struct PreparedNativeFrame {
    image: PreparedImage,
    revision: u64,
    pub(super) png: mado_runtime_comparison::images::PayloadBytes,
}

/// Transient source provenance stays with the single decoded frame, never metadata.
pub(super) struct NativeFrameOrigin {
    pub _geometry: mado_runtime_comparison::authoring_capture::CaptureGeometry,
    pub _request: mado_runtime_comparison::authoring_capture::CaptureIdentity,
    pub _source_frame: String,
    pub acquired_at_ms: u64,
    pub _captured_monotonic_us: u64,
}

#[derive(Clone, Serialize)]
pub struct RecognitionFrame {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub confirmed: bool,
    pub historical_capture_at_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct RecognitionView {
    pub owner: AuthoringRef,
    pub revision: String,
    pub capture_id: Option<String>,
    pub captures: Vec<CaptureDocument>,
    pub saved_captures: Vec<CaptureDocument>,
    pub migration_required: bool,
    pub document: Option<RecognitionDocument>,
    pub saved_document: Option<RecognitionDocument>,
    pub document_revision: u64,
    pub basis_confirmed: bool,
    pub other_bases_confirmed: bool,
    pub frame: Option<RecognitionFrame>,
    pub staged_crop_ids: Vec<String>,
    pub stale_crop_ids: Vec<String>,
    pub staged_crop_sources: BTreeMap<String, String>,
    pub capabilities: Value,
    pub configuration_revision: String,
    pub trial: Option<RecognitionTrial>,
}

#[derive(Clone, Serialize)]
pub struct RecognitionTrial {
    pub owner: AuthoringRef,
    pub revision: String,
    pub capture_id: String,
    pub document_revision: u64,
    pub frame_id: Option<String>,
    pub frame_revision: u64,
    pub configuration_revision: String,
    pub sample_id: Option<String>,
    pub stale: bool,
    #[serde(serialize_with = "crate::application::serialize_shared")]
    pub controller: Arc<Value>,
}

#[derive(Serialize)]
pub struct RecognitionSaved {
    pub mutation: AuthoringMutation,
    pub recognition: Option<RecognitionView>,
}

#[derive(Serialize)]
pub struct RecognitionCopy {
    pub capture_id: String,
    pub source: String,
    pub basis: GeometryBasis,
    pub verified: bool,
    pub definition_ids: Vec<String>,
    pub document_revision: u64,
}

fn stale() -> Fault {
    Fault::new(
        "StaleRecognition",
        "Recognition inputs changed; refresh the current view",
    )
}

fn invalid(message: &str) -> Fault {
    Fault::new("RecognitionInput", message)
}

impl RecognitionState {
    fn initialize(&mut self, candidate: &Candidate) -> Result<(), Fault> {
        if !self.initialized || self.source_revision != candidate.revision() {
            let metadata = candidate.recognition()?;
            let migration_required = matches!(&metadata, Some(RecognitionMetadata::Legacy(_)));
            let saved = match metadata {
                Some(RecognitionMetadata::Captures(package)) => package,
                Some(RecognitionMetadata::Legacy(document)) => {
                    let capture_id = match self.saved_package.captures.first() {
                        Some(capture) if self.migration_required => capture.capture_id.clone(),
                        _ => crate::storage::new_id()?,
                    };
                    RecognitionPackage {
                        captures: vec![CaptureDocument {
                            capture_id,
                            document,
                        }],
                        ..RecognitionPackage::default()
                    }
                }
                None => RecognitionPackage::default(),
            };
            if !self.initialized || self.package == self.saved_package {
                self.package = saved.clone();
                self.confirmed_captures = saved
                    .captures
                    .iter()
                    .map(|capture| capture.capture_id.clone())
                    .collect();
                if self
                    .capture_id
                    .as_ref()
                    .is_none_or(|id| self.package.document(id).is_err())
                {
                    self.frame = None;
                    self.preview_payload = None;
                    self.capture_id = self
                        .package
                        .captures
                        .first()
                        .map(|capture| capture.capture_id.clone());
                }
                self.select_document();
                self.reconcile_frame();
            }
            if self.initialized && saved != self.saved_package {
                self.advance()?;
                if let Some(frame) = &mut self.frame {
                    frame.confirmed = false;
                }
            }
            self.saved_package = saved;
            self.saved_document = self
                .capture_id
                .as_deref()
                .and_then(|id| self.saved_package.document(id).ok())
                .cloned();
            self.migration_required = migration_required;
            self.source_revision = candidate.revision().to_owned();
            self.initialized = true;
        }
        Ok(())
    }

    fn select_document(&mut self) {
        self.document = self
            .capture_id
            .as_deref()
            .and_then(|id| self.package.document(id).ok())
            .cloned();
        self.saved_document = self
            .capture_id
            .as_deref()
            .and_then(|id| self.saved_package.document(id).ok())
            .cloned();
        self.basis_confirmed = self
            .capture_id
            .as_ref()
            .is_some_and(|id| self.confirmed_captures.contains(id));
    }

    fn replace_document(&mut self, document: RecognitionDocument) -> Result<(), Fault> {
        if self.revision >= MAX_SESSION_COUNTER {
            return Err(invalid("Recognition revision exhausted"));
        }
        let id = self.capture_id.as_deref().ok_or_else(stale)?;
        let previous = std::mem::replace(self.package.document_mut(id)?, document);
        if let Err(error) = self.package.validate() {
            *self.package.document_mut(id)? = previous;
            return Err(error);
        }
        self.document = Some(self.package.document(id)?.clone());
        Ok(())
    }

    fn check_capture(&self, capture_id: Option<&str>) -> Result<(), Fault> {
        if self.capture_id.as_deref() != capture_id {
            return Err(stale());
        }
        Ok(())
    }

    fn basis_is_confirmed(&self, capture: &CaptureDocument) -> bool {
        self.confirmed_captures.contains(&capture.capture_id)
            || self
                .saved_package
                .document(&capture.capture_id)
                .is_ok_and(|saved| saved.basis == capture.document.basis)
    }

    fn release_frame(&mut self, leaving_capture: bool) -> Result<(), Fault> {
        if (leaving_capture || self.frame.is_some())
            && self.package.captures.iter().any(|capture| {
                Some(&capture.capture_id) == self.capture_id.as_ref()
                    && !self.basis_is_confirmed(capture)
            })
        {
            return Err(invalid(
                "Confirm changed content geometry or discard recognition changes before replacing the image",
            ));
        }
        self.advance()?;
        self.frame = None;
        self.preview_payload = None;
        self.preview_generation = self.preview_generation.checked_add(1).ok_or_else(stale)?;
        Ok(())
    }

    fn stage_crops(
        &mut self,
        crop_ids: &[String],
        crop_sources: &BTreeMap<String, String>,
    ) -> Result<(), Fault> {
        let distinct = check_crop_sources(crop_ids, crop_sources)?;
        if crop_ids.is_empty() {
            self.staged_crops
                .retain(|crop| Some(&crop.capture_id) != self.capture_id.as_ref());
            return Ok(());
        }
        let capture_id = self.capture_id.as_deref().ok_or_else(stale)?;
        let document = self.document.as_ref().ok_or_else(stale)?;
        let mut bytes = 0usize;
        let mut decoded = 0usize;
        for crop in self
            .staged_crops
            .iter()
            .filter(|crop| crop.capture_id != capture_id)
        {
            bytes = bytes.checked_add(crop.png.len()).ok_or_else(stale)?;
            decoded = decoded.checked_add(crop.decoded_bytes).ok_or_else(stale)?;
        }
        let mut planned = Vec::new();
        for id in crop_ids {
            let fingerprint = crop_fingerprint(document, id)?;
            let source = &crop_sources[id];
            if let Some(staged) = self.staged_crops.iter().find(|crop| {
                crop.capture_id == capture_id
                    && crop.definition_id == *id
                    && crop.frame_id == *source
            }) {
                if staged.fingerprint != fingerprint {
                    return Err(invalid(
                        "Pending crop inputs changed; discard that selection before choosing newer pixels",
                    ));
                }
                bytes = bytes.checked_add(staged.png.len()).ok_or_else(stale)?;
                decoded = decoded
                    .checked_add(staged.decoded_bytes)
                    .ok_or_else(stale)?;
            } else {
                let frame = self.confirmed_frame()?;
                if frame.id != *source {
                    return Err(invalid("Selected crop original is no longer available"));
                }
                let rect = document
                    .definition(id)?
                    .region
                    .map_to_pixels(&document.basis)?;
                let pixels = rect.width as usize * rect.height as usize;
                if pixels > images::CROP_MAX_PIXELS {
                    return Err(invalid("Pending crop exceeds the crop pixel policy"));
                }
                decoded = decoded.checked_add(pixels * 4).ok_or_else(stale)?;
                planned.push((id, source, fingerprint, rect));
            }
            if decoded > images::PACKAGE_DECODED_BYTES || bytes > images::PACKAGE_IMAGE_BYTES {
                return Err(invalid("Pending crops exceed the aggregate image policy"));
            }
        }
        // Validate every source and aggregate pixel bound before allocating any new PNG.
        // Publication of the staged set happens only after every encode succeeds.
        let mut additions = Vec::with_capacity(planned.len());
        for (id, source, fingerprint, rect) in planned {
            let encoded = images::encode_crop(
                &self.confirmed_frame()?.image,
                [rect.x, rect.y, rect.width, rect.height],
            )?;
            bytes = bytes
                .checked_add(encoded.as_bytes().len())
                .ok_or_else(stale)?;
            if bytes > images::PACKAGE_IMAGE_BYTES {
                return Err(invalid("Pending crops exceed the aggregate image policy"));
            }
            let (png, reservation) = encoded.into_parts();
            additions.push(StagedCrop {
                capture_id: capture_id.to_owned(),
                definition_id: id.clone(),
                frame_id: source.clone(),
                fingerprint,
                png: images::PayloadBytes::from_reserved(png, reservation)?,
                decoded_bytes: rect.width as usize * rect.height as usize * 4,
            });
        }
        self.staged_crops.retain(|crop| {
            crop.capture_id != capture_id
                || (distinct.contains(&crop.definition_id)
                    && !additions
                        .iter()
                        .any(|new| new.definition_id == crop.definition_id))
        });
        self.staged_crops.extend(additions);
        Ok(())
    }

    fn prepare_new_capture(
        &mut self,
        owner: &AuthoringRef,
        image: DecodedImage,
        confirmed: bool,
    ) -> Result<PreparedImage, Fault> {
        let frame_revision = self
            .next_frame
            .checked_add(1)
            .filter(|value| *value <= MAX_SESSION_COUNTER)
            .ok_or_else(stale)?;
        let capture_id = crate::storage::new_id()?;
        if self.package.document(&capture_id).is_ok() {
            return Err(invalid("Capture identity collision"));
        }
        let document = RecognitionDocument {
            version: 1,
            rounding: 1,
            basis: GeometryBasis {
                frame_width: image.width,
                frame_height: image.height,
                content: PixelRect {
                    x: 0,
                    y: 0,
                    width: image.width,
                    height: image.height,
                },
            },
            definitions: Vec::new(),
            template_rights: None,
        };
        // Validate the aggregate, retaining capacity for the allocation-free commit.
        // Workspaces remains locked; the temporary append never becomes visible.
        self.package.captures.push(CaptureDocument {
            capture_id: capture_id.clone(),
            document: document.clone(),
        });
        let validation = self.package.validate();
        let capture = self.package.captures.pop().expect("staged capture");
        validation?;
        let saved_document = self.saved_package.document(&capture_id).ok().cloned();
        Ok(PreparedImage {
            capture: Some(capture),
            capture_id: Some(capture_id),
            document: Some(document),
            saved_document,
            frame: Some(Frame {
                id: format!("{}-frame-{frame_revision}", owner.token),
                revision: frame_revision,
                image: Arc::new(image),
                confirmed,
                native: None,
            }),
        })
    }

    fn commit_image(&mut self, prepared: &mut PreparedImage) {
        let frame = prepared.frame.as_ref().expect("unpublished frame");
        if let Some(capture) = prepared.capture.take() {
            if frame.confirmed {
                self.confirmed_captures.insert(capture.capture_id.clone());
            }
            self.package.captures.push(capture);
            std::mem::swap(&mut self.capture_id, &mut prepared.capture_id);
            std::mem::swap(&mut self.document, &mut prepared.document);
            std::mem::swap(&mut self.saved_document, &mut prepared.saved_document);
        }
        // Historical basis confirmation still describes staged pixels after a resize.
        // Only the new raster needs reconfirmation; edits to the basis revoke both.
        self.next_frame = frame.revision;
        self.basis_confirmed = frame.confirmed;
        self.prepared_capture = None;
        std::mem::swap(&mut self.frame, &mut prepared.frame);
    }

    fn prepare_image(
        &mut self,
        owner: &AuthoringRef,
        image: DecodedImage,
        new_capture: bool,
        confirm_default: bool,
    ) -> Result<PreparedImage, Fault> {
        if new_capture || self.capture_id.is_none() {
            return self.prepare_new_capture(owner, image, confirm_default);
        }
        let frame_revision = self
            .next_frame
            .checked_add(1)
            .filter(|value| *value <= MAX_SESSION_COUNTER)
            .ok_or_else(stale)?;
        let document = self.document.as_ref().ok_or_else(stale)?;
        let confirmed = self
            .capture_id
            .as_ref()
            .is_some_and(|id| self.confirmed_captures.contains(id))
            && document.basis.frame_width == image.width
            && document.basis.frame_height == image.height;
        // A changed raster cannot silently scale regions or repair user geometry.
        // Keep the old basis until an explicit edit and confirmation against this frame.
        Ok(PreparedImage {
            capture: None,
            capture_id: None,
            document: None,
            saved_document: None,
            frame: Some(Frame {
                id: format!("{}-frame-{frame_revision}", owner.token),
                revision: frame_revision,
                image: Arc::new(image),
                confirmed,
                native: None,
            }),
        })
    }

    fn install_image(
        &mut self,
        owner: &AuthoringRef,
        image: DecodedImage,
        new_capture: bool,
        confirm_default: bool,
    ) -> Result<(), Fault> {
        let mut prepared = self.prepare_image(owner, image, new_capture, confirm_default)?;
        self.commit_image(&mut prepared);
        Ok(())
    }

    fn reconcile_frame(&mut self) {
        if self
            .frame
            .as_ref()
            .zip(self.document.as_ref())
            .is_some_and(|(frame, document)| {
                document.basis.frame_width != frame.image.width
                    || document.basis.frame_height != frame.image.height
            })
        {
            self.frame = None;
            self.preview_payload = None;
        }
    }

    fn advance(&mut self) -> Result<(), Fault> {
        if self.revision >= MAX_SESSION_COUNTER {
            return Err(invalid("Recognition revision exhausted"));
        }
        self.revision += 1;
        if let Some(trial) = &mut self.trial {
            trial.stale = true;
        }
        Ok(())
    }

    fn check(&self, revision: u64, frame: Option<&str>) -> Result<(), Fault> {
        if self.revision != revision || self.frame.as_ref().map(|frame| frame.id.as_str()) != frame
        {
            return Err(stale());
        }
        Ok(())
    }

    fn confirmed_frame(&self) -> Result<&Frame, Fault> {
        self.frame
            .as_ref()
            .filter(|frame| frame.confirmed)
            .ok_or_else(|| invalid("Load an image and confirm its content geometry first"))
    }

    /// Settles this lease's run where `collect` ends its ownership, so no command is
    /// admitted between terminal evidence and the retained outcome.
    pub(super) fn settle(&mut self, package_revision: &str, run: &str, controller: &Arc<Value>) {
        let Some(pending) = self
            .run
            .as_mut()
            .filter(|pending| pending.id == run && pending.settled.is_none())
        else {
            return;
        };
        pending.settled = Some(controller.clone());
        if let Some(ticket) = pending.trial.take() {
            let stale = ticket.revision != package_revision
                || Some(ticket.capture_id.as_str()) != self.capture_id.as_deref()
                || ticket.document_revision != self.revision
                || (ticket.sample_id.is_none()
                    && self.frame.as_ref().map(|frame| frame.id.as_str())
                        != ticket.frame_id.as_deref());
            self.trial = Some(ticket.into_trial(stale, controller.clone()));
        } else if let Some(maximum) = capability_limit(controller) {
            self.max_ocr_zones = Some(maximum);
        }
    }

    /// A settled cleanup failure whose command has not returned it yet.
    pub(super) fn unreturned_cleanup(&self) -> Option<Fault> {
        incomplete_cleanup(self.run.as_ref()?.settled.as_ref()?)
    }
}

/// Cleanup is reported beside the primary outcome. A returned runner envelope carries
/// its own cleanup and reaping facts; a worker fault attests cleanup only in its context.
fn cleanup_facts(controller: &Value) -> (bool, Value) {
    let result = &controller["result"];
    let error = &controller["error"];
    let (cleanup, primary) = if error.is_null() {
        (&result["cleanup"], &result["primary"]["category"])
    } else {
        (&error["context"]["cleanup"], &error["category"])
    };
    (
        cleanup["clean"] == true,
        json!({"operation":controller["operation"],"primary":primary,"cleanup":cleanup,
            "child_reaped":result["child_reaped"],"forced":result["forced"],"exit_code":result["exit_code"]}),
    )
}

/// Only unconfirmed child ownership contains the Edit session: incomplete cleanup without
/// an explicit reaping fact, or a supervision containment fault. A reaped child with
/// incomplete cleanup stays a visible failed outcome and does not refuse Save or Exit.
pub(super) fn containment(controller: &Value) -> Option<Fault> {
    let (clean, facts) = cleanup_facts(controller);
    ((!clean && facts["child_reaped"] != true) || facts["primary"] == "Containment").then(|| {
        Fault::new(
            "RecognitionCleanup",
            "Recognition child ownership is unconfirmed; close the application before further work",
        )
        .with_context(facts)
    })
}

fn incomplete_cleanup(controller: &Value) -> Option<Fault> {
    let (clean, facts) = cleanup_facts(controller);
    (!clean).then(|| {
        Fault::new("RecognitionCleanup", "Recognition cleanup is incomplete").with_context(facts)
    })
}

/// A failed capability read keeps its primary category, with cleanup and reaping facts
/// beside it; with no primary failure, incomplete cleanup is the failure.
fn capability_failure(controller: &Value) -> Option<Fault> {
    let error = &controller["error"];
    if !error.is_null() {
        return Some(serde_json::from_value(error.clone()).unwrap_or_else(|_| stale()));
    }
    let primary = &controller["result"]["primary"];
    if primary.is_null() {
        return incomplete_cleanup(controller);
    }
    let mut fault: Fault = serde_json::from_value(primary.clone()).unwrap_or_else(|_| stale());
    let (_, mut facts) = cleanup_facts(controller);
    let cause = std::mem::take(&mut fault.context);
    facts["stage"] = cause["stage"].clone();
    facts["cause"] = cause;
    fault.context = facts;
    Some(fault)
}

fn capability_limit(controller: &Value) -> Option<usize> {
    let result = &controller["result"];
    if !controller["error"].is_null()
        || !result["primary"].is_null()
        || result["cleanup"]["clean"] != true
    {
        return None;
    }
    result["capabilities"]["max_ocr_zones"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
}

/// Saved correspondence compares stored meaning. Definition revisions are edit fences, so a
/// hand-restored definition is saved-current without resetting its revision.
fn same_saved_content(draft: &RecognitionDocument, stored: &RecognitionDocument) -> bool {
    let RecognitionDocument {
        version,
        rounding,
        basis,
        definitions,
        template_rights,
    } = draft;
    *version == stored.version
        && *rounding == stored.rounding
        && *basis == stored.basis
        && *template_rights == stored.template_rights
        && definitions.len() == stored.definitions.len()
        && definitions
            .iter()
            .zip(&stored.definitions)
            .all(|(definition, stored)| {
                let RecognitionDefinition {
                    id,
                    name,
                    revision: _,
                    kind,
                    region,
                    expected,
                    template,
                    saved,
                } = definition;
                *id == stored.id
                    && *name == stored.name
                    && *kind == stored.kind
                    && *region == stored.region
                    && *expected == stored.expected
                    && *template == stored.template
                    && *saved == stored.saved
            })
}

impl Application {
    fn recognition_configuration(&self) -> Result<String, Fault> {
        identity(&lock(&self.store).settings()?.ocr_environment)
    }

    pub(super) fn recognition_snapshot(
        &self,
        state: &mut Workspaces,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<RecognitionView, Fault> {
        let candidate = state.authoring_revision(owner, revision)?;
        let configuration_revision = self.recognition_configuration()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        let mut trial = recognition
            .trial
            .clone()
            .filter(|trial| Some(trial.capture_id.as_str()) == recognition.capture_id.as_deref());
        if let Some(trial) = &mut trial {
            trial.stale |= trial.configuration_revision != configuration_revision
                || trial.revision != revision
                || trial.document_revision != recognition.revision;
        }
        let mut staged_crop_ids = Vec::new();
        let mut stale_crop_ids = Vec::new();
        let mut staged_crop_sources = BTreeMap::new();
        for crop in recognition
            .staged_crops
            .iter()
            .filter(|crop| Some(&crop.capture_id) == recognition.capture_id.as_ref())
        {
            staged_crop_sources.insert(crop.definition_id.clone(), crop.frame_id.clone());
            let valid = recognition.document.as_ref().is_some_and(|document| {
                crop_fingerprint(document, &crop.definition_id)
                    .is_ok_and(|fingerprint| fingerprint == crop.fingerprint)
            });
            if valid {
                staged_crop_ids.push(crop.definition_id.clone());
            } else {
                stale_crop_ids.push(crop.definition_id.clone());
            }
        }
        Ok(RecognitionView {
            owner: owner.clone(),
            capture_id: recognition.capture_id.clone(),
            captures: recognition.package.captures.clone(),
            saved_captures: recognition.saved_package.captures.clone(),
            migration_required: recognition.migration_required,
            revision: revision.to_owned(),
            document: recognition.document.clone(),
            saved_document: recognition.saved_document.clone(),
            document_revision: recognition.revision,
            basis_confirmed: recognition.basis_confirmed,
            staged_crop_ids,
            stale_crop_ids,
            staged_crop_sources,
            other_bases_confirmed: recognition
                .package
                .captures
                .iter()
                .filter(|capture| Some(&capture.capture_id) != recognition.capture_id.as_ref())
                .all(|capture| recognition.basis_is_confirmed(capture)),
            frame: recognition.frame.as_ref().map(|frame| RecognitionFrame {
                id: frame.id.clone(),
                width: frame.image.width,
                height: frame.image.height,
                revision: frame.revision,
                confirmed: frame.confirmed,
                historical_capture_at_ms: frame.native.as_ref().map(|source| source.acquired_at_ms),
            }),
            capabilities: json!({
                "max_ocr_zones":recognition.max_ocr_zones,
                "diagnostic_regions":256,"diagnostic_bytes":262144,"expected_bytes":4096,
                "image_policy":{
                    "input_bytes":images::INPUT_MAX_BYTES,"input_pixels":images::INPUT_MAX_PIXELS,
                    "crop_bytes":images::CROP_MAX_BYTES,"crop_pixels":images::CROP_MAX_PIXELS,
                    "package_image_bytes":images::PACKAGE_IMAGE_BYTES,
                    "package_non_image_bytes":images::PACKAGE_NON_IMAGE_BYTES,
                    "package_bytes":images::PACKAGE_BYTES,
                    "package_decoded_bytes":images::PACKAGE_DECODED_BYTES,
                    "replay_decoded_bytes":images::REPLAY_DECODED_BYTES,
                    "payload_bytes":images::PAYLOAD_BYTES,"preview_pixels":images::CROP_MAX_PIXELS,
                }
            }),
            configuration_revision,
            trial,
        })
    }

    pub fn recognition_view(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub fn recognition_update(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        document: RecognitionDocument,
        frame_id: Option<&str>,
        document_revision: u64,
        capture_id: &str,
    ) -> Result<RecognitionView, Fault> {
        document.validate()?;
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check(document_revision, frame_id)?;
        recognition.check_capture(Some(capture_id))?;
        if let Some(frame) = &recognition.frame {
            if document.basis.frame_width != frame.image.width
                || document.basis.frame_height != frame.image.height
            {
                return Err(invalid("Geometry basis does not match the current frame"));
            }
        }
        let basis_changed = recognition
            .document
            .as_ref()
            .is_none_or(|old| old.basis != document.basis);
        if recognition.document.as_ref() != Some(&document) {
            recognition.replace_document(document)?;
            recognition.advance()?;
            if basis_changed {
                recognition.basis_confirmed = false;
                recognition.confirmed_captures.remove(capture_id);
                if let Some(frame) = &mut recognition.frame {
                    frame.confirmed = false;
                }
            }
        }
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub fn recognition_confirm(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        frame_id: &str,
        document_revision: u64,
        capture_id: &str,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.check(document_revision, Some(frame_id))?;
        recognition.check_capture(Some(capture_id))?;
        let document = recognition.document.as_ref().ok_or_else(stale)?;
        document.validate()?;
        let frame = recognition.frame.as_mut().ok_or_else(stale)?;
        if document.basis.frame_width != frame.image.width
            || document.basis.frame_height != frame.image.height
        {
            return Err(stale());
        }
        frame.confirmed = true;
        recognition.basis_confirmed = true;
        recognition.confirmed_captures.insert(capture_id.to_owned());
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub fn recognition_discard(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        capture_id: Option<&str>,
        document_revision: u64,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check_capture(capture_id)?;
        recognition.check(
            document_revision,
            recognition.frame.as_ref().map(|frame| frame.id.as_str()),
        )?;
        recognition.advance()?;
        recognition.package = recognition.saved_package.clone();
        recognition.staged_crops.clear();
        recognition.confirmed_captures = recognition
            .package
            .captures
            .iter()
            .map(|capture| capture.capture_id.clone())
            .collect();
        if recognition
            .capture_id
            .as_ref()
            .is_none_or(|id| recognition.package.document(id).is_err())
        {
            recognition.capture_id = recognition
                .package
                .captures
                .first()
                .map(|capture| capture.capture_id.clone());
        }
        if recognition.capture_id.as_deref() != capture_id {
            recognition.frame = None;
            recognition.preview_payload = None;
        }
        recognition.select_document();
        recognition.reconcile_frame();
        if let Some(frame) = &mut recognition.frame {
            frame.confirmed = false;
        }
        self.recognition_snapshot(&mut state, owner, revision)
    }

    fn settle_before_image<'a>(
        &'a self,
        owner: &AuthoringRef,
        mut state: MutexGuard<'a, Workspaces>,
    ) -> Result<MutexGuard<'a, Workspaces>, Fault> {
        self.collect(&mut state);
        if state
            .owner
            .as_ref()
            .is_some_and(|operation| !operation.terminal)
        {
            self.authoring_stop(owner)?;
            let deadline = Instant::now() + Duration::from_secs(4);
            while state
                .owner
                .as_ref()
                .is_some_and(|operation| !operation.terminal)
            {
                if Instant::now() >= deadline {
                    return Err(Fault::new(
                        "RecognitionCleanup",
                        "Previous child has not settled; replacement refused",
                    ));
                }
                drop(state);
                std::thread::sleep(Duration::from_millis(5));
                state = lock(&self.workspaces);
                state.authoring(owner)?;
                self.collect(&mut state);
            }
        }
        state.work_idle()?;
        Ok(state)
    }

    pub fn recognition_select(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        capture_id: Option<&str>,
        document_revision: u64,
        selected_capture_id: &str,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        {
            let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
            recognition.initialize(&candidate)?;
            recognition.check_capture(capture_id)?;
            if recognition.revision != document_revision {
                return Err(stale());
            }
            recognition.package.document(selected_capture_id)?;
            if capture_id == Some(selected_capture_id) {
                return self.recognition_snapshot(&mut state, owner, revision);
            }
        }
        state = self.settle_before_image(owner, state)?;
        state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.release_frame(true)?;
        recognition.capture_id = Some(selected_capture_id.to_owned());
        recognition.select_document();
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub fn recognition_load(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        path: &Path,
        capture_id: Option<&str>,
        document_revision: u64,
        new_capture: bool,
    ) -> Result<RecognitionView, Fault> {
        self.load_recognition_source(
            owner,
            revision,
            capture_id,
            document_revision,
            new_capture,
            |_| Ok(path),
        )
    }

    pub fn recognition_load_cached(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        cache: &crate::capture_cache::CaptureCache,
        capture_id: &str,
        document_revision: u64,
    ) -> Result<RecognitionView, Fault> {
        self.load_recognition_source(
            owner,
            revision,
            Some(capture_id),
            document_revision,
            false,
            |candidate| cache.image_path(candidate.package_id(), capture_id),
        )
    }

    fn load_recognition_source<P: AsRef<Path>>(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        capture_id: Option<&str>,
        document_revision: u64,
        new_capture: bool,
        resolve: impl FnOnce(&crate::authoring::Candidate) -> Result<P, Fault>,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        {
            let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
            recognition.initialize(&candidate)?;
            recognition.check_capture(capture_id)?;
            if recognition.revision != document_revision {
                return Err(stale());
            }
        }
        state = self.settle_before_image(owner, state)?;
        state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.release_frame(new_capture)?;
        let prepared_revision = recognition.revision;
        let path = resolve(&candidate)?;
        drop(state);
        let image = read_image(path.as_ref())?;
        let mut state = lock(&self.workspaces);
        state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.check_capture(capture_id)?;
        recognition.check(prepared_revision, None)?;
        recognition.install_image(owner, image, new_capture, true)?;
        self.recognition_snapshot(&mut state, owner, revision)
    }

    /// Releases the sole original before an external acquisition allocates pixels.
    /// The caller retains request, cancellation and native publication authority.
    pub fn recognition_prepare_capture(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        capture_id: Option<&str>,
        document_revision: u64,
        new_capture: bool,
        crop_ids: &[String],
        crop_sources: &BTreeMap<String, String>,
    ) -> Result<u64, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        {
            let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
            recognition.initialize(&candidate)?;
            recognition.check_capture(capture_id)?;
            if recognition.revision != document_revision {
                return Err(stale());
            }
        }
        state = self.settle_before_image(owner, state)?;
        self.prepare_native_frame(
            &mut state,
            owner,
            revision,
            capture_id,
            document_revision,
            new_capture,
            crop_ids,
            crop_sources,
        )
    }

    /// Installs an accepted detached original, refreshing unless a new namespace was requested.
    /// No acquisition, cache read or native authority is implied by this operation.
    pub fn recognition_install_capture(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        capture_id: Option<&str>,
        document_revision: u64,
        image: DecodedImage,
        new_capture: bool,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.check_capture(capture_id)?;
        recognition.check(document_revision, None)?;
        if recognition.prepared_capture != Some((document_revision, new_capture)) {
            return Err(stale());
        }
        let mut prepared = recognition.prepare_image(owner, image, new_capture, false)?;
        recognition.advance()?;
        recognition.commit_image(&mut prepared);
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub(super) fn prepare_native_frame(
        &self,
        state: &mut Workspaces,
        owner: &AuthoringRef,
        revision: &str,
        capture_id: Option<&str>,
        document_revision: u64,
        new_capture: bool,
        crop_ids: &[String],
        crop_sources: &BTreeMap<String, String>,
    ) -> Result<u64, Fault> {
        let candidate = state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check_capture(capture_id)?;
        if recognition.revision != document_revision {
            return Err(stale());
        }
        if recognition.revision >= MAX_SESSION_COUNTER {
            return Err(invalid("Recognition revision exhausted"));
        }
        let generation = recognition
            .preview_generation
            .checked_add(1)
            .ok_or_else(stale)?;
        recognition.stage_crops(crop_ids, crop_sources)?;
        // Refresh is not an assertion that user-defined content geometry is correct.
        // Once staged, no old full raster survives external acquisition or its failure.
        recognition.advance()?;
        recognition.frame = None;
        recognition.preview_payload = None;
        recognition.preview_generation = generation;
        recognition.prepared_capture = Some((recognition.revision, new_capture));
        Ok(recognition.revision)
    }

    pub(super) fn prepare_native_install(
        &self,
        state: &mut Workspaces,
        owner: &AuthoringRef,
        revision: &str,
        capture_id: Option<&str>,
        document_revision: u64,
        capture: mado_runtime_comparison::authoring_capture::DetachedCapture,
        new_capture: bool,
    ) -> Result<PreparedNativeFrame, Fault> {
        state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.check_capture(capture_id)?;
        recognition.check(document_revision, None)?;
        if recognition.prepared_capture != Some((document_revision, new_capture)) {
            return Err(stale());
        }
        let next_revision = recognition
            .revision
            .checked_add(1)
            .filter(|value| *value <= MAX_SESSION_COUNTER)
            .ok_or_else(|| invalid("Recognition revision exhausted"))?;
        let mut image = recognition.prepare_image(owner, capture.image, new_capture, false)?;
        image.frame.as_mut().expect("prepared frame").native = Some(NativeFrameOrigin {
            _geometry: capture.geometry,
            _request: capture.identity,
            _source_frame: capture.frame_identity,
            acquired_at_ms: capture.acquired_at_ms,
            _captured_monotonic_us: capture.captured_monotonic_us,
        });
        Ok(PreparedNativeFrame {
            image,
            revision: next_revision,
            png: capture.png,
        })
    }

    /// Caller holds Workspaces and the final Stop fence; all fallible work is complete.
    pub(super) fn commit_native_frame(
        &self,
        state: &mut Workspaces,
        prepared: &mut PreparedNativeFrame,
    ) {
        let recognition = &mut state
            .authoring
            .as_mut()
            .expect("reserved authoring")
            .recognition;
        recognition.revision = prepared.revision;
        if let Some(trial) = &mut recognition.trial {
            trial.stale = true;
        }
        recognition.commit_image(&mut prepared.image);
    }

    pub fn recognition_preview(
        &self,
        owner: &AuthoringRef,
        frame_id: &str,
        capture_id: &str,
    ) -> Result<Vec<u8>, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring(owner)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.check_capture(Some(capture_id))?;
        let frame = recognition
            .frame
            .as_ref()
            .filter(|frame| frame.id == frame_id)
            .ok_or_else(stale)?;
        let image = frame.image.clone();
        recognition.preview_generation = recognition
            .preview_generation
            .checked_add(1)
            .ok_or_else(stale)?;
        let generation = recognition.preview_generation;
        let _previous_display = recognition.preview_payload.take();
        drop(state);
        let preview = images::preview(&image)?;
        let encoded = images::encode_crop(&preview, [0, 0, preview.width, preview.height])?;
        let (bytes, encoded_reservation) = encoded.into_parts();
        let display_bytes = preview
            .rgba
            .len()
            .checked_add(bytes.len().checked_mul(2).ok_or_else(stale)?)
            .ok_or_else(stale)?;
        let display = images::reserve_payload(display_bytes)?;
        let mut state = lock(&self.workspaces);
        state.authoring(owner)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        if recognition.preview_generation != generation
            || recognition.capture_id.as_deref() != Some(capture_id)
            || recognition.frame.as_ref().map(|frame| frame.id.as_str()) != Some(frame_id)
        {
            return Err(stale());
        }
        recognition.preview_payload = Some(display);
        drop(encoded_reservation);
        Ok(bytes)
    }

    pub fn recognition_release_preview(&self, owner: &AuthoringRef) {
        if let Some(lease) = lock(&self.workspaces)
            .authoring
            .as_mut()
            .filter(|lease| lease.owner == *owner)
        {
            lease.recognition.preview_payload = None;
            lease.recognition.preview_generation =
                lease.recognition.preview_generation.saturating_add(1);
        }
    }

    pub fn recognition_capabilities(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<RecognitionView, Fault> {
        let (command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let mut stop = lock(&self.authoring_stop);
        let run = self.runner.recognition_capabilities()?;
        self.own_recognition_run(&mut state, owner, &run, &mut stop, None);
        drop(stop);
        drop(state);
        drop(command);
        let (state, controller) = self.settle_recognition_run(owner, &run)?;
        drop(state);
        // `collect` retained a successful limit; a failed read reports its own outcome.
        if let Some(fault) = capability_failure(&controller) {
            return Err(fault);
        }
        if capability_limit(&controller).is_none() {
            return Err(invalid("Engine did not return a grouped OCR capability"));
        }
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.recognition_snapshot(&mut state, owner, revision)
    }

    fn own_recognition_run(
        &self,
        state: &mut Workspaces,
        owner: &AuthoringRef,
        run: &str,
        stop: &mut Option<super::StopOwner>,
        trial: Option<TrialTicket>,
    ) {
        state.owner = Some(OperationOwner {
            run: run.to_owned(),
            workspace: Some(owner.workspace.clone()),
            check: None,
            terminal: false,
        });
        if let Some(lease) = state.authoring.as_mut() {
            lease.recognition.run = Some(RecognitionRun {
                id: run.to_owned(),
                trial,
                settled: None,
            });
        }
        if let Some(slot) = stop.as_mut() {
            slot.run = Some(run.to_owned());
        }
    }

    /// Waits until `collect` has settled this lease's run, then takes that outcome in the
    /// same critical section, which the caller keeps to reply. Until then shutdown can
    /// still report a settled cleanup failure this command never returned.
    fn settle_recognition_run(
        &self,
        owner: &AuthoringRef,
        run: &str,
    ) -> Result<(MutexGuard<'_, Workspaces>, Arc<Value>), Fault> {
        loop {
            let mut state = lock(&self.workspaces);
            state.authoring(owner)?;
            self.collect(&mut state);
            let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
            let settled = recognition
                .run
                .as_ref()
                .filter(|pending| pending.id == run)
                .ok_or_else(stale)?
                .settled
                .clone();
            if let Some(controller) = settled {
                recognition.run = None;
                if let Some(slot) = lock(&self.authoring_stop).as_mut() {
                    if slot.run.as_deref() == Some(run) {
                        slot.run = None;
                    }
                }
                return Ok((state, controller));
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn recognition_trial(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        frame_id: Option<&str>,
        document_revision: u64,
        selected_ids: &[String],
        sample_id: Option<&str>,
        capture_id: &str,
    ) -> Result<RecognitionTrial, Fault> {
        let (command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let environment = lock(&self.store).settings()?.ocr_environment;
        let configuration_revision = identity(&environment)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check_capture(Some(capture_id))?;
        if sample_id.is_some() {
            // Saved samples use package pixels, not the currently loaded frame.
            if recognition.revision != document_revision || frame_id.is_some() {
                return Err(stale());
            }
        } else {
            recognition.check(document_revision, frame_id)?;
        }
        let document = recognition
            .document
            .as_ref()
            .ok_or_else(|| invalid("Define a recognition region first"))?;
        document.validate()?;
        let document = document.clone();
        let image = if sample_id.is_some() {
            None
        } else {
            Some(recognition.confirmed_frame()?.image.clone())
        };
        let frame_revision = if sample_id.is_some() {
            0
        } else {
            recognition.confirmed_frame()?.revision
        };
        let maximum = recognition.max_ocr_zones;
        let selected = selected_ids.to_vec();
        let sample = sample_id.map(str::to_owned);
        let publisher = self.publisher.clone();
        let capture = capture_id.to_owned();
        let expected_revision = revision.to_owned();
        let captured = TrialIdentity {
            owner: owner.token.clone(),
            revision: revision.to_owned(),
            frame: frame_id.unwrap_or("sample").to_owned(),
            frame_revision,
            content_revision: document_revision,
            zones_revision: document_revision,
            configuration_revision: configuration_revision.clone(),
        };
        let ticket = TrialTicket {
            capture_id: capture_id.to_owned(),
            owner: owner.clone(),
            revision: revision.to_owned(),
            document_revision,
            frame_id: frame_id.map(str::to_owned),
            frame_revision,
            configuration_revision,
            sample_id: sample_id.map(str::to_owned),
        };
        let mut stop = lock(&self.authoring_stop);
        let run = self.runner.recognition_trial(
            captured,
            move |control| {
                control.check()?;
                let fresh = publisher.open(candidate.root())?;
                control.check()?;
                if fresh.revision() != expected_revision {
                    return Err(Fault::new(
                        "AuthoringConflict",
                        "Source changed before recognition trial",
                    ));
                }
                let prepared = prepare_trial(
                    &document,
                    &capture,
                    &fresh,
                    image,
                    &selected,
                    sample.as_deref(),
                    maximum,
                )?;
                control.check()?;
                Ok(prepared)
            },
            environment,
        )?;
        self.own_recognition_run(&mut state, owner, &run, &mut stop, Some(ticket));
        drop(stop);
        drop(state);
        drop(command);
        let (mut state, controller) = self.settle_recognition_run(owner, &run)?;
        let current_configuration = self.recognition_configuration()?;
        let trial = state
            .authoring
            .as_mut()
            .ok_or_else(stale)?
            .recognition
            .trial
            .as_mut()
            .filter(|trial| trial.controller["run"] == controller["run"])
            .ok_or_else(stale)?;
        // An effective OCR configuration change during the run permanently stales it.
        trial.stale |= trial.configuration_revision != current_configuration;
        Ok(trial.clone())
    }

    pub fn recognition_save(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        document_revision: u64,
        crop_ids: &[String],
        capture_id: &str,
        crop_sources: &BTreeMap<String, String>,
    ) -> Result<RecognitionSaved, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check_capture(Some(capture_id))?;
        if recognition.revision >= MAX_SESSION_COUNTER {
            return Err(invalid("Recognition revision exhausted"));
        }
        if recognition.revision != document_revision {
            return Err(stale());
        }
        let document = recognition
            .document
            .clone()
            .ok_or_else(|| invalid("No recognition definitions to save"))?;
        document.validate()?;
        // Only confirmed setup becomes reusable package geometry. Saving a new
        // basis must not bypass confirmation by reopening the package afterward.
        if recognition
            .package
            .captures
            .iter()
            .any(|capture| !recognition.basis_is_confirmed(capture))
        {
            return Err(invalid("Confirm changed content geometry before saving"));
        }
        check_crop_sources(crop_ids, crop_sources)?;
        let mut selected = Vec::with_capacity(crop_ids.len());
        let mut frame = None;
        for id in crop_ids {
            let source = &crop_sources[id];
            let staged = recognition.staged_crops.iter().find(|crop| {
                crop.capture_id == capture_id
                    && crop.definition_id == *id
                    && crop.frame_id == *source
            });
            let png = if let Some(staged) = staged {
                if staged.fingerprint != crop_fingerprint(&document, id)? {
                    return Err(invalid(
                        "Pending crop inputs changed; select its pixels again before saving",
                    ));
                }
                Some(staged.png.clone())
            } else {
                let current = recognition.confirmed_frame()?;
                if current.id != *source {
                    return Err(invalid("Selected crop original is no longer available"));
                }
                if frame.is_none() {
                    frame = Some(current.image.clone());
                }
                None
            };
            selected.push((id, png));
        }
        let package = recognition.package.clone();
        drop(state);
        let mut crops = Vec::with_capacity(crop_ids.len());
        for (id, png) in selected {
            let png = if let Some(png) = png {
                png
            } else {
                let definition = document.definition(id)?;
                let rect = definition.region.map_to_pixels(&document.basis)?;
                let encoded = images::encode_crop(
                    frame.as_deref().ok_or_else(stale)?,
                    [rect.x, rect.y, rect.width, rect.height],
                )?;
                let (png, reservation) = encoded.into_parts();
                images::PayloadBytes::from_reserved(png, reservation)?
            };
            crops.push(SelectedCrop {
                definition_id: id.clone(),
                png,
            });
        }
        let committed = self.publisher.publish_recognition(
            &candidate,
            revision,
            RecognitionSave {
                package,
                capture_id: capture_id.to_owned(),
                crops,
            },
        )?;
        let mut state = lock(&self.workspaces);
        let lease = state.authoring.as_mut().ok_or_else(stale)?;
        lease.revision = committed.committed_revision.clone();
        lease.recognition.initialized = false;
        lease.recognition.revision += 1;
        lease
            .recognition
            .staged_crops
            .retain(|crop| crop.capture_id != capture_id);
        if let Some(trial) = &mut lease.recognition.trial {
            trial.stale = true;
        }
        let mut mutation = AuthoringMutation {
            owner: owner.clone(),
            committed_revision: committed.committed_revision.clone(),
            view: committed
                .candidate
                .as_ref()
                .map(|candidate| super::view(owner, candidate)),
            refresh_error: committed.refresh_error,
        };
        if let Some(candidate) = committed.candidate {
            match candidate.recognition() {
                Ok(Some(RecognitionMetadata::Captures(package))) => {
                    lease.recognition.package = package.clone();
                    lease.recognition.saved_package = package;
                    lease.recognition.migration_required = false;
                    lease.recognition.select_document();
                    lease.recognition.source_revision = candidate.revision().to_owned();
                    lease.recognition.initialized = true;
                }
                result => {
                    mutation.view = None;
                    mutation.refresh_error = Some(result.err().unwrap_or_else(stale));
                }
            }
            lease.candidate = Arc::new(candidate);
        }
        let recognition = if mutation.view.is_some() {
            match self.recognition_snapshot(&mut state, owner, &committed.committed_revision) {
                Ok(view) => Some(view),
                Err(error) => {
                    mutation.view = None;
                    mutation.refresh_error = Some(error);
                    None
                }
            }
        } else {
            None
        };
        Ok(RecognitionSaved {
            mutation,
            recognition,
        })
    }

    pub fn recognition_copy(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        document_revision: u64,
        definition_ids: &[String],
        mode: SnippetKind,
        capture_id: &str,
    ) -> Result<RecognitionCopy, Fault> {
        // Command admission stays held throughout; only the workspaces lock is released
        // while the package is recaptured.
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check_capture(Some(capture_id))?;
        if recognition.revision != document_revision {
            return Err(stale());
        }
        let frame_id = recognition.confirmed_frame()?.id.clone();
        // Poll, preview and the preview Destroyed handler must not wait behind this capture.
        drop(state);
        // Reopen saved source before returning a runtime alias; an old trial is not asset authority.
        let saved = self.publisher.open(candidate.root())?;
        if saved.revision() != revision {
            return Err(Fault::new(
                "AuthoringConflict",
                "Source changed before Copy",
            ));
        }
        let configuration = self.recognition_configuration()?;
        let state = lock(&self.workspaces);
        state.authoring_revision(owner, revision)?;
        let recognition = &state.authoring.as_ref().ok_or_else(stale)?.recognition;
        recognition.check_capture(Some(capture_id))?;
        recognition.check(document_revision, Some(frame_id.as_str()))?;
        let frame = recognition.confirmed_frame()?;
        let document = recognition.document.as_ref().ok_or_else(stale)?;
        let template_current = recognition
            .saved_document
            .as_ref()
            .is_some_and(|stored| same_saved_content(document, stored));
        // The cached engine report is the only grouped-request bound; setup and template
        // Copy do not need it.
        let source = generate_snippet(
            document,
            definition_ids,
            mode,
            "mado-host-v1",
            true,
            template_current,
            recognition.max_ocr_zones,
        )?;
        // Game content setup is geometry data and never recognition evidence. Evidence
        // shares the displayed trial row's owner, package, draft, frame and configuration
        // fences, so a stale row cannot certify a fresh receipt.
        let verified = mode != SnippetKind::GameContent
            && recognition.trial.as_ref().is_some_and(|trial| {
                !trial.stale
                    && trial.owner == *owner
                    && trial.revision == revision
                    && trial.capture_id == capture_id
                    && trial.document_revision == document_revision
                    && trial.sample_id.is_none()
                    && trial.frame_id.as_deref() == Some(frame.id.as_str())
                    && trial.frame_revision == frame.revision
                    && trial.configuration_revision == configuration
                    && trial.controller["error"].is_null()
                    && trial.controller["result"]["primary"].is_null()
                    && trial.controller["result"]["cleanup"]["clean"] == true
                    && definition_ids
                        .iter()
                        .all(|id| observed_definition(&trial.controller["result"]["result"], id))
            });
        Ok(RecognitionCopy {
            capture_id: capture_id.to_owned(),
            source,
            basis: document.basis,
            verified,
            definition_ids: definition_ids.to_vec(),
            document_revision,
        })
    }
}

impl Application {
    /// Keeps command admission through the irreversible clipboard write.
    pub fn recognition_publish_copy(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        copied: RecognitionCopy,
        publish: impl FnOnce(&str) -> Result<(), Fault>,
    ) -> Result<RecognitionCopy, Fault> {
        let (_command, state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        let recognition = &state.authoring.as_ref().ok_or_else(stale)?.recognition;
        recognition.check_capture(Some(&copied.capture_id))?;
        if recognition.revision != copied.document_revision {
            return Err(stale());
        }
        recognition.confirmed_frame()?;
        publish(&copied.source)?;
        Ok(copied)
    }
}

fn prepare_trial(
    document: &RecognitionDocument,
    capture_id: &str,
    candidate: &Candidate,
    image: Option<Arc<DecodedImage>>,
    selected: &[String],
    sample: Option<&str>,
    maximum: Option<usize>,
) -> Result<(TrialFrame, TrialSelection), Fault> {
    if let Some(sample) = sample {
        if !selected.is_empty() && (selected.len() != 1 || selected[0] != sample) {
            return Err(invalid("Sample trial accepts only its saved definition"));
        }
        if document.definition(sample)?.kind != RecognitionKind::Ocr {
            return Err(invalid("Only OCR samples support sample rechecking"));
        }
        let bytes = candidate
            .recognition_crop(capture_id, sample)?
            .ok_or_else(|| invalid("Save an OCR sample crop first"))?;
        let image = Arc::new(images::decode_png(&bytes, ImageKind::Crop)?);
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: image.width,
            height: image.height,
        };
        return Ok((
            TrialFrame { image },
            TrialSelection::Ocr {
                zones: vec![OcrZone {
                    id: sample.to_owned(),
                    rect,
                }],
            },
        ));
    }
    let image = image.ok_or_else(stale)?;
    let distinct: BTreeSet<_> = selected.iter().collect();
    if selected.is_empty() || distinct.len() != selected.len() {
        return Err(invalid("Select distinct recognition definitions"));
    }
    let definitions = selected
        .iter()
        .map(|id| document.definition(id))
        .collect::<Result<Vec<_>, _>>()?;
    let selection = if definitions
        .iter()
        .all(|definition| definition.kind == RecognitionKind::Ocr)
    {
        let maximum = maximum
            .ok_or_else(|| invalid("Read the engine OCR capability before starting a trial"))?;
        if definitions.len() > maximum {
            return Err(invalid(
                "Selected OCR zones exceed the engine grouped-request limit",
            ));
        }
        TrialSelection::Ocr {
            zones: definitions
                .iter()
                .map(|definition| {
                    Ok(OcrZone {
                        id: definition.id.clone(),
                        rect: definition.region.map_to_pixels(&document.basis)?,
                    })
                })
                .collect::<Result<_, Fault>>()?,
        }
    } else if definitions.len() == 1 && definitions[0].kind == RecognitionKind::Template {
        TrialSelection::Template {
            template: template_input(document, capture_id, &definitions[0].id, candidate, &image)?,
        }
    } else {
        return Err(invalid("Select either OCR zones or one template"));
    };
    Ok((TrialFrame { image }, selection))
}

fn template_input(
    document: &RecognitionDocument,
    capture_id: &str,
    id: &str,
    candidate: &Candidate,
    image: &DecodedImage,
) -> Result<TemplateInput, Fault> {
    let definition = document.definition(id)?;
    let saved_document = candidate.recognition()?;
    let unchanged = saved_document
        .as_ref()
        .and_then(|saved| saved.document(capture_id).ok()?.definition(id).ok())
        .is_some_and(|saved| saved.region == definition.region && saved.saved == definition.saved);
    let png = if unchanged && definition.saved.is_some() {
        candidate
            .recognition_crop(capture_id, id)?
            .ok_or_else(stale)?
    } else {
        let rect = definition.region.map_to_pixels(&document.basis)?;
        let encoded = images::encode_crop(image, [rect.x, rect.y, rect.width, rect.height])?;
        let (bytes, reservation) = encoded.into_parts();
        images::PayloadBytes::from_reserved(bytes, reservation)?
    };
    let info = images::validate_png(&png, ImageKind::Crop)?;
    let mut staged = document.clone();
    staged.definitions.retain(|definition| definition.id == id);
    let selected = &mut staged.definitions[0];
    let asset = selected.saved.as_ref().map_or_else(
        || format!("recognition_crop_{id}"),
        |saved| saved.asset.clone(),
    );
    selected.saved = Some(SavedCrop {
        asset: asset.clone(),
        sha256: format!("{:x}", Sha256::digest(&png)),
        width: info.width,
        height: info.height,
    });
    let search = selected
        .template
        .as_ref()
        .ok_or_else(|| invalid("Template search settings are missing"))?
        .search_region
        .map_to_pixels(&staged.basis)?;
    if search.width < info.width || search.height < info.height {
        return Err(invalid(
            "Template search region is smaller than the pattern",
        ));
    }
    let (maps, manifest) = build_template_assets(std::iter::once(&staged), candidate.package_id())?
        .ok_or_else(stale)?;
    let template_id = maps.templates.get(&asset).ok_or_else(stale)?.clone();
    let template_path = maps
        .package_entries
        .iter()
        .find_map(|(path, value)| (value == &asset).then(|| path.clone()))
        .ok_or_else(stale)?;
    Ok(TemplateInput {
        id: id.to_owned(),
        search,
        manifest,
        png,
        template_id,
        template_path,
    })
}

fn observed_definition(result: &Value, id: &str) -> bool {
    if result["kind"] == "ocr" {
        result["zones"].as_array().is_some_and(|zones| {
            zones.iter().any(|zone| {
                zone["id"] == id
                    && zone["regions"]
                        .as_array()
                        .is_some_and(|regions| !regions.is_empty())
            })
        })
    } else {
        result["id"] == id
            && result["matches"]
                .as_array()
                .is_some_and(|matches| !matches.is_empty())
    }
}

#[derive(PartialEq, Eq)]
struct ImageStamp {
    length: u64,
    modified: SystemTime,
    identity: [u64; 4],
}
fn image_stamp(metadata: &Metadata) -> Result<ImageStamp, Fault> {
    if !metadata.is_file() {
        return Err(invalid("Selected image must be a regular file"));
    }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        [
            metadata.dev(),
            metadata.ino(),
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ]
    };
    #[cfg(windows)]
    let identity = {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(invalid("Image reparse points are refused"));
        }
        [
            metadata.creation_time(),
            metadata.last_write_time(),
            u64::from(metadata.file_attributes()),
            0,
        ]
    };
    #[cfg(not(any(unix, windows)))]
    let identity = [0; 4];
    Ok(ImageStamp {
        length: metadata.len(),
        modified: metadata.modified().map_err(image_io)?,
        identity,
    })
}
fn image_io(error: std::io::Error) -> Fault {
    Fault::new(
        "RecognitionImage",
        "Selected image is missing or unreadable",
    )
    .with_context(json!({"io_kind":format!("{:?}",error.kind())}))
}
fn read_image(path: &Path) -> Result<DecodedImage, Fault> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("png"))
    {
        return Err(invalid("Explicitly select an absolute PNG path"));
    }
    let resolved = path.canonicalize().map_err(image_io)?;
    let before = image_stamp(&resolved.metadata().map_err(image_io)?)?;
    let size =
        usize::try_from(before.length).map_err(|_| invalid("Image byte length overflows"))?;
    if size == 0 || size > images::INPUT_MAX_BYTES {
        return Err(invalid("Input PNG exceeds its compressed byte allowance"));
    }
    let _compressed = images::reserve_payload(size.checked_add(65536).ok_or_else(stale)?)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000);
    }
    let mut file: File = options.open(&resolved).map_err(image_io)?;
    if image_stamp(&file.metadata().map_err(image_io)?)? != before {
        return Err(stale());
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut bytes = Vec::with_capacity(size);
    let mut buffer = [0u8; 65536];
    while bytes.len() < size {
        if Instant::now() >= deadline {
            return Err(Fault::new("Timeout", "Image preparation deadline exceeded"));
        }
        let count = (size - bytes.len()).min(buffer.len());
        file.read_exact(&mut buffer[..count]).map_err(image_io)?;
        bytes.extend_from_slice(&buffer[..count]);
    }
    let mut trailing = [0; 1];
    if file.read(&mut trailing).map_err(image_io)? != 0
        || image_stamp(&file.metadata().map_err(image_io)?)? != before
        || image_stamp(&resolved.metadata().map_err(image_io)?)? != before
        || path.canonicalize().map_err(image_io)? != resolved
    {
        return Err(stale());
    }
    let decoded = images::decode_png(&bytes, ImageKind::Input)?;
    if Instant::now() >= deadline {
        return Err(Fault::new("Timeout", "Image preparation deadline exceeded"));
    }
    Ok(decoded)
}

pub struct RecognitionPickerGuard {
    application: Arc<Application>,
    owner: AuthoringRef,
    revision: String,
}

impl RecognitionPickerGuard {
    pub fn validate(&self) -> Result<(), Fault> {
        let (_command, state) = self.application.command_state()?;
        state.authoring_revision(&self.owner, &self.revision)?;
        Ok(())
    }
}

impl Drop for RecognitionPickerGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.application.workspaces);
        if state.target_picker.as_ref() == Some(&self.owner.workspace) {
            state.target_picker = None;
        }
    }
}

impl Application {
    pub fn begin_recognition_picker(
        self: &Arc<Self>,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<RecognitionPickerGuard, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle_except_native()?;
        if state
            .authoring
            .as_ref()
            .is_some_and(|lease| lease.native.operation_active())
        {
            return Err(Fault::new(
                "NativeCaptureBusy",
                "Wait for native operation and picker cleanup",
            ));
        }
        if !cfg!(any(target_os = "macos", windows)) {
            return Err(Fault::new(
                "RecognitionPlatform",
                "PNG selection requires macOS or Windows",
            ));
        }
        state.target_picker = Some(owner.workspace.clone());
        Ok(RecognitionPickerGuard {
            application: self.clone(),
            owner: owner.clone(),
            revision: revision.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests;
