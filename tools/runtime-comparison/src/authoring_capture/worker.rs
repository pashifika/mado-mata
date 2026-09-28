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
    pub settlement: WorkerSettlement,
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
    Select(String),
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
    Finished(WorkerCaptureResult),
}

/// One retained provider/target namespace. Capture consumes it, including on failure.
/// The background owner keeps its reservation even when native containment is late.
pub struct AuthoringCaptureWorker {
    identity: CaptureIdentity,
    candidates: Vec<Candidate>,
    selected: Option<Candidate>,
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
        Self::from_child(child, request, started, SELECTION_LIMIT)
    }

    fn from_child(
        child: OwnedChild,
        request: DiscoveryRequest,
        started: Instant,
        selection_limit: Duration,
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
            .spawn(move || {
                supervise(
                    child,
                    request,
                    receive,
                    send,
                    shared,
                    started,
                    selection_limit,
                )
            })
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
                .or(result.settlement.primary)
                .unwrap_or_else(protocol::protocol_fault)),
            Reply::Selected(_) => Err(protocol::protocol_fault()),
        }
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn select(&mut self, key: &str) -> Result<Candidate, Fault> {
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
        self.requests
            .try_send(Request::Select(key.into()))
            .map_err(|_| protocol::protocol_fault())?;
        match self
            .replies
            .recv()
            .map_err(|_| protocol::protocol_fault())?
        {
            Reply::Selected(current) if current == candidate => {
                self.selected = Some(current.clone());
                Ok(current)
            }
            Reply::Finished(result) => Err(result
                .primary
                .or(result.settlement.primary)
                .unwrap_or_else(protocol::protocol_fault)),
            _ => Err(stale()),
        }
    }

    pub fn capture(
        self,
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
        self.wait_finished()
    }

    pub fn settle(self) -> WorkerSettlement {
        self.cancel.cancel();
        self.wait_finished().settlement
    }

    fn wait_finished(&self) -> WorkerCaptureResult {
        loop {
            match self.replies.recv() {
                Ok(Reply::Finished(result)) => return result,
                Ok(Reply::Discovered(_) | Reply::Selected(_)) => {}
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
                        settlement,
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
    selection_limit: Duration,
) {
    let result = supervise_inner(
        &mut child,
        &request,
        requests,
        &replies,
        &cancel,
        started,
        selection_limit,
    );
    match result {
        Ok(result) => {
            retain_settlement(&cancel, &result.settlement);
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
                settlement,
            }));
        }
    }
}

fn supervise_inner(
    child: &mut OwnedChild,
    request: &DiscoveryRequest,
    requests: mpsc::Receiver<Request>,
    replies: &mpsc::SyncSender<Reply>,
    cancel: &WorkerCancel,
    started: Instant,
    selection_limit: Duration,
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
    let mut deadline = started + request.timeout;
    let mut discovered = false;
    let mut expected: Option<(CaptureIdentity, CaptureGeometry, Arc<CaptureAuthority>)> = None;
    let mut selecting: Option<(String, Instant)> = None;
    let mut reservation = None;
    let mut terminal = None;
    let mut primary = None;
    let mut stop = None;
    let mut forced = false;
    let mut reported_unreaped = false;
    let mut write = Some(write);
    let mut accept = |event: Result<Received, Fault>,
                      deadline: &mut Instant,
                      primary: &mut Option<Fault>,
                      selecting: &mut Option<(String, Instant)>,
                      discovered: &mut bool| {
        match event {
            Ok(Received::Discovered(identity, candidates))
                if !*discovered
                    && identity == request.identity
                    && Instant::now() < *deadline
                    && !cancel.0.cancelled.load(Ordering::Acquire) =>
            {
                *discovered = true;
                *deadline = Instant::now() + selection_limit;
                if replies.try_send(Reply::Discovered(candidates)).is_err() {
                    primary.get_or_insert_with(protocol::cancelled);
                }
            }
            Ok(Received::Selected(candidate))
                if selecting.as_ref().is_some_and(|(key, until)| {
                    *key == candidate.key && Instant::now() < *until
                }) =>
            {
                *selecting = None;
                if replies.try_send(Reply::Selected(candidate)).is_err() {
                    primary.get_or_insert_with(protocol::cancelled);
                }
            }
            Ok(Received::Terminal {
                primary: error,
                clean,
                pending,
            }) if terminal.is_none() => {
                terminal = Some((error, clean, pending));
            }
            Ok(_) => {
                primary.get_or_insert_with(|| {
                    if *discovered && Instant::now() >= *deadline {
                        protocol::expired()
                    } else {
                        protocol::protocol_fault()
                    }
                });
            }
            Err(error) => {
                primary.get_or_insert(error);
            }
        }
    };
    let exit = loop {
        for event in events.try_iter() {
            accept(
                event,
                &mut deadline,
                &mut primary,
                &mut selecting,
                &mut discovered,
            );
        }
        if let Ok(incoming) = requests.try_recv() {
            if expected.is_some()
                || selecting.is_some()
                || stop.is_some()
                || Instant::now() >= deadline
                || cancel.0.cancelled.load(Ordering::Acquire)
            {
                primary.get_or_insert_with(protocol::expired);
                if let Request::Capture { authority, .. } = incoming {
                    authority.cancel();
                }
            } else {
                let command = match incoming {
                    Request::Select(key) => {
                        selecting =
                            Some((key.clone(), deadline.min(Instant::now() + DISCOVERY_LIMIT)));
                        Command::Select { key }
                    }
                    Request::Capture {
                        mut command,
                        authority,
                        reservation: held,
                        deadline: until,
                    } => {
                        if let Command::Capture {
                            identity,
                            candidate,
                            timeout,
                        } = &mut command
                        {
                            deadline = until;
                            *timeout = until.saturating_duration_since(Instant::now());
                            expected = Some((identity.clone(), candidate.geometry, authority));
                            reservation = Some(held);
                        }
                        command
                    }
                };
                if write
                    .as_ref()
                    .is_none_or(|send| send.try_send(command).is_err())
                {
                    primary.get_or_insert_with(protocol::protocol_fault);
                }
            }
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
            } else if Instant::now() >= deadline {
                Some(if expected.is_none() && discovered {
                    protocol::expired()
                } else {
                    Fault::new(
                        "DeadlineExceeded",
                        "Authoring worker operation deadline expired",
                    )
                })
            } else if selecting
                .as_ref()
                .is_some_and(|(_, until)| Instant::now() >= *until)
            {
                Some(Fault::new(
                    "DeadlineExceeded",
                    "Retained window selection revalidation expired",
                ))
            } else {
                primary.clone()
            };
            if let Some(reason) = reason {
                primary.get_or_insert(reason);
                if let Some((_, _, authority)) = &expected {
                    authority.cancel();
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
                    settlement,
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
        accept(
            event,
            &mut deadline,
            &mut primary,
            &mut selecting,
            &mut discovered,
        );
    }
    drop(accept);
    let (child_primary, child_clean, pending) =
        terminal.unwrap_or_else(|| (Some(protocol::protocol_fault()), false, None));
    if primary.is_none() {
        primary = child_primary;
    }
    if writer_failed {
        primary.get_or_insert_with(protocol::protocol_fault);
    }
    if cancel.0.cancelled.load(Ordering::Acquire) {
        if let Some((_, _, authority)) = &expected {
            authority.cancel();
        }
        primary.get_or_insert_with(protocol::cancelled);
    }
    forced |= exit.code() == Some(124);
    let clean = child_clean && exit.success() && !forced;
    if !clean && primary.is_none() {
        primary = Some(Fault::new(
            "CaptureCleanup",
            "Capture worker did not detach and exit cleanly",
        ));
    }
    if expected.is_some() && Instant::now() >= deadline {
        if let Some((_, _, authority)) = &expected {
            authority.cancel();
        }
        primary.get_or_insert_with(|| {
            Fault::new("DeadlineExceeded", "Capture publication deadline expired")
        });
    }
    // Once reaped, child image reservations can be released before host decoding.
    drop(reservation);
    let capture = match (pending, expected) {
        (Some(pending), Some((identity, geometry, authority))) if primary.is_none() && clean => {
            if pending.header.geometry != geometry {
                primary = Some(stale());
                None
            } else {
                match pending.accept_after_reap(
                    ChildSettlement::Reaped(exit),
                    &authority,
                    &identity,
                ) {
                    Ok(capture)
                        if Instant::now() < deadline
                            && !cancel.0.cancelled.load(Ordering::Acquire) =>
                    {
                        Some(capture)
                    }
                    Ok(_) => {
                        authority.cancel();
                        primary = Some(if cancel.0.cancelled.load(Ordering::Acquire) {
                            protocol::cancelled()
                        } else {
                            Fault::new("DeadlineExceeded", "Capture publication deadline expired")
                        });
                        None
                    }
                    Err(error) => {
                        primary = Some(error);
                        None
                    }
                }
            }
        }
        (None, Some(_)) if primary.is_none() => {
            primary = Some(protocol::protocol_fault());
            None
        }
        (Some(_), _) => {
            primary.get_or_insert_with(protocol::protocol_fault);
            None
        }
        _ => None,
    };
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
        capture,
        primary,
        settlement,
    })
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
