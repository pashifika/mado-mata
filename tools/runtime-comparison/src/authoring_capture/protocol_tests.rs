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
        frame_identity: format!("controlled-{}", identity.request_id),
        identity,
        geometry,
        acquired_at_ms: 1,
        captured_monotonic_us: 2,
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
    let (send, _receive) = mpsc::sync_channel(8);
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
            identity: identity(),
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

fn selected() -> Event {
    Event::Selected {
        identity: identity(),
        candidate: candidate(),
    }
}

fn completed(bytes: &mut Vec<u8>, id: CaptureIdentity) {
    protocol::write_event(
        bytes,
        &Event::Frame {
            capture: CaptureDescriptor {
                identity: id.clone(),
                geometry: candidate().geometry,
            },
        },
    )
    .unwrap();
    transfer(id.clone(), candidate().geometry)
        .write_private(bytes)
        .unwrap();
    protocol::write_event(
        bytes,
        &Event::Captured {
            identity: id,
            session_clean: true,
        },
    )
    .unwrap();
}

fn terminal(bytes: &mut Vec<u8>) {
    protocol::write_event(
        bytes,
        &Event::Terminal {
            primary: None,
            clean: true,
        },
    )
    .unwrap();
}

#[test]
fn pixels_without_matching_clean_receipt_never_reach_the_supervisor() {
    let mut stale = identity();
    stale.request_id.push_str("-stale");
    for receipt in [
        None,
        Some(Event::Captured {
            identity: identity(),
            session_clean: false,
        }),
        Some(Event::Captured {
            identity: stale,
            session_clean: true,
        }),
        Some(Event::Terminal {
            primary: Some(protocol::cancelled()),
            clean: true,
        }),
    ] {
        let mut bytes = Vec::new();
        protocol::write_event(&mut bytes, &discovered()).unwrap();
        protocol::write_event(&mut bytes, &selected()).unwrap();
        protocol::write_event(
            &mut bytes,
            &Event::Frame {
                capture: CaptureDescriptor {
                    identity: identity(),
                    geometry: candidate().geometry,
                },
            },
        )
        .unwrap();
        transfer(identity(), candidate().geometry)
            .write_private(&mut bytes)
            .unwrap();
        if let Some(receipt) = receipt {
            protocol::write_event(&mut bytes, &receipt).unwrap();
        }
        let (send, receive) = mpsc::sync_channel(8);
        assert_eq!(
            protocol::read_events(&mut bytes.as_slice(), &send)
                .unwrap_err()
                .category,
            "CaptureTransport",
        );
        assert!(
            !receive
                .try_iter()
                .any(|event| matches!(event, Ok(protocol::Received::Captured(_))))
        );
    }
}

#[test]
fn multiple_frames_are_length_delimited_and_only_terminal_requires_eof() {
    let mut bytes = Vec::new();
    protocol::write_event(&mut bytes, &discovered()).unwrap();
    protocol::write_event(&mut bytes, &selected()).unwrap();
    completed(&mut bytes, identity());
    let mut second = identity();
    second.request_id.push_str("-second");
    completed(&mut bytes, second);
    terminal(&mut bytes);
    parse(&bytes).unwrap();
    assert_eq!(
        parse(&bytes[..bytes.len() - 1]).unwrap_err().category,
        "CaptureTransport"
    );
    bytes.push(0);
    assert_eq!(parse(&bytes).unwrap_err().category, "CaptureTransport");
}

#[test]
fn stale_binding_and_unsolicited_frames_are_refused() {
    for select in [false, true] {
        let mut bytes = Vec::new();
        protocol::write_event(&mut bytes, &discovered()).unwrap();
        if select {
            protocol::write_event(&mut bytes, &selected()).unwrap();
        }
        let mut id = identity();
        id.binding_revision.push_str("-stale");
        completed(&mut bytes, id);
        assert_eq!(parse(&bytes).unwrap_err().category, "CaptureTransport");
    }
}

#[test]
fn package_rebase_preserves_selection_scope_and_refuses_the_old_package() {
    for field in 0..5 {
        let mut bytes = Vec::new();
        protocol::write_event(&mut bytes, &discovered()).unwrap();
        protocol::write_event(&mut bytes, &selected()).unwrap();
        let mut rebased = identity();
        rebased.package_revision = "saved-package".into();
        match field {
            0 => rebased.owner.push_str("-foreign"),
            1 => rebased.binding_revision.push_str("-foreign"),
            2 => rebased.selection_generation += 1,
            3 => rebased.request_id.push_str("-foreign"),
            _ => {}
        }
        protocol::write_event(
            &mut bytes,
            &Event::PackageRebased {
                identity: rebased.clone(),
            },
        )
        .unwrap();
        if field < 4 {
            assert_eq!(parse(&bytes).unwrap_err().category, "CaptureTransport");
        } else {
            let mut old_package = bytes.clone();
            completed(&mut old_package, identity());
            assert_eq!(
                parse(&old_package).unwrap_err().category,
                "CaptureTransport"
            );
            rebased.request_id = "saved-package-capture".into();
            completed(&mut bytes, rebased);
            terminal(&mut bytes);
            parse(&bytes).unwrap();
        }
    }
}

#[test]
fn optional_unbound_discovery_still_enforces_identity_and_metadata_bounds() {
    let mut request = DiscoveryRequest {
        identity: identity(),
        executable_or_bundle: None,
        exact_window_title: None,
        verified_processes: Vec::new(),
        timeout: DISCOVERY_LIMIT,
    };
    request.validate().unwrap();
    request.exact_window_title = Some(String::new());
    request.validate().unwrap();
    request.exact_window_title = Some("x".repeat(1025));
    assert_eq!(request.validate().unwrap_err().category, "CaptureTransport");
    request.exact_window_title = None;
    request.executable_or_bundle = Some("relative".into());
    assert_eq!(request.validate().unwrap_err().category, "CaptureTransport");
    request.executable_or_bundle = None;
    request.verified_processes.push(ProcessCorrespondence {
        process_id: 0,
        process_lifetime: 1,
        executable_path: std::env::current_exe().unwrap(),
    });
    assert_eq!(request.validate().unwrap_err().category, "CaptureTransport");
}

#[test]
fn candidate_overflow_duplicate_keys_and_incompatible_coordinates_never_form_selection() {
    assert_eq!(
        protocol::validate_candidates(&[]).unwrap_err().category,
        "CaptureTransport",
    );
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
            identity: identity(),
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
