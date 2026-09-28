use super::protocol::{self, CaptureDescriptor, Command, Event};
use super::*;
use mado_pilot as mp;
use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64},
    mpsc,
};
use std::thread;
use std::time::Instant;

struct ChildControl {
    started: Instant,
    deadline_ms: AtomicU64,
    stopped_ms: AtomicU64,
    finished: AtomicBool,
    pending: AtomicBool,
    token: mp::CancellationToken,
}
impl ChildControl {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            deadline_ms: AtomicU64::new(DISCOVERY_LIMIT.as_millis() as u64),
            stopped_ms: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            token: mp::CancellationToken::new(),
        }
    }
    fn elapsed_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX - 1)) as u64
            + 1
    }
    fn deadline(&self, duration: Duration) {
        self.deadline_ms.store(
            self.elapsed_ms()
                .saturating_add(duration.as_millis() as u64),
            Ordering::Release,
        );
    }
    fn idle(&self) {
        self.deadline_ms.store(0, Ordering::Release);
    }

    fn cancel(&self) {
        let _ = self.stopped_ms.compare_exchange(
            0,
            self.elapsed_ms(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.token.cancel();
    }
    fn check(&self) -> Result<(), Fault> {
        if self.stopped_ms.load(Ordering::Acquire) != 0 {
            return Err(protocol::cancelled());
        }
        let deadline = self.deadline_ms.load(Ordering::Acquire);
        if deadline != 0 && self.elapsed_ms() >= deadline {
            self.cancel();
            return Err(Fault::new(
                "DeadlineExceeded",
                "Authoring worker deadline expired",
            ));
        }
        Ok(())
    }
    fn operation(&self, limit: Duration) -> Result<mp::OperationContext, Fault> {
        self.check()?;
        mp::OperationContext::new()
            .with_cancellation(self.token.clone())
            .with_timeout(limit)
            .map_err(|error| sdk_error("operation", error))
    }
}

/// Private entry only: an EOF/invalid command revokes the token even if a native
/// call or stdout is blocked. The watchdog contains only this owned executable.
pub(super) fn child() -> Result<bool, Fault> {
    let control = Arc::new(ChildControl::new());
    let watch = control.clone();
    thread::Builder::new()
        .name("authoring-native-watchdog".into())
        .spawn(move || {
            while !watch.finished.load(Ordering::Acquire) {
                let _ = watch.check();
                let stopped = watch.stopped_ms.load(Ordering::Acquire);
                if stopped != 0
                    && watch.elapsed_ms().saturating_sub(stopped)
                        >= (CLEANUP_LIMIT + CONTAINMENT_LIMIT).as_millis() as u64
                {
                    std::process::exit(124);
                }
                thread::sleep(Duration::from_millis(2));
            }
        })
        .map_err(|_| protocol::protocol_fault())?;
    let mut input = std::io::stdin();
    let mut request: DiscoveryRequest =
        protocol::read_message(&mut input, protocol::COMMAND_BYTES)?;
    request.validate()?;
    let remaining = request
        .timeout
        .checked_sub(control.started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| Fault::new("DeadlineExceeded", "Discovery request deadline expired"))?;
    control.deadline(remaining);
    let operation = control.operation(remaining)?;
    let (commands, receive) = mpsc::sync_channel(1);
    let stop = control.clone();
    thread::Builder::new()
        .name("authoring-native-parent".into())
        .spawn(move || {
            loop {
                match protocol::read_message::<Command>(&mut input, protocol::COMMAND_BYTES) {
                    Ok(
                        command @ (Command::Capture { .. }
                        | Command::Select { .. }
                        | Command::RebasePackage { .. }),
                    ) => {
                        if stop.pending.swap(true, Ordering::AcqRel)
                            || commands.try_send(command).is_err()
                        {
                            stop.cancel();
                            break;
                        }
                    }
                    _ => {
                        stop.cancel();
                        break;
                    }
                }
            }
        })
        .map_err(|_| protocol::protocol_fault())?;
    let mut output = std::io::stdout();
    let discovered = discover(&request, &operation, &control);
    let (engine, retained) = match discovered {
        Ok(value) => value,
        Err(error) => return terminal(&mut output, &control, Some(error), true),
    };
    control.check()?;
    protocol::write_event(
        &mut output,
        &Event::Discovered {
            identity: request.identity.clone(),
            engine_revision: crate::model::ENGINE_REVISION.into(),
            candidates: retained
                .iter()
                .map(|entry| entry.candidate.clone())
                .collect(),
        },
    )?;
    control.check()?;
    control.idle();
    let mut selected_key: Option<String> = None;
    let mut request_ids = BTreeSet::new();
    let (primary, clean) = loop {
        if let Err(error) = control.check() {
            break (Some(error), true);
        }
        let command = match receive.recv_timeout(Duration::from_millis(5)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break (Some(protocol::cancelled()), true);
            }
        };
        match command {
            Command::Select {
                key,
                binding_revision,
            } => {
                control.deadline(DISCOVERY_LIMIT);
                let validation = (|| {
                    let mut bound = request.identity.clone();
                    bound.binding_revision = binding_revision;
                    bound.validate()?;
                    if selected_key
                        .as_ref()
                        .is_some_and(|selected| selected != &key || bound != request.identity)
                    {
                        return Err(stale());
                    }
                    let operation = control.operation(DISCOVERY_LIMIT)?;
                    let selected = retained
                        .iter()
                        .find(|entry| entry.candidate.key == key)
                        .ok_or_else(stale)?;
                    revalidate(&engine, selected, &request, &operation)?;
                    control.check()?;
                    Ok((bound, selected.candidate.clone()))
                })();
                let (bound, candidate) = match validation {
                    Ok(value) => value,
                    Err(error) => break (Some(error), true),
                };
                request.identity = bound;
                selected_key = Some(key);
                control.pending.store(false, Ordering::Release);
                protocol::write_event(
                    &mut output,
                    &Event::Selected {
                        identity: request.identity.clone(),
                        candidate,
                    },
                )?;
                control.check()?;
                control.idle();
            }
            Command::RebasePackage { identity } => {
                control.deadline(DISCOVERY_LIMIT);
                if let Err(error) = identity.validate().and_then(|()| {
                    if selected_key.is_none()
                        || !protocol::same_package_rebase(&request.identity, &identity)
                    {
                        return Err(stale());
                    }
                    control.check()
                }) {
                    break (Some(error), true);
                }
                // Fresh application/package proof belongs to the host. No SDK
                // discovery or native target replacement occurs for this rebase.
                request.identity = identity;
                control.pending.store(false, Ordering::Release);
                protocol::write_event(
                    &mut output,
                    &Event::PackageRebased {
                        identity: request.identity.clone(),
                    },
                )?;
                control.check()?;
                control.idle();
            }
            Command::Capture {
                identity,
                candidate,
                timeout,
            } => {
                let prepared = (|| {
                    identity.validate()?;
                    if selected_key.as_deref() != Some(candidate.key.as_str())
                        || !protocol::same_selection(&request.identity, &identity)
                        || timeout.is_zero()
                        || timeout > ACQUISITION_LIMIT
                    {
                        return Err(stale());
                    }
                    protocol::admit_request(&mut request_ids, &identity.request_id)?;
                    control.check()?;
                    control.deadline(timeout);
                    let operation = control.operation(timeout)?;
                    let selected = retained
                        .iter()
                        .find(|entry| entry.candidate.key == candidate.key)
                        .ok_or_else(stale)?;
                    if selected.candidate != candidate {
                        return Err(stale());
                    }
                    let window = revalidate(&engine, selected, &request, &operation)?;
                    control.check()?;
                    if engine.reads_permissions() {
                        let permission = engine
                            .permission(mp::PermissionKind::ScreenCapture, &operation)
                            .map_err(|error| sdk_error("capture_permission", error))?;
                        if !permission.is_granted() {
                            return Err(Fault::new("NativeCapturePermission", "Screen capture is not granted to mado-runtime-comparison; review its Screen Recording access in macOS System Settings")
                                .with_context(serde_json::json!({"stage":"capture_permission","status":permission.state().as_str(),
                                    "responsible_executable":"mado-runtime-comparison"})));
                        }
                    }
                    let authority = CaptureAuthority::new(identity.clone())?;
                    Ok(acquire(
                        &engine,
                        selected.target,
                        candidate.geometry,
                        window,
                        &identity,
                        &authority,
                        &operation,
                        || control.check(),
                    ))
                })();
                let outcome = match prepared {
                    Ok(outcome) => outcome,
                    Err(error) => break (Some(error), true),
                };
                let clean = matches!(outcome.cleanup, CaptureCleanup::Clean);
                let primary = outcome.primary.or_else(|| match outcome.cleanup {
                    CaptureCleanup::Unconfirmed(error) => Some(error),
                    CaptureCleanup::Clean => None,
                });
                let transfer = match (primary, outcome.transfer) {
                    (None, Some(transfer)) if clean => transfer,
                    (primary, _) => {
                        break (primary.or_else(|| Some(protocol::protocol_fault())), clean);
                    }
                };
                if let Err(error) = control.check() {
                    break (Some(error), true);
                }
                // acquire() has committed the original frame and cleanly closed its
                // session. The provider Engine and original TargetId remain owned.
                protocol::write_event(
                    &mut output,
                    &Event::Frame {
                        capture: CaptureDescriptor {
                            identity: identity.clone(),
                            geometry: candidate.geometry,
                        },
                    },
                )?;
                transfer.write_private(&mut output)?;
                drop(transfer);
                control.check()?;
                control.pending.store(false, Ordering::Release);
                protocol::write_event(
                    &mut output,
                    &Event::Captured {
                        identity,
                        session_clean: true,
                    },
                )?;
                control.check()?;
                control.idle();
            }
            Command::Cancel => break (Some(protocol::cancelled()), true),
        }
    };
    control.deadline(CLEANUP_LIMIT);
    drop(retained);
    drop(engine);
    terminal(&mut output, &control, primary, clean)
}

fn terminal(
    output: &mut impl Write,
    control: &ChildControl,
    mut primary: Option<Fault>,
    clean: bool,
) -> Result<bool, Fault> {
    if let Err(error) = control.check() {
        primary.get_or_insert(error);
    }
    control.deadline(CLEANUP_LIMIT);
    protocol::write_event(output, &Event::Terminal { primary, clean })?;
    if !clean {
        control.cancel();
        loop {
            thread::park_timeout(Duration::from_millis(5));
        }
    }
    control.finished.store(true, Ordering::Release);
    Ok(true)
}

struct Retained {
    target: mp::TargetId,
    candidate: Candidate,
}

fn revalidate(
    engine: &mp::Engine,
    retained: &Retained,
    request: &DiscoveryRequest,
    operation: &mp::OperationContext,
) -> Result<mp::WindowGeometry, Fault> {
    // This operation addresses the original provider record, never rediscovery.
    let description = engine
        .describe_window(retained.target, operation)
        .map_err(|error| sdk_error("describe_window", error))?;
    if description.id() != retained.target {
        return Err(unverifiable());
    }
    let current = describe(&description, request, &retained.candidate.key)?;
    if current != retained.candidate {
        return Err(Fault::new(
            "CaptureGeometry",
            "Selected window correspondence or geometry changed; select again",
        ));
    }
    Ok(description.window().ok_or_else(unverifiable)?.geometry())
}

fn metadata_engine() -> Result<mp::Engine, Fault> {
    #[cfg(target_os = "macos")]
    let engine = mp::macos_engine(mp::NativeEngineRequest::new());
    #[cfg(windows)]
    let engine = mp::windows_engine(mp::NativeEngineRequest::new());
    #[cfg(any(target_os = "macos", windows))]
    {
        engine.map_err(|error| sdk_error("engine_initialization", error))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Err(Fault::new(
            "NativePlatformUnsupported",
            "Native authoring requires macOS or Windows",
        ))
    }
}

fn discover(
    request: &DiscoveryRequest,
    operation: &mp::OperationContext,
    control: &ChildControl,
) -> Result<(mp::Engine, Vec<Retained>), Fault> {
    canonical_selection(request)?;
    control.check()?;
    // The public non-OCR constructors initialize no models, load no ONNX runtime,
    // open no session and request no permissions. Input is never opened or used.
    let engine = metadata_engine()?;
    control.check()?;
    // The facade lists provider inventory; the 64-candidate policy bounds the
    // verified matching set, not OS inventory allocation inside that operation.
    let targets = engine
        .discover(operation)
        .map_err(|error| sdk_error("discovery", error))?;
    let mut retained = Vec::new();
    let mut processes = BTreeSet::new();
    for target in targets {
        control.check()?;
        if target.capability().kind() != Some(mp::TargetKind::Window)
            || !target.capability().capture().may_attempt()
        {
            continue;
        }
        if request
            .exact_window_title
            .as_ref()
            .is_some_and(|title| target.name() != title)
        {
            continue;
        }
        let candidate = (|| {
            let identity = target.process_identity().ok_or_else(unverifiable)?;
            if !request.verified_processes.is_empty()
                && !request.verified_processes.iter().any(|process| {
                    process.process_id == identity.process_id().get()
                        && process.process_lifetime == identity.lifetime()
                })
            {
                return Ok(None);
            }
            let executable = identity
                .executable_path()
                .canonicalize()
                .map_err(|_| unverifiable())?;
            if !process_matches(
                request,
                identity.process_id().get(),
                identity.lifetime(),
                &executable,
            ) {
                return Ok(None);
            }
            // This key is an index inside one retained namespace, not a native key.
            let key = format!("candidate-{}", retained.len() + 1);
            let description = engine
                .describe_window(target.id(), operation)
                .map_err(|error| sdk_error("describe_window", error))?;
            if description.id() != target.id() {
                return Err(unverifiable());
            }
            describe(&description, request, &key).map(Some)
        })();
        let candidate = match inventory_candidate(request, candidate)? {
            Some(candidate) => candidate,
            None => continue,
        };
        processes.insert((candidate.process_id, candidate.process_lifetime));
        if processes.len() > CANDIDATE_LIMIT || retained.len() >= CANDIDATE_LIMIT {
            return Err(protocol::candidate_overflow());
        }
        retained.push(Retained {
            target: target.id(),
            candidate,
        });
    }
    if retained.is_empty() {
        return Err(Fault::new(
            "NativeTargetUnavailable",
            "No verified eligible window satisfies the selected application and exact-window constraints",
        ));
    }
    control.check()?;
    Ok((engine, retained))
}

fn inventory_candidate(
    request: &DiscoveryRequest,
    result: Result<Option<Candidate>, Fault>,
) -> Result<Option<Candidate>, Fault> {
    match result {
        // Broad inventory can exclude an unusable individual window. A constrained
        // request must expose a relevant target failure, never silently substitute.
        Err(error)
            if request.executable_or_bundle.is_none()
                && request.exact_window_title.is_none()
                && request.verified_processes.is_empty()
                && (matches!(
                    error.category.as_str(),
                    "NativeIdentityUnverifiable"
                        | "CaptureGeometry"
                        | "CaptureImage"
                        | "CaptureTransport"
                ) || (error.category == "NativeCapture"
                    && matches!(
                        error.context["status"].as_str(),
                        Some("target_lost" | "unsupported" | "permission_denied")
                    ))) =>
        {
            Ok(None)
        }
        result => result,
    }
}

fn canonical_selection(request: &DiscoveryRequest) -> Result<(), Fault> {
    let Some(selection) = &request.executable_or_bundle else {
        return Ok(());
    };
    let canonical = selection.canonicalize().map_err(|_| unverifiable())?;
    if &canonical != selection {
        return Err(unverifiable());
    }
    #[cfg(target_os = "macos")]
    if !canonical.is_dir()
        || request.verified_processes.is_empty()
        || canonical
            .extension()
            .is_none_or(|extension| extension != "app")
    {
        return Err(unverifiable());
    }
    #[cfg(windows)]
    if !canonical.is_file()
        || canonical
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("exe"))
    {
        return Err(unverifiable());
    }
    Ok(())
}

fn describe(
    description: &mp::TargetDescription,
    request: &DiscoveryRequest,
    key: &str,
) -> Result<Candidate, Fault> {
    canonical_selection(request)?;
    if description.capability().kind() != Some(mp::TargetKind::Window)
        || !description.capability().capture().may_attempt()
        || request
            .exact_window_title
            .as_ref()
            .is_some_and(|title| description.name() != title)
    {
        return Err(unverifiable());
    }
    let process = description.process_identity().ok_or_else(unverifiable)?;
    let executable_path = process
        .executable_path()
        .canonicalize()
        .map_err(|_| unverifiable())?;
    let application_bundle_path = process
        .application_bundle_path()
        .map(|path| path.canonicalize().map_err(|_| unverifiable()))
        .transpose()?;
    #[cfg(target_os = "macos")]
    if application_bundle_path.is_none() {
        return Err(unverifiable());
    }
    if !process_matches(
        request,
        process.process_id().get(),
        process.lifetime(),
        &executable_path,
    ) {
        return Err(unverifiable());
    }
    let window = description.window().ok_or_else(unverifiable)?;
    let native_window = match window.id() {
        mp::NativeWindowId::Macos(id) => NativeWindow::Macos(id.get()),
        mp::NativeWindowId::Windows(id) => NativeWindow::Windows(id.get()),
        _ => return Err(unverifiable()),
    };
    let area = match window.geometry().area() {
        mp::WindowCaptureArea::MacosWindow => CaptureArea::MacosWindow,
        mp::WindowCaptureArea::WindowsExtendedFrame => CaptureArea::WindowsExtendedFrame,
        mp::WindowCaptureArea::WindowsClient => CaptureArea::WindowsClient,
        _ => return Err(unverifiable()),
    };
    let candidate = Candidate {
        key: key.into(),
        label: description.name().into(),
        process_id: process.process_id().get(),
        process_lifetime: process.lifetime(),
        executable_path,
        application_bundle_path,
        native_window,
        area,
        geometry: geometry(window.geometry())?,
    };
    candidate.validate()?;
    Ok(candidate)
}

fn process_matches(
    request: &DiscoveryRequest,
    process_id: u32,
    lifetime: u64,
    executable: &std::path::Path,
) -> bool {
    if !request.verified_processes.is_empty()
        && !request
            .verified_processes
            .iter()
            .any(|process| process.matches(process_id, lifetime, executable))
    {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        request.executable_or_bundle.is_none() || !request.verified_processes.is_empty()
    }
    #[cfg(not(target_os = "macos"))]
    {
        request
            .executable_or_bundle
            .as_ref()
            .is_none_or(|path| executable == path)
    }
}

pub(super) fn geometry(window: mp::WindowGeometry) -> Result<CaptureGeometry, Fault> {
    let extent = window.extent();
    let placement = window.placement();
    let (x, y) = placement.desktop_origin();
    let scale = placement.desktop_scale();
    let unit = match window.desktop_unit() {
        mp::NativeDesktopUnit::MacosPoints => CoordinateUnit::MacosGlobalPoints,
        mp::NativeDesktopUnit::WindowsPhysicalPixels => CoordinateUnit::WindowsPhysicalPixels,
        _ => return Err(unverifiable()),
    };
    let geometry = CaptureGeometry {
        x,
        y,
        width: f64::from(extent.width()) / scale.x(),
        height: f64::from(extent.height()) / scale.y(),
        pixels_per_unit_x: scale.x(),
        pixels_per_unit_y: scale.y(),
        pixel_width: extent.width(),
        pixel_height: extent.height(),
        unit,
    };
    geometry.validate()?;
    Ok(geometry)
}

fn unverifiable() -> Fault {
    Fault::new(
        "NativeIdentityUnverifiable",
        "Native window or process correspondence cannot be independently verified",
    )
}
fn sdk_error(stage: &str, error: mp::Error) -> Fault {
    super::acquisition::sdk_error(stage, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn verified_mounted_process_matches_without_installation_path_fallback() {
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let request = DiscoveryRequest {
            identity: super::super::protocol_tests::identity(),
            executable_or_bundle: Some(std::env::temp_dir().join("Selected.app")),
            exact_window_title: None,
            verified_processes: vec![ProcessCorrespondence {
                process_id: 42,
                process_lifetime: 777,
                executable_path: executable.clone(),
            }],
            timeout: DISCOVERY_LIMIT,
        };
        assert!(process_matches(&request, 42, 777, &executable));
        for (pid, lifetime, path) in [
            (43, 777, executable.as_path()),
            (42, 778, executable.as_path()),
            (
                42,
                777,
                request.executable_or_bundle.as_ref().unwrap().as_path(),
            ),
        ] {
            assert!(!process_matches(&request, pid, lifetime, path));
        }
        let unverified = DiscoveryRequest {
            verified_processes: Vec::new(),
            ..request
        };
        assert!(!process_matches(&unverified, 42, 777, &executable));
        let unbound = DiscoveryRequest {
            executable_or_bundle: None,
            ..unverified
        };
        assert!(process_matches(&unbound, 42, 777, &executable));
    }

    #[test]
    fn broad_inventory_skips_only_individually_unusable_windows() {
        let mut request = DiscoveryRequest {
            identity: super::super::protocol_tests::identity(),
            executable_or_bundle: None,
            exact_window_title: None,
            verified_processes: Vec::new(),
            timeout: DISCOVERY_LIMIT,
        };
        assert!(
            inventory_candidate(&request, Err(unverifiable()))
                .unwrap()
                .is_none()
        );
        let valid = super::super::protocol_tests::candidate();
        assert!(inventory_candidate(&request, Ok(Some(valid.clone()))).unwrap() == Some(valid));
        for error in [
            protocol::candidate_overflow(),
            protocol::cancelled(),
            sdk_error(
                "describe_window",
                mp::Error::new(mp::Status::DeadlineExceeded, "expired"),
            ),
        ] {
            let category = error.category.clone();
            assert!(
                matches!(inventory_candidate(&request, Err(error)), Err(error) if error.category == category)
            );
        }
        request.exact_window_title = Some("Selected title".into());
        assert!(
            matches!(inventory_candidate(&request, Err(unverifiable())), Err(error) if error.category == "NativeIdentityUnverifiable")
        );
        request.exact_window_title = None;
        request.executable_or_bundle = Some(std::env::current_exe().unwrap());
        assert!(inventory_candidate(&request, Err(unverifiable())).is_err());
        request.executable_or_bundle = None;
        request.verified_processes.push(ProcessCorrespondence {
            process_id: 1,
            process_lifetime: 1,
            executable_path: std::env::current_exe().unwrap(),
        });
        assert!(inventory_candidate(&request, Err(unverifiable())).is_err());
    }

    #[test]
    fn idle_engine_has_no_lifetime_deadline_but_operations_and_eof_remain_bounded() {
        let control = ChildControl::new();
        control.deadline_ms.store(1, Ordering::Release);
        control.idle();
        control.check().unwrap();
        assert!(
            control
                .operation(ACQUISITION_LIMIT)
                .unwrap()
                .remaining()
                .unwrap()
                <= ACQUISITION_LIMIT
        );
        control.cancel();
        assert_eq!(control.check().unwrap_err().category, "Cancelled");
        let expired = ChildControl::new();
        expired.deadline_ms.store(1, Ordering::Release);
        assert_eq!(expired.check().unwrap_err().category, "DeadlineExceeded");
        assert_eq!(expired.check().unwrap_err().category, "Cancelled");
    }
}
