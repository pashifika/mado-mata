//! Socket admission, ticket correlation and bounded lifetimes.

use super::endpoint::{self, Endpoint, Readable};
use super::protocol::{self, Claimed, Outcome, Request};
use super::{
    FRAME_BYTES, HostOwner, MAX_CLIENTS, MAX_PENDING, Notify, OUTBOX_FRAMES, RESPONSE_TIMEOUT,
    lock, stale_ticket,
};
use mado_runtime_comparison::model::Fault;
use serde_json::Value;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);
/// Backoff after an unexpected accept failure such as descriptor exhaustion.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(super) struct Hooks {
    pub notify: Notify,
    pub host_owner: HostOwner,
}

#[derive(Clone, Copy)]
pub(super) struct Settings {
    /// From admission until a reply; also bounds the arrival of a started frame.
    pub timeout: Duration,
    /// A client that stops reading for this long is disconnected.
    pub write_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout: RESPONSE_TIMEOUT,
            write_timeout: Duration::from_secs(10),
        }
    }
}

struct Client {
    id: u64,
    stream: UnixStream,
    workers: AtomicU8,
    outbox: Mutex<Option<SyncSender<Vec<u8>>>>,
}

impl Client {
    fn send(&self, frame: Vec<u8>) {
        let mut outbox = lock(&self.outbox);
        if outbox
            .as_ref()
            .is_some_and(|sender| sender.try_send(frame).is_err())
        {
            outbox.take();
            let _ = self.stream.shutdown(Shutdown::Both);
        }
    }

    /// The writer flushes queued frames, then shuts the socket down.
    fn close(&self) {
        lock(&self.outbox).take();
    }
}

struct Pending {
    ticket: String,
    client: Arc<Client>,
    id: String,
    /// Moved to the controller at claim.
    request: Option<Request>,
    deadline: Instant,
    dispatched: bool,
}

impl Pending {
    fn outcome(&self) -> Outcome {
        if self.dispatched {
            Outcome::Unknown
        } else {
            Outcome::NotApplied
        }
    }
}

struct State {
    accepting: bool,
    registered: bool,
    stopped: bool,
    next_client: u64,
    clients: Vec<Arc<Client>>,
    requests: Vec<Pending>,
}

struct Hub {
    instance: String,
    token: String,
    hooks: Hooks,
    settings: Settings,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Close,
}

enum FrameError {
    Invalid,
    TooLarge,
    Io,
}

impl From<io::Error> for FrameError {
    fn from(_: io::Error) -> Self {
        Self::Io
    }
}

/// Constant-time for equal lengths; the token length is public.
fn same_secret(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn read_until(mut stream: &UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_frame(mut stream: &UnixStream, timeout: Duration) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0_u8; 4];
    // A connection may idle between frames, never inside one.
    stream.set_read_timeout(None)?;
    loop {
        match stream.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    let deadline = Instant::now() + timeout;
    read_until(stream, &mut header[1..], deadline)?;
    // The length is checked before any allocation or parsing.
    let length = usize::try_from(u32::from_be_bytes(header)).map_err(|_| FrameError::TooLarge)?;
    if length == 0 {
        return Err(FrameError::Invalid);
    }
    if length > FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let mut body = vec![0_u8; length];
    read_until(stream, &mut body, deadline)?;
    Ok(Some(body))
}

fn write_loop(mut stream: &UnixStream, outbox: Receiver<Vec<u8>>) {
    for frame in outbox {
        if stream.write_all(&frame).is_err() {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

impl Hub {
    fn refusal(&self, id: Option<&str>, code: &str, message: &str, outcome: Outcome) -> Vec<u8> {
        protocol::refusal_frame(&self.instance, id, code, message, outcome)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn accept_loop(self: &Arc<Self>, listener: &UnixListener, wake: &UnixStream) {
        loop {
            match endpoint::wait_readable(listener, wake) {
                Ok(Readable::Wake) => return,
                Ok(Readable::Listener) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
            match listener.accept() {
                Ok((stream, _)) => self.connect(stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionAborted
                    ) => {}
                Err(_) => thread::sleep(ACCEPT_RETRY),
            }
        }
    }

    fn connect(self: &Arc<Self>, stream: UnixStream) {
        // BSD-derived systems let accepted sockets inherit the listener's O_NONBLOCK.
        if stream.set_nonblocking(false).is_err()
            || stream
                .set_write_timeout(Some(self.settings.write_timeout))
                .is_err()
            || endpoint::peer_uid(&stream).ok() != Some(endpoint::effective_uid())
        {
            return;
        }
        let (sender, outbox) = mpsc::sync_channel(OUTBOX_FRAMES);
        let admitted = {
            let mut state = self.state();
            if !state.accepting {
                Err(("shutdown", "The application is closing", stream))
            } else if state.clients.len() >= MAX_CLIENTS {
                Err((
                    "busy",
                    "Too many collaboration clients are connected",
                    stream,
                ))
            } else {
                state.next_client += 1;
                let client = Arc::new(Client {
                    id: state.next_client,
                    stream,
                    workers: AtomicU8::new(2),
                    outbox: Mutex::new(Some(sender)),
                });
                state.clients.push(client.clone());
                Ok(client)
            }
        };
        let client = match admitted {
            Ok(client) => client,
            Err((code, message, stream)) => {
                // One small frame fits the empty send buffer; no read, no thread.
                let _ =
                    (&stream).write_all(&self.refusal(None, code, message, Outcome::NotApplied));
                return;
            }
        };
        let hub = self.clone();
        let writer_client = client.clone();
        let started = thread::Builder::new()
            .name("collaboration-writer".into())
            .spawn(move || {
                write_loop(&writer_client.stream, outbox);
                hub.worker_done(&writer_client);
            });
        if started.is_err() {
            self.disconnect(&client);
            // Neither worker started; release both reservations.
            self.worker_done(&client);
            self.worker_done(&client);
            return;
        }
        let hub = self.clone();
        let reader_client = client.clone();
        let started = thread::Builder::new()
            .name("collaboration-reader".into())
            .spawn(move || {
                hub.read_loop(&reader_client);
                hub.worker_done(&reader_client);
            });
        if started.is_err() {
            self.disconnect(&client);
            self.worker_done(&client);
        }
    }

    fn read_loop(&self, client: &Arc<Client>) {
        loop {
            let refusal = match read_frame(&client.stream, self.settings.timeout) {
                Ok(Some(body)) => {
                    if self.frame(client, body) == Flow::Close {
                        break;
                    }
                    continue;
                }
                Ok(None) | Err(FrameError::Io) => break,
                Err(FrameError::Invalid) => ("invalid_frame", "A frame must contain a JSON object"),
                Err(FrameError::TooLarge) => (
                    "frame_too_large",
                    "The frame exceeds the advertised frameBytes limit",
                ),
            };
            client.send(self.refusal(None, refusal.0, refusal.1, Outcome::NotApplied));
            break;
        }
        self.disconnect(client);
    }

    fn close_with(&self, client: &Client, code: &str, message: &str) -> Flow {
        client.send(self.refusal(None, code, message, Outcome::NotApplied));
        Flow::Close
    }

    /// Authenticates one frame, then handles a cancel or a request.
    fn frame(&self, client: &Arc<Client>, body: Vec<u8>) -> Flow {
        let parsed = serde_json::from_slice::<Value>(&body);
        drop(body);
        let Ok(Value::Object(mut fields)) = parsed else {
            return self.close_with(
                client,
                "invalid_frame",
                "A frame must contain a JSON object",
            );
        };
        match fields.remove("protocol") {
            Some(Value::Number(number)) if number.as_u64() == Some(u64::from(super::PROTOCOL)) => {}
            Some(Value::Number(_)) => {
                return self.close_with(
                    client,
                    "unsupported_protocol",
                    "This application speaks collaboration protocol 1",
                );
            }
            _ => return self.close_with(client, "invalid_frame", "The frame has no protocol"),
        }
        let instance = fields.remove("instance");
        if instance.as_ref().and_then(Value::as_str) != Some(self.instance.as_str()) {
            return self.close_with(
                client,
                "instance_mismatch",
                "The frame names another application instance",
            );
        }
        let token = fields.remove("token");
        if !token
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|token| same_secret(token.as_bytes(), self.token.as_bytes()))
        {
            return self.close_with(client, "unauthorized", "The frame is not authenticated");
        }
        if let Some(cancel) = fields.remove("cancel") {
            return match cancel {
                Value::String(id) if fields.is_empty() && protocol::valid_request_id(&id) => {
                    self.cancel(client, &id);
                    Flow::Continue
                }
                _ => self.close_with(
                    client,
                    "invalid_frame",
                    "A cancel frame names one request id",
                ),
            };
        }
        let id = match fields.remove("id") {
            Some(Value::String(id)) if protocol::valid_request_id(&id) => id,
            _ => return self.close_with(client, "invalid_frame", "The request id is invalid"),
        };
        match protocol::parse_request(id.clone(), fields) {
            Ok(request) => self.admit(client, id, request),
            Err(refusal) => client.send(self.refusal(
                Some(&id),
                refusal.code,
                &refusal.message,
                Outcome::NotApplied,
            )),
        }
        Flow::Continue
    }

    fn admit(&self, client: &Arc<Client>, id: String, request: Request) {
        let ticket = {
            let mut state = self.state();
            let refusal = if !state.accepting {
                Some(("shutdown", "The application is closing"))
            } else if !state.registered {
                Some((
                    "unavailable",
                    "The desktop authoring controller is unavailable",
                ))
            } else if state
                .requests
                .iter()
                .any(|pending| pending.client.id == client.id && pending.id == id)
            {
                Some((
                    "duplicate_request",
                    "A request with this id is still in flight",
                ))
            } else if state.requests.len() >= MAX_PENDING {
                Some(("busy", "Too many collaboration requests are pending"))
            } else {
                None
            };
            if let Some((code, message)) = refusal {
                client.send(self.refusal(Some(&id), code, message, Outcome::NotApplied));
                return;
            }
            let ticket = format!("c{}", NEXT_TICKET.fetch_add(1, Ordering::Relaxed));
            state.requests.push(Pending {
                ticket: ticket.clone(),
                client: client.clone(),
                id,
                request: Some(request),
                deadline: Instant::now() + self.settings.timeout,
                dispatched: false,
            });
            self.changed.notify_all();
            ticket
        };
        if !(self.hooks.notify)(&ticket) {
            self.withdraw(
                |pending| pending.ticket == ticket,
                "unavailable",
                "The desktop authoring controller is unavailable",
            );
        }
    }

    /// Answers and forgets the first unclaimed request matching `select`.
    fn withdraw(&self, select: impl Fn(&Pending) -> bool, code: &str, message: &str) {
        let mut state = self.state();
        if let Some(index) = state
            .requests
            .iter()
            .position(|pending| !pending.dispatched && select(pending))
        {
            let pending = state.requests.remove(index);
            pending.client.send(self.refusal(
                Some(&pending.id),
                code,
                message,
                Outcome::NotApplied,
            ));
            self.changed.notify_all();
        }
    }

    /// A claimed request is unaffected; its real reply follows.
    fn cancel(&self, client: &Client, id: &str) {
        self.withdraw(
            |pending| pending.client.id == client.id && pending.id == id,
            "cancelled",
            "The request was cancelled before dispatch",
        );
    }

    fn disconnect(&self, client: &Client) {
        let mut state = self.state();
        // Unclaimed work is withdrawn; claimed work still settles, unobserved.
        state
            .requests
            .retain(|pending| pending.dispatched || pending.client.id != client.id);
        self.changed.notify_all();
        drop(state);
        client.close();
    }

    fn worker_done(&self, client: &Client) {
        if client.workers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.state()
                .clients
                .retain(|current| current.id != client.id);
        }
    }

    fn claim(&self, ticket: &str) -> Option<Claimed> {
        // Sampled before the hub lock; the controller re-checks at handling.
        let host_owner = (self.hooks.host_owner)();
        let mut state = self.state();
        if !state.accepting || !state.registered {
            return None;
        }
        let index = state
            .requests
            .iter()
            .position(|pending| pending.ticket == ticket && !pending.dispatched)?;
        let pending = &state.requests[index];
        let refusal = if Instant::now() >= pending.deadline {
            Some(("timeout", "The request expired before dispatch"))
        } else if pending
            .request
            .as_ref()
            .is_some_and(|request| !request.matches_owner(host_owner.as_deref()))
        {
            Some(("stale_owner", "The Edit owner is no longer current"))
        } else {
            None
        };
        if let Some((code, message)) = refusal {
            let pending = state.requests.remove(index);
            pending.client.send(self.refusal(
                Some(&pending.id),
                code,
                message,
                Outcome::NotApplied,
            ));
            self.changed.notify_all();
            return None;
        }
        let pending = &mut state.requests[index];
        pending.dispatched = true;
        let request = pending.request.take()?;
        Some(Claimed::new(pending.ticket.clone(), request))
    }

    fn reply(&self, ticket: &str, response: Value) -> Result<(), Fault> {
        let mut state = self.state();
        let index = state
            .requests
            .iter()
            .position(|pending| pending.ticket == ticket && pending.dispatched)
            .ok_or_else(stale_ticket)?;
        let pending = state.requests.remove(index);
        // Shutdown must not close the outbox between retiring the ticket and queuing its result.
        let result = match protocol::parse_response(response) {
            Ok(response) => {
                let frame = protocol::reply_frame(&self.instance, &pending.id, &response)
                    .unwrap_or_else(|| {
                        self.refusal(
                            Some(&pending.id),
                            "reply_too_large",
                            "The reply exceeds the advertised frameBytes limit",
                            Outcome::Unknown,
                        )
                    });
                pending.client.send(frame);
                Ok(())
            }
            Err(fault) => {
                pending.client.send(self.refusal(
                    Some(&pending.id),
                    "invalid_reply",
                    "The desktop returned an invalid reply",
                    Outcome::Unknown,
                ));
                Err(fault)
            }
        };
        self.changed.notify_all();
        result
    }

    fn register(&self) {
        let mut state = self.state();
        if state.accepting {
            state.registered = true;
        }
    }

    fn unregister(&self) {
        let mut state = self.state();
        state.registered = false;
        for pending in state.requests.drain(..) {
            pending.client.send(self.refusal(
                Some(&pending.id),
                "unavailable",
                "The desktop authoring controller became unavailable",
                pending.outcome(),
            ));
        }
        self.changed.notify_all();
    }

    fn expire(&self, state: &mut State, now: Instant) {
        let mut index = 0;
        while index < state.requests.len() {
            if state.requests[index].deadline > now {
                index += 1;
                continue;
            }
            let pending = state.requests.remove(index);
            let message = if pending.dispatched {
                "The desktop did not reply in time"
            } else {
                "The request expired before dispatch"
            };
            pending.client.send(self.refusal(
                Some(&pending.id),
                "timeout",
                message,
                pending.outcome(),
            ));
            self.changed.notify_all();
        }
    }

    fn timer_loop(&self) {
        let mut state = self.state();
        while !state.stopped {
            let now = Instant::now();
            self.expire(&mut state, now);
            let next = state.requests.iter().map(|pending| pending.deadline).min();
            state = match next {
                Some(deadline) => {
                    self.changed
                        .wait_timeout(state, deadline.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

pub(super) struct Server {
    hub: Arc<Hub>,
    wake: UnixStream,
    accept: Mutex<Option<JoinHandle<()>>>,
    timer: Mutex<Option<JoinHandle<()>>>,
    endpoint: Mutex<Endpoint>,
    shutdown: Mutex<bool>,
}

fn spawn_fault(error: &io::Error) -> Fault {
    Fault::new(
        "CollaborationUnavailable",
        format!("Could not start the collaboration endpoint: {error}"),
    )
}

impl Server {
    pub(super) fn start(root: &Path, hooks: Hooks, settings: Settings) -> Result<Self, Fault> {
        let instance = crate::storage::new_id()?;
        let token = endpoint::random_hex::<32>()?;
        let (mut published, listener) = endpoint::create(root, &instance, &token)?;
        let (wake, wake_reader) = match UnixStream::pair() {
            Ok(pair) => pair,
            Err(error) => {
                published.remove();
                return Err(spawn_fault(&error));
            }
        };
        let server = Self {
            hub: Arc::new(Hub {
                instance,
                token,
                hooks,
                settings,
                state: Mutex::new(State {
                    accepting: true,
                    registered: false,
                    stopped: false,
                    next_client: 0,
                    clients: Vec::new(),
                    requests: Vec::new(),
                }),
                changed: Condvar::new(),
            }),
            wake,
            accept: Mutex::new(None),
            timer: Mutex::new(None),
            endpoint: Mutex::new(published),
            shutdown: Mutex::new(false),
        };
        let hub = server.hub.clone();
        let accept = thread::Builder::new()
            .name("collaboration-accept".into())
            .spawn(move || hub.accept_loop(&listener, &wake_reader));
        let hub = server.hub.clone();
        let started = accept.and_then(|accept| {
            *lock(&server.accept) = Some(accept);
            thread::Builder::new()
                .name("collaboration-timer".into())
                .spawn(move || hub.timer_loop())
        });
        match started {
            Ok(timer) => {
                *lock(&server.timer) = Some(timer);
                Ok(server)
            }
            Err(error) => {
                server.shutdown(Duration::ZERO);
                Err(spawn_fault(&error))
            }
        }
    }

    pub(super) fn instance(&self) -> &str {
        &self.hub.instance
    }

    /// Connected clients and retained requests.
    #[cfg(test)]
    pub(super) fn occupancy(&self) -> (usize, usize) {
        let state = self.hub.state();
        (state.clients.len(), state.requests.len())
    }

    pub(super) fn register(&self) {
        self.hub.register();
    }

    pub(super) fn unregister(&self) {
        self.hub.unregister();
    }

    pub(super) fn claim(&self, ticket: &str) -> Option<Claimed> {
        self.hub.claim(ticket)
    }

    pub(super) fn reply(&self, ticket: &str, response: Value) -> Result<(), Fault> {
        self.hub.reply(ticket, response)
    }

    pub(super) fn shutdown(&self, drain: Duration) {
        let mut finished = lock(&self.shutdown);
        if *finished {
            return;
        }
        {
            let mut state = self.hub.state();
            state.accepting = false;
            state.registered = false;
            let hub = &self.hub;
            state.requests.retain(|pending| {
                if pending.dispatched {
                    return true;
                }
                pending.client.send(hub.refusal(
                    Some(&pending.id),
                    "shutdown",
                    "The application is closing",
                    Outcome::NotApplied,
                ));
                false
            });
            self.hub.changed.notify_all();
        }
        // Stop accepting connections and withdraw the advertisement first.
        let _ = self.wake.shutdown(Shutdown::Both);
        if let Some(accept) = lock(&self.accept).take() {
            let _ = accept.join();
        }
        lock(&self.endpoint).remove();

        let deadline = Instant::now() + drain;
        let mut state = self.hub.state();
        loop {
            let now = Instant::now();
            if now >= deadline || state.requests.is_empty() {
                break;
            }
            state = self
                .hub
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        for pending in state.requests.drain(..) {
            pending.client.send(self.hub.refusal(
                Some(&pending.id),
                "shutdown",
                "The application closed before the desktop replied",
                Outcome::Unknown,
            ));
        }
        for client in state.clients.drain(..) {
            client.close();
        }
        state.stopped = true;
        self.hub.changed.notify_all();
        drop(state);
        if let Some(timer) = lock(&self.timer).take() {
            let _ = timer.join();
        }
        *finished = true;
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown(Duration::ZERO);
    }
}
