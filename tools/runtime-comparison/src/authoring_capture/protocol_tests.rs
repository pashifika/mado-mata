use super::protocol::{self, CaptureDescriptor, Event};
use super::*;
use std::sync::mpsc;

pub(super) fn identity() -> CaptureIdentity {
    CaptureIdentity {
        owner: "protocol-owner".into(),
        package_revision: "package-revision".into(),
        binding_revision: "binding-revision".into(),
        selection_generation: 1,
        request_id: "capture-request".into(),
    }
}
pub(super) fn candidate() -> Candidate {
    Candidate {
        key: "candidate-1".into(),
        label: "Controlled protocol window".into(),
        process_id: 1,
        process_lifetime: 1,
        executable_path: std::env::current_exe().unwrap(),
        application_bundle_path: None,
        native_window: NativeWindow::Macos(1),
        area: CaptureArea::MacosWindow,
        geometry: CaptureGeometry {
            x: -2.0,
            y: 3.0,
            width: 2.0,
            height: 2.0,
            pixels_per_unit_x: 1.0,
            pixels_per_unit_y: 1.0,
            pixel_width: 2,
            pixel_height: 2,
            unit: CoordinateUnit::MacosGlobalPoints,
        },
    }
}
pub(super) fn transfer(identity: CaptureIdentity, geometry: CaptureGeometry) -> CaptureTransfer {
    let image = DecodedImage::from_rgba(2, 2, vec![0x39; 16]).unwrap();
    let png = images::encode_input(&image).unwrap();
    let header = Header {
        identity,
        geometry,
        acquired_at_ms: 1,
        captured_monotonic_us: 2,
        frame_identity: "controlled-private-transfer".into(),
        png_bytes: png.as_bytes().len(),
    };
    CaptureTransfer {
        header: crate::model::encode_bounded(&header, HEADER_BYTES).unwrap(),
        png,
    }
}
fn discovered() -> Event {
    Event::Discovered {
        identity: identity(),
        engine_revision: crate::model::ENGINE_REVISION.into(),
        candidates: vec![candidate()],
    }
}
fn parse(bytes: &[u8]) -> Result<(), Fault> {
    let (send, _receive) = mpsc::sync_channel(4);
    protocol::read_events(&mut &*bytes, &send)
}

#[test]
fn oversized_metadata_length_is_refused_before_reading_its_payload() {
    struct PrefixOnly {
        prefix: std::io::Cursor<[u8; 4]>,
    }
    impl Read for PrefixOnly {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            assert!(
                self.prefix.position() < 4,
                "untrusted payload must not be read or allocated"
            );
            self.prefix.read(bytes)
        }
    }
    let mut input = PrefixOnly {
        prefix: std::io::Cursor::new(u32::MAX.to_le_bytes()),
    };
    let error = protocol::read_message::<DiscoveryRequest>(&mut input, protocol::COMMAND_BYTES)
        .err()
        .unwrap();
    assert_eq!(error.category, "CaptureTransport");
}

#[test]
fn duplicate_discovery_unsolicited_selection_and_bad_revision_are_refused() {
    let mut duplicate = Vec::new();
    protocol::write_event(&mut duplicate, &discovered()).unwrap();
    protocol::write_event(&mut duplicate, &discovered()).unwrap();
    assert_eq!(parse(&duplicate).unwrap_err().category, "CaptureTransport");
    let mut unsolicited = Vec::new();
    protocol::write_event(
        &mut unsolicited,
        &Event::Selected {
            candidate: candidate(),
        },
    )
    .unwrap();
    assert_eq!(
        parse(&unsolicited).unwrap_err().category,
        "CaptureTransport"
    );
    let mut revision = Vec::new();
    protocol::write_event(
        &mut revision,
        &Event::Discovered {
            identity: identity(),
            engine_revision: "foreign-pin".into(),
            candidates: vec![candidate()],
        },
    )
    .unwrap();
    assert_eq!(parse(&revision).unwrap_err().category, "CaptureTransport");
}

#[test]
fn incomplete_cleanup_or_failure_cannot_announce_a_png() {
    for (clean, primary) in [
        (false, None),
        (true, Some(Fault::new("Cancelled", "cancelled"))),
    ] {
        let mut bytes = Vec::new();
        protocol::write_event(&mut bytes, &discovered()).unwrap();
        protocol::write_event(
            &mut bytes,
            &Event::Terminal {
                clean,
                primary,
                capture: Some(CaptureDescriptor {
                    identity: identity(),
                    geometry: candidate().geometry,
                }),
            },
        )
        .unwrap();
        assert_eq!(parse(&bytes).unwrap_err().category, "CaptureTransport");
    }
}

#[test]
fn png_transfer_requires_terminal_eof_and_rejects_a_second_frame() {
    let mut bytes = Vec::new();
    protocol::write_event(&mut bytes, &discovered()).unwrap();
    protocol::write_event(
        &mut bytes,
        &Event::Terminal {
            clean: true,
            primary: None,
            capture: Some(CaptureDescriptor {
                identity: identity(),
                geometry: candidate().geometry,
            }),
        },
    )
    .unwrap();
    transfer(identity(), candidate().geometry)
        .write_private(&mut bytes)
        .unwrap();
    parse(&bytes).unwrap();
    let missing_byte = &bytes[..bytes.len() - 1];
    assert_eq!(
        parse(missing_byte).unwrap_err().category,
        "CaptureTransport"
    );
    bytes.push(0);
    assert_eq!(parse(&bytes).unwrap_err().category, "CaptureTransport");
}

#[test]
fn candidate_overflow_duplicate_keys_and_incompatible_coordinates_never_form_selection() {
    let candidates: Vec<_> = (0..CANDIDATE_LIMIT)
        .map(|index| {
            let mut candidate = candidate();
            candidate.key = format!("candidate-{index}");
            candidate.process_id = index as u32 + 1;
            candidate
        })
        .collect();
    protocol::validate_candidates(&candidates).unwrap();
    let mut overflow = candidates.clone();
    overflow.push(candidate());
    assert_eq!(
        protocol::validate_candidates(&overflow)
            .unwrap_err()
            .category,
        "NativeCandidateLimit"
    );
    let mut duplicate = candidates;
    duplicate[1].key = duplicate[0].key.clone();
    assert_eq!(
        protocol::validate_candidates(&duplicate)
            .unwrap_err()
            .category,
        "CaptureTransport"
    );
    let mut incompatible = candidate();
    incompatible.geometry.unit = CoordinateUnit::WindowsPhysicalPixels;
    assert_eq!(
        protocol::validate_candidates(&[incompatible])
            .unwrap_err()
            .category,
        "CaptureTransport"
    );
}

#[test]
fn a_full_event_queue_refuses_output_without_blocking_reaping() {
    let mut bytes = Vec::new();
    protocol::write_event(&mut bytes, &discovered()).unwrap();
    protocol::write_event(
        &mut bytes,
        &Event::Selected {
            candidate: candidate(),
        },
    )
    .unwrap();
    let (events, receive) = mpsc::sync_channel(1);
    let (done, completion) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let result = protocol::read_events(&mut bytes.as_slice(), &events);
        let _ = done.send(result);
    });
    let result = completion.recv_timeout(Duration::from_secs(1));
    // Release the receiver even on regression so a blocking sender can settle.
    drop(receive);
    reader.join().unwrap();
    assert_eq!(
        result
            .expect("protocol reader must not block on event delivery")
            .unwrap_err()
            .category,
        "CaptureTransport"
    );
}
