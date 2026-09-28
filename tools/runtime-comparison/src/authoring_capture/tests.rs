use super::*;
use mado_pilot as mp;
use mado_pilot_capture::{CaptureProvider, CaptureSession, SessionDescription, StreamState};
use mado_pilot_runtime::{EngineWiring, Matcher, PackageLoader};
use mado_pilot_testkit::{ControlledMatcher, ControlledProducer};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const PROVIDER: mp::ProviderId = mp::ProviderId::new("consumer-controlled");

#[derive(Debug)]
struct Port {
    target: mp::TargetId,
    description: SessionDescription,
    stream: StreamState,
    producer: ControlledProducer,
    acquisitions: AtomicUsize,
    commits: AtomicUsize,
    closes: AtomicUsize,
    close_fails: bool,
    cancel_on_open: Option<mp::CancellationToken>,
}

impl CaptureSession for Port {
    fn description(&self) -> SessionDescription {
        self.description.clone()
    }
    fn frame(
        &self,
        request: &mp::FrameRequest,
        operation: &mp::OperationContext,
    ) -> mp::Result<mp::Frame> {
        self.acquisitions.fetch_add(1, Ordering::Relaxed);
        self.stream.frame(request, operation)
    }
    fn commit_frame(&self, frame: &mp::Frame, operation: &mp::OperationContext) -> mp::Result<()> {
        self.commits.fetch_add(1, Ordering::Relaxed);
        self.stream.commit_frame(frame, operation)
    }
    fn close(&self, operation: &mp::OperationContext) -> mp::Result<()> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        if self.close_fails {
            return Err(mp::Error::new(
                mp::Status::DeadlineExceeded,
                "controlled close did not settle",
            ));
        }
        self.stream.drain(operation)
    }
    fn lifecycle(&self) -> mp::Lifecycle {
        self.stream.lifecycle()
    }
}

#[derive(Debug)]
struct Provider(Arc<Port>);
impl CaptureProvider for Provider {
    fn provider(&self) -> mp::ProviderId {
        PROVIDER
    }
    fn discover(&self, _: &mp::OperationContext) -> mp::Result<Vec<mp::TargetDescription>> {
        Ok(vec![mp::TargetDescription::new(
            self.0.target,
            "controlled window",
            mp::PixelExtent::new(4, 3),
            mp::PixelFormat::Bgra8,
            mp::CoordinateSupport::with_target_placement(),
        )])
    }
    fn open(
        &self,
        target: mp::TargetId,
        _: &mp::OpenRequest,
        _: &mp::OperationContext,
    ) -> mp::Result<Arc<dyn CaptureSession>> {
        if target != self.0.target {
            return Err(mp::Error::new(mp::Status::TargetLost, "foreign target"));
        }
        if let Some(token) = &self.0.cancel_on_open {
            token.cancel();
        }
        Ok(self.0.clone())
    }
}

fn geometry() -> CaptureGeometry {
    CaptureGeometry {
        x: -20.0,
        y: 10.0,
        width: 2.0,
        height: 1.5,
        pixels_per_unit_x: 2.0,
        pixels_per_unit_y: 2.0,
        pixel_width: 4,
        pixel_height: 3,
        unit: CoordinateUnit::MacosGlobalPoints,
    }
}
fn window_geometry() -> mp::WindowGeometry {
    mp::WindowGeometry::new(
        mp::WindowCaptureArea::MacosWindow,
        mp::TargetPlacement::new((-20.0, 10.0), (2.0, 1.5), mp::Scale::new(2.0, 2.0).unwrap())
            .unwrap(),
        mp::PixelExtent::new(4, 3),
    )
}
fn identity() -> CaptureIdentity {
    CaptureIdentity {
        owner: "owner-a".into(),
        package_revision: "package-a".into(),
        binding_revision: "binding-a".into(),
        selection_generation: 7,
        request_id: "request-a".into(),
    }
}
fn port(
    issuer: &mado_pilot_core::IdentityIssuer,
    target: mp::TargetId,
    close_fails: bool,
    cancel_on_open: Option<mp::CancellationToken>,
) -> Arc<Port> {
    let stream = issuer.issue_stream().unwrap();
    let extent = mp::PixelExtent::new(4, 3);
    let port = Arc::new(Port {
        target,
        description: SessionDescription::new(
            target,
            stream,
            extent,
            mp::PixelFormat::Bgra8,
            mp::CoordinateSupport::with_target_placement(),
        ),
        stream: StreamState::new(stream),
        producer: ControlledProducer::new(extent, mp::PixelFormat::Bgra8, 1, 1).unwrap(),
        acquisitions: AtomicUsize::new(0),
        commits: AtomicUsize::new(0),
        closes: AtomicUsize::new(0),
        close_fails,
        cancel_on_open,
    });
    port.stream
        .publish_storage(
            port.producer
                .placed_publication(
                    0x39,
                    mp::TargetPlacement::new(
                        (-20.0, 10.0),
                        (2.0, 1.5),
                        mp::Scale::new(2.0, 2.0).unwrap(),
                    )
                    .unwrap(),
                    mp::Continuity::Continuous,
                )
                .unwrap(),
        )
        .unwrap();
    port
}

fn fixture(
    close_fails: bool,
    cancel_on_open: Option<mp::CancellationToken>,
) -> (mp::Engine, Arc<Port>) {
    let issuer = mado_pilot_core::IdentityIssuer::new();
    let target = issuer.issue_target(PROVIDER).unwrap();
    let port = port(&issuer, target, close_fails, cancel_on_open);
    let engine = mp::Engine::new(EngineWiring {
        engine: issuer.engine(),
        capture: Arc::new(Provider(port.clone())),
        matcher: Matcher::new(Arc::new(ControlledMatcher::new(mp::PixelFormat::Rgba8))),
        loader: PackageLoader::new(),
        ocr: None,
        input: None,
        permission: None,
    })
    .unwrap();
    (engine, port)
}
fn operation() -> mp::OperationContext {
    mp::OperationContext::new()
        .with_timeout(ACQUISITION_LIMIT)
        .unwrap()
}

#[test]
fn recorded_terminal_after_mapping_before_host_commit_refuses_the_prepared_png() {
    let (engine, port) = fixture(false, None);
    let id = identity();
    let authority = CaptureAuthority::new(id.clone()).unwrap();
    let result = acquire(
        &engine,
        port.target,
        geometry(),
        window_geometry(),
        &id,
        &authority,
        &operation(),
        || {
            if port.producer.conversions() > 0 {
                port.stream.terminate(mp::CaptureFault::TargetLost);
            }
            Ok(())
        },
    );
    assert_eq!(result.primary.unwrap().context["status"], "target_lost");
    assert!(result.transfer.is_none());
    assert!(matches!(result.cleanup, CaptureCleanup::Clean));
    assert_eq!(port.acquisitions.load(Ordering::Relaxed), 1);
    assert_eq!(port.closes.load(Ordering::Relaxed), 1);
}

#[test]
fn cancellation_during_preparation_discards_pixels_and_still_closes() {
    let (engine, port) = fixture(false, None);
    let id = identity();
    let authority = CaptureAuthority::new(id.clone()).unwrap();
    let result = acquire(
        &engine,
        port.target,
        geometry(),
        window_geometry(),
        &id,
        &authority,
        &operation(),
        || {
            if port.producer.conversions() > 0 {
                authority.cancel();
            }
            Ok(())
        },
    );
    assert_eq!(result.primary.unwrap().category, "Cancelled");
    assert!(result.transfer.is_none());
    assert_eq!(port.closes.load(Ordering::Relaxed), 1);
}

#[test]
fn foreign_owner_package_binding_selection_and_request_never_admit_a_frame() {
    for field in 0..5 {
        let (engine, port) = fixture(false, None);
        let id = identity();
        let authority = CaptureAuthority::new(id.clone()).unwrap();
        let mut stale = id;
        match field {
            0 => stale.owner.push('x'),
            1 => stale.package_revision.push('x'),
            2 => stale.binding_revision.push('x'),
            3 => stale.selection_generation += 1,
            _ => stale.request_id.push('x'),
        }
        let result = acquire(
            &engine,
            port.target,
            geometry(),
            window_geometry(),
            &stale,
            &authority,
            &operation(),
            || Ok(()),
        );
        assert_eq!(result.primary.unwrap().category, "StaleCapture");
        assert_eq!(port.acquisitions.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn changed_geometry_and_incomplete_close_never_produce_usable_transfer() {
    let id = identity();
    let (engine, port) = fixture(false, None);
    let mut moved = geometry();
    moved.x += 1.0;
    let result = acquire(
        &engine,
        port.target,
        moved,
        window_geometry(),
        &id,
        &CaptureAuthority::new(id.clone()).unwrap(),
        &operation(),
        || Ok(()),
    );
    assert_eq!(result.primary.unwrap().category, "CaptureGeometry");
    assert!(result.transfer.is_none());
    assert_eq!(port.producer.conversions(), 0);
    let (engine, port) = fixture(true, None);
    let result = acquire(
        &engine,
        port.target,
        geometry(),
        window_geometry(),
        &id,
        &CaptureAuthority::new(id.clone()).unwrap(),
        &operation(),
        || Ok(()),
    );
    assert!(result.primary.is_none());
    assert!(matches!(result.cleanup, CaptureCleanup::Unconfirmed(_)));
    assert!(result.transfer.is_none());
}

#[test]
fn interrupted_native_open_rolls_back_without_claiming_a_public_cleanup_receipt() {
    for close_fails in [false, true] {
        let token = mp::CancellationToken::new();
        let (engine, port) = fixture(close_fails, Some(token.clone()));
        let id = identity();
        let operation = operation().with_cancellation(token);
        let result = acquire(
            &engine,
            port.target,
            geometry(),
            window_geometry(),
            &id,
            &CaptureAuthority::new(id.clone()).unwrap(),
            &operation,
            || Ok(()),
        );
        assert_eq!(result.primary.unwrap().context["status"], "cancelled");
        assert_eq!(port.closes.load(Ordering::Relaxed), 1);
        assert_eq!(port.acquisitions.load(Ordering::Relaxed), 0);
        assert!(matches!(result.cleanup, CaptureCleanup::Unconfirmed(_)));
        assert!(result.transfer.is_none());
    }
}

#[test]
fn detached_pixels_remain_historical_and_one_authority_cannot_recapture() {
    let (engine, port) = fixture(false, None);
    let id = identity();
    let authority = CaptureAuthority::new(id.clone()).unwrap();
    let result = acquire(
        &engine,
        port.target,
        geometry(),
        window_geometry(),
        &id,
        &authority,
        &operation(),
        || Ok(()),
    );
    assert!(matches!(result.cleanup, CaptureCleanup::Clean));
    assert_eq!(port.closes.load(Ordering::Relaxed), 1);
    let transfer = result.transfer.unwrap();
    port.stream.terminate(mp::CaptureFault::TargetLost);
    let mut wire = Vec::new();
    transfer.write_private(&mut wire).unwrap();
    let accepted = PendingCapture::read_private(&mut wire.as_slice(), &id, &geometry())
        .unwrap()
        .accept_after_session_close(&authority, &id)
        .unwrap();
    assert_eq!((accepted.image.width, accepted.image.height), (4, 3));
    assert_eq!(accepted.image.rgba, vec![0x39; 48]);
    assert_eq!(
        images::decode_png(&accepted.png, ImageKind::Input)
            .unwrap()
            .rgba,
        accepted.image.rgba
    );
    let duplicate = PendingCapture::read_private(&mut wire.as_slice(), &id, &geometry())
        .unwrap()
        .accept_after_session_close(&authority, &id);
    assert!(matches!(duplicate, Err(error) if error.category == "CaptureConsumed"));
    let second = acquire(
        &engine,
        port.target,
        geometry(),
        window_geometry(),
        &id,
        &authority,
        &operation(),
        || Ok(()),
    );
    assert_eq!(second.primary.unwrap().category, "CaptureConsumed");
    assert_eq!(port.acquisitions.load(Ordering::Relaxed), 1);
}

#[test]
fn original_operation_cancellation_and_deadline_win_after_full_preparation() {
    for deadline in [false, true] {
        let (engine, port) = fixture(false, None);
        let id = identity();
        let authority = CaptureAuthority::new(id.clone()).unwrap();
        let clock = Arc::new(mado_pilot_testkit::ManualClock::new());
        let token = mp::CancellationToken::new();
        let operation = mp::OperationContext::new()
            .with_clock(clock.clone())
            .with_timeout(ACQUISITION_LIMIT)
            .unwrap()
            .with_cancellation(token.clone());
        let result = acquire(
            &engine,
            port.target,
            geometry(),
            window_geometry(),
            &id,
            &authority,
            &operation,
            || {
                if port.producer.conversions() > 0 {
                    if deadline {
                        clock.advance(ACQUISITION_LIMIT);
                    } else {
                        token.cancel();
                    }
                }
                Ok(())
            },
        );
        let primary = result.primary.unwrap();
        assert_eq!(primary.context["stage"], "commit");
        assert_eq!(
            primary.context["status"],
            if deadline {
                "deadline_exceeded"
            } else {
                "cancelled"
            }
        );
        assert!(result.transfer.is_none());
        assert!(matches!(result.cleanup, CaptureCleanup::Clean));
        assert_eq!(port.acquisitions.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn cancellation_after_clean_session_close_still_refuses_publication() {
    let (engine, port) = fixture(false, None);
    let id = identity();
    let authority = CaptureAuthority::new(id.clone()).unwrap();
    let transfer = acquire(
        &engine,
        port.target,
        geometry(),
        window_geometry(),
        &id,
        &authority,
        &operation(),
        || Ok(()),
    )
    .transfer
    .unwrap();
    let mut wire = Vec::new();
    transfer.write_private(&mut wire).unwrap();
    authority.cancel();
    let cancelled = PendingCapture::read_private(&mut wire.as_slice(), &id, &geometry())
        .unwrap()
        .accept_after_session_close(&authority, &id);
    assert!(matches!(cancelled, Err(error) if error.category == "Cancelled"));
}

#[derive(Debug)]
struct ReusableProvider {
    ports: [Arc<Port>; 2],
    opens: AtomicUsize,
    discoveries: AtomicUsize,
}

impl CaptureProvider for ReusableProvider {
    fn provider(&self) -> mp::ProviderId {
        PROVIDER
    }

    fn discover(&self, operation: &mp::OperationContext) -> mp::Result<Vec<mp::TargetDescription>> {
        self.discoveries.fetch_add(1, Ordering::Relaxed);
        Provider(self.ports[0].clone()).discover(operation)
    }

    fn open(
        &self,
        target: mp::TargetId,
        _: &mp::OpenRequest,
        _: &mp::OperationContext,
    ) -> mp::Result<Arc<dyn CaptureSession>> {
        if target != self.ports[0].target {
            return Err(mp::Error::new(
                mp::Status::TargetLost,
                "foreign retained target",
            ));
        }
        let index = self.opens.fetch_add(1, Ordering::Relaxed);
        self.ports
            .get(index)
            .map(|port| port.clone() as Arc<dyn CaptureSession>)
            .ok_or_else(|| mp::Error::new(mp::Status::TargetLost, "unexpected repeated open"))
    }
}

#[test]
fn one_engine_reopens_only_the_original_target_and_closes_each_frame_session() {
    let issuer = mado_pilot_core::IdentityIssuer::new();
    let target = issuer.issue_target(PROVIDER).unwrap();
    let provider = Arc::new(ReusableProvider {
        ports: [
            port(&issuer, target, false, None),
            port(&issuer, target, false, None),
        ],
        opens: AtomicUsize::new(0),
        discoveries: AtomicUsize::new(0),
    });
    let engine = mp::Engine::new(EngineWiring {
        engine: issuer.engine(),
        capture: provider.clone(),
        matcher: Matcher::new(Arc::new(ControlledMatcher::new(mp::PixelFormat::Rgba8))),
        loader: PackageLoader::new(),
        ocr: None,
        input: None,
        permission: None,
    })
    .unwrap();
    let mut frame_ids = Vec::new();
    for index in 0..2 {
        let mut id = identity();
        id.request_id = format!("request-{index}");
        let authority = CaptureAuthority::new(id.clone()).unwrap();
        let outcome = acquire(
            &engine,
            target,
            geometry(),
            window_geometry(),
            &id,
            &authority,
            &operation(),
            || Ok(()),
        );
        assert!(outcome.primary.is_none());
        assert!(matches!(outcome.cleanup, CaptureCleanup::Clean));
        let port = &provider.ports[index];
        assert_eq!(port.acquisitions.load(Ordering::Relaxed), 1);
        assert_eq!(port.producer.conversions(), 1);
        assert_eq!(port.commits.load(Ordering::Relaxed), 1);
        assert_eq!(port.closes.load(Ordering::Relaxed), 1);
        let mut wire = Vec::new();
        outcome.transfer.unwrap().write_private(&mut wire).unwrap();
        let accepted = PendingCapture::read_private(&mut wire.as_slice(), &id, &geometry())
            .unwrap()
            .accept_after_session_close(&authority, &id)
            .unwrap();
        assert_eq!(accepted.identity.request_id, id.request_id);
        assert_eq!(accepted.image.rgba, vec![0x39; 48]);
        frame_ids.push(accepted.frame_identity);
    }
    assert_ne!(frame_ids[0], frame_ids[1]);
    assert_eq!(provider.opens.load(Ordering::Relaxed), 2);
    assert_eq!(provider.discoveries.load(Ordering::Relaxed), 0);
}
