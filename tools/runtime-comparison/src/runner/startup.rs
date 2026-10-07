use crate::desktop::{LaunchDisposition, NativePhase, NativeProgress, NativeTargetStatus};
use crate::model::{Control, Fault};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

static IDENTITY: OnceLock<(String, u64)> = OnceLock::new();

pub(super) fn identify(run: &str, attempt: u64) {
    let _ = IDENTITY.set((run.to_owned(), attempt));
}

pub(crate) fn emit_native_transition(phase: u8, deadline: Instant) -> Result<(), Fault> {
    if let Some((run, attempt)) = IDENTITY.get() {
        super::protocol::emit(&serde_json::json!({
            "event":"NativeTransition","run":run,"attempt":attempt,"phase":phase,
            "deadline":super::clock::SharedDeadline::from_instant(deadline)?,
        }))?;
    }
    Ok(())
}

fn initial_progress(attempt: u64) -> NativeProgress {
    NativeProgress {
        attempt,
        status: NativeTargetStatus::NotRequested,
        phase: NativePhase::Readiness,
        launch: LaunchDisposition::NotRequested,
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartupReply {
    pub progress: NativeProgress,
    pub configuration: Option<Value>,
    pub fault: Option<Fault>,
}

type Probe =
    Box<dyn FnMut(&Control, &dyn Fn(NativeProgress)) -> Result<Option<Value>, Fault> + Send>;
type ProbeResult = (Probe, Result<Option<Value>, Fault>);
struct PreparationState {
    probe: Option<Probe>,
    worker: Option<JoinHandle<ProbeResult>>,
    bound: bool,
    failure: Option<Fault>,
}

/// A probe owns the captured application binding until it physically returns.
/// Neither timeout, child exit nor a Script exception detaches this worker.
pub(crate) struct NativePreparation {
    control: Arc<Control>,
    progress: Arc<Mutex<NativeProgress>>,
    state: Mutex<PreparationState>,
}

impl NativePreparation {
    pub(crate) fn new(
        control: Arc<Control>,
        attempt: u64,
        probe: impl FnMut(&Control, &dyn Fn(NativeProgress)) -> Result<Option<Value>, Fault>
        + Send
        + 'static,
    ) -> Self {
        Self {
            control,
            progress: Arc::new(Mutex::new(initial_progress(attempt))),
            state: Mutex::new(PreparationState {
                probe: Some(Box::new(probe)),
                worker: None,
                bound: false,
                failure: None,
            }),
        }
    }

    pub(crate) fn progress(&self) -> NativeProgress {
        *self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn request(&self) -> Result<(), Fault> {
        self.control.check()?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.worker.is_some() || state.bound {
            return Err(Fault::new(
                "Transport",
                "startup probe already active or target already bound",
            ));
        }
        let mut probe = state
            .probe
            .take()
            .ok_or_else(|| Fault::new("Transport", "startup resolver unavailable"))?;
        let control = Arc::clone(&self.control);
        let progress = Arc::clone(&self.progress);
        let attempt = self.progress().attempt;
        {
            let mut progress = progress.lock().unwrap_or_else(|e| e.into_inner());
            if progress.status == NativeTargetStatus::NotRequested {
                progress.status = NativeTargetStatus::Pending;
                progress.phase = NativePhase::TargetDiscovery;
            }
        }
        state.worker = Some(
            thread::Builder::new()
                .name("native-target-probe".into())
                .spawn(move || {
                    let report = |mut value: NativeProgress| {
                        value.attempt = attempt;
                        *progress.lock().unwrap_or_else(|e| e.into_inner()) = value;
                    };
                    let result = control
                        .check()
                        .and_then(|()| probe(&control, &report))
                        .and_then(|target| control.check().map(|()| target));
                    (probe, result)
                })
                .map_err(|error| Fault::new("Startup", error.to_string()))?,
        );
        Ok(())
    }

    pub(crate) fn poll(&self) -> Option<StartupReply> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return None;
        }
        let result = state.worker.take().expect("finished worker").join();
        let outcome = match result {
            Ok((probe, outcome)) => {
                state.probe = Some(probe);
                outcome
            }
            Err(_) => Err(Fault::new("Controller", "native target probe panicked")
                .with_context(serde_json::json!({"native_cleanup":"unverified"}))),
        };
        match outcome {
            Ok(configuration) => {
                state.bound = configuration.is_some();
                Some(StartupReply {
                    progress: self.progress(),
                    configuration,
                    fault: None,
                })
            }
            Err(fault) => {
                state.bound = true;
                state.failure = Some(fault.clone());
                Some(StartupReply {
                    progress: self.progress(),
                    configuration: None,
                    fault: Some(fault),
                })
            }
        }
    }

    pub(crate) fn settle(&self) -> Option<Fault> {
        let worker = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker
            .take();
        let failure = worker.and_then(|worker| {
            self.control.cancel();
            match worker.join() {
                Ok((_, result)) => result.err(),
                Err(_) => Some(
                    Fault::new("Controller", "native target probe panicked")
                        .with_context(serde_json::json!({"native_cleanup":"unverified"})),
                ),
            }
        });
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(failure) = failure {
            state.failure.get_or_insert(failure);
        }
        state.failure.clone()
    }
}

impl Drop for NativePreparation {
    fn drop(&mut self) {
        let _ = self.settle();
    }
}

struct LinkState {
    sequence: u64,
    pending: bool,
    reply: Option<StartupReply>,
}

/// The input reader only deposits a bounded reply; it never enters the VM or SDK.
pub(crate) struct StartupLink {
    state: Mutex<LinkState>,
    requests: mpsc::SyncSender<u64>,
}

impl StartupLink {
    pub(crate) fn new() -> (Arc<Self>, mpsc::Receiver<u64>) {
        let (requests, receiver) = mpsc::sync_channel(1);
        (
            Arc::new(Self {
                state: Mutex::new(LinkState {
                    sequence: 0,
                    pending: false,
                    reply: None,
                }),
                requests,
            }),
            receiver,
        )
    }

    pub(crate) fn request(&self) -> Result<(), Fault> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.pending || state.reply.is_some() {
            return Err(Fault::new(
                "Transport",
                "startup reply has not been consumed",
            ));
        }
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| Fault::new("LimitExceeded", "startup sequence exhausted"))?;
        self.requests
            .try_send(state.sequence)
            .map_err(|_| Fault::new("Transport", "startup request channel unavailable"))?;
        state.pending = true;
        Ok(())
    }

    pub(crate) fn receive(&self, sequence: u64, reply: StartupReply) -> Result<(), Fault> {
        if reply.progress.status != NativeTargetStatus::Pending
            || (reply.configuration.is_some() && reply.fault.is_some())
            || (reply.fault.is_none()
                && matches!(
                    reply.progress.launch,
                    LaunchDisposition::Rejected | LaunchDisposition::Uncertain
                ))
        {
            return Err(Fault::new(
                "Transport",
                "invalid preparation outcome; capture readiness is child-owned",
            ));
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.pending || state.sequence != sequence || state.reply.is_some() {
            return Err(Fault::new(
                "StaleIdentity",
                "unsolicited or stale startup reply",
            ));
        }
        state.pending = false;
        state.reply = Some(reply);
        Ok(())
    }

    pub(crate) fn failure(&self) -> Option<Fault> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reply
            .as_ref()
            .and_then(|reply| reply.fault.clone())
    }

    pub(crate) fn progress(&self) -> Option<NativeProgress> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reply
            .as_ref()
            .map(|reply| reply.progress)
    }

    pub(crate) fn take(&self) -> Option<StartupReply> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reply
            .take()
    }
}

#[cfg(test)]
mod tests;
