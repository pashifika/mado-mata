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
    token: mp::CancellationToken,
}
impl ChildControl {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            deadline_ms: AtomicU64::new(DISCOVERY_LIMIT.as_millis() as u64),
            stopped_ms: AtomicU64::new(0),
            finished: AtomicBool::new(false),
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
        if self.elapsed_ms() >= self.deadline_ms.load(Ordering::Acquire) {
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
    let request: DiscoveryRequest = protocol::read_message(&mut input, protocol::COMMAND_BYTES)?;
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
            let mut capturing = false;
            loop {
                let next = protocol::read_message::<Command>(&mut input, protocol::COMMAND_BYTES);
                if capturing {
                    stop.cancel();
                    break;
                }
                match next {
                    Ok(command @ Command::Capture { .. }) => {
                        capturing = true;
                        if commands.try_send(command).is_err() {
                            stop.cancel();
                            break;
                        }
                    }
                    Ok(Command::Select { key }) if !key.is_empty() && key.len() <= 128 => {
                        if commands.try_send(Command::Select { key }).is_err() {
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
        Err(error) => return terminal(&mut output, &control, Some(error), true, None, None),
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
    control.deadline(SELECTION_LIMIT);
    let mut selected_key = None;
    let command = loop {
        if let Err(error) = control.check() {
            let error = if control.elapsed_ms() >= control.deadline_ms.load(Ordering::Acquire) {
                protocol::expired()
            } else {
                error
            };
            return terminal(&mut output, &control, Some(error), true, None, None);
        }
        match receive.recv_timeout(Duration::from_millis(5)) {
            Ok(Command::Select { key }) => {
                let validation = (|| {
                    let remaining = Duration::from_millis(
                        control
                            .deadline_ms
                            .load(Ordering::Acquire)
                            .saturating_sub(control.elapsed_ms()),
                    );
                    let operation = control.operation(remaining.min(DISCOVERY_LIMIT))?;
                    let retained = retained
                        .iter()
                        .find(|entry| entry.candidate.key == key)
                        .ok_or_else(stale)?;
                    revalidate(&engine, retained, &request, &operation)?;
                    control.check()?;
                    Ok(retained.candidate.clone())
                })();
                match validation {
                    Ok(candidate) => {
                        selected_key = Some(key);
                        protocol::write_event(&mut output, &Event::Selected { candidate })?;
                    }
                    Err(error) => {
                        return terminal(&mut output, &control, Some(error), true, None, None);
                    }
                }
            }
            Ok(command) => break command,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return terminal(
                    &mut output,
                    &control,
                    Some(protocol::cancelled()),
                    true,
                    None,
                    None,
                );
            }
        }
    };
    let Command::Capture {
        identity,
        candidate,
        timeout,
    } = command
    else {
        return terminal(
            &mut output,
            &control,
            Some(protocol::cancelled()),
            true,
            None,
            None,
        );
    };
    let prepared = (|| {
        identity.validate()?;
        if selected_key.as_deref() != Some(candidate.key.as_str()) {
            return Err(stale());
        }
        if !protocol::same_selection(&request.identity, &identity)
            || timeout.is_zero()
            || timeout > ACQUISITION_LIMIT
        {
            return Err(stale());
        }
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
        let outcome = acquire(
            &engine,
            selected.target,
            candidate.geometry,
            window,
            &identity,
            &authority,
            &operation,
            || control.check(),
        );
        Ok(outcome)
    })();
    // No target authority remains after this single-use worker exits.
    drop(retained);
    drop(engine);
    match prepared {
        Ok(outcome) => {
            let clean = matches!(outcome.cleanup, CaptureCleanup::Clean);
            let primary = outcome.primary.or_else(|| match outcome.cleanup {
                CaptureCleanup::Unconfirmed(error) => Some(error),
                CaptureCleanup::Clean => None,
            });
            terminal(
                &mut output,
                &control,
                primary,
                clean,
                Some(CaptureDescriptor {
                    identity,
                    geometry: candidate.geometry,
                }),
                outcome.transfer,
            )
        }
        Err(error) => terminal(&mut output, &control, Some(error), true, None, None),
    }
}

fn terminal(
    output: &mut impl Write,
    control: &ChildControl,
    mut primary: Option<Fault>,
    clean: bool,
    descriptor: Option<CaptureDescriptor>,
    mut transfer: Option<CaptureTransfer>,
) -> Result<bool, Fault> {
    if let Err(error) = control.check() {
        primary.get_or_insert(error);
        transfer = None;
    }
    if !clean {
        transfer = None;
    }
    // Bound publication/pipe blocking after the native session has closed.
    control.deadline(CLEANUP_LIMIT);
    protocol::write_event(
        output,
        &Event::Terminal {
            primary,
            clean,
            capture: if transfer.is_some() { descriptor } else { None },
        },
    )?;
    if let Some(transfer) = transfer {
        transfer.write_private(output)?;
    }
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
        let identity = target.process_identity().ok_or_else(unverifiable)?;
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
            continue;
        }
        processes.insert((identity.process_id().get(), identity.lifetime()));
        if processes.len() > CANDIDATE_LIMIT || retained.len() >= CANDIDATE_LIMIT {
            return Err(protocol::candidate_overflow());
        }
        // This key is an index inside one retained namespace, not a native key.
        let key = format!("candidate-{}", retained.len() + 1);
        let description = engine
            .describe_window(target.id(), operation)
            .map_err(|error| sdk_error("describe_window", error))?;
        if description.id() != target.id() {
            return Err(unverifiable());
        }
        let candidate = describe(&description, request, &key)?;
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

fn canonical_selection(request: &DiscoveryRequest) -> Result<(), Fault> {
    let canonical = request
        .executable_or_bundle
        .canonicalize()
        .map_err(|_| unverifiable())?;
    if canonical != request.executable_or_bundle {
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
        || !request.verified_processes.is_empty()
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
    #[cfg(target_os = "macos")]
    {
        request
            .verified_processes
            .iter()
            .any(|process| process.matches(process_id, lifetime, executable))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (process_id, lifetime);
        executable == request.executable_or_bundle
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn verified_mounted_process_matches_without_installation_path_fallback() {
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let request = DiscoveryRequest {
            identity: super::super::protocol_tests::identity(),
            executable_or_bundle: std::env::temp_dir().join("Selected.app"),
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
            (42, 777, request.executable_or_bundle.as_path()),
        ] {
            assert!(!process_matches(&request, pid, lifetime, path));
        }
        let unverified = DiscoveryRequest {
            verified_processes: Vec::new(),
            ..request
        };
        assert!(!process_matches(&unverified, 42, 777, &executable));
    }
}
