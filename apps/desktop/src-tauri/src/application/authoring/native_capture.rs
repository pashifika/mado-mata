//! Retained worker ownership. Native metadata never crosses the renderer boundary.
use super::{AuthoringRef, RecognitionView};
use crate::application::{Application, MAX_SESSION_COUNTER, Workspaces, lock};
use mado_runtime_comparison::authoring_capture::{
    AuthoringCaptureWorker, Candidate, CaptureArea, CaptureAuthority, CaptureIdentity,
    CoordinateUnit, DiscoveryRequest, NativeWindow, ProcessCorrespondence, WorkerCancel,
    WorkerSettlement,
};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::{Fault, identity};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

mod windows_file;
use windows_file::WindowsExecutableGuard;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCoordinateUnit {
    MacosGlobalPoints,
    WindowsPhysicalPixels,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCaptureArea {
    MacosWindow,
    WindowsExtendedFrame,
    WindowsClient,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeCaptureBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub unit: NativeCoordinateUnit,
}
/// Deliberately not serializable: native keys and rectangles stay in the host.
#[derive(Clone, Debug)]
pub struct NativePickerCandidate {
    pub id: String,
    pub application_label: String,
    pub window_label: String,
    pub native_window_id: u64,
    pub process_id: u32,
    pub capture_area: NativeCaptureArea,
    pub bounds: NativeCaptureBounds,
}
pub struct NativePickerSnapshot {
    pub owner: AuthoringRef,
    pub package_revision: String,
    pub selection_generation: u64,
    pub candidates: Vec<NativePickerCandidate>,
    pub cancelled: Arc<AtomicBool>,
}
#[derive(Clone, Debug, Serialize)]
pub struct NativeCandidateView {
    pub id: String,
    pub application_label: String,
    pub window_label: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct NativeSelectionView {
    pub owner: AuthoringRef,
    pub revision: String,
    pub selection_generation: u64,
    pub selected_id: Option<String>,
    pub candidates: Vec<NativeCandidateView>,
    pub status: &'static str,
    pub platform: &'static str,
    pub occupied: bool,
    pub error: Option<Fault>,
}
#[derive(Serialize)]
pub struct NativeCaptureResult {
    pub recognition: RecognitionView,
    pub selection: NativeSelectionView,
    pub cache: Option<crate::capture_cache::CacheWrite>,
}

pub(super) struct NativeControl {
    cancelled: Arc<AtomicBool>,
    worker: Option<WorkerCancel>,
    authority: Option<Arc<CaptureAuthority>>,
}
impl NativeControl {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(authority) = &self.authority {
            authority.cancel();
        }
        if let Some(worker) = &self.worker {
            worker.cancel();
        }
    }
}

struct Publication<'a> {
    owner: &'a AuthoringRef,
    package: &'a crate::authoring::Candidate,
    source: &'a Source,
    correlation: &'a CaptureIdentity,
    authority: &'a Arc<CaptureAuthority>,
    cancelled: &'a AtomicBool,
    deadline: Instant,
}

#[derive(Clone)]
enum Source {
    Macos {
        internal_name: String,
        record: crate::target::TargetRecord,
        declaration: TargetDeclaration,
        proof: crate::target::AuthoringApplication,
    },
    Windows {
        selected_path: PathBuf,
        executable: WindowsExecutableGuard,
        declaration: TargetDeclaration,
    },
}
impl Source {
    fn path(&self) -> &Path {
        match self {
            Self::Macos { record, .. } => Path::new(
                &record
                    .binding
                    .as_ref()
                    .expect("validated binding")
                    .resolution
                    .game
                    .path,
            ),
            Self::Windows { executable, .. } => executable.canonical_path(),
        }
    }
    fn title(&self) -> Option<String> {
        match self {
            Self::Macos { record, .. } => Some(
                record
                    .binding
                    .as_ref()
                    .expect("validated binding")
                    .configuration
                    .window_title
                    .clone(),
            ),
            Self::Windows { declaration, .. } => declaration.window_title.clone(),
        }
    }
    fn binding_revision(&self) -> Result<String, Fault> {
        match self {
            Self::Macos { record, proof, .. } => identity(&(record, &proof.installation)),
            Self::Windows { executable, .. } => {
                identity(&(executable.canonical_path(), executable.identity()))
            }
        }
    }
    fn accepts(&self, candidate: &Candidate) -> bool {
        match self {
            Self::Macos { proof, .. } => {
                matches!(candidate.native_window, NativeWindow::Macos(_))
                    && proof.processes.iter().any(|process| {
                        process.pid == candidate.process_id
                            && process.lifetime == candidate.process_lifetime
                            && process.executable == candidate.executable_path
                    })
            }
            // Disk identity stays pinned here; the original worker owns process/window lifetime.
            Self::Windows { .. } => matches!(candidate.native_window, NativeWindow::Windows(_)),
        }
    }
    fn application_label(&self) -> String {
        safe_label(
            self.path()
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Application"),
            "Application",
        )
    }
}

pub(crate) struct NativeState {
    generation: u64,
    active: bool,
    picker: bool,
    worker: Option<AuthoringCaptureWorker>,
    cancel: Option<WorkerCancel>,
    cancelled: Arc<AtomicBool>,
    source: Option<Source>,
    correlation: Option<CaptureIdentity>,
    candidates: Vec<Candidate>,
    selected: Option<String>,
    status: &'static str,
    error: Option<Fault>,
}
impl Default for NativeState {
    fn default() -> Self {
        Self {
            generation: 0,
            active: false,
            picker: false,
            worker: None,
            cancel: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            source: None,
            correlation: None,
            candidates: Vec::new(),
            selected: None,
            status: "unselected",
            error: None,
        }
    }
}
impl NativeState {
    pub(crate) fn occupied(&self) -> bool {
        self.active || self.picker || self.worker.is_some() || self.cancel.is_some()
    }
    pub(crate) fn operation_active(&self) -> bool {
        self.active || self.picker
    }
    pub(crate) fn view(&self, owner: &AuthoringRef, revision: &str) -> NativeSelectionView {
        let application_label = self
            .source
            .as_ref()
            .map(Source::application_label)
            .unwrap_or_default();
        let cancelling = self.occupied() && self.cancelled.load(Ordering::Acquire);
        NativeSelectionView {
            owner: owner.clone(),
            revision: revision.into(),
            selection_generation: self.generation,
            selected_id: if cancelling {
                None
            } else {
                self.selected.clone()
            },
            status: if cancelling {
                "cancelling"
            } else {
                self.status
            },
            occupied: self.occupied(),
            platform: if cfg!(target_os = "macos") {
                "macos"
            } else if cfg!(windows) {
                "windows"
            } else {
                "unsupported"
            },
            candidates: self
                .candidates
                .iter()
                .filter(|_| !cancelling)
                .map(|candidate| NativeCandidateView {
                    id: candidate.key.clone(),
                    application_label: application_label.clone(),
                    window_label: safe_label(&candidate.label, "Window"),
                })
                .collect(),
            error: self.error.clone(),
        }
    }
    fn check(&self, generation: u64) -> Result<(), Fault> {
        if self.generation != generation
            || self.active
            || self.cancelled.load(Ordering::Acquire)
            || self.worker.is_none()
            || self.cancel.as_ref().is_some_and(WorkerCancel::is_finished)
        {
            return Err(stale());
        }
        Ok(())
    }
    fn retire(&mut self, status: &'static str) {
        self.worker = None;
        self.cancel = None;
        self.source = None;
        self.correlation = None;
        self.candidates.clear();
        self.selected = None;
        self.active = false;
        self.status = status;
    }
}

fn stale() -> Fault {
    Fault::new(
        "StaleNativeSelection",
        "Discover and explicitly select a current window before capture",
    )
}

fn publication_checkpoint(cancel: &AtomicBool, deadline: Instant) -> Result<(), Fault> {
    if cancel.load(Ordering::Acquire) {
        return Err(cancelled());
    }
    if Instant::now() >= deadline {
        return Err(Fault::new(
            "NativeCaptureTimeout",
            "Native authoring deadline expired",
        ));
    }
    Ok(())
}
fn cancelled() -> Fault {
    Fault::new("Cancelled", "Native authoring was cancelled")
}
fn cleanup(settlement: &WorkerSettlement) -> Option<Fault> {
    (!settlement.child_reaped || !settlement.clean || settlement.forced).then(|| {
        Fault::new(
            "NativeCaptureCleanup",
            "Native worker cleanup was not clean; exit and relaunch before reuse",
        )
        .with_context(
            serde_json::json!({"child_reaped":settlement.child_reaped,"forced":settlement.forced}),
        )
    })
}
fn safe_label(value: &str, fallback: &str) -> String {
    // Titles can contain control sequences or literal paths; neither belongs in routine UI metadata.
    if value.contains('/') || value.contains('\\') {
        return fallback.into();
    }
    let label: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect();
    if label.trim().is_empty() {
        fallback.into()
    } else {
        label
    }
}

fn public_fault(error: Fault) -> Fault {
    // Correspondence faults can contain canonical paths in diagnostic context.
    // The renderer gets categories and safe stages, not the private evidence.
    let mut fault = Fault::new(&error.category, &error.message);
    for key in ["stage", "status", "child_reaped", "forced"] {
        if let Some(value) = error.context.get(key) {
            fault.context[key] = value.clone();
        }
    }
    fault
}

impl Application {
    pub(in crate::application) fn cancel_native(&self) {
        if let Some(control) = lock(&self.authoring_stop)
            .as_ref()
            .and_then(|slot| slot.native.as_ref())
        {
            control.cancel();
        }
    }

    pub(in crate::application) fn collect_native(&self, state: &mut Workspaces) {
        let Some(lease) = state.authoring.as_mut() else {
            return;
        };
        if lease.native.active {
            return;
        }
        let Some(settlement) = lease
            .native
            .cancel
            .as_ref()
            .and_then(WorkerCancel::try_settlement)
        else {
            return;
        };
        let failure = cleanup(&settlement);
        if let Some(error) = &failure {
            lease.containment = Some(error.clone());
            lease.native.error = Some(error.clone());
        }
        let was_cancelled = lease.native.cancelled.load(Ordering::Acquire);
        lease.native.cancelled.store(true, Ordering::Release);
        if !settlement.child_reaped {
            return;
        }
        let status = if failure.is_some() {
            "failed"
        } else if was_cancelled {
            "cancelled"
        } else {
            "expired"
        };
        lease.native.error = failure.or_else(|| settlement.primary.clone());
        lease.native.retire(status);
        if let Some(slot) = lock(&self.authoring_stop).as_mut() {
            slot.native = None;
        }
    }

    pub(in crate::application) fn settle_native_selection<'a>(
        &'a self,
        owner: &AuthoringRef,
        mut state: MutexGuard<'a, Workspaces>,
    ) -> Result<MutexGuard<'a, Workspaces>, Fault> {
        let lease = state
            .authoring
            .as_mut()
            .filter(|lease| lease.owner == *owner)
            .ok_or_else(stale)?;
        if let Some(error) = &lease.containment {
            return Err(error.clone());
        }
        if lease.native.active || lease.native.picker {
            return Err(Fault::new(
                "NativeCaptureBusy",
                "Wait for native operation and picker cleanup",
            ));
        }
        let Some(worker) = lease.native.worker.take() else {
            return Ok(state);
        };
        lease.native.active = true;
        self.cancel_native();
        drop(state);
        let settlement = worker.settle();
        state = lock(&self.workspaces);
        let lease = state
            .authoring
            .as_mut()
            .filter(|lease| lease.owner == *owner)
            .ok_or_else(stale)?;
        lease.native.active = false;
        if let Some(error) = cleanup(&settlement) {
            lease.containment = Some(error.clone());
            lease.native.error = Some(error.clone());
            if settlement.child_reaped {
                lease.native.retire("failed");
            }
            return Err(error);
        }
        let status = if lease.native.error.is_some() {
            "failed"
        } else {
            "cancelled"
        };
        lease.native.retire(status);
        if let Some(slot) = lock(&self.authoring_stop).as_mut() {
            slot.native = None;
        }
        Ok(state)
    }

    fn native_source(
        &self,
        candidate: &crate::authoring::Candidate,
        internal_name: &str,
        executable: Option<&Path>,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> Result<Source, Fault> {
        let current = self.publisher.open(candidate.root())?;
        if current.revision() != candidate.revision() {
            return Err(stale());
        }
        let inventory = candidate.validate()?;
        let declaration = inventory
            .target()?
            .ok_or_else(|| Fault::new("TargetUndeclared", "Package has no target declaration"))?;
        if cfg!(target_os = "macos") {
            let record = lock(&self.store).read_target(internal_name, candidate.package_id())?;
            let binding = record.binding.as_ref().ok_or_else(|| {
                Fault::new(
                    "TargetUnbound",
                    "Save a compatible application bundle binding first",
                )
            })?;
            if !binding.compatible(
                candidate.package_id(),
                &declaration.id,
                &declaration.identity()?,
            ) || binding.configuration.game.kind != "bundle"
            {
                return Err(Fault::new(
                    "TargetIncompatible",
                    "Capture requires a compatible saved application bundle",
                ));
            }
            let proof = crate::target::authoring_application(
                &binding.configuration,
                &declaration,
                &binding.resolution,
                cancelled,
                deadline,
            )?;
            Ok(Source::Macos {
                internal_name: internal_name.into(),
                record,
                declaration,
                proof,
            })
        } else if cfg!(windows) {
            if declaration.macos.is_some() {
                return Err(Fault::new(
                    "TargetIncompatible",
                    "A macOS-specific target does not authorize Windows capture",
                ));
            }
            let path = executable.ok_or_else(|| {
                Fault::new(
                    "NativeExecutableRequired",
                    "Select the actual game's absolute executable",
                )
            })?;
            let executable = WindowsExecutableGuard::open(path, cancelled, deadline)?;
            Ok(Source::Windows {
                selected_path: path.to_path_buf(),
                executable,
                declaration,
            })
        } else {
            Err(Fault::new(
                "NativeCapturePlatform",
                "Native capture requires macOS or Windows",
            ))
        }
    }

    fn revalidate_native_source(
        &self,
        source: &Source,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> Result<(), Fault> {
        if cancelled.load(Ordering::Acquire) {
            return Err(self::cancelled());
        }
        if let Source::Macos {
            internal_name,
            record,
            declaration,
            proof,
        } = source
        {
            let current = lock(&self.store).read_target(internal_name, &record.package_id)?;
            if current != *record {
                return Err(stale());
            }
            let binding = record.binding.as_ref().ok_or_else(stale)?;
            let current = crate::target::authoring_application(
                &binding.configuration,
                declaration,
                &binding.resolution,
                cancelled,
                deadline,
            )?;
            if current != *proof {
                return Err(stale());
            }
        }
        if let Source::Windows {
            selected_path,
            executable,
            ..
        } = source
        {
            executable.validate(selected_path, cancelled, deadline)?;
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(self::cancelled());
        }
        if Instant::now() >= deadline {
            return Err(Fault::new(
                "NativeCaptureTimeout",
                "Native authoring deadline expired",
            ));
        }
        Ok(())
    }

    fn revalidate_detached_installation(
        &self,
        source: &Source,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<(), Fault> {
        publication_checkpoint(cancel, deadline)?;
        match source {
            Source::Macos {
                record,
                declaration,
                proof,
                ..
            } => {
                let binding = record.binding.as_ref().ok_or_else(stale)?;
                crate::target::revalidate_authoring_installation(
                    &binding.configuration,
                    declaration,
                    &binding.resolution,
                    proof,
                    cancel,
                    deadline,
                )?;
            }
            Source::Windows {
                selected_path,
                executable,
                ..
            } => {
                executable.validate(selected_path, cancel, deadline)?;
            }
        }
        publication_checkpoint(cancel, deadline)
    }

    fn revalidate_native_publication(&self, publication: &Publication<'_>) -> Result<(), Fault> {
        publication_checkpoint(publication.cancelled, publication.deadline)?;
        // Read mutable package/binding state after image and static-installation preparation.
        let current = self.publisher.open(publication.package.root())?;
        if current.revision() != publication.correlation.package_revision
            || publication.source.binding_revision()? != publication.correlation.binding_revision
        {
            return Err(stale());
        }
        if let Source::Macos {
            internal_name,
            record,
            ..
        } = publication.source
        {
            let current = lock(&self.store).read_target(internal_name, &record.package_id)?;
            if current != *record {
                return Err(stale());
            }
        }
        publication_checkpoint(publication.cancelled, publication.deadline)
    }

    fn publish_native_frame(
        &self,
        state: &mut Workspaces,
        publication: Publication<'_>,
        prepare: impl FnOnce(&mut Workspaces) -> Result<super::recognition::PreparedNativeFrame, Fault>,
    ) -> Result<mado_runtime_comparison::images::PayloadBytes, Fault> {
        publication_checkpoint(publication.cancelled, publication.deadline)?;
        publication.authority.check(publication.correlation)?;
        // ID generation, aggregate metadata validation and allocation must not hold Stop.
        // NativeControl and the operation reservation remain installed throughout preparation.
        let mut prepared = prepare(state)?;
        self.revalidate_native_publication(&publication)?;
        {
            let stop = lock(&self.authoring_stop);
            let control =
                stop.as_ref()
                    .filter(|slot| slot.owner == *publication.owner)
                    .and_then(|slot| slot.native.as_ref())
                    .filter(|control| {
                        std::ptr::eq(control.cancelled.as_ref(), publication.cancelled)
                            && control.authority.as_ref().is_some_and(|authority| {
                                Arc::ptr_eq(authority, publication.authority)
                            })
                    })
                    .ok_or_else(stale)?;
            publication_checkpoint(&control.cancelled, publication.deadline)?;
            if self.closing.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            publication.authority.check(publication.correlation)?;
            self.commit_native_frame(state, &mut prepared);
        }
        // Replaced metadata and rejected prepared pixels are dropped outside the Stop fence.
        Ok(prepared.png)
    }

    pub fn native_discover(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        executable: Option<&Path>,
    ) -> Result<NativeSelectionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        let candidate = state.authoring_revision(owner, revision)?;
        let internal_name = state
            .resolve_workspace(&owner.workspace)?
            .internal_name
            .clone();
        self.collect(&mut state);
        state = self.settle_native_selection(owner, state)?;
        state.work_idle()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let cancel = Arc::new(AtomicBool::new(false));
        let lease = state.authoring.as_mut().ok_or_else(stale)?;
        lease.native.generation = lease
            .native
            .generation
            .checked_add(1)
            .filter(|value| *value <= MAX_SESSION_COUNTER)
            .ok_or_else(stale)?;
        lease.native.active = true;
        lease.native.status = "discovering";
        lease.native.error = None;
        lease.native.cancelled = cancel.clone();
        let generation = lease.native.generation;
        lock(&self.authoring_stop)
            .as_mut()
            .ok_or_else(stale)?
            .native = Some(NativeControl {
            cancelled: cancel.clone(),
            worker: None,
            authority: None,
        });
        drop(state);
        let mut worker = None;
        let result = (|| {
            let source =
                self.native_source(&candidate, &internal_name, executable, &cancel, deadline)?;
            let correlation = CaptureIdentity {
                owner: owner.token.clone(),
                package_revision: revision.into(),
                binding_revision: source.binding_revision()?,
                selection_generation: generation,
                request_id: format!("{}-selection-{generation}", owner.token),
            };
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let request = DiscoveryRequest {
                identity: correlation.clone(),
                executable_or_bundle: source.path().to_path_buf(),
                exact_window_title: source.title(),
                verified_processes: match &source {
                    Source::Macos { proof, .. } => proof
                        .processes
                        .iter()
                        .map(|process| ProcessCorrespondence {
                            process_id: process.pid,
                            process_lifetime: process.lifetime,
                            executable_path: process.executable.clone(),
                        })
                        .collect(),
                    Source::Windows { .. } => Vec::new(),
                },
                timeout: deadline.saturating_duration_since(Instant::now()),
            };
            // The reservation and independent cancellation exist before child creation.
            worker = Some(AuthoringCaptureWorker::spawn(&self.native_engine, request)?);
            let handle = worker.as_ref().ok_or_else(stale)?.cancel_handle();
            {
                let mut stop = lock(&self.authoring_stop);
                let control = stop
                    .as_mut()
                    .and_then(|slot| slot.native.as_mut())
                    .ok_or_else(stale)?;
                control.worker = Some(handle.clone());
                if control.cancelled.load(Ordering::Acquire) {
                    handle.cancel();
                }
            }
            let candidates = worker.as_mut().ok_or_else(stale)?.discover()?;
            if cancel.load(Ordering::Acquire) || self.closing.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            if candidates.is_empty()
                || candidates.len() > 64
                || candidates.iter().any(|candidate| {
                    candidate.process_id == std::process::id() || !source.accepts(candidate)
                })
            {
                return Err(Fault::new(
                    "NativeCorrespondence",
                    "No complete verified eligible window set is available",
                ));
            }
            Ok((source, correlation, candidates))
        })();
        let mut state = lock(&self.workspaces);
        let lease = state.authoring.as_mut().ok_or_else(stale)?;
        lease.native.cancel = worker.as_ref().map(AuthoringCaptureWorker::cancel_handle);
        lease.native.worker = worker;
        lease.native.active = false;
        match result {
            Ok((source, correlation, candidates)) => {
                lease.native.source = Some(source);
                lease.native.correlation = Some(correlation);
                lease.native.candidates = candidates;
                lease.native.status = "selecting";
            }
            Err(error) => {
                let error = public_fault(error);
                lease.native.error = Some(error.clone());
                lease.native.status = "failed";
                if lease.native.worker.is_some() {
                    drop(self.settle_native_selection(owner, state)?);
                } else {
                    lock(&self.authoring_stop)
                        .as_mut()
                        .ok_or_else(stale)?
                        .native = None;
                }
                return Err(error);
            }
        }
        Ok(state
            .authoring
            .as_ref()
            .ok_or_else(stale)?
            .native
            .view(owner, revision))
    }

    pub fn native_release_selection(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<NativeSelectionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state = self.settle_native_selection(owner, state)?;
        Ok(state
            .authoring
            .as_ref()
            .ok_or_else(stale)?
            .native
            .view(owner, revision))
    }

    pub fn native_picker_snapshot(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<NativePickerSnapshot, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle_except_native()?;
        let native = &mut state.authoring.as_mut().ok_or_else(stale)?.native;
        native.check(native.generation)?;
        if native.picker {
            return Err(Fault::new(
                "NativePickerBusy",
                "Native picker is already open",
            ));
        }
        let application_label = native
            .source
            .as_ref()
            .ok_or_else(stale)?
            .application_label();
        let candidates = native
            .candidates
            .iter()
            .map(|candidate| NativePickerCandidate {
                id: candidate.key.clone(),
                application_label: application_label.clone(),
                window_label: safe_label(&candidate.label, "Window"),
                native_window_id: match candidate.native_window {
                    NativeWindow::Macos(id) => u64::from(id),
                    NativeWindow::Windows(id) => id,
                },
                process_id: candidate.process_id,
                capture_area: match candidate.area {
                    CaptureArea::MacosWindow => NativeCaptureArea::MacosWindow,
                    CaptureArea::WindowsExtendedFrame => NativeCaptureArea::WindowsExtendedFrame,
                    CaptureArea::WindowsClient => NativeCaptureArea::WindowsClient,
                },
                bounds: NativeCaptureBounds {
                    x: candidate.geometry.x,
                    y: candidate.geometry.y,
                    width: candidate.geometry.width,
                    height: candidate.geometry.height,
                    unit: match candidate.geometry.unit {
                        CoordinateUnit::MacosGlobalPoints => {
                            NativeCoordinateUnit::MacosGlobalPoints
                        }
                        CoordinateUnit::WindowsPhysicalPixels => {
                            NativeCoordinateUnit::WindowsPhysicalPixels
                        }
                    },
                },
            })
            .collect();
        native.picker = true;
        Ok(NativePickerSnapshot {
            owner: owner.clone(),
            package_revision: revision.into(),
            selection_generation: native.generation,
            candidates,
            cancelled: native.cancelled.clone(),
        })
    }

    pub fn native_select_candidate(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        generation: u64,
        candidate_id: &str,
    ) -> Result<NativeSelectionView, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle_except_native()?;
        let native = &mut state.authoring.as_mut().ok_or_else(stale)?.native;
        native.check(generation)?;
        if native.picker {
            return Err(Fault::new(
                "NativePickerBusy",
                "Remove picker overlays before selection",
            ));
        }
        let expected = native
            .candidates
            .iter()
            .find(|candidate| candidate.key == candidate_id)
            .ok_or_else(stale)?
            .clone();
        let source = native.source.clone().ok_or_else(stale)?;
        let cancel = native.cancelled.clone();
        let mut worker = native.worker.take().ok_or_else(stale)?;
        native.active = true;
        drop(state);
        let result = self
            .revalidate_native_source(&source, &cancel, Instant::now() + Duration::from_secs(5))
            .and_then(|()| worker.select(candidate_id))
            .and_then(|selected| {
                if selected == expected {
                    Ok(())
                } else {
                    Err(stale())
                }
            });
        let mut state = lock(&self.workspaces);
        let native = &mut state.authoring.as_mut().ok_or_else(stale)?.native;
        native.worker = Some(worker);
        native.active = false;
        if let Err(error) = result {
            let error = public_fault(error);
            native.error = Some(error.clone());
            drop(self.settle_native_selection(owner, state)?);
            return Err(error);
        }
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        native.selected = Some(candidate_id.into());
        native.status = "selected";
        Ok(native.view(owner, revision))
    }

    pub fn native_picker_finished(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        generation: u64,
    ) -> Result<(), Fault> {
        // Cleanup can finish after Stop/closing. It must not require ordinary command admission.
        let mut state = lock(&self.workspaces);
        state.authoring_revision(owner, revision)?;
        let native = &mut state.authoring.as_mut().ok_or_else(stale)?.native;
        if native.generation != generation || !native.picker {
            return Err(Fault::new(
                "StaleNativePicker",
                "No matching native picker reservation exists",
            ));
        }
        native.picker = false;
        self.collect_native(&mut state);
        Ok(())
    }

    pub fn native_capture(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        generation: u64,
        capture_id: Option<&str>,
        document_revision: u64,
        cache: Option<Result<crate::capture_cache::CaptureCache, Fault>>,
    ) -> Result<NativeCaptureResult, Fault> {
        let (_command, mut state) = self.command_state()?;
        let package = state.authoring_revision(owner, revision)?;
        self.collect(&mut state);
        state.work_idle_except_native()?;
        {
            let native = &state.authoring.as_ref().ok_or_else(stale)?.native;
            native.check(generation)?;
            if native.picker || native.selected.is_none() {
                return Err(stale());
            }
        }
        let prepared =
            self.prepare_native_frame(&mut state, owner, revision, capture_id, document_revision)?;
        let native = &mut state.authoring.as_mut().ok_or_else(stale)?.native;
        let mut correlation = native.correlation.clone().ok_or_else(stale)?;
        correlation.request_id = format!("{}-capture-{generation}-{prepared}", owner.token);
        let authority = Arc::new(CaptureAuthority::new(correlation.clone())?);
        let source = native.source.clone().ok_or_else(stale)?;
        let cancel = native.cancelled.clone();
        let worker = native.worker.take().ok_or_else(stale)?;
        native.active = true;
        native.status = "capturing";
        lock(&self.authoring_stop)
            .as_mut()
            .and_then(|slot| slot.native.as_mut())
            .ok_or_else(stale)?
            .authority = Some(authority.clone());
        drop(state);
        let deadline = Instant::now() + Duration::from_secs(10);
        let validation = self
            .revalidate_native_source(&source, &cancel, deadline)
            .and_then(|()| {
                let current = self.publisher.open(package.root())?;
                if current.revision() != revision {
                    return Err(stale());
                }
                Ok(())
            });
        let (capture, primary, settlement) = match validation {
            Ok(()) => {
                let result = worker.capture(
                    correlation.clone(),
                    authority.clone(),
                    deadline.saturating_duration_since(Instant::now()),
                );
                (result.capture, result.primary, result.settlement)
            }
            Err(error) => {
                authority.cancel();
                let settlement = worker.settle();
                (None, Some(error), settlement)
            }
        };
        state = lock(&self.workspaces);
        let lease = state.authoring.as_mut().ok_or_else(stale)?;
        if let Some(error) = cleanup(&settlement) {
            lease.native.active = false;
            if settlement.child_reaped {
                lease.native.retire("failed");
            }
            lease.native.status = "failed";
            lease.containment = Some(error.clone());
            lease.native.error = Some(error.clone());
            return Err(public_fault(primary.unwrap_or(error)));
        }
        let result = (|| {
            state.authoring_revision(owner, revision)?;
            publication_checkpoint(&cancel, deadline)?;
            if let Some(error) = primary {
                return Err(error);
            }
            let capture = capture
                .ok_or_else(|| Fault::new("NativeCapture", "Worker returned no accepted frame"))?;
            authority.check(&capture.identity)?;
            self.publish_native_frame(
                &mut state,
                Publication {
                    owner,
                    package: &package,
                    source: &source,
                    correlation: &correlation,
                    authority: &authority,
                    cancelled: &cancel,
                    deadline,
                },
                |state| {
                    let prepared = self.prepare_native_install(
                        state, owner, revision, capture_id, prepared, capture,
                    )?;
                    // A detached frame is historical: validate only the retained installation,
                    // not the process/window that may already have exited after capture commit.
                    self.revalidate_detached_installation(&source, &cancel, deadline)?;
                    Ok(prepared)
                },
            )
        })();
        let lease = state.authoring.as_mut().ok_or_else(stale)?;
        lease
            .native
            .retire(if result.is_ok() { "consumed" } else { "failed" });
        lease.native.error = result.as_ref().err().cloned().map(public_fault);
        drop(source);
        if let Some(slot) = lock(&self.authoring_stop).as_mut() {
            slot.native = None;
        }
        let png = result.map_err(public_fault)?;
        let recognition = self.recognition_snapshot(&mut state, owner, revision)?;
        let selection = state
            .authoring
            .as_ref()
            .ok_or_else(stale)?
            .native
            .view(owner, revision);
        drop(state);
        let cache = cache.map(|cache| match cache {
            Ok(cache) => cache.persist(
                package.package_id(),
                recognition
                    .capture_id
                    .as_deref()
                    .expect("accepted capture namespace"),
                png.as_slice(),
            ),
            Err(error) => crate::capture_cache::CacheWrite {
                cached: false,
                error: Some(error),
            },
        });
        Ok(NativeCaptureResult {
            recognition,
            selection,
            cache,
        })
    }
}

#[cfg(test)]
#[path = "native_capture/tests.rs"]
mod publication_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> Candidate {
        Candidate {
            key: "candidate-1".into(),
            label: "/private/selected/window".into(),
            process_id: 42,
            process_lifetime: 777,
            executable_path: PathBuf::from("/private/game.exe"),
            application_bundle_path: None,
            native_window: NativeWindow::Windows(73),
            area: CaptureArea::WindowsClient,
            geometry: mado_runtime_comparison::authoring_capture::CaptureGeometry {
                x: -900.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
                pixels_per_unit_x: 1.0,
                pixels_per_unit_y: 1.0,
                pixel_width: 100,
                pixel_height: 50,
                unit: CoordinateUnit::WindowsPhysicalPixels,
            },
        }
    }

    #[test]
    fn native_candidate_renderer_projection_excludes_private_provenance() {
        let native = NativeState {
            candidates: vec![candidate()],
            selected: Some("candidate-1".into()),
            ..NativeState::default()
        };
        let owner = AuthoringRef {
            workspace: crate::application::WorkspaceRef {
                workspace_id: "workspace".into(),
                revision: 1,
            },
            token: "owner".into(),
        };
        let value = serde_json::to_value(native.view(&owner, "revision")).unwrap();
        assert_eq!(
            value["candidates"],
            serde_json::json!([{
                "id":"candidate-1", "application_label":"", "window_label":"Window",
            }])
        );
        let serialized = serde_json::to_string(&value).unwrap();
        for private in [
            "process_id",
            "process_lifetime",
            "native_window",
            "geometry",
            "/private/",
        ] {
            assert!(!serialized.contains(private), "{private}");
        }
    }

    #[test]
    fn native_cancel_does_not_release_reservation_or_wait_on_command_and_store() {
        let sources = super::super::tests::Sources::new();
        let app = sources.app();
        let editor = app
            .authoring_create(
                &crate::application::test_support::view_ref(
                    &app.create_workspace("native-slot", "Native slot").unwrap(),
                ),
                "native-slot",
            )
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = lock(&app.workspaces);
            state.authoring.as_mut().unwrap().native.active = true;
            lock(&app.authoring_stop).as_mut().unwrap().native = Some(NativeControl {
                cancelled: cancelled.clone(),
                worker: None,
                authority: None,
            });
        }
        assert!(
            matches!(app.recognition_capabilities(&editor.owner, &editor.revision),
            Err(error) if error.category == "NativeCaptureBusy")
        );
        let command = lock(&app.commands);
        let store = lock(&app.store);
        let state = lock(&app.workspaces);
        let worker = app.clone();
        let owner = editor.owner.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let _ = send.send(worker.authoring_stop(&owner));
        });
        let result = receive.recv_timeout(Duration::from_secs(1));
        drop(state);
        drop(store);
        drop(command);
        thread.join().unwrap();
        assert!(result.unwrap().unwrap());
        assert!(cancelled.load(Ordering::Acquire));
        let mut state = lock(&app.workspaces);
        assert_eq!(state.work_idle().unwrap_err().category, "NativeCaptureBusy");
        let native = &mut state.authoring.as_mut().unwrap().native;
        native.active = false;
        native.picker = true;
        native.retire("cancelled");
        assert_eq!(state.work_idle().unwrap_err().category, "NativeCaptureBusy");
        state.authoring.as_mut().unwrap().native.picker = false;
        state.work_idle().unwrap();
        lock(&app.authoring_stop).as_mut().unwrap().native = None;
    }

    #[test]
    fn native_cleanup_never_qualifies_forced_or_unreaped_pixels() {
        for settlement in [
            WorkerSettlement {
                child_reaped: false,
                clean: false,
                forced: false,
                primary: None,
            },
            WorkerSettlement {
                child_reaped: true,
                clean: false,
                forced: true,
                primary: None,
            },
            WorkerSettlement {
                child_reaped: true,
                clean: true,
                forced: true,
                primary: None,
            },
        ] {
            assert_eq!(
                cleanup(&settlement).unwrap().category,
                "NativeCaptureCleanup"
            );
        }
        let cancelled = WorkerSettlement {
            child_reaped: true,
            clean: true,
            forced: false,
            primary: Some(super::cancelled()),
        };
        assert!(
            cleanup(&cancelled).is_none(),
            "clean cancellation can release the slot without accepting an image"
        );
    }
}
