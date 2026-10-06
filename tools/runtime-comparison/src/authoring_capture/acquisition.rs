use super::*;
use mado_pilot as mp;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub enum CaptureCleanup {
    Clean,
    /// Public failed-open reports do not expose a structured rollback receipt.
    /// Never infer clean native release from its primary status or error wording.
    Unconfirmed(Fault),
}

pub struct CaptureOutcome {
    pub primary: Option<Fault>,
    pub cleanup: CaptureCleanup,
    pub transfer: Option<CaptureTransfer>,
}

/// The caller owns the engine and its ORIGINAL retained target. This function
/// opens capture-only, requests one frame, and never discovers a replacement.
/// Execute inside an owned worker: an OS call may outlive its logical deadline.
pub fn acquire(
    engine: &mp::Engine,
    target: mp::TargetId,
    geometry: CaptureGeometry,
    window_geometry: mp::WindowGeometry,
    identity: &CaptureIdentity,
    authority: &CaptureAuthority,
    operation: &mp::OperationContext,
    mut host_checkpoint: impl FnMut() -> Result<(), Fault>,
) -> CaptureOutcome {
    if let Err(error) = admission(
        &geometry,
        identity,
        authority,
        operation,
        &mut host_checkpoint,
    ) {
        return CaptureOutcome {
            primary: Some(error),
            cleanup: CaptureCleanup::Clean,
            transfer: None,
        };
    }
    let prepared = (|| {
        if super::native::geometry(window_geometry)? != geometry {
            return Err(Fault::new(
                "CaptureGeometry",
                "Selected geometry differs from the retained SDK window",
            ));
        }
        let limits = mp::CaptureResourceLimits::new(
            (images::INPUT_MAX_PIXELS * 4) as u64,
            NATIVE_STORAGE_BYTES as u64,
        )
        .map_err(|error| sdk_error("resource_limits", error))?;
        let request = mp::OpenRequest::new()
            .require_window_geometry(window_geometry)
            .with_resource_limits(limits);
        Ok(request)
    })();
    let request = match prepared {
        Ok(request) => request,
        Err(error) => {
            return CaptureOutcome {
                primary: Some(error),
                cleanup: CaptureCleanup::Clean,
                transfer: None,
            };
        }
    };
    // Engine::open is explicitly capture-only; no InputOpenRequest is constructed.
    match engine.open(target, &request, operation) {
        Ok(session) => capture(
            session,
            geometry,
            identity,
            authority,
            operation,
            host_checkpoint,
        ),
        Err(error) => CaptureOutcome {
            primary: Some(sdk_error("open", error)),
            cleanup: CaptureCleanup::Unconfirmed(Fault::new(
                "CaptureCleanup",
                "Native open rollback has no public cleanup receipt",
            )),
            transfer: None,
        },
    }
}

fn admission(
    geometry: &CaptureGeometry,
    identity: &CaptureIdentity,
    authority: &CaptureAuthority,
    operation: &mp::OperationContext,
    checkpoint: &mut impl FnMut() -> Result<(), Fault>,
) -> Result<(), Fault> {
    geometry.validate()?;
    if operation
        .remaining()
        .is_none_or(|remaining| remaining.is_zero() || remaining > ACQUISITION_LIMIT)
    {
        return Err(Fault::new(
            "CaptureAuthority",
            "Capture requires a finite operation of at most ten seconds",
        ));
    }
    checkpoint()?;
    authority.admit(identity)
}

fn capture(
    session: mp::Session,
    geometry: CaptureGeometry,
    identity: &CaptureIdentity,
    authority: &CaptureAuthority,
    operation: &mp::OperationContext,
    mut checkpoint: impl FnMut() -> Result<(), Fault>,
) -> CaptureOutcome {
    let prepared = (|| {
        authority.check(identity)?;
        checkpoint()?;
        // This is the sole acquisition. No liveness probe, retry, or newer-frame request.
        let frame = session
            .acquire_frame(&mp::FrameRequest::latest(), operation)
            .map_err(|error| sdk_error("acquire", error))?;
        let descriptor = frame.descriptor();
        let extent = descriptor.extent();
        let bytes = images::checked_rgba_bytes(extent.width(), extent.height(), ImageKind::Input)?;
        let row = (extent.width() as usize)
            .checked_mul(4)
            .ok_or_else(|| invalid("native row arithmetic overflow"))?;
        let native_bytes = descriptor
            .stride()
            .checked_mul(extent.height() as usize)
            .filter(|length| *length <= images::INPUT_MAX_PIXELS * 4)
            .ok_or_else(|| invalid("native stride exceeds decoded image bound"))?;
        if descriptor.stride() < row || native_bytes != descriptor.byte_len() {
            return Err(invalid("native frame stride is invalid"));
        }
        validate_geometry(&frame, &geometry)?;
        let mapping = session
            .map_frame(&frame, mp::PixelFormat::Rgba8, operation)
            .map_err(|error| sdk_error("map", error))?;
        let mapped = mapping.descriptor();
        if mapping.stamp() != frame.stamp()
            || mapped.extent() != extent
            || mapped.format() != mp::PixelFormat::Rgba8
            || mapped.stride() < row
            || mapped.stride().checked_mul(extent.height() as usize) != Some(mapping.bytes().len())
            || mapping.bytes().len() > images::INPUT_MAX_PIXELS * 4
        {
            return Err(invalid("mapped pixels do not describe the original frame"));
        }
        let reservation = images::reserve_payload(bytes)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(bytes)
            .map_err(|_| invalid("capture allocation failed"))?;
        for scanline in mapping.bytes().chunks_exact(mapped.stride()) {
            rgba.extend_from_slice(&scanline[..row]);
        }
        let image =
            DecodedImage::from_reserved_rgba(extent.width(), extent.height(), rgba, reservation)?;
        let png = images::encode_input(&image)?;
        let acquired_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|time| u64::try_from(time.as_millis()).ok())
            .ok_or_else(|| invalid("historical capture time is unavailable"))?;
        let captured_monotonic_us =
            u64::try_from(frame.captured_at().since_origin().as_micros())
                .map_err(|_| invalid("source capture time cannot be represented"))?;
        let header = Header {
            identity: identity.clone(),
            geometry,
            acquired_at_ms,
            captured_monotonic_us,
            frame_identity: frame.stamp().to_string(),
            png_bytes: png.as_bytes().len(),
        };
        // Complete bounded output candidate, including metadata encoding, BEFORE
        // the SDK linearization point. Never fabricate/reconstruct frame/operation.
        let header = crate::model::encode_bounded(&header, HEADER_BYTES)?;
        authority.check(identity)?;
        checkpoint()?;
        authority.check(identity)?;
        session
            .commit_frame(&frame, operation)
            .map_err(|error| sdk_error("commit", error))?;
        authority.check(identity)?;
        Ok(CaptureTransfer { header, png })
    })();
    finish(session, prepared, identity, authority, checkpoint)
}

fn validate_geometry(frame: &mp::Frame, expected: &CaptureGeometry) -> Result<(), Fault> {
    let placement = frame.transform().target().ok_or_else(|| {
        Fault::new(
            "CaptureGeometry",
            "Frame has no authoritative capture placement",
        )
    })?;
    let extent = frame.descriptor().extent();
    let origin = placement.desktop_origin();
    let scale = placement.desktop_scale();
    if origin != (expected.x, expected.y)
        || scale.x() != expected.pixels_per_unit_x
        || scale.y() != expected.pixels_per_unit_y
        || extent.width() != expected.pixel_width
        || extent.height() != expected.pixel_height
        || f64::from(extent.width()) / scale.x() != expected.width
        || f64::from(extent.height()) / scale.y() != expected.height
    {
        return Err(Fault::new(
            "CaptureGeometry",
            "Selected window moved, resized or changed display scale",
        ));
    }
    Ok(())
}

fn finish(
    session: mp::Session,
    prepared: Result<CaptureTransfer, Fault>,
    identity: &CaptureIdentity,
    authority: &CaptureAuthority,
    mut checkpoint: impl FnMut() -> Result<(), Fault>,
) -> CaptureOutcome {
    // Independent cleanup context: cancellation of acquisition cannot cancel release.
    let cleanup = mp::OperationContext::new()
        .with_timeout(CLEANUP_LIMIT)
        .and_then(|operation| session.close(&operation));
    let cleanup = match cleanup {
        Ok(()) => CaptureCleanup::Clean,
        Err(error) => CaptureCleanup::Unconfirmed(sdk_error("close", error)),
    };
    let prepared = prepared.and_then(|candidate| {
        authority.check(identity)?;
        checkpoint()?;
        Ok(candidate)
    });
    match (prepared, &cleanup) {
        (Ok(transfer), CaptureCleanup::Clean) => CaptureOutcome {
            primary: None,
            cleanup,
            transfer: Some(transfer),
        },
        (Ok(_), CaptureCleanup::Unconfirmed(_)) => CaptureOutcome {
            primary: None,
            cleanup,
            transfer: None,
        },
        (Err(error), _) => CaptureOutcome {
            primary: Some(error),
            cleanup,
            transfer: None,
        },
    }
}

pub(super) fn sdk_error(stage: &str, error: mp::Error) -> Fault {
    // Native detail may contain private executable paths. Preserve typed status only.
    Fault::new("NativeCapture", "Public capture operation was refused")
        .with_context(serde_json::json!({"stage":stage,"status":error.status().as_str()}))
}
