use super::{AuthoringMutation, AuthoringRef, OperationOwner};
use crate::application::{Application, MAX_SESSION_COUNTER, Workspaces, lock};
use crate::authoring::{Candidate, RecognitionSave, SelectedCrop};
use mado_runtime_comparison::images::{self, DecodedImage, ImageKind, PayloadReservation};
use mado_runtime_comparison::model::{Fault, identity};
use mado_runtime_comparison::recognition::{
    GeometryBasis, PixelRect, RecognitionDocument, RecognitionKind, SavedCrop, SnippetKind,
    build_template_assets, generate_snippet,
};
use mado_runtime_comparison::recognition_trial::{
    OcrZone, TemplateInput, TrialFrame, TrialIdentity, TrialSelection,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

#[derive(Default)]
pub(super) struct RecognitionState {
    initialized: bool,
    source_revision: String,
    document: Option<RecognitionDocument>,
    saved_document: Option<RecognitionDocument>,
    revision: u64,
    frame: Option<Frame>,
    next_frame: u64,
    max_ocr_zones: Option<usize>,
    trial: Option<RecognitionTrial>,
    preview_payload: Option<PayloadReservation>,
    preview_generation: u64,
}

struct Frame {
    id: String,
    revision: u64,
    image: Arc<DecodedImage>,
    confirmed: bool,
}

#[derive(Clone, Serialize)]
pub struct RecognitionFrame {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub confirmed: bool,
}

#[derive(Serialize)]
pub struct RecognitionView {
    pub owner: AuthoringRef,
    pub revision: String,
    pub document: Option<RecognitionDocument>,
    pub saved_document: Option<RecognitionDocument>,
    pub document_revision: u64,
    pub frame: Option<RecognitionFrame>,
    pub capabilities: Value,
    pub configuration_revision: String,
    pub trial: Option<RecognitionTrial>,
}

#[derive(Clone, Serialize)]
pub struct RecognitionTrial {
    pub owner: AuthoringRef,
    pub revision: String,
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
    pub source: String,
    pub basis: GeometryBasis,
    pub verified: bool,
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
            let saved = candidate.recognition()?;
            if !self.initialized || self.document == self.saved_document {
                self.document = saved.clone();
                self.reconcile_frame();
            }
            if self.initialized && saved != self.saved_document {
                self.advance()?;
                if let Some(frame) = &mut self.frame {
                    frame.confirmed = false;
                }
            }
            self.saved_document = saved;
            self.source_revision = candidate.revision().to_owned();
            self.initialized = true;
        }
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
}

impl Application {
    fn recognition_configuration(&self) -> Result<String, Fault> {
        identity(&lock(&self.store).settings()?.ocr_environment)
    }

    fn recognition_snapshot(
        &self,
        state: &mut Workspaces,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<RecognitionView, Fault> {
        let candidate = state.authoring_revision(owner, revision)?;
        let configuration_revision = self.recognition_configuration()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        let mut trial = recognition.trial.clone();
        if let Some(trial) = &mut trial {
            trial.stale |= trial.configuration_revision != configuration_revision
                || trial.revision != revision
                || trial.document_revision != recognition.revision;
        }
        Ok(RecognitionView {
            owner: owner.clone(),
            revision: revision.to_owned(),
            document: recognition.document.clone(),
            saved_document: recognition.saved_document.clone(),
            document_revision: recognition.revision,
            frame: recognition.frame.as_ref().map(|frame| RecognitionFrame {
                id: frame.id.clone(),
                width: frame.image.width,
                height: frame.image.height,
                revision: frame.revision,
                confirmed: frame.confirmed,
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
    ) -> Result<RecognitionView, Fault> {
        document.validate()?;
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check(document_revision, frame_id)?;
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
            recognition.advance()?;
            recognition.document = Some(document);
            if basis_changed {
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
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.check(document_revision, Some(frame_id))?;
        let document = recognition.document.as_ref().ok_or_else(stale)?;
        document.validate()?;
        let frame = recognition.frame.as_mut().ok_or_else(stale)?;
        if document.basis.frame_width != frame.image.width
            || document.basis.frame_height != frame.image.height
        {
            return Err(stale());
        }
        frame.confirmed = true;
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub fn recognition_discard(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.advance()?;
        recognition.document = recognition.saved_document.clone();
        recognition.reconcile_frame();
        if let Some(frame) = &mut recognition.frame {
            frame.confirmed = false;
        }
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub fn recognition_load(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        path: &Path,
    ) -> Result<RecognitionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
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
                self.collect(&mut state);
            }
        }
        state.work_idle()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        let replacement = recognition.next_frame != 0 || recognition.document.is_some();
        recognition.advance()?;
        recognition.frame = None;
        recognition.preview_payload = None;
        recognition.next_frame = recognition
            .next_frame
            .checked_add(1)
            .filter(|value| *value <= MAX_SESSION_COUNTER)
            .ok_or_else(stale)?;
        let frame_revision = recognition.next_frame;
        drop(state);
        let image = Arc::new(read_image(path)?);
        let mut state = lock(&self.workspaces);
        state.authoring_revision(owner, revision)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        let full = PixelRect {
            x: 0,
            y: 0,
            width: image.width,
            height: image.height,
        };
        let document = recognition
            .document
            .get_or_insert_with(|| RecognitionDocument {
                version: 1,
                rounding: 1,
                basis: GeometryBasis {
                    frame_width: image.width,
                    frame_height: image.height,
                    content: full,
                },
                definitions: Vec::new(),
                template_rights: None,
            });
        document.basis.frame_width = image.width;
        document.basis.frame_height = image.height;
        if document
            .basis
            .content
            .validate_in(image.width, image.height)
            .is_err()
        {
            document.basis.content = full;
        }
        recognition.frame = Some(Frame {
            id: format!("{}-frame-{frame_revision}", owner.token),
            revision: frame_revision,
            image,
            confirmed: !replacement,
        });
        self.recognition_snapshot(&mut state, owner, revision)
    }

    pub fn recognition_preview(
        &self,
        owner: &AuthoringRef,
        frame_id: &str,
    ) -> Result<Vec<u8>, Fault> {
        let mut state = lock(&self.workspaces);
        state.authoring(owner)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
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
        self.own_recognition_run(&mut state, owner, &run, &mut stop);
        drop(stop);
        drop(state);
        drop(command);
        let controller = self.settle_recognition_run(owner, &run)?;
        if !controller["error"].is_null() {
            return Err(serde_json::from_value(controller["error"].clone()).map_err(|_| stale())?);
        }
        let maximum = controller["result"]["capabilities"]["max_ocr_zones"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("Engine did not return a grouped OCR capability"))?;
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        state
            .authoring
            .as_mut()
            .ok_or_else(stale)?
            .recognition
            .max_ocr_zones = Some(maximum);
        self.recognition_snapshot(&mut state, owner, revision)
    }

    fn own_recognition_run(
        &self,
        state: &mut Workspaces,
        owner: &AuthoringRef,
        run: &str,
        stop: &mut Option<super::StopOwner>,
    ) {
        state.owner = Some(OperationOwner {
            run: run.to_owned(),
            workspace: Some(owner.workspace.clone()),
            check: None,
            terminal: false,
        });
        if let Some(slot) = stop.as_mut() {
            slot.run = Some(run.to_owned());
        }
    }

    fn settle_recognition_run(&self, owner: &AuthoringRef, run: &str) -> Result<Arc<Value>, Fault> {
        loop {
            let mut state = lock(&self.workspaces);
            state.authoring(owner)?;
            self.collect(&mut state);
            let operation = state
                .owner
                .as_ref()
                .filter(|operation| operation.run == run)
                .ok_or_else(stale)?;
            if operation.terminal {
                let result = state.controller.clone();
                if let Some(slot) = lock(&self.authoring_stop).as_mut() {
                    if slot.run.as_deref() == Some(run) {
                        slot.run = None;
                    }
                }
                if result["result"]["cleanup"]["clean"] == false
                    || result["error"]["context"]["cleanup"]["clean"] == false
                {
                    state.authoring.as_mut().ok_or_else(stale)?.containment = Some(Fault::new(
                        "RecognitionCleanup",
                        "Recognition cleanup is incomplete; close the application before further work",
                    ));
                }
                return Ok(result);
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
    ) -> Result<RecognitionTrial, Fault> {
        let (command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let environment = lock(&self.store).settings()?.ocr_environment;
        let configuration_revision = identity(&environment)?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        recognition.check(document_revision, frame_id)?;
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
        self.own_recognition_run(&mut state, owner, &run, &mut stop);
        drop(stop);
        drop(state);
        drop(command);
        let controller = self.settle_recognition_run(owner, &run)?;
        let mut state = lock(&self.workspaces);
        state.authoring(owner)?;
        let current_configuration = self.recognition_configuration()?;
        let lease = state.authoring.as_mut().ok_or_else(stale)?;
        let recognition = &mut lease.recognition;
        let is_stale = lease.revision != revision
            || recognition.revision != document_revision
            || current_configuration != configuration_revision
            || recognition.frame.as_ref().map(|frame| frame.id.as_str()) != frame_id;
        let result = RecognitionTrial {
            owner: owner.clone(),
            revision: revision.to_owned(),
            document_revision,
            frame_id: frame_id.map(str::to_owned),
            frame_revision,
            configuration_revision,
            sample_id: sample_id.map(str::to_owned),
            stale: is_stale,
            controller,
        };
        recognition.trial = Some(result.clone());
        Ok(result)
    }

    pub fn recognition_save(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        document_revision: u64,
        crop_ids: &[String],
    ) -> Result<RecognitionSaved, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
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
        let distinct: BTreeSet<_> = crop_ids.iter().collect();
        if distinct.len() != crop_ids.len() {
            return Err(invalid("Crop selection contains duplicates"));
        }
        let frame = if crop_ids.is_empty() {
            None
        } else {
            Some(recognition.confirmed_frame()?.image.clone())
        };
        drop(state);
        let mut crops = Vec::with_capacity(crop_ids.len());
        for id in crop_ids {
            let definition = document.definition(id)?;
            let rect = definition.region.map_to_pixels(&document.basis)?;
            let encoded = images::encode_crop(
                frame.as_deref().ok_or_else(stale)?,
                [rect.x, rect.y, rect.width, rect.height],
            )?;
            let (png, reservation) = encoded.into_parts();
            crops.push(SelectedCrop {
                definition_id: id.clone(),
                png: images::PayloadBytes::from_reserved(png, reservation)?,
            });
        }
        let committed = self.publisher.publish_recognition(
            &candidate,
            revision,
            RecognitionSave { document, crops },
        )?;
        let mut state = lock(&self.workspaces);
        let lease = state.authoring.as_mut().ok_or_else(stale)?;
        lease.revision = committed.committed_revision.clone();
        lease.recognition.initialized = false;
        lease.recognition.revision += 1;
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
                Ok(document) => {
                    lease.recognition.document = document;
                    lease.recognition.saved_document = lease.recognition.document.clone();
                    lease.recognition.source_revision = candidate.revision().to_owned();
                    lease.recognition.initialized = true;
                }
                Err(error) => {
                    mutation.view = None;
                    mutation.refresh_error = Some(error);
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
        definition_id: &str,
        mode: SnippetKind,
    ) -> Result<RecognitionCopy, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        let configuration = self.recognition_configuration()?;
        let recognition = &mut state.authoring.as_mut().ok_or_else(stale)?.recognition;
        recognition.initialize(&candidate)?;
        if recognition.revision != document_revision {
            return Err(stale());
        }
        recognition.confirmed_frame()?;
        let document = recognition.document.as_ref().ok_or_else(stale)?;
        let template_current = recognition
            .saved_document
            .as_ref()
            .is_some_and(|saved| saved == document);
        // Reopen saved source before returning a runtime alias; an old trial is not asset authority.
        let saved = self.publisher.open(candidate.root())?;
        if saved.revision() != revision {
            return Err(Fault::new(
                "AuthoringConflict",
                "Source changed before Copy",
            ));
        }
        let source = generate_snippet(
            document,
            definition_id,
            mode,
            "mado-host-v1",
            true,
            template_current,
            1000,
        )?;
        let verified = recognition.trial.as_ref().is_some_and(|trial| {
            !trial.stale
                && trial.sample_id.is_none()
                && trial.document_revision == document_revision
                && trial.configuration_revision == configuration
                && trial.controller["error"].is_null()
                && trial.controller["result"]["primary"].is_null()
                && trial.controller["result"]["cleanup"]["clean"] == true
                && observed_definition(&trial.controller["result"]["result"], definition_id)
        });
        Ok(RecognitionCopy {
            source,
            basis: document.basis,
            verified,
            document_revision,
        })
    }
}

fn prepare_trial(
    document: &RecognitionDocument,
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
            .recognition_crop(sample)?
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
            template: template_input(document, &definitions[0].id, candidate, &image)?,
        }
    } else {
        return Err(invalid("Select either OCR zones or one template"));
    };
    Ok((TrialFrame { image }, selection))
}

fn template_input(
    document: &RecognitionDocument,
    id: &str,
    candidate: &Candidate,
    image: &DecodedImage,
) -> Result<TemplateInput, Fault> {
    let definition = document.definition(id)?;
    let saved_document = candidate.recognition()?;
    let unchanged = saved_document
        .as_ref()
        .and_then(|saved| saved.definition(id).ok())
        .is_some_and(|saved| saved.region == definition.region && saved.saved == definition.saved);
    let png = if unchanged && definition.saved.is_some() {
        candidate.recognition_crop(id)?.ok_or_else(stale)?
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
    let (maps, manifest) =
        build_template_assets(&staged, candidate.package_id())?.ok_or_else(stale)?;
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
        state.work_idle()?;
        if !cfg!(target_os = "macos") {
            return Err(Fault::new(
                "RecognitionPlatform",
                "PNG selection requires macOS",
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
