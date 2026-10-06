//! One-frame consumer boundary. Native target discovery is deliberately separate:
//! callers must retain the SDK's exact target, never reconstruct one from a title/PID.
use crate::images::{self, DecodedImage, EncodedImage, ImageKind, PayloadReservation};
use crate::model::Fault;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

pub const ACQUISITION_LIMIT: Duration = Duration::from_secs(10);
pub const CLEANUP_LIMIT: Duration = Duration::from_secs(1);
pub const CONTAINMENT_LIMIT: Duration = Duration::from_secs(2);
const HEADER_BYTES: usize = 16 * 1024;
const MAGIC: &[u8; 8] = b"MMCAP01\0";
const CANCELLED: u8 = 1;
const PUBLISHED: u8 = 4;

/// Host-issued correlation, not native authority and never portable package metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureIdentity {
    pub owner: String,
    pub package_revision: String,
    pub binding_revision: String,
    pub selection_generation: u64,
    pub request_id: String,
}

impl CaptureIdentity {
    fn validate(&self) -> Result<(), Fault> {
        if self.selection_generation == 0
            || self.selection_generation > 9_007_199_254_740_991
            || [
                &self.owner,
                &self.package_revision,
                &self.binding_revision,
                &self.request_id,
            ]
            .into_iter()
            .any(|value| value.is_empty() || value.len() > 1024)
        {
            return Err(invalid("capture correlation is invalid"));
        }
        Ok(())
    }
}

/// Revocation does not wait on stores, logs, native calls, or the command mutex.
/// A new owner/selection gets a new authority; the old one can never be repurposed.
pub struct CaptureAuthority {
    identity: CaptureIdentity,
    state: AtomicU8,
}

impl CaptureAuthority {
    pub fn new(identity: CaptureIdentity) -> Result<Self, Fault> {
        identity.validate()?;
        Ok(Self {
            identity,
            state: AtomicU8::new(0),
        })
    }

    pub fn cancel(&self) {
        self.state.fetch_or(CANCELLED, Ordering::AcqRel);
    }

    pub fn check(&self, identity: &CaptureIdentity) -> Result<(), Fault> {
        if self.identity != *identity {
            return Err(stale());
        }
        if self.state.load(Ordering::Acquire) & CANCELLED != 0 {
            return Err(Fault::new(
                "Cancelled",
                "Authoring capture authority was cancelled",
            ));
        }
        Ok(())
    }

    // Cancellation and host acceptance arbitrate on the same atomic state.
    // Neither this host fence nor its acquisition bit replaces SDK commit_frame.
    fn claim(&self, identity: &CaptureIdentity, flag: u8) -> Result<(), Fault> {
        self.check(identity)?;
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & (CANCELLED | flag) == 0).then_some(state | flag)
            })
            .map_err(|state| {
                if state & CANCELLED != 0 {
                    Fault::new("Cancelled", "Authoring capture authority was cancelled")
                } else {
                    Fault::new("CaptureConsumed", "Capture requires a new explicit request")
                }
            })?;
        Ok(())
    }

    #[cfg(feature = "engine")]
    fn admit(&self, identity: &CaptureIdentity) -> Result<(), Fault> {
        self.claim(identity, 2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateUnit {
    MacosGlobalPoints,
    WindowsPhysicalPixels,
}

/// Discovery must supply the actual captured area, not decorated-window guesses.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub pixels_per_unit_x: f64,
    pub pixels_per_unit_y: f64,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub unit: CoordinateUnit,
}

impl CaptureGeometry {
    pub fn validate(&self) -> Result<usize, Fault> {
        let bytes =
            images::checked_rgba_bytes(self.pixel_width, self.pixel_height, ImageKind::Input)?;
        if ![
            self.x,
            self.y,
            self.width,
            self.height,
            self.x + self.width,
            self.y + self.height,
            self.pixels_per_unit_x,
            self.pixels_per_unit_y,
        ]
        .into_iter()
        .all(f64::is_finite)
            || self.width <= 0.0
            || self.height <= 0.0
            || self.pixels_per_unit_x <= 0.0
            || self.pixels_per_unit_y <= 0.0
            || self.width * self.pixels_per_unit_x != f64::from(self.pixel_width)
            || self.height * self.pixels_per_unit_y != f64::from(self.pixel_height)
        {
            return Err(Fault::new(
                "CaptureGeometry",
                "Capture geometry is unsupported or changed",
            ));
        }
        Ok(bytes)
    }
}

/// Reserve in the parent BEFORE admitting capture to the metadata-only worker.
/// The required SDK ceiling includes producer, detached and mapped image payload
/// (and observed row padding), not driver/GPU allocations or total process RSS.
/// In addition, charge the packed original, PNG and bounded encoder scratch.
/// Hold this reservation until clean per-frame completion or physical reap on failure;
/// incoming PNG owns a separate charge.
pub fn reserve_worker_payload(
    geometry: &CaptureGeometry,
    native_storage_ceiling: usize,
) -> Result<PayloadReservation, Fault> {
    let bytes = geometry.validate()?;
    if native_storage_ceiling < bytes {
        return Err(invalid(
            "native storage bound does not cover the selected frame",
        ));
    }
    let total = bytes
        .checked_add(native_storage_ceiling)
        .and_then(|n| n.checked_add(images::INPUT_MAX_BYTES))
        .and_then(|n| n.checked_add(1024 * 1024 + geometry.pixel_width as usize * 12))
        .ok_or_else(|| invalid("capture payload arithmetic overflow"))?;
    images::reserve_payload(total)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    identity: CaptureIdentity,
    geometry: CaptureGeometry,
    acquired_at_ms: u64,
    captured_monotonic_us: u64,
    frame_identity: String,
    png_bytes: usize,
}

/// Cleanly closed inside the worker, but NOT yet accepted by the host. Publication
/// requires the correlated per-frame session-close receipt, not Engine termination.
pub struct CaptureTransfer {
    header: Vec<u8>,
    png: EncodedImage,
}

impl CaptureTransfer {
    /// Private binary pipe only. Never send this payload to controller/log JSON.
    pub fn write_private(&self, output: &mut impl Write) -> Result<(), Fault> {
        output
            .write_all(MAGIC)
            .and_then(|()| output.write_all(&(self.header.len() as u32).to_le_bytes()))
            .and_then(|()| output.write_all(&self.header))
            .and_then(|()| output.write_all(self.png.as_bytes()))
            .and_then(|()| output.flush())
            .map_err(|_| transport())
    }
}

/// Pipe data remains unusable until its clean session receipt is verified.
struct PendingCapture {
    header: Header,
    png: Vec<u8>,
    reservation: PayloadReservation,
}

pub struct DetachedCapture {
    pub identity: CaptureIdentity,
    pub geometry: CaptureGeometry,
    pub acquired_at_ms: u64,
    /// Source timestamp in the worker SDK clock domain, not a wall-clock time.
    pub captured_monotonic_us: u64,
    pub frame_identity: String,
    pub image: DecodedImage,
    pub png: images::PayloadBytes,
}

impl PendingCapture {
    fn read_private(
        input: &mut impl Read,
        expected: &CaptureIdentity,
        geometry: &CaptureGeometry,
    ) -> Result<Self, Fault> {
        let mut magic = [0u8; 8];
        let mut length = [0u8; 4];
        input
            .read_exact(&mut magic)
            .and_then(|()| input.read_exact(&mut length))
            .map_err(|_| transport())?;
        let length = u32::from_le_bytes(length) as usize;
        if &magic != MAGIC || length == 0 || length > HEADER_BYTES {
            return Err(transport());
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes).map_err(|_| transport())?;
        let header: Header = serde_json::from_slice(&bytes).map_err(|_| transport())?;
        header.identity.validate()?;
        header.geometry.validate()?;
        if header.identity != *expected || header.geometry != *geometry {
            return Err(stale());
        }
        if header.frame_identity.is_empty()
            || header.frame_identity.len() > 1024
            || header.png_bytes == 0
            || header.png_bytes > images::INPUT_MAX_BYTES
        {
            return Err(transport());
        }
        let reservation = images::reserve_payload(header.png_bytes)?;
        let mut png = Vec::new();
        png.try_reserve_exact(header.png_bytes)
            .map_err(|_| invalid("capture transfer allocation failed"))?;
        png.resize(header.png_bytes, 0);
        input.read_exact(&mut png).map_err(|_| transport())?;
        Ok(Self {
            header,
            png,
            reservation,
        })
    }

    /// Only the worker owner may accept a correlated clean session completion.
    /// Cancellation and exact host correlation are checked again before publication.
    fn accept_after_session_close(
        self,
        authority: &CaptureAuthority,
        expected: &CaptureIdentity,
    ) -> Result<DetachedCapture, Fault> {
        authority.check(expected)?;
        if self.header.identity != *expected {
            return Err(stale());
        }
        let image = images::decode_png(&self.png, ImageKind::Input)?;
        if image.width != self.header.geometry.pixel_width
            || image.height != self.header.geometry.pixel_height
        {
            return Err(invalid(
                "transferred pixels disagree with committed geometry",
            ));
        }
        authority.check(expected)?;
        let png = images::PayloadBytes::from_reserved(self.png, self.reservation)?;
        authority.claim(expected, PUBLISHED)?;
        Ok(DetachedCapture {
            identity: self.header.identity,
            geometry: self.header.geometry,
            acquired_at_ms: self.header.acquired_at_ms,
            captured_monotonic_us: self.header.captured_monotonic_us,
            frame_identity: self.header.frame_identity,
            image,
            png,
        })
    }
}

fn invalid(message: &str) -> Fault {
    Fault::new("CaptureImage", message)
}
fn stale() -> Fault {
    Fault::new(
        "StaleCapture",
        "Capture owner, package, binding or selection changed",
    )
}
fn transport() -> Fault {
    Fault::new(
        "CaptureTransport",
        "Incomplete or invalid private capture transfer",
    )
}

mod protocol;
mod worker;
pub use protocol::{
    CANDIDATE_LIMIT, Candidate, CaptureArea, DISCOVERY_LIMIT, DiscoveryRequest,
    NATIVE_STORAGE_BYTES, NativeWindow, ProcessCorrespondence,
};
pub use worker::{AuthoringCaptureWorker, WorkerCancel, WorkerCaptureResult, WorkerSettlement};

#[cfg(feature = "engine")]
mod native;

/// Application-owned private protocol, never a native fallback or script runner.
pub fn authoring_capture_child() -> Result<bool, Fault> {
    #[cfg(feature = "engine")]
    {
        native::child()
    }
    #[cfg(not(feature = "engine"))]
    {
        let error = Fault::new(
            "EngineUnavailable",
            "Native authoring requires the engine-enabled worker",
        );
        protocol::write_event(
            &mut std::io::stdout(),
            &protocol::Event::Terminal {
                primary: Some(error),
                clean: true,
            },
        )?;
        Ok(true)
    }
}

#[cfg(feature = "engine")]
mod acquisition;
#[cfg(test)]
mod protocol_tests;
#[cfg(feature = "engine")]
pub use acquisition::{CaptureCleanup, CaptureOutcome, acquire};

#[cfg(all(test, feature = "engine"))]
mod tests;
