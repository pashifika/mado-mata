use super::super::protocol::{CaptureDescriptor, Event};
use super::super::protocol_tests::{candidate, identity, transfer};
use super::*;
use std::process::Command as ProcessCommand;

const FIXTURE: &str = "MADO_AUTHORING_PROTOCOL_FIXTURE";
const READY: &[u8] = b"AUTHORING_PROTOCOL_READY\n";
static SERIAL: Mutex<()> = Mutex::new(());

fn request() -> DiscoveryRequest {
    DiscoveryRequest {
        identity: identity(),
        executable_or_bundle: None,
        exact_window_title: None,
        verified_processes: Vec::new(),
        timeout: DISCOVERY_LIMIT,
    }
}

fn fixture_child(mode: &str, marker: Option<&Path>) -> OwnedChild {
    let mut command = ProcessCommand::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "authoring_capture::worker::tests::owned_protocol_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FIXTURE, mode);
    if let Some(marker) = marker {
        command.env("MADO_AUTHORING_PROTOCOL_MARKER", marker);
    }
    let mut child =
        OwnedChild::spawn(&mut command, ChildStdio::Protocol, Environment::Inherited).unwrap();
    // Consume the Rust harness preamble, not any private protocol bytes.
    let mut output = child.stdout.take().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        while bytes.len() < 4096 {
            let mut byte = [0];
            if output.read(&mut byte).unwrap_or(0) == 0 {
                break;
            }
            bytes.push(byte[0]);
            if bytes.ends_with(READY) {
                let _ = send.send(Some(output));
                return;
            }
        }
        let _ = send.send(None);
    });
    let ready = receive.recv_timeout(Duration::from_secs(10));
    if ready.is_err() {
        let _ = child.kill();
    }
    reader.join().unwrap();
    child.stdout = Some(
        ready
            .unwrap()
            .expect("owned fixture must reach protocol entry"),
    );
    child
}

fn fixture(mode: &str) -> AuthoringCaptureWorker {
    AuthoringCaptureWorker::from_child(fixture_child(mode, None), request(), Instant::now())
        .unwrap()
}

struct Marker(std::path::PathBuf);
impl Marker {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        Self(std::env::temp_dir().join(format!("mado-capture-{}-{nonce}", std::process::id(),)))
    }
    fn wait(&self) {
        let until = Instant::now() + Duration::from_secs(5);
        while !self.0.exists() {
            assert!(Instant::now() < until, "child must admit the later capture");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Marker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn fixture_candidates() -> Vec<Candidate> {
    let first = candidate();
    let mut second = candidate();
    second.key = "candidate-2".into();
    second.native_window = NativeWindow::Macos(2);
    vec![first, second]
}

fn finish_fixture(output: &mut impl Write) -> ! {
    protocol::write_event(
        output,
        &Event::Terminal {
            primary: Some(protocol::cancelled()),
            clean: true,
        },
    )
    .unwrap();
    std::process::exit(0);
}

/// Actual owned child/process transport, with controlled pixels and no native API.
#[test]
fn owned_protocol_fixture() {
    let Ok(mode) = std::env::var(FIXTURE) else {
        return;
    };
    let mut output = std::io::stdout();
    output.write_all(READY).unwrap();
    output.flush().unwrap();
    if mode == "refuse" {
        protocol::write_event(
            &mut output,
            &Event::Terminal {
                primary: Some(Fault::new("EngineUnavailable", "engine feature is absent")),
                clean: true,
            },
        )
        .unwrap();
        std::process::exit(0);
    }
    let mut input = std::io::stdin();
    let request: DiscoveryRequest =
        protocol::read_message(&mut input, protocol::COMMAND_BYTES).unwrap();
    let mut bound = request.identity.clone();
    protocol::write_event(
        &mut output,
        &Event::Discovered {
            identity: request.identity,
            engine_revision: crate::model::ENGINE_REVISION.into(),
            candidates: fixture_candidates(),
        },
    )
    .unwrap();
    let mut captures = 0;
    let mut first_frame = None;
    loop {
        match protocol::read_message::<Command>(&mut input, protocol::COMMAND_BYTES) {
            Ok(Command::Select { key }) => {
                let mut candidate = fixture_candidates()
                    .into_iter()
                    .find(|item| item.key == key)
                    .unwrap();
                if mode == "moved" {
                    candidate.geometry.x += 1.0;
                }
                protocol::write_event(
                    &mut output,
                    &Event::Selected {
                        identity: bound.clone(),
                        candidate,
                    },
                )
                .unwrap();
            }
            Ok(Command::RebaseSource { identity }) => {
                assert!(protocol::same_source_rebase(
                    &bound,
                    &identity,
                    captures == 0
                ));
                bound = identity;
                if mode == "foreign-rebase" {
                    bound.binding_revision.push_str("-foreign");
                }
                protocol::write_event(
                    &mut output,
                    &Event::SourceRebased {
                        identity: bound.clone(),
                    },
                )
                .unwrap();
            }
            Ok(Command::Capture {
                mut identity,
                candidate,
                ..
            }) => {
                captures += 1;
                if mode == "hold"
                    || (captures == 2 && (mode == "hold-later" || mode == "eof-later"))
                {
                    if let Ok(marker) = std::env::var("MADO_AUTHORING_PROTOCOL_MARKER") {
                        std::fs::write(marker, b"capture admitted").unwrap();
                    }
                    if mode == "eof-later" {
                        let _ =
                            protocol::read_message::<Command>(&mut input, protocol::COMMAND_BYTES);
                        finish_fixture(&mut output);
                    }
                    loop {
                        thread::park_timeout(Duration::from_secs(1));
                    }
                }
                if mode == "stale" && captures == 2 {
                    identity.request_id.push_str("-foreign");
                }
                protocol::write_event(
                    &mut output,
                    &Event::Frame {
                        capture: CaptureDescriptor {
                            identity: identity.clone(),
                            geometry: candidate.geometry,
                        },
                    },
                )
                .unwrap();
                let mut transfer = transfer(identity.clone(), candidate.geometry);
                let mut header: Header = serde_json::from_slice(&transfer.header).unwrap();
                if captures == 1 {
                    first_frame = Some(header.frame_identity.clone());
                } else if mode == "duplicate-frame" {
                    header.frame_identity = first_frame.clone().unwrap();
                    transfer.header = crate::model::encode_bounded(&header, HEADER_BYTES).unwrap();
                }
                transfer.write_private(&mut output).unwrap();
                drop(transfer);
                protocol::write_event(
                    &mut output,
                    &Event::Captured {
                        identity,
                        session_clean: mode != "unclean",
                    },
                )
                .unwrap();
            }
            _ => finish_fixture(&mut output),
        }
    }
}

fn capture(worker: &mut AuthoringCaptureWorker, request_id: &str) -> WorkerCaptureResult {
    let mut id = worker.identity.clone();
    id.request_id = request_id.into();
    worker.capture(
        id.clone(),
        Arc::new(CaptureAuthority::new(id).unwrap()),
        ACQUISITION_LIMIT,
    )
}

#[test]
fn two_captures_retain_the_same_child_until_explicit_release() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("clean");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[1].key).unwrap();
    worker
        .rebase_source(&identity().package_revision, "fresh-host-proof")
        .unwrap();
    let cancel = worker.cancel_handle();
    let first = capture(&mut worker, "first");
    assert!(first.primary.is_none() && first.session_clean && first.settlement.is_none());
    let first = first.capture.unwrap();
    worker
        .rebase_source("saved-package-revision", "fresh-host-proof")
        .unwrap();
    worker.select(&candidates[1].key).unwrap();
    let second = capture(&mut worker, "second");
    assert!(second.primary.is_none() && second.session_clean && second.settlement.is_none());
    let second = second.capture.unwrap();
    assert_ne!(first.identity.request_id, second.identity.request_id);
    assert_ne!(first.frame_identity, second.frame_identity);
    assert_eq!(second.identity.binding_revision, "fresh-host-proof");
    assert_eq!(first.identity.package_revision, identity().package_revision);
    assert_eq!(second.identity.package_revision, "saved-package-revision");
    assert_eq!(first.image.rgba, vec![0x39; 16]);
    assert_eq!(second.image.rgba, vec![0x39; 16]);
    assert!(!cancel.is_finished() && cancel.try_settlement().is_none());
    let settlement = worker.settle();
    assert!(settlement.child_reaped && settlement.clean && !settlement.forced);
    assert!(cancel.is_finished());
}

#[test]
fn cancellation_during_later_capture_discards_pixels_and_contains_only_owned_child() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let marker = Marker::new();
    let mut worker = AuthoringCaptureWorker::from_child(
        fixture_child("hold-later", Some(&marker.0)),
        request(),
        Instant::now(),
    )
    .unwrap();
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    assert!(capture(&mut worker, "first").capture.is_some());
    let cancel = worker.cancel_handle();
    let mut id = worker.identity.clone();
    id.request_id = "second".into();
    let authority = Arc::new(CaptureAuthority::new(id.clone()).unwrap());
    let active_authority = authority.clone();
    let active = thread::spawn(move || worker.capture(id, active_authority, ACQUISITION_LIMIT));
    marker.wait();
    authority.cancel();
    cancel.cancel();
    let result = active.join().unwrap();
    assert!(result.capture.is_none() && !result.session_clean);
    let settlement = result.settlement.unwrap();
    assert!(settlement.child_reaped && settlement.forced && !settlement.clean);
    assert!(cancel.is_finished());
}

#[test]
fn changed_selection_metadata_and_attempted_switch_never_capture_a_replacement() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("moved");
    let candidates = worker.discover().unwrap();
    assert_eq!(
        worker.select(&candidates[0].key).err().unwrap().category,
        "StaleCapture"
    );
    let result = worker.settle();
    assert!(result.child_reaped && result.clean && !result.forced);
    let mut worker = fixture("clean");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    assert_eq!(
        worker.select(&candidates[1].key).err().unwrap().category,
        "StaleCapture"
    );
    assert_eq!(
        capture(&mut worker, "original")
            .capture
            .unwrap()
            .identity
            .binding_revision,
        identity().binding_revision
    );
    assert!(worker.settle().clean);
}

#[test]
fn acquisition_deadline_contains_an_unresponsive_retained_worker() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("hold");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    let id = identity();
    let result = worker.capture(
        id.clone(),
        Arc::new(CaptureAuthority::new(id).unwrap()),
        Duration::from_millis(100),
    );
    assert_eq!(result.primary.unwrap().category, "DeadlineExceeded");
    assert!(result.capture.is_none() && !result.session_clean);
    let settlement = result.settlement.unwrap();
    assert!(settlement.child_reaped && settlement.forced && !settlement.clean);
}

#[test]
fn stale_request_duplicate_frame_and_unclean_receipt_are_never_published() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    for mode in ["stale", "duplicate-frame", "unclean", "duplicate-request"] {
        let mut worker = fixture(mode);
        let candidates = worker.discover().unwrap();
        worker.select(&candidates[0].key).unwrap();
        if mode != "unclean" {
            assert!(capture(&mut worker, "first").capture.is_some());
        }
        let result = capture(
            &mut worker,
            if mode == "duplicate-request" {
                "first"
            } else {
                "second"
            },
        );
        assert!(result.capture.is_none() && !result.session_clean && result.primary.is_some());
        assert!(result.settlement.unwrap().child_reaped);
    }
}

#[test]
fn dropping_the_retained_owner_reaps_after_a_completed_frame() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("clean");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    assert!(capture(&mut worker, "first").capture.is_some());
    let cancel = worker.cancel_handle();
    drop(worker);
    let until = Instant::now() + Duration::from_secs(5);
    while !cancel.is_finished() {
        assert!(
            Instant::now() < until,
            "dropped owner must retain supervision until reap"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let settlement = cancel.try_settlement().unwrap();
    assert!(settlement.child_reaped && settlement.clean && !settlement.forced);
}

#[test]
fn parent_control_eof_during_later_capture_settles_without_another_frame() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let marker = Marker::new();
    let mut child = fixture_child("eof-later", Some(&marker.0));
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    protocol::write_message(&mut input, &request(), protocol::COMMAND_BYTES).unwrap();
    assert!(matches!(
        protocol::read_message::<Event>(&mut output, 256 * 1024).unwrap(),
        Event::Discovered { .. }
    ));
    protocol::write_message(
        &mut input,
        &Command::Select {
            key: candidate().key,
        },
        protocol::COMMAND_BYTES,
    )
    .unwrap();
    assert!(matches!(
        protocol::read_message::<Event>(&mut output, 256 * 1024).unwrap(),
        Event::Selected { .. }
    ));
    protocol::write_message(
        &mut input,
        &Command::Capture {
            identity: identity(),
            candidate: candidate(),
            timeout: ACQUISITION_LIMIT,
        },
        protocol::COMMAND_BYTES,
    )
    .unwrap();
    assert!(matches!(
        protocol::read_message::<Event>(&mut output, 256 * 1024).unwrap(),
        Event::Frame { .. }
    ));
    let pending =
        PendingCapture::read_private(&mut output, &identity(), &candidate().geometry).unwrap();
    assert!(matches!(
        protocol::read_message::<Event>(&mut output, 256 * 1024).unwrap(),
        Event::Captured {
            session_clean: true,
            ..
        }
    ));
    let authority = CaptureAuthority::new(identity()).unwrap();
    assert_eq!(
        pending
            .accept_after_session_close(&authority, &identity())
            .unwrap()
            .image
            .rgba,
        vec![0x39; 16]
    );
    let mut later = identity();
    later.request_id = "later".into();
    protocol::write_message(
        &mut input,
        &Command::Capture {
            identity: later,
            candidate: candidate(),
            timeout: ACQUISITION_LIMIT,
        },
        protocol::COMMAND_BYTES,
    )
    .unwrap();
    marker.wait();
    drop(input);
    let until = Instant::now() + Duration::from_secs(5);
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        assert!(Instant::now() < until, "parent EOF must settle owned child");
        thread::sleep(Duration::from_millis(2));
    };
    assert!(exit.success());
    assert!(matches!(
        protocol::read_message::<Event>(&mut output, 256 * 1024).unwrap(),
        Event::Terminal { clean: true, .. }
    ));
    assert_eq!(output.read(&mut [0]).unwrap(), 0);
}

#[test]
fn early_engine_refusal_is_not_replaced_by_a_broken_input_pipe() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("refuse");
    assert_eq!(
        worker.discover().err().unwrap().category,
        "EngineUnavailable"
    );
    let settlement = worker.settle();
    assert!(settlement.child_reaped && settlement.clean && !settlement.forced);
}

#[test]
fn package_rebase_does_not_allow_a_stale_capture_or_reset_replay_history() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    for stale_package in [true, false] {
        let mut worker = fixture("clean");
        assert_eq!(
            worker
                .rebase_source("saved", &identity().binding_revision)
                .err()
                .unwrap()
                .category,
            "StaleCapture"
        );
        let candidates = worker.discover().unwrap();
        worker.select(&candidates[0].key).unwrap();
        assert!(capture(&mut worker, "first").capture.is_some());
        worker
            .rebase_source("saved", &identity().binding_revision)
            .unwrap();
        let mut stale = if stale_package {
            identity()
        } else {
            worker.identity.clone()
        };
        stale.request_id = if stale_package { "second" } else { "first" }.into();
        let result = worker.capture(
            stale.clone(),
            Arc::new(CaptureAuthority::new(stale).unwrap()),
            ACQUISITION_LIMIT,
        );
        assert_eq!(result.primary.unwrap().category, "StaleCapture");
        assert!(result.capture.is_none());
        assert!(result.settlement.unwrap().child_reaped);
    }
}

#[test]
fn exhausted_capture_history_refuses_without_publishing_or_replacing_worker() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("clean");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    for index in 0..protocol::CAPTURE_REQUEST_LIMIT {
        protocol::admit_request(&mut worker.request_ids, &format!("previous-{index}")).unwrap();
    }
    worker
        .rebase_source("saved", &identity().binding_revision)
        .unwrap();
    let result = capture(&mut worker, "over-limit");
    assert_eq!(result.primary.unwrap().category, "NativeCaptureLimit");
    assert!(result.capture.is_none() && !result.session_clean);
    let settlement = result.settlement.unwrap();
    assert!(settlement.child_reaped && settlement.clean && !settlement.forced);
}

#[test]
fn committed_binding_rebase_refuses_old_revision_and_post_capture_rebinding() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("clean");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    let previous = worker.identity.clone();
    worker
        .rebase_source(&previous.package_revision, "committed-binding")
        .unwrap();
    let first = capture(&mut worker, "committed-first");
    assert!(first.primary.is_none() && first.session_clean);
    let first = first.capture.unwrap();
    assert_eq!(first.identity.binding_revision, "committed-binding");
    assert_eq!(first.geometry, candidates[0].geometry);
    assert_eq!(first.image.rgba, vec![0x39; 16]);
    assert_eq!(
        worker
            .rebase_source(&previous.package_revision, "another-binding")
            .unwrap_err()
            .category,
        "StaleCapture",
    );
    let mut stale = previous;
    stale.request_id = "obsolete-binding".into();
    let result = worker.capture(
        stale.clone(),
        Arc::new(CaptureAuthority::new(stale).unwrap()),
        ACQUISITION_LIMIT,
    );
    assert!(result.capture.is_none());
    assert_eq!(result.primary.unwrap().category, "StaleCapture");
    let settlement = result.settlement.unwrap();
    assert!(settlement.child_reaped && settlement.clean && !settlement.forced);
}

#[test]
fn source_rebase_acknowledgement_must_match_the_committed_revision() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("foreign-rebase");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    assert_eq!(
        worker
            .rebase_source(&identity().package_revision, "committed-binding")
            .unwrap_err()
            .category,
        "StaleCapture",
    );
    let settlement = worker.settle();
    assert!(settlement.child_reaped && settlement.clean && !settlement.forced);
}
