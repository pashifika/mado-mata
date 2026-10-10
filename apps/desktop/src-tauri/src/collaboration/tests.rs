use super::endpoint::DIRECTORY;
use super::transport::Settings;
use super::{Collaboration, FRAME_BYTES, lock};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const OWNER: &str = "authoring-1-1";
const PACKAGE: &str = "package-1";
const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    collaboration: Collaboration,
    tickets: Receiver<String>,
    host_owner: Arc<Mutex<Option<String>>>,
    deliver: Arc<AtomicBool>,
}

impl Fixture {
    fn new() -> Self {
        Self::with(Settings::default())
    }

    fn with(settings: Settings) -> Self {
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "mado-collaboration-{}",
            crate::storage::new_id().unwrap()
        ));
        let root = base.join("root");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)
            .unwrap();
        let (sender, tickets) = mpsc::channel();
        let host_owner = Arc::new(Mutex::new(Some(OWNER.to_owned())));
        let deliver = Arc::new(AtomicBool::new(true));
        let (owner, delivering) = (host_owner.clone(), deliver.clone());
        let collaboration = Collaboration::with_settings(
            Some(root.clone()),
            Arc::new(move |ticket: &str| {
                delivering.load(Ordering::SeqCst) && sender.send(ticket.to_owned()).is_ok()
            }),
            Arc::new(move || lock(&owner).clone()),
            settings,
        );
        Self {
            base,
            root,
            collaboration,
            tickets,
            host_owner,
            deliver,
        }
    }

    fn collaboration_directory(&self) -> PathBuf {
        self.root.join(DIRECTORY)
    }

    /// Starts the endpoint and reads its descriptor as an adapter would.
    fn ready(&self) -> (PathBuf, Value) {
        let ready = self.collaboration.ready().unwrap();
        assert_eq!(ready.protocol, 1);
        let path = self
            .collaboration_directory()
            .join(format!("{}.json", ready.instance));
        let descriptor: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(descriptor["instance"], ready.instance.as_str());
        (path, descriptor)
    }

    fn ticket(&self) -> String {
        self.tickets.recv_timeout(WAIT).unwrap()
    }

    fn no_ticket(&self) {
        assert!(matches!(self.tickets.try_recv(), Err(TryRecvError::Empty)));
    }

    fn set_host_owner(&self, owner: Option<&str>) {
        *lock(&self.host_owner) = owner.map(str::to_owned);
    }

    fn occupancy(&self) -> (usize, usize) {
        self.collaboration.server().unwrap().occupancy()
    }

    /// Server threads observe disconnects asynchronously; wait boundedly.
    fn wait_for(&self, expected: impl Fn((usize, usize)) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !expected(self.occupancy()) {
            assert!(Instant::now() < deadline, "server state did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn reply(&self, ticket: &str, response: Value) {
        self.collaboration.reply(ticket, response).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.collaboration.shutdown(Duration::ZERO);
        let _ = fs::remove_dir_all(&self.base);
    }
}

struct Peer {
    stream: UnixStream,
    instance: String,
    token: String,
}

impl Peer {
    fn connect(descriptor: &Value) -> Self {
        let stream = UnixStream::connect(descriptor["socket"].as_str().unwrap()).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        Self {
            stream,
            instance: descriptor["instance"].as_str().unwrap().to_owned(),
            token: descriptor["token"].as_str().unwrap().to_owned(),
        }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    fn send(&mut self, value: &Value) {
        let bytes = serde_json::to_vec(value).unwrap();
        self.raw(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
        self.raw(&bytes);
    }

    fn envelope(&self, id: &str, owner: Option<&str>, operation: &Value) -> Value {
        json!({
            "protocol": 1, "id": id, "instance": self.instance, "token": self.token,
            "owner": owner, "package": owner.map(|_| PACKAGE), "operation": operation,
        })
    }

    fn request(&mut self, id: &str, owner: Option<&str>, operation: &Value) {
        let envelope = self.envelope(id, owner, operation);
        self.send(&envelope);
    }

    fn cancel(&mut self, id: &str) {
        let frame =
            json!({"protocol": 1, "instance": self.instance, "token": self.token, "cancel": id});
        self.send(&frame);
    }

    fn receive(&mut self) -> Value {
        let mut header = [0_u8; 4];
        self.stream.read_exact(&mut header).unwrap();
        let length = usize::try_from(u32::from_be_bytes(header)).unwrap();
        assert!(length <= FRAME_BYTES);
        let mut body = vec![0_u8; length];
        self.stream.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn closed(&mut self) -> bool {
        let mut byte = [0_u8; 1];
        matches!(self.stream.read(&mut byte), Ok(0))
    }
}

fn describe() -> Value {
    json!({"kind": "describe"})
}

fn assert_refusal(frame: &Value, id: Option<&str>, code: &str, outcome: &str) {
    assert_eq!(frame["protocol"], 1, "{frame}");
    assert_eq!(frame["id"], json!(id), "{frame}");
    assert_eq!(frame["owner"], Value::Null, "{frame}");
    assert_eq!(frame["ok"], false, "{frame}");
    assert_eq!(frame["error"]["code"], code, "{frame}");
    assert_eq!(frame["error"]["outcome"], outcome, "{frame}");
    assert!(frame.get("result").is_none(), "{frame}");
}

fn ok(result: &Value) -> Value {
    json!({"owner": OWNER, "ok": true, "result": result})
}

#[test]
fn private_endpoint_relays_requests_through_main_window_tickets() {
    let fixture = Fixture::new();
    let (path, descriptor) = fixture.ready();
    let uid = fs::metadata(&fixture.root).unwrap().uid();

    let file = fs::symlink_metadata(&path).unwrap();
    assert!(file.file_type().is_file());
    assert_eq!(
        (file.uid(), file.mode() & 0o777, file.nlink()),
        (uid, 0o600, 1)
    );
    assert!(file.len() <= 4096);
    let directory = fs::symlink_metadata(fixture.collaboration_directory()).unwrap();
    assert_eq!((directory.uid(), directory.mode() & 0o777), (uid, 0o700));
    let mut fields: Vec<_> = descriptor.as_object().unwrap().keys().cloned().collect();
    fields.sort();
    assert_eq!(
        fields,
        ["instance", "limits", "pid", "protocol", "socket", "token"]
    );
    assert_eq!(descriptor["protocol"], 1);
    assert_eq!(descriptor["pid"], std::process::id());
    let token = descriptor["token"].as_str().unwrap();
    assert!(
        token.len() == 64
            && token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    let socket = Path::new(descriptor["socket"].as_str().unwrap());
    assert!(socket.as_os_str().len() <= 100);
    assert_eq!(socket.canonicalize().unwrap(), socket);
    let socket_file = fs::symlink_metadata(socket).unwrap();
    assert!(socket_file.file_type().is_socket());
    assert_eq!(
        (socket_file.uid(), socket_file.mode() & 0o777),
        (uid, 0o600)
    );
    let socket_directory = fs::symlink_metadata(socket.parent().unwrap()).unwrap();
    assert_eq!(
        (socket_directory.uid(), socket_directory.mode() & 0o777),
        (uid, 0o700)
    );
    // Attaching again keeps the same endpoint.
    assert_eq!(
        fixture.collaboration.ready().unwrap().instance,
        descriptor["instance"].as_str().unwrap()
    );

    let mut peer = Peer::connect(&descriptor);
    peer.request("d1", None, &describe());
    let ticket = fixture.ticket();
    let claimed = serde_json::to_value(fixture.collaboration.claim(&ticket).unwrap()).unwrap();
    assert_eq!(
        claimed,
        json!({"ticket": ticket, "request": {"id": "d1", "owner": null, "package": null, "operation": {"kind": "describe"}}})
    );
    assert!(fixture.collaboration.claim(&ticket).is_none());
    let described = json!({"protocol": 1, "owner": OWNER, "package": PACKAGE});
    fixture.reply(&ticket, ok(&described));
    assert_eq!(
        peer.receive(),
        json!({"protocol": 1, "id": "d1", "instance": descriptor["instance"], "owner": OWNER, "ok": true, "result": described})
    );
    assert_eq!(
        fixture
            .collaboration
            .reply(&ticket, ok(&json!({})))
            .unwrap_err()
            .category,
        "CollaborationTicket"
    );

    peer.request(
        "r1",
        Some(OWNER),
        &json!({"kind": "read", "resource": "file-1", "limit": 65_536}),
    );
    let ticket = fixture.ticket();
    let claimed = serde_json::to_value(fixture.collaboration.claim(&ticket).unwrap()).unwrap();
    assert_eq!(
        claimed["request"],
        json!({"id": "r1", "owner": OWNER, "package": PACKAGE, "operation": {"kind": "read", "resource": "file-1", "limit": 65_536}})
    );
    let refused = json!({"code": "stale_version", "message": "Reread", "resource": "file-1", "outcome": "not_applied"});
    fixture.reply(
        &ticket,
        json!({"owner": OWNER, "ok": false, "error": refused}),
    );
    assert_eq!(
        peer.receive(),
        json!({"protocol": 1, "id": "r1", "instance": descriptor["instance"], "owner": OWNER, "ok": false, "error": refused})
    );

    // Line endings and non-BMP text pass through untouched.
    let edit = json!({"kind": "edit", "edits": [
        {"resource": "file-1", "version": "v.1", "ranges": [{"from": 0, "to": 2, "text": "a\r\n\u{1F600}"}]},
        {"resource": "file-2", "version": "v.2", "text": "b\r"}
    ], "dependencies": [{"resource": "file-3", "version": "v.3"}]});
    peer.request("e1", Some(OWNER), &edit);
    let ticket = fixture.ticket();
    let claimed = serde_json::to_value(fixture.collaboration.claim(&ticket).unwrap()).unwrap();
    assert_eq!(claimed["request"]["operation"], edit);
    let edited =
        json!({"resources": [{"resource": "file-1", "version": "v.4"}], "savedRevision": "s1"});
    fixture.reply(&ticket, ok(&edited));
    assert_eq!(peer.receive()["result"], edited);
}

#[test]
fn frame_lengths_are_checked_before_allocation_and_parsing() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();

    // A frame of exactly the advertised maximum is admitted.
    let mut peer = Peer::connect(&descriptor);
    let mut body = serde_json::to_vec(&peer.envelope("max", None, &describe())).unwrap();
    body.resize(FRAME_BYTES, b' ');
    peer.raw(&u32::try_from(FRAME_BYTES).unwrap().to_be_bytes());
    peer.raw(&body);
    let ticket = fixture.ticket();
    assert!(fixture.collaboration.claim(&ticket).is_some());
    fixture.reply(&ticket, ok(&json!({})));
    assert_eq!(peer.receive()["id"], "max");

    // Oversized lengths are refused from the header alone.
    for length in [u32::try_from(FRAME_BYTES + 1).unwrap(), u32::MAX] {
        let mut peer = Peer::connect(&descriptor);
        peer.raw(&length.to_be_bytes());
        assert_refusal(&peer.receive(), None, "frame_too_large", "not_applied");
        assert!(peer.closed());
    }
    let bodies: [&[u8]; 4] = [b"", b"{x}", b"[1]", b"\xff\xfe"];
    for body in bodies {
        let mut peer = Peer::connect(&descriptor);
        peer.raw(&u32::try_from(body.len()).unwrap().to_be_bytes());
        peer.raw(body);
        assert_refusal(&peer.receive(), None, "invalid_frame", "not_applied");
        assert!(peer.closed());
    }
    fixture.no_ticket();
}

#[test]
fn unauthenticated_or_malformed_requests_never_dispatch() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();
    let closing = [
        ("token", json!("0".repeat(64)), "unauthorized"),
        ("token", Value::Null, "unauthorized"),
        ("instance", json!("other"), "instance_mismatch"),
        ("protocol", json!(2), "unsupported_protocol"),
        ("protocol", json!("1"), "invalid_frame"),
        ("id", json!("bad id"), "invalid_frame"),
        ("id", json!("x".repeat(129)), "invalid_frame"),
    ];
    for (field, value, code) in closing {
        let mut peer = Peer::connect(&descriptor);
        let mut envelope = peer.envelope("a", None, &describe());
        envelope[field] = value;
        peer.send(&envelope);
        assert_refusal(&peer.receive(), None, code, "not_applied");
        assert!(peer.closed(), "{field}");
    }

    let mut peer = Peer::connect(&descriptor);
    let mut extra = peer.envelope("extra", None, &describe());
    extra["argv"] = json!(["sh"]);
    peer.send(&extra);
    assert_refusal(
        &peer.receive(),
        Some("extra"),
        "invalid_request",
        "not_applied",
    );
    let refused = [
        (None, json!({"kind": "save"}), "invalid_request"),
        (
            None,
            json!({"kind": "describe", "path": "/"}),
            "invalid_request",
        ),
        (
            None,
            json!({"kind": "read", "resource": "file-1"}),
            "owner_required",
        ),
        (
            Some(OWNER),
            json!({"kind": "read", "resource": "file-1", "limit": 0}),
            "invalid_request",
        ),
        (
            Some(OWNER),
            json!({"kind": "read", "resource": "file-1", "limit": 65_537}),
            "invalid_request",
        ),
        (
            Some(OWNER),
            json!({"kind": "read", "resource": "file-1", "offset": 1.5}),
            "invalid_request",
        ),
        (
            Some(OWNER),
            json!({"kind": "read", "resource": "file 1"}),
            "invalid_request",
        ),
        (
            Some("owner 1"),
            json!({"kind": "read", "resource": "file-1"}),
            "invalid_request",
        ),
        (
            Some(OWNER),
            json!({"kind": "edit", "edits": []}),
            "invalid_request",
        ),
        (
            None,
            json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": ""}]}),
            "owner_required",
        ),
        (
            Some(OWNER),
            json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": "", "ranges": []}]}),
            "invalid_request",
        ),
        (
            Some(OWNER),
            json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "ranges": []}]}),
            "invalid_request",
        ),
        (
            Some(OWNER),
            json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "ranges": [{"from": -1, "to": 0, "text": ""}]}]}),
            "invalid_request",
        ),
    ];
    for (index, (owner, operation, code)) in refused.iter().enumerate() {
        let id = format!("q{index}");
        peer.request(&id, *owner, operation);
        assert_refusal(&peer.receive(), Some(&id), code, "not_applied");
    }
    fixture.no_ticket();

    // The connection survives refused requests; in-flight ids stay unique.
    peer.request("same", None, &describe());
    let ticket = fixture.ticket();
    peer.request("same", None, &describe());
    assert_refusal(
        &peer.receive(),
        Some("same"),
        "duplicate_request",
        "not_applied",
    );
    fixture.no_ticket();
    assert!(fixture.collaboration.claim(&ticket).is_some());
    fixture.reply(&ticket, ok(&json!({})));
    assert_eq!(peer.receive()["ok"], true);
}

#[test]
fn cancellation_withdraws_only_unclaimed_requests() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();
    let mut peer = Peer::connect(&descriptor);

    peer.request("a", None, &describe());
    let unclaimed = fixture.ticket();
    peer.cancel("a");
    assert_refusal(&peer.receive(), Some("a"), "cancelled", "not_applied");
    assert!(fixture.collaboration.claim(&unclaimed).is_none());

    peer.request(
        "b",
        Some(OWNER),
        &json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": "x"}]}),
    );
    let claimed = fixture.ticket();
    assert!(fixture.collaboration.claim(&claimed).is_some());
    peer.cancel("b");
    peer.cancel("unknown");
    fixture.reply(&claimed, ok(&json!({"resources": []})));
    // A cancel is never acknowledged: the next frame is the real result.
    let frame = peer.receive();
    assert_eq!((&frame["id"], &frame["ok"]), (&json!("b"), &json!(true)));

    let mut malformed = Peer::connect(&descriptor);
    let mut frame = json!({"protocol": 1, "instance": malformed.instance, "token": malformed.token, "cancel": "a"});
    frame["operation"] = describe();
    malformed.send(&frame);
    assert_refusal(&malformed.receive(), None, "invalid_frame", "not_applied");
    assert!(malformed.closed());
}

#[test]
fn disconnect_withdraws_unclaimed_work_and_loses_claimed_replies() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();

    let mut first = Peer::connect(&descriptor);
    first.request("x", None, &describe());
    let unclaimed = fixture.ticket();
    drop(first);
    fixture.wait_for(|(clients, requests)| clients == 0 && requests == 0);
    assert!(fixture.collaboration.claim(&unclaimed).is_none());

    let mut second = Peer::connect(&descriptor);
    second.request(
        "y",
        Some(OWNER),
        &json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": "x"}]}),
    );
    let claimed = fixture.ticket();
    assert!(fixture.collaboration.claim(&claimed).is_some());
    drop(second);
    fixture.wait_for(|(clients, requests)| clients == 0 && requests == 1);
    // The controller's result is accepted, but its client can only observe `unknown`.
    fixture.reply(&claimed, ok(&json!({"resources": []})));
    fixture.wait_for(|(_, requests)| requests == 0);

    let mut third = Peer::connect(&descriptor);
    third.request("z", None, &describe());
    let ticket = fixture.ticket();
    assert!(fixture.collaboration.claim(&ticket).is_some());
    fixture.reply(&ticket, ok(&json!({})));
    assert_eq!(third.receive()["id"], "z");
}

#[test]
fn expiry_distinguishes_not_applied_from_unknown() {
    let fixture = Fixture::with(Settings {
        timeout: Duration::from_secs(1),
        write_timeout: WAIT,
    });
    let (_, descriptor) = fixture.ready();
    let mut peer = Peer::connect(&descriptor);

    peer.request("late", None, &describe());
    let unclaimed = fixture.ticket();
    assert_refusal(&peer.receive(), Some("late"), "timeout", "not_applied");
    assert!(fixture.collaboration.claim(&unclaimed).is_none());

    peer.request(
        "slow",
        Some(OWNER),
        &json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": "x"}]}),
    );
    let claimed = fixture.ticket();
    assert!(fixture.collaboration.claim(&claimed).is_some());
    assert_refusal(&peer.receive(), Some("slow"), "timeout", "unknown");
    assert_eq!(
        fixture
            .collaboration
            .reply(&claimed, ok(&json!({})))
            .unwrap_err()
            .category,
        "CollaborationTicket"
    );

    // A started frame must finish within the same bound.
    let mut stalled = Peer::connect(&descriptor);
    stalled.raw(&[0, 0, 0, 10, b'{']);
    assert!(stalled.closed());
    fixture.no_ticket();
}

#[test]
fn partial_progress_does_not_restart_the_frame_deadline() {
    let fixture = Fixture::with(Settings {
        timeout: Duration::from_secs(1),
        write_timeout: WAIT,
    });
    let (_, descriptor) = fixture.ready();
    let mut peer = Peer::connect(&descriptor);
    peer.raw(&[0]);
    std::thread::sleep(Duration::from_millis(700));
    peer.raw(&[0, 0, 10, b'{']);
    peer.stream
        .set_read_timeout(Some(Duration::from_millis(600)))
        .unwrap();
    assert!(
        peer.closed(),
        "Partial input must not extend the original deadline"
    );
    fixture.no_ticket();
}

#[test]
fn unavailable_controller_and_stale_host_owner_refuse_dispatch() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();
    let mut peer = Peer::connect(&descriptor);
    let edit = json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": "x"}]});

    fixture.collaboration.unavailable();
    peer.request("u1", None, &describe());
    assert_refusal(&peer.receive(), Some("u1"), "unavailable", "not_applied");
    fixture.no_ticket();
    fixture.collaboration.ready().unwrap();

    fixture.deliver.store(false, Ordering::SeqCst);
    peer.request("u2", None, &describe());
    assert_refusal(&peer.receive(), Some("u2"), "unavailable", "not_applied");
    fixture.deliver.store(true, Ordering::SeqCst);

    // Read/edit dispatch requires the host's current lease, not only the frontend's view.
    for host in [Some("authoring-1-2"), None] {
        fixture.set_host_owner(host);
        peer.request("s1", Some(OWNER), &edit);
        let ticket = fixture.ticket();
        assert!(fixture.collaboration.claim(&ticket).is_none());
        assert_refusal(&peer.receive(), Some("s1"), "stale_owner", "not_applied");
    }
    peer.request("s2", None, &describe());
    let ticket = fixture.ticket();
    assert!(fixture.collaboration.claim(&ticket).is_some());
    fixture.reply(
        &ticket,
        json!({"owner": null, "ok": true, "result": {"owner": null}}),
    );
    assert_eq!(peer.receive()["owner"], Value::Null);

    fixture.set_host_owner(Some(OWNER));
    peer.request("s3", Some(OWNER), &edit);
    let claimed = fixture.ticket();
    peer.request("s4", None, &describe());
    let unclaimed = fixture.ticket();
    assert!(fixture.collaboration.claim(&claimed).is_some());
    fixture.collaboration.unavailable();
    let mut outcomes = [peer.receive(), peer.receive()];
    outcomes.sort_by_key(|frame| frame["id"].as_str().unwrap().to_owned());
    assert_refusal(&outcomes[0], Some("s3"), "unavailable", "unknown");
    assert_refusal(&outcomes[1], Some("s4"), "unavailable", "not_applied");
    assert!(fixture.collaboration.claim(&unclaimed).is_none());
    assert!(
        fixture
            .collaboration
            .reply(&claimed, ok(&json!({})))
            .is_err()
    );
}

#[test]
fn create_requires_the_same_absence_or_owner_observed_at_request_time() {
    let fixture = Fixture::new();
    fixture.set_host_owner(None);
    let (_, descriptor) = fixture.ready();
    let mut peer = Peer::connect(&descriptor);
    let create = json!({"kind":"create", "workspace":"w1", "packageId":"example.new"});
    peer.request("absent", None, &create);
    let ticket = fixture.ticket();
    // A human acquired Edit between discovery and claim: no implicit replacement.
    fixture.set_host_owner(Some(OWNER));
    assert!(fixture.collaboration.claim(&ticket).is_none());
    assert_refusal(
        &peer.receive(),
        Some("absent"),
        "stale_owner",
        "not_applied",
    );

    fixture.set_host_owner(None);
    peer.request("still-absent", None, &create);
    let ticket = fixture.ticket();
    assert!(fixture.collaboration.claim(&ticket).is_some());
    fixture.reply(
        &ticket,
        json!({"owner":null,"ok":true,"result":{"connection":{"owner":OWNER,"package":PACKAGE}}}),
    );
    assert_eq!(peer.receive()["owner"], Value::Null);

    peer.request(
        "retired",
        Some(OWNER),
        &json!({"kind":"save", "resource":"file", "version":"v1"}),
    );
    let ticket = fixture.ticket();
    assert!(fixture.collaboration.claim(&ticket).is_none());
    assert_refusal(
        &peer.receive(),
        Some("retired"),
        "stale_owner",
        "not_applied",
    );
}

#[test]
fn malformed_controller_replies_report_unknown() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();
    let mut peer = Peer::connect(&descriptor);
    let malformed = [
        json!({"owner": OWNER, "ok": true}),
        json!({"owner": OWNER, "ok": true, "result": {}, "error": {"code": "x", "message": "y"}}),
        json!({"owner": OWNER, "ok": false, "error": {"code": "Bad-Code", "message": "y"}}),
        json!({"owner": OWNER, "ok": false, "error": {"code": "x", "message": "y", "path": "/"}}),
        json!({"owner": OWNER, "ok": true, "result": {}, "token": "secret"}),
    ];
    for (index, response) in malformed.into_iter().enumerate() {
        let id = format!("m{index}");
        peer.request(&id, None, &describe());
        let ticket = fixture.ticket();
        assert!(fixture.collaboration.claim(&ticket).is_some());
        assert_eq!(
            fixture
                .collaboration
                .reply(&ticket, response)
                .unwrap_err()
                .category,
            "CollaborationReply"
        );
        assert_refusal(&peer.receive(), Some(&id), "invalid_reply", "unknown");
    }
}

#[test]
fn client_and_pending_bounds_refuse_with_structured_frames() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();
    let mut peers: Vec<_> = (0..4)
        .map(|index| {
            let mut peer = Peer::connect(&descriptor);
            peer.request(&format!("c{index}"), None, &describe());
            peer
        })
        .collect();
    let tickets: Vec<_> = (0..4).map(|_| fixture.ticket()).collect();
    let mut fifth = Peer::connect(&descriptor);
    assert_refusal(&fifth.receive(), None, "busy", "not_applied");
    assert!(fifth.closed());
    for ticket in &tickets {
        assert!(fixture.collaboration.claim(ticket).is_some());
        fixture.reply(ticket, ok(&json!({})));
    }
    for peer in &mut peers {
        assert_eq!(peer.receive()["ok"], true);
    }
    peers.truncate(1);
    fixture.wait_for(|(clients, _)| clients == 1);

    let peer = &mut peers[0];
    for index in 0..16 {
        peer.request(&format!("p{index}"), None, &describe());
    }
    let tickets: Vec<_> = (0..16).map(|_| fixture.ticket()).collect();
    peer.request("p16", None, &describe());
    assert_refusal(&peer.receive(), Some("p16"), "busy", "not_applied");
    fixture.no_ticket();
    for ticket in &tickets {
        assert!(fixture.collaboration.claim(ticket).is_some());
        fixture.reply(ticket, ok(&json!({})));
    }
    for _ in 0..16 {
        assert_eq!(peer.receive()["ok"], true);
    }
}

#[test]
fn shutdown_drains_claimed_work_and_removes_only_owned_files() {
    let fixture = Fixture::new();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(fixture.collaboration_directory())
        .unwrap();
    let unrelated = fixture.collaboration_directory().join("other.json");
    fs::write(&unrelated, b"{}").unwrap();
    let (path, descriptor) = fixture.ready();
    let socket = PathBuf::from(descriptor["socket"].as_str().unwrap());
    let mut peer = Peer::connect(&descriptor);
    peer.request(
        "claimed",
        Some(OWNER),
        &json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": "x"}]}),
    );
    let claimed = fixture.ticket();
    assert!(fixture.collaboration.claim(&claimed).is_some());
    peer.request("waiting", None, &describe());
    let waiting = fixture.ticket();

    std::thread::scope(|scope| {
        let shutdown = scope.spawn(|| fixture.collaboration.shutdown(WAIT));
        assert_refusal(&peer.receive(), Some("waiting"), "shutdown", "not_applied");
        assert!(fixture.collaboration.claim(&waiting).is_none());
        let deadline = Instant::now() + WAIT;
        while path.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        // The draining controller still answers claimed work.
        fixture.reply(&claimed, ok(&json!({"resources": []})));
        shutdown.join().unwrap();
    });
    let frame = peer.receive();
    assert_eq!(
        (&frame["id"], &frame["ok"]),
        (&json!("claimed"), &json!(true))
    );
    assert!(peer.closed());

    assert!(!path.exists() && !socket.exists() && !socket.parent().unwrap().exists());
    assert_eq!(fs::read(&unrelated).unwrap(), b"{}");
    // A stale descriptor cannot reach a listener.
    assert!(UnixStream::connect(&socket).is_err());
    assert_eq!(
        fixture.collaboration.ready().unwrap_err().category,
        "Closing"
    );
}

#[test]
fn shutdown_without_drain_reports_claimed_work_unknown() {
    let fixture = Fixture::new();
    let (_, descriptor) = fixture.ready();
    let mut peer = Peer::connect(&descriptor);
    peer.request(
        "claimed",
        Some(OWNER),
        &json!({"kind": "edit", "edits": [{"resource": "f", "version": "v", "text": "x"}]}),
    );
    let ticket = fixture.ticket();
    assert!(fixture.collaboration.claim(&ticket).is_some());
    fixture.collaboration.shutdown(Duration::ZERO);
    assert_refusal(&peer.receive(), Some("claimed"), "shutdown", "unknown");
    assert!(peer.closed());
    assert!(
        fixture
            .collaboration
            .reply(&ticket, ok(&json!({})))
            .is_err()
    );
}

#[test]
fn shutdown_leaves_substituted_endpoint_files() {
    let fixture = Fixture::new();
    let (path, _) = fixture.ready();
    fs::rename(&path, fixture.base.join("moved.json")).unwrap();
    fs::write(&path, b"substitute").unwrap();
    fixture.collaboration.shutdown(Duration::ZERO);
    assert_eq!(fs::read(&path).unwrap(), b"substitute");
}

#[test]
fn endpoint_requires_an_existing_private_configuration_root() {
    let fixture = Fixture::new();
    let missing = fixture.base.join("missing");
    let absent = Collaboration::with_settings(
        Some(missing.clone()),
        Arc::new(|_: &str| true),
        Arc::new(|| None),
        Settings::default(),
    );
    assert_eq!(
        absent.ready().unwrap_err().category,
        "CollaborationUnavailable"
    );
    assert!(!missing.exists());
    let unconfigured = Collaboration::new(None, Arc::new(|_: &str| true), Arc::new(|| None));
    assert_eq!(
        unconfigured.ready().unwrap_err().category,
        "CollaborationUnavailable"
    );

    let directory = fixture.collaboration_directory();
    let elsewhere = fixture.base.join("elsewhere");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&elsewhere)
        .unwrap();
    std::os::unix::fs::symlink(&elsewhere, &directory).unwrap();
    assert_eq!(
        fixture.collaboration.ready().unwrap_err().category,
        "CollaborationUnavailable"
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    fs::remove_file(&directory).unwrap();

    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        fixture.collaboration.ready().unwrap_err().category,
        "CollaborationUnavailable"
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();

    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o770)).unwrap();
    assert_eq!(
        fixture.collaboration.ready().unwrap_err().category,
        "CollaborationUnavailable"
    );
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();

    // A refused start leaves no endpoint and can be retried.
    fixture.ready();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
}
