use super::*;
use crate::environment::{Configuration, blocked, validate_ocr};
use crate::model::Control;
use mado_pilot as mp;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

struct Cancellation {
    token: mp::CancellationToken,
    done: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Cancellation {
    fn new(control: Arc<Control>) -> Result<Self, Fault> {
        let token = mp::CancellationToken::new();
        let worker_token = token.clone();
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        let worker = thread::Builder::new()
            .name("recognition-cancellation".into())
            .spawn(move || {
                while !worker_done.load(Ordering::Acquire) {
                    if control.check().is_err() {
                        worker_token.cancel();
                        break;
                    }
                    thread::park_timeout(Duration::from_millis(1));
                }
            })
            .map_err(|_| {
                Fault::new(
                    "Initialization",
                    "could not start recognition cancellation bridge",
                )
            })?;
        Ok(Self {
            token,
            done,
            worker: Some(worker),
        })
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.token.cancel();
        self.done.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

fn fault(stage: &str, error: mp::Error) -> Fault {
    let category = match error.status() {
        mp::Status::Cancelled => "Cancelled",
        mp::Status::DeadlineExceeded => "Timeout",
        mp::Status::InvalidArgument => "RecognitionRequest",
        _ => "Blocked",
    };
    // Backend details can contain model paths or observed text; keep only typed status.
    Fault::new(category, "saved-frame recognition engine operation failed")
        .with_context(json!({"stage":stage,"status":error.status().as_str(),"engine_revision":crate::model::ENGINE_REVISION}))
}

fn context(token: &mp::CancellationToken, millis: u64) -> Result<mp::OperationContext, Fault> {
    if millis == 0 {
        return Err(Fault::new(
            "Timeout",
            "recognition operation deadline expired",
        ));
    }
    mp::OperationContext::new()
        .with_cancellation(token.clone())
        .with_timeout(Duration::from_millis(millis))
        .map_err(|error| fault("operation_context", error))
}

pub(crate) fn execute(
    request: WireRequest,
    rgba: Vec<u8>,
    png: Vec<u8>,
    configuration: Value,
    control: Arc<Control>,
    settled: impl FnOnce(&Result<Value, Fault>),
) -> (Result<Value, Fault>, Value) {
    let mut session = None;
    let mut engine = None;
    let mut bridge = None;
    let primary = (|| {
        control.check()?;
        let bytes = request.validate(Some(mp::MAX_OCR_ZONES))?;
        if rgba.len() != bytes || png.len() != request.png_bytes() {
            return Err(invalid(
                "binary image payload length disagrees with the request",
            ));
        }
        let config: Configuration<Value> = serde_json::from_value(configuration).map_err(|_| {
            blocked(
                "configuration_validation",
                "unsupported recognition resource configuration",
            )
        })?;
        if config.version != 1 || config.native.is_some() || config.replay.is_some() {
            return Err(blocked(
                "configuration_validation",
                "recognition accepts only App OCR resources, not targets or corpora",
            ));
        }
        bridge = Some(Cancellation::new(control.clone())?);
        let token = &bridge.as_ref().expect("bridge initialized").token;
        let operation = context(
            token,
            DURATION_MS.saturating_sub(control.elapsed_us() / 1000),
        )?;
        validate_ocr(&config, &control)?;
        let profile = match config.ocr.profile.as_str() {
            mp::ACCEPTED_G004_PROFILE_ID => mp::OcrProviderProfile::NativeG004,
            mp::ACCEPTED_BOUNDED_PROFILE_ID => mp::OcrProviderProfile::BoundedDetector,
            _ => return Err(blocked("ocr_unsupported", "unsupported OCR profile")),
        };
        let provider = mp::OcrProviderConfig::new(
            profile,
            mp::OcrExecutionProviderPolicy::Cpu,
            &config.ocr.model_root,
            &config.ocr.runtime.path,
        );
        let descriptor = mp::FrameDescriptor::packed(
            mp::PixelExtent::new(request.width, request.height),
            mp::PixelFormat::Rgba8,
        )
        .map_err(|error| fault("frame_descriptor", error.into()))?;
        let replay = mp::replay::ReplayFrame::new(
            descriptor,
            mp::MonotonicInstant::ORIGIN,
            mp::Continuity::Continuous,
            None,
            rgba.into_boxed_slice(),
        )
        .map_err(|error| fault("saved_frame", error.into()))?;
        let target = mp::replay::ReplayTarget::new("authoring-saved-frame", vec![replay])
            .map_err(|error| fault("saved_frame", error.into()))?;
        let source = mp::replay::ReplaySource::from_targets(vec![target])
            .map_err(|error| fault("saved_frame", error.into()))?;
        engine = Some(
            mp::replay_engine_with_ocr_provider(
                mp::ReplayEngineRequest::new(source),
                &provider,
                &operation,
            )
            .map_err(|error| fault("backend_initialization", error))?,
        );
        let engine = engine.as_ref().expect("engine initialized");
        validate_ocr(&config, &control)?;
        let provider = engine.ocr_provider().ok_or_else(|| {
            blocked(
                "backend_initialization",
                "OCR provider identity unavailable",
            )
        })?;
        if provider.active_provider() != mp::OcrExecutionProvider::Cpu
            || provider.initialization_fell_back()
        {
            return Err(blocked(
                "ocr_provider_mismatch",
                "configured CPU provider was not selected exactly",
            ));
        }
        let targets = engine
            .discover(&operation)
            .map_err(|error| fault("saved_frame_discovery", error))?;
        if targets.len() != 1 {
            return Err(blocked(
                "saved_frame_discovery",
                "saved frame must expose exactly one replay target",
            ));
        }
        session = Some(
            engine
                .open(targets[0].id(), &mp::OpenRequest::new(), &operation)
                .map_err(|error| fault("session_open", error))?,
        );
        let session = session.as_ref().expect("session initialized");
        let frame = session
            .acquire_frame(&mp::FrameRequest::latest(), &operation)
            .map_err(|error| fault("saved_frame_acquisition", error))?;
        let diagnostics = match &request.selection {
            WireSelection::Ocr { zones } => ocr(engine, session, &frame, zones, &operation)?,
            WireSelection::Template {
                id,
                search,
                manifest,
                template_id,
                template_path,
                ..
            } => {
                let info = crate::images::validate_png(&png, ImageKind::Crop)?;
                if info.width > search.width || info.height > search.height {
                    return Err(invalid("template pixels do not fit the search ROI"));
                }
                let package = mp::MemoryPackage::new()
                    .with_entry(
                        "madopilot-package.json",
                        Arc::<[u8]>::from(manifest.as_bytes()),
                    )
                    .with_entry(template_path, Arc::<[u8]>::from(png));
                let package = engine
                    .load_package(&mp::PackageSource::memory(package), &operation)
                    .map_err(|_| {
                        blocked(
                            "template_validation",
                            "reviewed template manifest/hash/dimensions could not be validated",
                        )
                    })?;
                let prepared = engine
                    .prepare_from_package(&package, template_id, &operation)
                    .map_err(|error| fault("template_preparation", error.into()))?;
                let view = frame
                    .view(rect(*search)?, mp::ClipPolicy::Reject)
                    .map_err(|error| fault("template_roi", error.into()))?;
                // One extra match detects projection overflow without materializing an unbounded result list.
                let options = mp::MatchOptions::from_defaults(prepared.defaults())
                    .with_max_results(
                        prepared
                            .defaults()
                            .max_results()
                            .min(DIAGNOSTIC_REGIONS as u32 + 1),
                    )
                    .map_err(|error| fault("template_request", error.into()))?;
                let find = mp::FindRequest::view(&view, &prepared, options)
                    .map_err(|error| fault("template_request", error))?;
                let outcome = session
                    .find_template(&find, &operation)
                    .map_err(|error| fault("template_recognition", error))?;
                let matches = outcome.result().matches();
                if matches.len() > DIAGNOSTIC_REGIONS {
                    return Err(Fault::new(
                        "RecognitionOutputLimit",
                        "template diagnostics exceed the result count bound",
                    ));
                }
                let values: Vec<_> = matches
                    .iter()
                    .map(|found| {
                        let bounds = found.bounds();
                        json!({"score":found.score(),"bounds":{"x":bounds.left(),"y":bounds.top(),
                        "width":bounds.width(),"height":bounds.height()}})
                    })
                    .collect();
                bounded_diagnostics(
                    json!({"kind":"template","id":id,
                    "threshold":outcome.result().options().min_score(),
                    "outcome":if matches.is_empty() {"no_match"} else {"matched"},"matches":values}),
                    matches.len(),
                )?
            }
        };
        control.check()?;
        Ok(diagnostics)
    })();
    settled(&primary);
    control.cancel();
    if let Some(bridge) = &bridge {
        bridge.token.cancel();
    }
    let started = Instant::now();
    let close = context(&mp::CancellationToken::new(), CLEANUP_MS).and_then(|operation| {
        session.as_ref().map_or(Ok(()), |session| {
            session
                .close(&operation)
                .map_err(|error| fault("session_close", error))
        })
    });
    let clean = close.is_ok() && session.as_ref().is_none_or(mp::Session::is_closed);
    let cleanup = json!({"clean":clean,"status":if clean {"CleanupFinished"} else {"IncompleteCleanup"},
        "session_closed":clean,"close_error":close.err(),"elapsed_us":started.elapsed().as_micros(),
        "native_input":"not_applicable_saved_frame"});
    if clean {
        drop(session);
        drop(engine);
        drop(bridge);
    } else {
        // Quarantine unresolved engine ownership until the supervisor contains this child.
        std::mem::forget(session);
        std::mem::forget(engine);
        std::mem::forget(bridge);
    }
    (primary, cleanup)
}

fn rect(value: PixelRect) -> Result<mp::Rect, Fault> {
    mp::Rect::from_origin_size(
        mp::CoordinateSpace::CapturePixels,
        f64::from(value.x),
        f64::from(value.y),
        f64::from(value.width),
        f64::from(value.height),
    )
    .map_err(|error| fault("recognition_roi", error.into()))
}

fn ocr(
    engine: &mp::Engine,
    session: &mp::Session,
    frame: &mp::Frame,
    zones: &[OcrZone],
    operation: &mp::OperationContext,
) -> Result<Value, Fault> {
    let backend = engine
        .ocr_backend()
        .ok_or_else(|| blocked("ocr_backend", "OCR backend identity unavailable"))?;
    let rois: Vec<_> = zones
        .iter()
        .map(|zone| rect(zone.rect).map(|rect| mp::OcrZone::new(rect, mp::ClipPolicy::Reject)))
        .collect::<Result<_, _>>()?;
    let request = mp::OcrZoneScanRequest::new(
        frame,
        backend.backend_identity(),
        backend.model_identity(),
        &rois,
        mp::CoordinateSpace::CapturePixels,
        operation,
    )
    .map_err(|error| fault("ocr_request", error))?;
    let scanned = session
        .scan_ocr_zones(request)
        .map_err(|error| fault("ocr_recognition", error))?;
    let mut count = 0usize;
    let mut text_bytes = 0usize;
    let mut output = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let group = scanned
            .group(index)
            .ok_or_else(|| blocked("ocr_projection", "engine omitted selected OCR zone"))?;
        let mut regions = Vec::new();
        for region in group.iter() {
            count += 1;
            text_bytes = text_bytes.saturating_add(region.text().len());
            if count > DIAGNOSTIC_REGIONS || text_bytes > DIAGNOSTIC_BYTES {
                return Err(Fault::new(
                    "RecognitionOutputLimit",
                    "recognition diagnostics exceed their finite projection budget",
                ));
            }
            let points = region.geometry().points();
            let left = points.iter().map(|p| p.x()).fold(f64::INFINITY, f64::min);
            let top = points.iter().map(|p| p.y()).fold(f64::INFINITY, f64::min);
            let right = points
                .iter()
                .map(|p| p.x())
                .fold(f64::NEG_INFINITY, f64::max);
            let bottom = points
                .iter()
                .map(|p| p.y())
                .fold(f64::NEG_INFINITY, f64::max);
            regions.push(
                json!({"text":region.text(),"confidence":region.confidence().get(),
                "bounds":{"x":left,"y":top,"width":right-left,"height":bottom-top},
                "geometry":points.iter().map(|point| [point.x(),point.y()]).collect::<Vec<_>>()}),
            );
        }
        output.push(json!({"id":zone.id,"outcome":if regions.is_empty() {"no_match"} else {"recognized"},"regions":regions}));
    }
    bounded_diagnostics(
        json!({"kind":"ocr","text_contract":TEXT_CONTRACT,"zones":output}),
        count,
    )
}
