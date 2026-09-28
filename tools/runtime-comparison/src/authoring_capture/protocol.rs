use super::*;
use serde::de::DeserializeOwned;
use std::path::PathBuf;

pub const DISCOVERY_LIMIT: Duration = Duration::from_secs(5);
pub const CANDIDATE_LIMIT: usize = 64;
pub const NATIVE_STORAGE_BYTES: usize = 256 * 1024 * 1024;
pub(super) const CAPTURE_REQUEST_LIMIT: usize = 4096;
const METADATA_BYTES: usize = 256 * 1024;
pub(super) const COMMAND_BYTES: usize = 16 * 1024;

/// Host-private policy, never deserialized from renderer-supplied native identity.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRequest {
    pub identity: CaptureIdentity,
    pub executable_or_bundle: Option<PathBuf>,
    pub exact_window_title: Option<String>,
    pub verified_processes: Vec<ProcessCorrespondence>,
    pub timeout: Duration,
}

impl DiscoveryRequest {
    pub(super) fn validate(&self) -> Result<(), Fault> {
        self.identity.validate()?;
        if self.timeout.is_zero() || self.timeout > DISCOVERY_LIMIT {
            return Err(protocol_fault());
        }
        if self.verified_processes.len() > CANDIDATE_LIMIT
            || self.verified_processes.iter().any(|process| {
                process.process_id == 0
                    || process.process_lifetime == 0
                    || !process.executable_path.is_absolute()
                    || process.executable_path.as_os_str().len() > 4096
            })
        {
            return Err(protocol_fault());
        }
        if self
            .executable_or_bundle
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || path.as_os_str().len() > 4096)
            || self
                .exact_window_title
                .as_ref()
                .is_some_and(|title| title.len() > 1024)
        {
            return Err(protocol_fault());
        }
        Ok(())
    }
}

/// Fresh host-verified correspondence; the worker still retains the SDK window authority.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCorrespondence {
    pub process_id: u32,
    pub process_lifetime: u64,
    pub executable_path: PathBuf,
}

impl ProcessCorrespondence {
    #[cfg(any(feature = "engine", test))]
    pub(super) fn matches(
        &self,
        process_id: u32,
        lifetime: u64,
        executable: &std::path::Path,
    ) -> bool {
        self.process_id == process_id
            && self.process_lifetime == lifetime
            && self.executable_path == executable
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeWindow {
    Macos(u32),
    Windows(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureArea {
    MacosWindow,
    WindowsExtendedFrame,
    WindowsClient,
}

/// Descriptive metadata only. The corresponding SDK target never leaves its worker.
/// Deliberately no Debug: paths, native IDs and titles must not enter routine logs.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub key: String,
    pub label: String,
    pub process_id: u32,
    pub process_lifetime: u64,
    pub executable_path: PathBuf,
    pub application_bundle_path: Option<PathBuf>,
    pub native_window: NativeWindow,
    pub area: CaptureArea,
    pub geometry: CaptureGeometry,
}

impl Candidate {
    pub(super) fn validate(&self) -> Result<(), Fault> {
        self.geometry.validate()?;
        let compatible = matches!(
            (self.native_window, self.area, self.geometry.unit),
            (
                NativeWindow::Macos(1..),
                CaptureArea::MacosWindow,
                CoordinateUnit::MacosGlobalPoints
            ) | (
                NativeWindow::Windows(1..),
                CaptureArea::WindowsExtendedFrame | CaptureArea::WindowsClient,
                CoordinateUnit::WindowsPhysicalPixels
            )
        );
        if !compatible
            || self.key.is_empty()
            || self.key.len() > 128
            || self.label.len() > 1024
            || self.process_id == 0
            || self.process_lifetime == 0
            || !self.executable_path.is_absolute()
            || self.executable_path.as_os_str().len() > 4096
            || self
                .application_bundle_path
                .as_ref()
                .is_some_and(|path| !path.is_absolute() || path.as_os_str().len() > 4096)
        {
            return Err(protocol_fault());
        }
        Ok(())
    }
}

pub(super) fn same_selection(left: &CaptureIdentity, right: &CaptureIdentity) -> bool {
    left.owner == right.owner
        && left.package_revision == right.package_revision
        && left.binding_revision == right.binding_revision
        && left.selection_generation == right.selection_generation
}

pub(super) fn same_package_rebase(current: &CaptureIdentity, next: &CaptureIdentity) -> bool {
    current.owner == next.owner
        && current.binding_revision == next.binding_revision
        && current.selection_generation == next.selection_generation
        && current.request_id == next.request_id
}

pub(super) fn admit_request(
    requests: &mut std::collections::BTreeSet<String>,
    request_id: &str,
) -> Result<(), Fault> {
    if requests.contains(request_id) {
        return Err(stale());
    }
    if requests.len() >= CAPTURE_REQUEST_LIMIT {
        return Err(capture_limit());
    }
    requests.insert(request_id.into());
    Ok(())
}

pub(super) fn capture_limit() -> Fault {
    Fault::new(
        "NativeCaptureLimit",
        "The retained Engine reached its capture limit; explicitly close and start capture again",
    )
}

pub(super) fn validate_candidates(candidates: &[Candidate]) -> Result<(), Fault> {
    if candidates.len() > CANDIDATE_LIMIT {
        return Err(candidate_overflow());
    }
    if candidates.is_empty() {
        return Err(protocol_fault());
    }
    let mut keys = std::collections::BTreeSet::new();
    let mut processes = std::collections::BTreeSet::new();
    for candidate in candidates {
        candidate.validate()?;
        if !keys.insert(&candidate.key) {
            return Err(protocol_fault());
        }
        processes.insert((candidate.process_id, candidate.process_lifetime));
    }
    if processes.len() > CANDIDATE_LIMIT {
        return Err(candidate_overflow());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Command {
    Select {
        key: String,
        binding_revision: String,
    },
    RebasePackage {
        identity: CaptureIdentity,
    },
    Capture {
        identity: CaptureIdentity,
        candidate: Candidate,
        timeout: Duration,
    },
    Cancel,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureDescriptor {
    pub identity: CaptureIdentity,
    pub geometry: CaptureGeometry,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Event {
    Discovered {
        identity: CaptureIdentity,
        engine_revision: String,
        candidates: Vec<Candidate>,
    },
    Selected {
        identity: CaptureIdentity,
        candidate: Candidate,
    },
    PackageRebased {
        identity: CaptureIdentity,
    },
    Frame {
        capture: CaptureDescriptor,
    },
    Captured {
        identity: CaptureIdentity,
        session_clean: bool,
    },
    Terminal {
        primary: Option<Fault>,
        clean: bool,
    },
}

pub(super) fn write_message(
    output: &mut impl Write,
    message: &impl Serialize,
    limit: usize,
) -> Result<(), Fault> {
    let bytes = crate::model::encode_bounded(message, limit).map_err(|_| protocol_fault())?;
    output
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|()| output.write_all(&bytes))
        .and_then(|()| output.flush())
        .map_err(|_| protocol_fault())
}

pub(super) fn read_message<T: DeserializeOwned>(
    input: &mut impl Read,
    limit: usize,
) -> Result<T, Fault> {
    let mut length = [0; 4];
    input
        .read_exact(&mut length)
        .map_err(|_| protocol_fault())?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > limit {
        return Err(protocol_fault());
    }
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).map_err(|_| protocol_fault())?;
    serde_json::from_slice(&bytes).map_err(|_| protocol_fault())
}

pub(super) fn write_event(output: &mut impl Write, event: &Event) -> Result<(), Fault> {
    write_message(output, event, METADATA_BYTES)
}

pub(super) enum Received {
    Discovered(CaptureIdentity, Vec<Candidate>),
    Selected(CaptureIdentity, Candidate),
    PackageRebased(CaptureIdentity),
    Captured(PendingCapture),
    Terminal { primary: Option<Fault>, clean: bool },
}

/// One discovery and retained selection, then explicitly requested bounded frames.
/// Each frame carries a clean session receipt; only Terminal requires pipe EOF.
pub(super) fn read_events(
    input: &mut impl Read,
    send: &std::sync::mpsc::SyncSender<Result<Received, Fault>>,
) -> Result<(), Fault> {
    let mut discovered = false;
    let mut selection: Option<(CaptureIdentity, Candidate)> = None;
    loop {
        match read_message::<Event>(input, METADATA_BYTES)? {
            Event::Discovered {
                identity,
                engine_revision,
                candidates,
            } => {
                if discovered || engine_revision != crate::model::ENGINE_REVISION {
                    return Err(protocol_fault());
                }
                identity.validate()?;
                validate_candidates(&candidates)?;
                discovered = true;
                send.try_send(Ok(Received::Discovered(identity, candidates)))
                    .map_err(|_| protocol_fault())?;
            }
            Event::Selected {
                identity,
                candidate,
            } => {
                if !discovered {
                    return Err(protocol_fault());
                }
                identity.validate()?;
                candidate.validate()?;
                if selection
                    .as_ref()
                    .is_some_and(|(bound, selected)| bound != &identity || selected != &candidate)
                {
                    return Err(protocol_fault());
                }
                selection = Some((identity.clone(), candidate.clone()));
                send.try_send(Ok(Received::Selected(identity, candidate)))
                    .map_err(|_| protocol_fault())?;
            }
            Event::PackageRebased { identity } => {
                identity.validate()?;
                let (current, _) = selection.as_mut().ok_or_else(protocol_fault)?;
                if !same_package_rebase(current, &identity) {
                    return Err(protocol_fault());
                }
                *current = identity.clone();
                send.try_send(Ok(Received::PackageRebased(identity)))
                    .map_err(|_| protocol_fault())?;
            }
            Event::Frame { capture } => {
                if selection.as_ref().is_none_or(|(identity, selected)| {
                    selected.geometry != capture.geometry
                        || !same_selection(identity, &capture.identity)
                }) {
                    return Err(protocol_fault());
                }
                let pending =
                    PendingCapture::read_private(input, &capture.identity, &capture.geometry)?;
                // The worker drops its PNG before this receipt. Keep the parent's
                // worker reservation until both pixels and clean completion arrive.
                if !matches!(
                    read_message::<Event>(input, METADATA_BYTES)?,
                    Event::Captured { identity, session_clean: true } if identity == capture.identity
                ) {
                    return Err(protocol_fault());
                }
                send.try_send(Ok(Received::Captured(pending)))
                    .map_err(|_| protocol_fault())?;
            }
            Event::Terminal { primary, clean } => {
                let mut extra = [0];
                if input.read(&mut extra).map_err(|_| protocol_fault())? != 0 {
                    return Err(protocol_fault());
                }
                send.try_send(Ok(Received::Terminal { primary, clean }))
                    .map_err(|_| protocol_fault())?;
                return Ok(());
            }
            Event::Captured { .. } => return Err(protocol_fault()),
        }
    }
}

pub(super) fn protocol_fault() -> Fault {
    Fault::new(
        "CaptureTransport",
        "Invalid or incomplete private authoring worker protocol",
    )
}
pub(super) fn candidate_overflow() -> Fault {
    Fault::new(
        "NativeCandidateLimit",
        "Native discovery exceeds the process or eligible-window candidate limit",
    )
}
pub(super) fn cancelled() -> Fault {
    Fault::new("Cancelled", "Authoring capture worker was cancelled")
}
pub(super) fn expired() -> Fault {
    Fault::new(
        "NativeSelectionExpired",
        "The retained Engine is no longer available; explicitly start capture again",
    )
}
