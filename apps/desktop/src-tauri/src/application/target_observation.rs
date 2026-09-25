use super::targets::target_context;
use super::{Application, Selected, TargetContext, WorkspaceRef, lock};
use crate::target::{self, ApplicationObservation, TargetExpectation};
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize)]
pub struct TargetApplicationResponse {
    pub context: TargetContext,
    pub revision: u64,
    pub binding_id: Option<String>,
    pub request_id: String,
    pub observation: ApplicationObservation,
}

#[derive(Default)]
pub(crate) struct ObservationSlot {
    active: Option<ObservationWork>,
}

impl ObservationSlot {
    fn available(&self) -> Result<(), Fault> {
        if self
            .active
            .as_ref()
            .is_some_and(|work| !work.finished.load(Ordering::Acquire))
        {
            Err(Fault::new(
                "TargetObservationBusy",
                "The previous application check is still returning from the OS",
            ))
        } else {
            Ok(())
        }
    }

    fn reserved(
        &mut self,
        workspace: &WorkspaceRef,
        request_id: &str,
    ) -> Result<&mut ObservationWork, Fault> {
        self.available()?;
        self.active
            .as_mut()
            .filter(|work| {
                work.workspace == *workspace
                    && work.request_id == request_id
                    && work.result.is_none()
                    && !work.cancelled.load(Ordering::Acquire)
            })
            .ok_or_else(cancelled)
    }
}

struct ObservationWork {
    workspace: WorkspaceRef,
    request_id: String,
    cancelled: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    result: Option<mpsc::SyncSender<Result<ApplicationObservation, Fault>>>,
}

impl ObservationWork {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        // A queued success is also rejected by the cancellation flag at publication.
        if let Some(result) = &self.result {
            let _ = result.try_send(Err(cancelled()));
        }
    }
}

struct WorkerFinished(Arc<AtomicBool>);

impl Drop for WorkerFinished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct PendingObservation {
    selected: Selected,
    expected: TargetExpectation,
    request_id: String,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    result: mpsc::Receiver<Result<ApplicationObservation, Fault>>,
}

impl Application {
    pub fn reserve_running_application(
        &self,
        workspace: &WorkspaceRef,
        request_id: &str,
    ) -> Result<(), Fault> {
        let result = (|| {
            if request_id.is_empty()
                || request_id.len() > 64
                || !request_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(Fault::new(
                    "TargetObservationRequest",
                    "Invalid application check request",
                ));
            }
            let (_command, mut state) = self.command_state()?;
            state.resolve(workspace)?;
            self.collect(&mut state);
            state.idle()?;
            if !cfg!(target_os = "macos") {
                return Err(Fault::new(
                    "TargetPlatform",
                    "Running application checks require macOS",
                ));
            }
            let mut slot = lock(&self.target_observation);
            slot.available()?;
            if let Some(previous) = slot.active.take() {
                previous.cancel();
            }
            slot.active = Some(ObservationWork {
                workspace: workspace.clone(),
                request_id: request_id.to_owned(),
                cancelled: Arc::new(AtomicBool::new(false)),
                finished: Arc::new(AtomicBool::new(true)),
                result: None,
            });
            Ok(())
        })();
        if let Err(error) = &result {
            self.record_application_check(workspace, "admission", Err(error));
        }
        result
    }

    pub fn check_running_application(
        &self,
        workspace: &WorkspaceRef,
        expected: &TargetExpectation,
        request_id: &str,
    ) -> Result<TargetApplicationResponse, Fault> {
        let pending = self
            .begin_running_application(workspace, expected, request_id)
            .inspect_err(|error| {
                self.record_application_check(workspace, "admission", Err(error));
            })?;
        self.finish_running_application(pending)
    }

    fn begin_running_application(
        &self,
        workspace: &WorkspaceRef,
        expected: &TargetExpectation,
        request_id: &str,
    ) -> Result<PendingObservation, Fault> {
        let (_command, mut state) = self.command_state()?;
        let selected = state.resolve(workspace)?.clone();
        self.collect(&mut state);
        state.idle()?;
        lock(&self.target_observation).reserved(workspace, request_id)?;
        let declaration =
            selected.package.target.clone().ok_or_else(|| {
                Fault::new("TargetUndeclared", "Package has no target declaration")
            })?;
        let record = lock(&self.store)
            .read_target(&selected.internal_name, &selected.inventory.package_id)?;
        record.compare(expected)?;
        let binding = record
            .binding
            .ok_or_else(|| Fault::new("TargetUnbound", "Save a target binding first"))?;
        if !binding.compatible(
            &selected.inventory.package_id,
            &declaration.id,
            &declaration.identity()?,
        ) {
            return Err(Fault::new(
                "TargetIncompatible",
                "Saved target declaration changed",
            ));
        }
        if binding.configuration.game.kind != "bundle" {
            return Err(Fault::new(
                "TargetObservationUnsupported",
                "Running application checks require a saved application bundle",
            ));
        }
        if !cfg!(target_os = "macos") {
            return Err(Fault::new(
                "TargetPlatform",
                "Running application checks require macOS",
            ));
        }
        let mut slot = lock(&self.target_observation);
        let work = slot.reserved(workspace, request_id)?;
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        let cancelled = work.cancelled.clone();
        let (send, receive) = mpsc::sync_channel(1);
        work.result = Some(send.clone());
        work.finished.store(false, Ordering::Release);
        // A failed spawn drops the captured lease too; it cannot strand occupancy.
        let finished = WorkerFinished(work.finished.clone());
        let worker_cancelled = cancelled.clone();
        std::thread::Builder::new()
            .name("target-application-check".into())
            .spawn(move || {
                let _finished = finished;
                let result = target::observe_application(
                    &binding.configuration,
                    &declaration,
                    &binding.resolution,
                    &worker_cancelled,
                    deadline,
                );
                let _ = send.try_send(result);
            })
            .map_err(|_| {
                Fault::new(
                    "TargetObservationWorker",
                    "Application check worker could not start",
                )
            })?;
        Ok(PendingObservation {
            selected,
            expected: expected.clone(),
            request_id: request_id.to_owned(),
            cancelled,
            deadline,
            result: receive,
        })
    }

    fn finish_running_application(
        &self,
        pending: PendingObservation,
    ) -> Result<TargetApplicationResponse, Fault> {
        let workspace = &pending.selected.workspace;
        let mut stage = "observation";
        let result = (|| {
            let remaining = pending.deadline.saturating_duration_since(Instant::now());
            let result = match pending.result.recv_timeout(remaining) {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    pending.cancelled.store(true, Ordering::Release);
                    return Err(Fault::new(
                        "TargetObservationTimeout",
                        "Application check timed out; its OS read may still be returning",
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Fault::new(
                        "TargetObservationWorker",
                        "Application check worker stopped without a result",
                    ));
                }
            };
            if pending.cancelled.load(Ordering::Acquire) || self.closing.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            if Instant::now() >= pending.deadline {
                pending.cancelled.store(true, Ordering::Release);
                return Err(Fault::new(
                    "TargetObservationTimeout",
                    "Application check exceeded its publication deadline",
                ));
            }
            let observation = result?;
            stage = "publication";
            // Publication holds ownership only for the final record comparison, never for OS reads.
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(&pending.selected.workspace)?;
            if selected.package.target_identity != pending.selected.package.target_identity {
                return Err(cancelled());
            }
            let store = match self.store.try_lock() {
                Ok(store) => store,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(Fault::new(
                        "TargetObservationBusy",
                        "Configuration is still in use; the observation was not published",
                    ));
                }
            };
            let record =
                store.read_target(&selected.internal_name, &selected.inventory.package_id)?;
            record.compare(&pending.expected)?;
            if pending.cancelled.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            if Instant::now() >= pending.deadline {
                return Err(Fault::new(
                    "TargetObservationTimeout",
                    "Application check exceeded its publication deadline",
                ));
            }
            Ok(TargetApplicationResponse {
                context: target_context(selected),
                revision: pending.expected.revision,
                binding_id: pending.expected.binding_id,
                request_id: pending.request_id,
                observation,
            })
        })();
        self.record_application_check(
            workspace,
            stage,
            result.as_ref().map(|response| &response.observation),
        );
        result
    }

    fn record_application_check(
        &self,
        workspace: &WorkspaceRef,
        stage: &'static str,
        result: Result<&ApplicationObservation, &Fault>,
    ) {
        let (status, error) = match result {
            Ok(observation) => (
                match observation.status.as_str() {
                    status @ ("matched" | "not_running" | "ambiguous" | "unverifiable") => status,
                    _ => "unknown",
                },
                None,
            ),
            Err(error) => (
                match error.category.as_str() {
                    "TargetObservationCancelled" => "cancelled",
                    "TargetObservationTimeout" => "timeout",
                    _ => "refused",
                },
                Some(error),
            ),
        };
        self.logger.emit(
            "Rust",
            if error.is_some() { "error" } else { "info" },
            None,
            Some(&workspace.workspace_id),
            if error.is_some() {
                "command.failed"
            } else {
                "target.application.checked"
            },
            "Application check finished",
            serde_json::json!({
                "action": "check_running_application", "status": status, "stage": stage,
                "category": error.map(|fault| fault.category.as_str()),
            }),
        );
    }

    pub fn cancel_running_application(&self, workspace: &WorkspaceRef, request_id: &str) -> bool {
        let slot = lock(&self.target_observation);
        let Some(work) = &slot.active else {
            return false;
        };
        if work.workspace != *workspace || work.request_id != request_id {
            return false;
        }
        work.cancel();
        true
    }

    pub(super) fn invalidate_target_observation(&self, workspace: Option<&WorkspaceRef>) {
        if let Some(work) = &lock(&self.target_observation).active {
            if workspace.is_none_or(|workspace| *workspace == work.workspace) {
                work.cancel();
            }
        }
    }

    pub fn begin_target_picker(
        self: &Arc<Self>,
        workspace: &WorkspaceRef,
    ) -> Result<TargetPickerGuard, Fault> {
        let (_command, mut state) = self.command_state()?;
        state.resolve(workspace)?;
        self.collect(&mut state);
        state.idle()?;
        if !cfg!(target_os = "macos") {
            return Err(Fault::new(
                "TargetPlatform",
                "Application selection requires macOS",
            ));
        }
        state.target_picker = Some(workspace.clone());
        Ok(TargetPickerGuard {
            application: self.clone(),
            workspace: workspace.clone(),
        })
    }
}

pub struct TargetPickerGuard {
    application: Arc<Application>,
    workspace: WorkspaceRef,
}

impl TargetPickerGuard {
    pub fn validate(&self) -> Result<(), Fault> {
        let (_command, state) = self.application.command_state()?;
        state.resolve(&self.workspace)?;
        Ok(())
    }
}

impl Drop for TargetPickerGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.application.workspaces);
        if state.target_picker.as_ref() == Some(&self.workspace) {
            state.target_picker = None;
        }
    }
}

fn cancelled() -> Fault {
    Fault::new(
        "TargetObservationCancelled",
        "Application check cancelled; an OS read may still be returning",
    )
}

#[cfg(test)]
mod tests;
