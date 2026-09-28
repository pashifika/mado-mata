use super::protocol::{self, Command, Received};
use super::*;
use crate::owned_child::{ChildStdio, Environment, OwnedChild};
use std::path::Path;
use std::sync::{Arc, Mutex, atomic::AtomicBool, mpsc};
use std::thread;
use std::time::Instant;

#[derive(Clone)]
pub struct WorkerSettlement {
    pub child_reaped: bool,
    pub clean: bool,
    pub forced: bool,
    pub primary: Option<Fault>,
}

pub struct WorkerCaptureResult {
    pub capture: Option<DetachedCapture>,
    pub primary: Option<Fault>,
    pub session_clean: bool,
    pub settlement: Option<WorkerSettlement>,
}

struct Shared {
    cancelled: AtomicBool,
    finished: AtomicBool,
    settlement: Mutex<Option<WorkerSettlement>>,
}

/// Cancellation never takes a command/store/result lock or writes a pipe.
#[derive(Clone)]
pub struct WorkerCancel(Arc<Shared>);
impl WorkerCancel {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
    pub fn is_finished(&self) -> bool {
        self.0.finished.load(Ordering::Acquire)
    }
    pub fn try_settlement(&self) -> Option<WorkerSettlement> {
        self.0
            .settlement
            .try_lock()
            .ok()
            .and_then(|value| value.clone())
    }
}

enum Request {
    Select {
        identity: CaptureIdentity,
        candidate: Candidate,
    },
    RebasePackage(CaptureIdentity),
    Capture {
        command: Command,
        authority: Arc<CaptureAuthority>,
        reservation: PayloadReservation,
        deadline: Instant,
    },
}
enum Reply {
    Discovered(Vec<Candidate>),
    Selected(Candidate),
    PackageRebased(CaptureIdentity),
    Captured(WorkerCaptureResult),
    Finished(WorkerCaptureResult),
}

/// One retained provider/target namespace, with a separately closed session per frame.
/// The background owner holds failed-operation reservations until physical reap.
pub struct AuthoringCaptureWorker {
    identity: CaptureIdentity,
    candidates: Vec<Candidate>,
    selected: Option<Candidate>,
    request_ids: std::collections::BTreeSet<String>,
    requests: mpsc::SyncSender<Request>,
    replies: mpsc::Receiver<Reply>,
    cancel: WorkerCancel,
}

impl AuthoringCaptureWorker {
    pub fn spawn(executable: &Path, request: DiscoveryRequest) -> Result<Self, Fault> {
        request.validate()?;
        if !executable.is_absolute() || !executable.is_file() {
            return Err(Fault::new(
                "EngineUnavailable",
                "The fixed authoring engine executable is unavailable",
            ));
        }
        let started = Instant::now();
        let mut command = std::process::Command::new(executable);
        command.arg("authoring-capture-child");
        // No model configuration or OCR loader is involved. Library stderr is private.
        let child = OwnedChild::spawn(&mut command, ChildStdio::Protocol, Environment::Inherited)
            .map_err(|_| {
            Fault::new(
                "EngineUnavailable",
                "The owned authoring engine could not start",
            )
        })?;
        Self::from_child(child, request, started)
    }

    fn from_child(
        child: OwnedChild,
        request: DiscoveryRequest,
        started: Instant,
    ) -> Result<Self, Fault> {
        let identity = request.identity.clone();
        let (requests, receive) = mpsc::sync_channel(1);
        let (send, replies) = mpsc::sync_channel(2);
        let cancel = WorkerCancel(Arc::new(Shared {
            cancelled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            settlement: Mutex::new(None),
        }));
        let shared = cancel.clone();
        thread::Builder::new()
            .name("authoring-capture-owner".into())
            .spawn(move || supervise(child, request, receive, send, shared, started))
            .map_err(|_| {
                Fault::new(
                    "ChildStartup",
                    "Authoring worker supervision could not start",
                )
            })?;
        Ok(Self {
            identity,
            candidates: Vec::new(),
            selected: None,
            request_ids: std::collections::BTreeSet::new(),
            requests,
            replies,
            cancel,
        })
    }

    pub fn cancel_handle(&self) -> WorkerCancel {
        self.cancel.clone()
    }

    pub fn discover(&mut self) -> Result<Vec<Candidate>, Fault> {
        if !self.candidates.is_empty() {
            if self.cancel.0.cancelled.load(Ordering::Acquire) {
                return Err(protocol::cancelled());
            }
            if self.cancel.is_finished() {
                return Err(protocol::expired());
            }
            return Ok(self.candidates.clone());
        }
        match self
            .replies
            .recv()
            .map_err(|_| protocol::protocol_fault())?
        {
            Reply::Discovered(candidates) => {
                self.candidates = candidates;
                Ok(self.candidates.clone())
            }
            Reply::Finished(result) => Err(result
                .primary
                .or_else(|| result.settlement.and_then(|settlement| settlement.primary))
                .unwrap_or_else(protocol::protocol_fault)),
            Reply::Selected(_) | Reply::PackageRebased(_) | Reply::Captured(_) => {
                Err(protocol::protocol_fault())
            }
        }
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn select(&mut self, key: &str, binding_revision: &str) -> Result<Candidate, Fault> {
        if self.cancel.0.cancelled.load(Ordering::Acquire) {
            return Err(protocol::cancelled());
        }
        if self.cancel.is_finished() {
            return Err(protocol::expired());
        }
        let candidate = self
            .candidates
            .iter()
            .find(|candidate| candidate.key == key)
            .ok_or_else(stale)?
            .clone();
        let mut identity = self.identity.clone();
        identity.binding_revision = binding_revision.into();
        identity.validate()?;
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected != &candidate || self.identity != identity)
        {
            return Err(stale());
        }
        self.requests
            .try_send(Request::Select {
                identity: identity.clone(),
                candidate: candidate.clone(),
            })
            .map_err(|_| protocol::protocol_fault())?;
        match self
            .replies
            .recv()
            .map_err(|_| protocol::protocol_fault())?
        {
            Reply::Selected(current) if current == candidate => {
                self.identity = identity;
                self.selected = Some(current.clone());
                Ok(current)
            }
            Reply::Finished(result) => Err(result
                .primary
                .or_else(|| result.settlement.and_then(|settlement| settlement.primary))
                .unwrap_or_else(protocol::protocol_fault)),
            _ => Err(stale()),
        }
    }

    /// The host must first prove that the saved package still names the same
    /// application binding. This changes correlation, never native target authority.
    pub fn rebase_package(&mut self, package_revision: &str) -> Result<(), Fault> {
        if self.cancel.0.cancelled.load(Ordering::Acquire) {
            return Err(protocol::cancelled());
        }
        if self.cancel.is_finished() {
            return Err(protocol::expired());
        }
        self.selected.as_ref().ok_or_else(stale)?;
        let mut identity = self.identity.clone();
        identity.package_revision = package_revision.into();
        identity.validate()?;
        if identity == self.identity {
            return Ok(());
        }
        self.requests
            .try_send(Request::RebasePackage(identity.clone()))
            .map_err(|_| protocol::protocol_fault())?;
        match self
            .replies
            .recv()
            .map_err(|_| protocol::protocol_fault())?
        {
            Reply::PackageRebased(current) if current == identity => {
                self.identity = current;
                Ok(())
            }
            Reply::Finished(result) => Err(result
                .primary
                .or_else(|| result.settlement.and_then(|settlement| settlement.primary))
                .unwrap_or_else(protocol::protocol_fault)),
            _ => {
                self.cancel.cancel();
                Err(stale())
            }
        }
    }

    pub fn capture(
        &mut self,
        identity: CaptureIdentity,
        authority: Arc<CaptureAuthority>,
        timeout: Duration,
    ) -> WorkerCaptureResult {
        let deadline = Instant::now() + timeout.min(ACQUISITION_LIMIT);
        let prepared = (|| {
            identity.validate()?;
            authority.check(&identity)?;
            if !protocol::same_selection(&self.identity, &identity) {
                return Err(stale());
            }
            protocol::admit_request(&mut self.request_ids, &identity.request_id)?;
            if timeout.is_zero() || timeout > ACQUISITION_LIMIT {
                return Err(protocol::protocol_fault());
            }
            let candidate = self.selected.clone().ok_or_else(stale)?;
            let reservation = reserve_worker_payload(&candidate.geometry, NATIVE_STORAGE_BYTES)?;
            Ok(Request::Capture {
                command: Command::Capture {
                    identity,
                    candidate,
                    timeout,
                },
                authority: authority.clone(),
                reservation,
                deadline,
            })
        })();
        if let Err(error) = prepared.and_then(|request| {
            self.requests
                .try_send(request)
                .map_err(|_| protocol::protocol_fault())
        }) {
            authority.cancel();
            self.cancel.cancel();
            let mut result = self.wait_finished();
            result.primary = Some(error);
            result.capture = None;
            return result;
        }
        self.wait_capture()
    }

    pub fn settle(self) -> WorkerSettlement {
        self.cancel.cancel();
        self.wait_finished().settlement.unwrap_or(WorkerSettlement {
            child_reaped: false,
            clean: false,
            forced: false,
            primary: Some(protocol::protocol_fault()),
        })
    }

    fn wait_capture(&self) -> WorkerCaptureResult {
        match self.replies.recv() {
            Ok(Reply::Captured(result) | Reply::Finished(result)) => result,
            _ => {
                self.cancel.cancel();
                self.wait_finished()
            }
        }
    }

    fn wait_finished(&self) -> WorkerCaptureResult {
        loop {
            match self.replies.recv() {
                Ok(Reply::Finished(result)) => return result,
                Ok(
                    Reply::Discovered(_)
                    | Reply::Selected(_)
                    | Reply::PackageRebased(_)
                    | Reply::Captured(_),
                ) => {}
                Err(_) => {
                    let settlement = self.cancel.try_settlement().unwrap_or(WorkerSettlement {
                        child_reaped: false,
                        clean: false,
                        forced: false,
                        primary: Some(protocol::protocol_fault()),
                    });
                    return WorkerCaptureResult {
                        capture: None,
                        primary: settlement.primary.clone(),
                        session_clean: false,
                        settlement: Some(settlement),
                    };
                }
            }
        }
    }
}
impl Drop for AuthoringCaptureWorker {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn retain_settlement(cancel: &WorkerCancel, settlement: &WorkerSettlement) {
    if let Ok(mut value) = cancel.0.settlement.lock() {
        *value = Some(settlement.clone());
    }
    if settlement.child_reaped {
        cancel.0.finished.store(true, Ordering::Release);
    }
}

fn supervise(
    mut child: OwnedChild,
    request: DiscoveryRequest,
    requests: mpsc::Receiver<Request>,
    replies: mpsc::SyncSender<Reply>,
    cancel: WorkerCancel,
    started: Instant,
) {
    let result = supervise_inner(&mut child, &request, requests, &replies, &cancel, started);
    match result {
        Ok(result) => {
            if let Some(settlement) = &result.settlement {
                retain_settlement(&cancel, settlement);
            }
            let _ = replies.send(Reply::Finished(result));
        }
        Err(error) => {
            // Startup failures still own the child. Never mark it settled merely
            // because a transport thread could not be installed.
            let _ = child.kill();
            let reaped = child.wait().is_ok();
            let settlement = WorkerSettlement {
                child_reaped: reaped,
                clean: false,
                forced: true,
                primary: Some(error.clone()),
            };
            retain_settlement(&cancel, &settlement);
            let _ = replies.send(Reply::Finished(WorkerCaptureResult {
                capture: None,
                primary: Some(error),
                session_clean: false,
                settlement: Some(settlement),
            }));
        }
    }
}

struct ActiveCapture {
    identity: CaptureIdentity,
    geometry: CaptureGeometry,
    authority: Arc<CaptureAuthority>,
    reservation: PayloadReservation,
}

struct SupervisorState {
    deadline: Option<Instant>,
    discovered: bool,
    candidates: Vec<Candidate>,
    selecting: Option<(CaptureIdentity, Candidate)>,
    selected: Option<(CaptureIdentity, Candidate)>,
    rebasing: Option<CaptureIdentity>,
    active: Option<ActiveCapture>,
    frame_ids: std::collections::BTreeSet<String>,
    terminal: Option<(Option<Fault>, bool)>,
}

impl SupervisorState {
    fn accept(
        &mut self,
        event: Received,
        request: &DiscoveryRequest,
        replies: &mpsc::SyncSender<Reply>,
        cancel: &WorkerCancel,
        stopping: bool,
    ) -> Result<(), Fault> {
        if let Received::Terminal { primary, clean } = event {
            if self.terminal.is_some() {
                return Err(protocol::protocol_fault());
            }
            self.terminal = Some((primary, clean));
            self.deadline = Some(Instant::now() + CLEANUP_LIMIT);
            return Ok(());
        }
        if stopping || self.terminal.is_some() || cancel.0.cancelled.load(Ordering::Acquire) {
            return Err(protocol::cancelled());
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(operation_expired());
        }
        match event {
            Received::Discovered(identity, candidates)
                if !self.discovered && identity == request.identity =>
            {
                self.discovered = true;
                self.deadline = None;
                self.candidates = candidates.clone();
                replies
                    .try_send(Reply::Discovered(candidates))
                    .map_err(|_| protocol::cancelled())?;
            }
            Received::Selected(identity, candidate) => {
                if self
                    .selecting
                    .as_ref()
                    .is_none_or(|(bound, selected)| bound != &identity || selected != &candidate)
                {
                    return Err(stale());
                }
                self.selecting = None;
                self.selected = Some((identity, candidate.clone()));
                self.deadline = None;
                replies
                    .try_send(Reply::Selected(candidate))
                    .map_err(|_| protocol::cancelled())?;
            }
            Received::PackageRebased(identity) => {
                if self.rebasing.as_ref() != Some(&identity) {
                    return Err(stale());
                }
                let (bound, _) = self.selected.as_mut().ok_or_else(stale)?;
                if !protocol::same_package_rebase(bound, &identity) {
                    return Err(stale());
                }
                *bound = identity.clone();
                self.rebasing = None;
                self.deadline = None;
                replies
                    .try_send(Reply::PackageRebased(identity))
                    .map_err(|_| protocol::cancelled())?;
            }
            Received::Captured(pending) => {
                let active = self.active.as_ref().ok_or_else(protocol::protocol_fault)?;
                if self.frame_ids.len() >= protocol::CAPTURE_REQUEST_LIMIT {
                    return Err(protocol::capture_limit());
                }
                if pending.header.identity != active.identity
                    || pending.header.geometry != active.geometry
                    || !self.frame_ids.insert(pending.header.frame_identity.clone())
                {
                    return Err(stale());
                }
                active.authority.check(&active.identity)?;
                // The correlated event can only be emitted after SDK close succeeds.
                // Release the worker's per-frame charge before host decoding.
                let active = self.active.take().ok_or_else(protocol::protocol_fault)?;
                drop(active.reservation);
                let capture =
                    match pending.accept_after_session_close(&active.authority, &active.identity) {
                        Ok(capture) => capture,
                        Err(error) => {
                            active.authority.cancel();
                            return Err(error);
                        }
                    };
                if cancel.0.cancelled.load(Ordering::Acquire)
                    || self
                        .deadline
                        .is_none_or(|deadline| Instant::now() >= deadline)
                {
                    active.authority.cancel();
                    return Err(if cancel.0.cancelled.load(Ordering::Acquire) {
                        protocol::cancelled()
                    } else {
                        operation_expired()
                    });
                }
                self.deadline = None;
                replies
                    .try_send(Reply::Captured(WorkerCaptureResult {
                        capture: Some(capture),
                        primary: None,
                        session_clean: true,
                        settlement: None,
                    }))
                    .map_err(|_| protocol::cancelled())?;
            }
            _ => return Err(protocol::protocol_fault()),
        }
        Ok(())
    }
}

fn operation_expired() -> Fault {
    Fault::new(
        "DeadlineExceeded",
        "Authoring worker operation deadline expired",
    )
}

fn supervise_inner(
    child: &mut OwnedChild,
    request: &DiscoveryRequest,
    requests: mpsc::Receiver<Request>,
    replies: &mpsc::SyncSender<Reply>,
    cancel: &WorkerCancel,
    started: Instant,
) -> Result<WorkerCaptureResult, Fault> {
    let mut input = child.stdin.take().ok_or_else(protocol::protocol_fault)?;
    let mut output = child.stdout.take().ok_or_else(protocol::protocol_fault)?;
    let invocation = crate::model::encode_bounded(request, protocol::COMMAND_BYTES)?;
    let (write, commands) = mpsc::sync_channel::<Command>(2);
    let writer = thread::Builder::new()
        .name("authoring-capture-control".into())
        .spawn(move || {
            input
                .write_all(&(invocation.len() as u32).to_le_bytes())
                .and_then(|()| input.write_all(&invocation))
                .and_then(|()| input.flush())
                .map_err(|_| protocol::protocol_fault())?;
            while let Ok(command) = commands.recv() {
                protocol::write_message(&mut input, &command, protocol::COMMAND_BYTES)?;
                if matches!(command, Command::Cancel) {
                    break;
                }
            }
            Ok::<(), Fault>(())
        })
        .map_err(|_| protocol::protocol_fault())?;
    let (events_send, events) = mpsc::sync_channel(3);
    let reader = thread::Builder::new()
        .name("authoring-capture-pixels".into())
        .spawn(move || {
            if let Err(error) = protocol::read_events(&mut output, &events_send) {
                let _ = events_send.try_send(Err(error));
            }
        })
        .map_err(|_| protocol::protocol_fault())?;
    let mut state = SupervisorState {
        deadline: Some(started + request.timeout),
        discovered: false,
        candidates: Vec::new(),
        selecting: None,
        selected: None,
        rebasing: None,
        active: None,
        frame_ids: std::collections::BTreeSet::new(),
        terminal: None,
    };
    let mut primary = None;
    let mut stop = None;
    let mut forced = false;
    let mut reported_unreaped = false;
    let mut write = Some(write);
    let exit = loop {
        for event in events.try_iter() {
            if let Err(error) = event.and_then(|event| {
                state.accept(
                    event,
                    request,
                    replies,
                    cancel,
                    stop.is_some() || primary.is_some(),
                )
            }) {
                primary.get_or_insert(error);
            }
        }
        match requests.try_recv() {
            Ok(incoming) => {
                let command = (|| {
                    if !state.discovered
                        || state.active.is_some()
                        || state.selecting.is_some()
                        || state.rebasing.is_some()
                        || stop.is_some()
                        || primary.is_some()
                        || state.terminal.is_some()
                        || cancel.0.cancelled.load(Ordering::Acquire)
                    {
                        if let Request::Capture { authority, .. } = incoming {
                            authority.cancel();
                        }
                        return Err(protocol::protocol_fault());
                    }
                    match incoming {
                        Request::Select {
                            identity,
                            candidate,
                        } => {
                            let mut bound = state.selected.as_ref().map_or_else(
                                || request.identity.clone(),
                                |(bound, _)| bound.clone(),
                            );
                            bound.binding_revision = identity.binding_revision.clone();
                            if identity != bound
                                || !state.candidates.contains(&candidate)
                                || state.selected.as_ref().is_some_and(|(bound, selected)| {
                                    bound != &identity || selected != &candidate
                                })
                            {
                                return Err(stale());
                            }
                            let command = Command::Select {
                                key: candidate.key.clone(),
                                binding_revision: identity.binding_revision.clone(),
                            };
                            state.selecting = Some((identity, candidate));
                            state.deadline = Some(Instant::now() + DISCOVERY_LIMIT);
                            Ok(command)
                        }
                        Request::RebasePackage(identity) => {
                            identity.validate()?;
                            let (bound, _) = state.selected.as_ref().ok_or_else(stale)?;
                            if !protocol::same_package_rebase(bound, &identity) {
                                return Err(stale());
                            }
                            state.rebasing = Some(identity.clone());
                            state.deadline = Some(Instant::now() + DISCOVERY_LIMIT);
                            Ok(Command::RebasePackage { identity })
                        }
                        Request::Capture {
                            mut command,
                            authority,
                            reservation,
                            deadline,
                        } => {
                            let Command::Capture {
                                identity,
                                candidate,
                                timeout,
                            } = &mut command
                            else {
                                return Err(protocol::protocol_fault());
                            };
                            if state.selected.as_ref().is_none_or(|(bound, selected)| {
                                !protocol::same_selection(bound, identity) || selected != candidate
                            }) {
                                authority.cancel();
                                return Err(stale());
                            }
                            *timeout = deadline.saturating_duration_since(Instant::now());
                            if timeout.is_zero() {
                                authority.cancel();
                                return Err(operation_expired());
                            }
                            state.deadline = Some(deadline);
                            state.active = Some(ActiveCapture {
                                identity: identity.clone(),
                                geometry: candidate.geometry,
                                authority,
                                reservation,
                            });
                            Ok(command)
                        }
                    }
                })();
                if let Err(error) = command.and_then(|command| {
                    write
                        .as_ref()
                        .ok_or_else(protocol::protocol_fault)?
                        .try_send(command)
                        .map_err(|_| protocol::protocol_fault())
                }) {
                    primary.get_or_insert(error);
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                primary.get_or_insert_with(protocol::cancelled);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match child.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) => {}
            Err(_) => {
                primary.get_or_insert_with(|| {
                    Fault::new("CaptureContainment", "Owned worker status is unavailable")
                });
            }
        }
        if stop.is_none() {
            let reason = if cancel.0.cancelled.load(Ordering::Acquire) {
                Some(protocol::cancelled())
            } else if state
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                Some(operation_expired())
            } else if let Some(active) = &state.active {
                active
                    .authority
                    .check(&active.identity)
                    .err()
                    .or_else(|| primary.clone())
            } else {
                primary.clone()
            };
            if let Some(reason) = reason {
                primary.get_or_insert(reason);
                if let Some(active) = &state.active {
                    active.authority.cancel();
                }
                stop = Some(Instant::now());
                if let Some(send) = write.take() {
                    let _ = send.try_send(Command::Cancel);
                }
            }
        }
        if let Some(stopped) = stop {
            if stopped.elapsed() >= CLEANUP_LIMIT {
                forced = true;
                let _ = child.kill();
            }
            if stopped.elapsed() >= CLEANUP_LIMIT + CONTAINMENT_LIMIT && !reported_unreaped {
                reported_unreaped = true;
                let settlement = WorkerSettlement {
                    child_reaped: false,
                    clean: false,
                    forced: true,
                    primary: Some(Fault::new(
                        "CaptureContainment",
                        "Owned worker reaping remains unconfirmed",
                    )),
                };
                retain_settlement(cancel, &settlement);
                let _ = replies.try_send(Reply::Finished(WorkerCaptureResult {
                    capture: None,
                    primary: primary.clone(),
                    session_clean: false,
                    settlement: Some(settlement),
                }));
                // Continue owning child and payload; no successor may use this slot.
            }
        }
        thread::sleep(Duration::from_millis(5));
    };
    drop(write);
    let writer_failed = !matches!(writer.join(), Ok(Ok(())));
    if reader.join().is_err() {
        primary.get_or_insert_with(protocol::protocol_fault);
    }
    for event in events.try_iter() {
        if let Err(error) =
            event.and_then(|event| state.accept(event, request, replies, cancel, true))
        {
            primary.get_or_insert(error);
        }
    }
    let (child_primary, child_clean) = state
        .terminal
        .unwrap_or_else(|| (Some(protocol::protocol_fault()), false));
    if primary.is_none() {
        primary = child_primary;
    }
    if writer_failed {
        primary.get_or_insert_with(protocol::protocol_fault);
    }
    if cancel.0.cancelled.load(Ordering::Acquire) {
        primary.get_or_insert_with(protocol::cancelled);
    }
    if let Some(active) = state.active {
        active.authority.cancel();
        primary.get_or_insert_with(protocol::protocol_fault);
    }
    forced |= exit.code() == Some(124);
    let clean = child_clean && exit.success() && !forced;
    if !clean {
        primary.get_or_insert_with(|| {
            Fault::new(
                "CaptureCleanup",
                "Native cleanup or owned worker containment was incomplete",
            )
        });
    }
    let settlement = WorkerSettlement {
        child_reaped: true,
        clean,
        forced,
        primary: if clean {
            primary.clone()
        } else {
            Some(Fault::new(
                "CaptureCleanup",
                "Native cleanup or owned worker containment was incomplete",
            ))
        },
    };
    Ok(WorkerCaptureResult {
        capture: None,
        primary,
        session_clean: false,
        settlement: Some(settlement),
    })
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
