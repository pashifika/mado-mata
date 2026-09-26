use super::evidence::{Observer, receive_frames};
use super::protocol::{emit, frame};
use super::supervision::{
    OwnedChild, configure_engine_loader, retain_child_build, settled_evidence, terminal_primary,
};
use crate::environment::Configuration;
use crate::images::{DecodedImage, PayloadBytes, reserve_payload};
use crate::model::{Control, Fault, Limits, encode_bounded};
use crate::recognition_trial::{
    self as trial, Capabilities, TrialRequest, TrialSelection, WireRequest, WireSelection,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

const RESULT_FRAME_BYTES: usize = trial::DIAGNOSTIC_BYTES + 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    run: String,
    request: Option<WireRequest>,
    configuration: Option<Value>,
}

pub(crate) fn run_capabilities(
    executable: &Path,
    run: &str,
    control: &Arc<Control>,
    observer: &Observer,
) -> Result<Value, Fault> {
    supervise(
        executable,
        Invocation {
            run: run.into(),
            request: None,
            configuration: None,
        },
        None,
        None,
        control,
        observer,
    )
}

pub(crate) fn run_trial(
    executable: &Path,
    run: &str,
    request: TrialRequest,
    configuration: Value,
    capability: &Capabilities,
    control: &Arc<Control>,
    observer: &Observer,
) -> Result<Value, Fault> {
    let (selection, png) = match request.selection {
        TrialSelection::Ocr { zones } => (WireSelection::Ocr { zones }, None),
        TrialSelection::Template { template } => (
            WireSelection::Template {
                id: template.id,
                search: template.search,
                manifest: String::from_utf8(template.manifest)
                    .map_err(|_| trial::invalid("template manifest must be UTF-8 JSON"))?,
                png_bytes: template.png.len(),
                template_id: template.template_id,
                template_path: template.template_path,
            },
            Some(template.png),
        ),
    };
    let wire = WireRequest {
        identity: request.identity,
        width: request.frame.image.width,
        height: request.frame.image.height,
        selection,
    };
    let frame_bytes = wire.validate(Some(capability.max_ocr_zones))?;
    if request.frame.image.rgba.len() != frame_bytes {
        return Err(trial::invalid(
            "frame payload disagrees with original dimensions",
        ));
    }
    // Charge retained replay source + open-session clone + converted capture mapping.
    // Matching additionally retains BGR pixels and the correlation result raster.
    let mut reserved = frame_bytes
        .checked_mul(3)
        .ok_or_else(|| trial::invalid("payload size overflow"))?;
    if let WireSelection::Template {
        search,
        manifest,
        png_bytes,
        ..
    } = &wire.selection
    {
        let manifest: Value = serde_json::from_str(manifest)
            .map_err(|_| trial::invalid("invalid template manifest"))?;
        let png = png
            .as_ref()
            .ok_or_else(|| trial::invalid("template PNG is missing"))?;
        let info = crate::images::validate_png(png, crate::images::ImageKind::Crop)?;
        if manifest["templates"][0]["width"].as_u64() != Some(u64::from(info.width))
            || manifest["templates"][0]["height"].as_u64() != Some(u64::from(info.height))
        {
            return Err(trial::invalid(
                "template PNG dimensions disagree with its manifest",
            ));
        }
        let pixels = info.width as usize * info.height as usize;
        let roi = search.width as usize * search.height as usize;
        let scratch = crate::images::png_scratch_bytes(png, crate::images::ImageKind::Crop)?;
        // PNG transfer/Arc conversion, bounded PNG validation scratch, decoded BGR,
        // BGR search and 32-bit response map. Native library RSS is not inferred.
        reserved = reserved
            .checked_add(png_bytes * 2 + pixels * 3 + roi * 7 + scratch)
            .ok_or_else(|| trial::invalid("payload size overflow"))?;
    }
    let _reservation = reserve_payload(reserved)?;
    supervise(
        executable,
        Invocation {
            run: run.into(),
            request: Some(wire),
            configuration: Some(configuration),
        },
        Some(request.frame.image),
        png,
        control,
        observer,
    )
}

fn supervise(
    executable: &Path,
    invocation: Invocation,
    image: Option<Arc<DecodedImage>>,
    png: Option<PayloadBytes>,
    control: &Arc<Control>,
    observer: &Observer,
) -> Result<Value, Fault> {
    control.check()?;
    if !executable.is_file() {
        return Err(Fault::new(
            "EngineUnavailable",
            "the fixed engine runner artifact is unavailable",
        ));
    }
    let mut bytes = encode_bounded(&invocation, trial::DIAGNOSTIC_BYTES - 1)?;
    bytes.push(b'\n');
    let run = &invocation.run;
    let mut command = Command::new(executable);
    command
        .arg("recognition-child")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env("ORT_DISABLE_TELEMETRY", "1");
    if let Some(value) = &invocation.configuration {
        let configuration: Configuration<Value> = serde_json::from_value(value.clone())
            .map_err(|_| trial::invalid("invalid App resource configuration"))?;
        configure_engine_loader(&mut command, &configuration)?;
    }
    control.check()?;
    let mut child = OwnedChild(command.spawn().map_err(|error| {
        Fault::new(
            "EngineUnavailable",
            "the fixed engine runner could not start",
        )
        .with_context(json!({"stage":"child_startup","io_kind":format!("{:?}",error.kind())}))
    })?);
    let pid = child.0.id();
    let pipe_fault = || {
        Fault::new("Transport", "owned recognition pipe unavailable").with_context(
            json!({"cleanup":{"clean":false,"child_started":true},"stage":"child_startup"}),
        )
    };
    let mut input = child.0.stdin.take().ok_or_else(pipe_fault)?;
    let stdout = child.0.stdout.take().ok_or_else(pipe_fault)?;
    let mut stderr = child.0.stderr.take().ok_or_else(pipe_fault)?;
    let (sender, commands) = mpsc::sync_channel::<Value>(1);
    let mut sender = Some(sender);
    let writer = thread::spawn(move || -> Result<(), Fault> {
        let write = (|| -> std::io::Result<()> {
            input.write_all(&bytes)?;
            if let Some(image) = image {
                input.write_all(&image.rgba)?;
            }
            if let Some(png) = png {
                input.write_all(&png)?;
            }
            input.flush()?;
            if let Ok(command) = commands.recv() {
                writeln!(input, "{command}")?;
            }
            Ok(())
        })();
        write.map_err(|_| Fault::new("Transport", "recognition input channel closed"))
    });
    let (receiver, reader) = receive_frames(stdout, None, run, 0, RESULT_FRAME_BYTES);
    // Drain, but never publish library stderr containing local paths or recognized data.
    let errors = thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        let mut bytes = 0u64;
        while let Ok(count) = stderr.read(&mut buffer) {
            if count == 0 {
                break;
            }
            bytes = bytes.saturating_add(count as u64);
        }
        bytes
    });
    let mut build = None;
    let mut terminal = None;
    let mut milestones = Vec::new();
    let mut protocol_fault = None;
    let mut forced = false;
    let mut stop = None;
    let started = Instant::now();
    let mut accept = |message: Result<Value, Fault>| {
        match message {
            Ok(value) if value["run"] == *run && value["attempt"] == 1 => {
                if value["event"] == "ChildStarted" {
                    if let Err(error) = retain_child_build(&value, pid, &mut build) {
                        protocol_fault = Some(error);
                        return true;
                    }
                }
                observer.progress(&value);
                if value["event"] == "Terminal" {
                    if terminal.replace(value).is_some() {
                        protocol_fault =
                            Some(Fault::new("Transport", "duplicate recognition terminal"));
                    }
                } else {
                    milestones.push(value);
                }
            }
            Ok(_) => {
                protocol_fault = Some(Fault::new("StaleIdentity", "foreign recognition evidence"))
            }
            Err(error) => protocol_fault = Some(error),
        }
        protocol_fault.is_some()
    };
    let mut faulted = false;
    let mut containment_fault = None;
    let exit = loop {
        for message in receiver.try_iter() {
            faulted |= accept(message);
        }
        match child.0.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) => {}
            Err(_) => {
                containment_fault = Some(Fault::new(
                    "Containment",
                    "recognition child status unavailable",
                ));
                faulted = true;
            }
        }
        let capability_timeout = invocation.request.is_none()
            && started.elapsed() >= Duration::from_millis(trial::CONTAINMENT_MS);
        if stop.is_none() && (control.check().is_err() || capability_timeout || faulted) {
            stop = Some(Instant::now());
            if let Some(sender) = sender.take() {
                let _ = sender.try_send(json!({"command":"Stop","run":run,"attempt":1}));
            }
        }
        if stop.is_some_and(|at: Instant| at.elapsed() >= Duration::from_millis(trial::CLEANUP_MS))
            || control.elapsed_us() / 1000 >= trial::DURATION_MS + trial::CONTAINMENT_MS
        {
            forced = true;
            // Keep ownership even if OS containment/reaping fails. An error must not
            // detach transport workers or admit a successor while the PID is outstanding.
            let _ = child.0.kill();
            match child.0.wait() {
                Ok(exit) => break exit,
                Err(_) => {
                    containment_fault = Some(Fault::new(
                        "Containment",
                        "recognition child reaping remains unsettled",
                    ));
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    };
    drop(sender);
    let writer_fault = writer.join().ok().and_then(Result::err);
    let _ = reader.join();
    for message in receiver.try_iter() {
        accept(message);
    }
    drop(accept);
    if let Some(error) = writer_fault {
        protocol_fault.get_or_insert(error);
    }
    if let Some(error) = containment_fault {
        protocol_fault.get_or_insert(error);
    }
    let stderr_bytes = errors.join().unwrap_or_default();
    let evidence = settled_evidence(terminal, &milestones);
    let mut primary = terminal_primary(&evidence, protocol_fault.as_ref(), forced);
    if build.is_none() {
        primary = Some(Fault::new("EngineUnavailable", "engine child exited before authenticated startup")
            .with_context(json!({"stage":"child_startup","exit_code":exit.code(),"stderr_bytes_discarded":stderr_bytes})));
    }
    if primary.is_none() {
        primary = control.check().err();
    }
    if primary.is_none() && (forced || exit.code() == Some(124)) {
        primary = Some(Fault::new(
            "Timeout",
            "recognition child required bounded containment",
        ));
    }
    let clean =
        build.is_some() && evidence["cleanup"]["clean"] == true && exit.success() && !forced;
    let cleanup = if clean {
        evidence["cleanup"].clone()
    } else {
        json!({"clean":false,"status":"IncompleteCleanup","child_cleanup":evidence["cleanup"]})
    };
    let is_trial = invocation.request.is_some();
    Ok(
        json!({"version":1,"operation":if is_trial {"recognition_trial"} else {"recognition_capabilities"},
        "identity":invocation.request.as_ref().map(|request| &request.identity),"primary":primary,
        "cleanup":cleanup,"child_reaped":true,"forced":forced || exit.code()==Some(124),"exit_code":exit.code(),
        "result":if is_trial {evidence["result"].clone()} else {Value::Null},
        "capabilities":evidence["capabilities"],"text_contract":trial::TEXT_CONTRACT,
        "elapsed_us":started.elapsed().as_micros(),"image_payload_scope":"owned image buffers; native-library scratch is not a total RSS bound"}),
    )
}

fn limits() -> Limits {
    Limits {
        duration_ms: trial::DURATION_MS,
        readiness_ms: trial::DURATION_MS,
        wait_ms: trial::DURATION_MS,
        cleanup_ms: trial::CLEANUP_MS,
        containment_ms: trial::CONTAINMENT_MS,
        queue_capacity: 1,
        handles: 1,
        log_records: 1,
        log_bytes: 1,
        vm_bytes: 1,
        max_actions: 1,
        snapshot_files: 1,
        snapshot_bytes: 1,
    }
}

/// Dedicated CLI dispatch: bounded metadata, then exact binary RGBA/PNG bytes, then Stop.
pub fn recognition_child() -> Result<bool, Fault> {
    let mut input = BufReader::new(std::io::stdin());
    let bytes = frame(&mut input, trial::DIAGNOSTIC_BYTES)?
        .ok_or_else(|| Fault::new("Transport", "missing recognition invocation"))?;
    let invocation: Invocation = serde_json::from_slice(&bytes)
        .map_err(|_| trial::invalid("invalid recognition invocation"))?;
    if invocation.run.is_empty() || invocation.run.len() > 256 {
        return Err(trial::invalid("invalid recognition run identity"));
    }
    let control = Arc::new(Control::new(&limits()));
    let finished = Arc::new(AtomicBool::new(false));
    let watch = control.clone();
    let watch_finished = finished.clone();
    thread::spawn(move || {
        while !watch_finished.load(Ordering::Acquire) {
            let _ = watch.check();
            let stop = watch.stop_us.load(Ordering::Acquire);
            if stop != 0 && watch.elapsed_us().saturating_sub(stop) > trial::CLEANUP_MS * 1000 {
                std::process::exit(124);
            }
            thread::sleep(Duration::from_millis(2));
        }
    });
    let run = invocation.run;
    emit(
        &json!({"event":"ChildStarted","run":run,"attempt":1,"pid":std::process::id(),
        "operation":if invocation.request.is_some() {"recognition_trial"} else {"recognition_capabilities"},
        "build":crate::report::build_identity()}),
    )?;
    let prepared = (|| {
        let capabilities = trial::capabilities()?;
        let Some(request) = invocation.request else {
            if invocation.configuration.is_some() {
                return Err(trial::invalid("capability probe does not accept resources"));
            }
            return Ok((None, capabilities));
        };
        let frame_bytes = request.validate(Some(capabilities.max_ocr_zones))?;
        let configuration = invocation.configuration.ok_or_else(|| {
            crate::environment::blocked("configuration_unset", "OCR resources are required")
        })?;
        let png_bytes = request.png_bytes();
        let reservation = reserve_payload(frame_bytes + png_bytes)?;
        let mut rgba = vec![0; frame_bytes];
        input
            .read_exact(&mut rgba)
            .map_err(|_| Fault::new("Transport", "incomplete recognition RGBA frame"))?;
        let mut png = vec![0; png_bytes];
        input
            .read_exact(&mut png)
            .map_err(|_| Fault::new("Transport", "incomplete recognition template PNG"))?;
        Ok((
            Some((request, rgba, png, configuration, reservation)),
            capabilities,
        ))
    })();
    let stop_control = control.clone();
    let stop_run = run.clone();
    thread::spawn(move || {
        let reason = match frame(&mut input, 1024) {
            Ok(Some(bytes))
                if serde_json::from_slice::<Value>(&bytes).ok()
                    == Some(json!({"command":"Stop","run":stop_run,"attempt":1})) =>
            {
                "Stop"
            }
            Ok(None) => "ControlLost",
            _ => "InvalidControl",
        };
        stop_control.cancel();
        let _ = emit(
            &json!({"event":"StopRequested","run":stop_run,"attempt":1,"reason":reason,"at_us":stop_control.elapsed_us()}),
        );
        let _ = emit(
            &json!({"event":"AdmissionClosed","run":stop_run,"attempt":1,"at_us":stop_control.elapsed_us()}),
        );
    });
    let (primary, result, capabilities, cleanup) = match prepared {
        Ok((None, capabilities)) => (
            None,
            Value::Null,
            json!(capabilities),
            json!({"clean":true,"status":"CleanupFinished","session_closed":true}),
        ),
        Ok((Some((request, rgba, png, configuration, _reservation)), _)) => {
            let settled = |result: &Result<Value, Fault>| {
                let _ = emit(&json!({"event":"EntrySettled","run":run,"attempt":1,
                    "primary":result.as_ref().err(),"result":result.as_ref().ok()}));
            };
            let (result, cleanup) =
                trial::execute(request, rgba, png, configuration, control.clone(), settled);
            match result {
                Ok(result) => (None, result, Value::Null, cleanup),
                Err(mut primary) => {
                    primary.bound_diagnostics();
                    (Some(primary), Value::Null, Value::Null, cleanup)
                }
            }
        }
        Err(mut primary) => {
            primary.bound_diagnostics();
            (
                Some(primary),
                Value::Null,
                Value::Null,
                json!({"clean":true,"status":"CleanupFinished","session_closed":true}),
            )
        }
    };
    control.cancel();
    let emission = emit(
        &json!({"event":"Terminal","run":run,"attempt":1,"primary":primary,
        "result":result,"capabilities":capabilities,"cleanup":cleanup}),
    );
    if cleanup["clean"] != true {
        loop {
            thread::park_timeout(Duration::from_millis(10));
        }
    }
    finished.store(true, Ordering::Release);
    emission.map(|()| true)
}
