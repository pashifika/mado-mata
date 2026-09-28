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
        executable_or_bundle: std::env::current_exe().unwrap(),
        exact_window_title: None,
        verified_processes: Vec::new(),
        timeout: DISCOVERY_LIMIT,
    }
}

fn fixture(mode: &str) -> AuthoringCaptureWorker {
    fixture_with_idle(mode, SELECTION_LIMIT)
}

fn fixture_with_idle(mode: &str, selection_limit: Duration) -> AuthoringCaptureWorker {
    let mut child = OwnedChild::spawn(
        ProcessCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "authoring_capture::worker::tests::owned_protocol_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(FIXTURE, mode),
        ChildStdio::Protocol,
        Environment::Inherited,
    )
    .unwrap();
    // The test harness preamble is not part of the private worker protocol.
    // Consume only through the fixture marker, leaving the real stdout pipe owned.
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
    AuthoringCaptureWorker::from_child(child, request(), Instant::now(), selection_limit).unwrap()
}

/// A real owned child speaking only controlled transport data. This test never
/// constructs an SDK/provider, acquires native pixels, or opens a target process.
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
                capture: None,
            },
        )
        .unwrap();
        std::process::exit(0);
    }
    let mut input = std::io::stdin();
    let request: DiscoveryRequest =
        protocol::read_message(&mut input, protocol::COMMAND_BYTES).unwrap();
    protocol::write_event(
        &mut output,
        &Event::Discovered {
            identity: request.identity,
            engine_revision: crate::model::ENGINE_REVISION.into(),
            candidates: vec![candidate()],
        },
    )
    .unwrap();
    loop {
        match protocol::read_message::<Command>(&mut input, protocol::COMMAND_BYTES) {
            Ok(Command::Select { .. }) => {
                let mut candidate = candidate();
                if mode == "moved" {
                    candidate.geometry.x += 1.0;
                }
                protocol::write_event(&mut output, &Event::Selected { candidate }).unwrap();
            }
            Ok(Command::Capture {
                identity,
                candidate,
                ..
            }) => {
                protocol::write_event(
                    &mut output,
                    &Event::Terminal {
                        primary: None,
                        clean: true,
                        capture: Some(CaptureDescriptor {
                            identity: identity.clone(),
                            geometry: candidate.geometry,
                        }),
                    },
                )
                .unwrap();
                transfer(identity, candidate.geometry)
                    .write_private(&mut output)
                    .unwrap();
                if mode == "hold" {
                    loop {
                        thread::park_timeout(Duration::from_secs(1));
                    }
                }
                std::process::exit(0);
            }
            _ if mode == "hold" => loop {
                thread::park_timeout(Duration::from_secs(1));
            },
            _ => {
                protocol::write_event(
                    &mut output,
                    &Event::Terminal {
                        primary: Some(protocol::cancelled()),
                        clean: true,
                        capture: None,
                    },
                )
                .unwrap();
                std::process::exit(0);
            }
        }
    }
}

#[test]
fn clean_owned_reap_is_required_before_private_pixels_become_a_capture() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("clean");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    let cancel = worker.cancel_handle();
    let id = identity();
    let result = worker.capture(
        id.clone(),
        Arc::new(CaptureAuthority::new(id).unwrap()),
        ACQUISITION_LIMIT,
    );
    assert!(result.primary.is_none());
    assert!(result.settlement.child_reaped && result.settlement.clean && !result.settlement.forced);
    assert!(cancel.is_finished());
    assert_eq!(result.capture.unwrap().image.rgba, vec![0x39; 16]);
}

#[test]
fn cancel_does_not_wait_for_command_owner_and_forced_reap_discards_pixels() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("hold");
    let candidates = worker.discover().unwrap();
    worker.select(&candidates[0].key).unwrap();
    let cancel = worker.cancel_handle();
    let id = identity();
    let authority = Arc::new(CaptureAuthority::new(id.clone()).unwrap());
    let capture_authority = authority.clone();
    let capture = thread::spawn(move || worker.capture(id, capture_authority, ACQUISITION_LIMIT));
    authority.cancel();
    cancel.cancel();
    let result = capture.join().unwrap();
    assert!(result.capture.is_none());
    assert!(result.settlement.child_reaped && result.settlement.forced && !result.settlement.clean);
    assert!(cancel.is_finished());
}

#[test]
fn changed_selection_metadata_is_not_committed_and_idle_cancel_reaps_cleanly() {
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
}

#[test]
fn acquisition_deadline_does_not_accept_a_png_from_an_unreaped_worker() {
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
    assert!(result.capture.is_none());
    assert!(result.settlement.child_reaped && result.settlement.forced && !result.settlement.clean);
}

#[test]
fn selection_revalidation_never_renews_the_idle_worker_lifetime() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture_with_idle("clean", Duration::from_secs(1));
    let candidates = worker.discover().unwrap();
    let started = Instant::now();
    loop {
        if worker.select(&candidates[0].key).is_err() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "metadata selection must not renew the idle deadline"
        );
    }
    let settlement = worker.settle();
    assert!(settlement.child_reaped && settlement.clean && !settlement.forced);
    assert_eq!(
        settlement.primary.unwrap().category,
        "NativeSelectionExpired"
    );
}

#[test]
fn dropping_the_metadata_owner_closes_control_and_reaps_without_capture() {
    let Ok(_serial) = SERIAL.lock() else {
        panic!("a previous worker lifecycle test poisoned serialization");
    };
    let mut worker = fixture("clean");
    worker.discover().unwrap();
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
