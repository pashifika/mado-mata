//! Public-facade replay. Native admission stays closed without target provenance.

use crate::model::Fault;
#[cfg(feature = "engine")]
use crate::model::{Control, Plan};
#[cfg(feature = "engine")]
use serde_json::{Value, json};
#[cfg(feature = "engine")]
use std::collections::BTreeMap;
#[cfg(feature = "engine")]
use std::sync::Arc;

#[cfg(feature = "engine")]
pub const REVISION: &str = crate::model::ENGINE_REVISION;

#[cfg(feature = "engine")]
fn blocked(stage: &str, reason: &str) -> Fault {
    Fault::new("Blocked", reason).with_context(json!({
        "stage": stage, "engine_revision": REVISION
    }))
}

#[cfg(not(feature = "engine"))]
pub fn release_runner_resources() -> Result<(), Fault> {
    Ok(())
}

#[cfg(feature = "engine")]
pub use enabled::{Engine, release_runner_resources};

#[cfg(feature = "engine")]
mod enabled {
    use super::*;
    use crate::model::Limits;
    use mado_pilot as mp;
    use serde::{Deserialize, Serialize, de::DeserializeOwned};
    use sha2::{Digest, Sha256};
    use std::fs::File;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard, TryLockError};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Configuration {
        version: u32,
        ocr: OcrConfig,
        native_libraries: Vec<Library>,
        replay: Option<ReplayConfig>,
        native: Option<NativeConfig>,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Library {
        path: PathBuf,
        sha256: String,
        bytes: u64,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct OcrConfig {
        model: String,
        profile: String,
        language: String,
        provider: String,
        runtime_profile: String,
        model_root: PathBuf,
        runtime: Library,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct ReplayConfig {
        corpus_id: String,
        frames: Vec<RecordedFrame>,
        package_entries: BTreeMap<String, String>,
        templates: BTreeMap<String, String>,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct RecordedFrame {
        asset: String,
        width: u32,
        height: u32,
        pixel_format: String,
        captured_ns: u64,
        discontinuous: bool,
        placement: Option<Placement>,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Placement {
        desktop_origin: [f64; 2],
        logical_size: [f64; 2],
        scale: [f64; 2],
    }

    // Declarative policy is checked without discovering windows or probing permissions.
    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct NativeConfig {
        executable_or_bundle: PathBuf,
        process_id: u32,
        process_lifetime: String,
        window_rule: String,
        operating_system: String,
        hardware: String,
        permission_executable: PathBuf,
        capture: CaptureAuthority,
        input: InputAuthority,
        geometry: Placement,
        recognition_language: String,
        visible_postcondition: String,
        cleanup_ms: u64,
        containment_ms: u64,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct CaptureAuthority {
        approved: bool,
        duration_ms: u64,
        max_frames: u64,
        wait_ms: u64,
        interval_ms: u64,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct InputAuthority {
        approved: bool,
        duration_ms: u64,
        max_actions: usize,
        route: String,
        focus: String,
        representative_actions: Vec<Value>,
    }

    struct Resources {
        identity: String,
        engine: mp::Engine,
        templates: BTreeMap<String, mp::PreparedTemplate>,
        facts: Value,
    }

    static RESOURCES: Mutex<Option<Arc<Resources>>> = Mutex::new(None);

    pub fn release_runner_resources() -> Result<(), Fault> {
        let mut cache = RESOURCES
            .lock()
            .map_err(|_| internal("resource cache poisoned"))?;
        if cache
            .as_ref()
            .is_some_and(|resources| Arc::strong_count(resources) != 1)
        {
            return Err(Fault::new(
                "ResourceBusy",
                "attempts still own engine resources",
            ));
        }
        *cache = None;
        Ok(())
    }

    struct Observation {
        frame: mp::Frame,
        value: Value,
    }

    // Keep the actual facade results, not just a script-visible summary.
    enum Retained {
        Template(mp::FindOutcome),
        Ocr(mp::OcrResult),
    }

    impl Retained {
        fn stamp(&self) -> mp::FrameStamp {
            match self {
                Self::Template(result) => result.result().stamp(),
                Self::Ocr(result) => result.stamp(),
            }
        }
    }

    struct Query {
        request: RecognitionRequest,
        deadline: Instant,
        terminal: Option<Result<Value, Fault>>,
    }

    struct State {
        attempt_id: String,
        session: Option<mp::Session>,
        observations: BTreeMap<String, Observation>,
        results: BTreeMap<String, Retained>,
        queries: BTreeMap<String, Query>,
        active_queries: usize,
        latest: Option<mp::FrameStamp>,
        serial: u64,
        cleanup: Option<Value>,
    }

    // Cancellation never acquires the VM, host-work, or engine-work mutex.
    struct CancellationBridge {
        token: mp::CancellationToken,
        done: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl CancellationBridge {
        fn new(control: Arc<Control>) -> Result<Self, Fault> {
            let token = mp::CancellationToken::new();
            let done = Arc::new(AtomicBool::new(false));
            let worker_token = token.clone();
            let worker_done = Arc::clone(&done);
            let worker = thread::Builder::new()
                .name("engine-cancellation".into())
                .spawn(move || {
                    while !worker_done.load(Ordering::Acquire) {
                        if control.check().is_err() {
                            worker_token.cancel();
                            break;
                        }
                        thread::park_timeout(Duration::from_millis(1));
                    }
                })
                .map_err(|error| Fault::new("Initialization", error.to_string()))?;
            Ok(Self {
                token,
                done,
                worker: Some(worker),
            })
        }
    }

    impl Drop for CancellationBridge {
        fn drop(&mut self) {
            self.token.cancel();
            self.done.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                worker.thread().unpark();
                let _ = worker.join();
            }
        }
    }

    struct InFlight<'a>(&'a AtomicUsize);
    impl Drop for InFlight<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub struct Engine {
        resources: Arc<Resources>,
        state: Mutex<State>,
        control: Arc<Control>,
        limits: Limits,
        run: String,
        attempt: u64,
        bridge: CancellationBridge,
        closing: AtomicBool,
        in_flight: AtomicUsize,
        handles: AtomicUsize,
    }

    impl Engine {
        pub fn new(
            plan: &Plan,
            assets: &BTreeMap<String, Vec<u8>>,
            control: Arc<Control>,
            attempt_id: &str,
            attempt: u64,
        ) -> Result<Self, Fault> {
            let raw = plan.native_config.as_ref().ok_or_else(|| {
                blocked(
                    "configuration_unset",
                    "native_config must supply explicit replay and OCR configuration",
                )
            })?;
            let config: Configuration = serde_json::from_value(raw.clone())
                .map_err(|error| blocked("configuration_validation", &error.to_string()))?;
            if config.version != 1 {
                return Err(blocked(
                    "configuration_unsupported",
                    "unsupported engine configuration version",
                ));
            }
            control.check()?;
            if plan.lane == "native" {
                validate_native(config.native.as_ref(), plan)?;
                validate_ocr(&config, &control)?;
                return Err(blocked(
                    "target_identity_unavailable",
                    "pinned public facade cannot correlate TargetId with executable/bundle path and process lifetime; native discovery, capture and input are refused",
                ));
            }
            if plan.lane != "replay" || config.native.is_some() {
                return Err(blocked(
                    "configuration_validation",
                    "replay requires lane replay and no native authority",
                ));
            }
            let replay = config.replay.as_ref().ok_or_else(|| {
                blocked("replay_unset", "replay corpus configuration is required")
            })?;
            let bridge = CancellationBridge::new(Arc::clone(&control))?;
            let operation = operation(&bridge.token, plan.limits.duration_ms)?;
            let model = validate_ocr(&config, &control)?;
            let mut digest = Sha256::new();
            digest.update(REVISION.as_bytes());
            digest
                .update(serde_json::to_vec(&config).map_err(|error| internal(&error.to_string()))?);
            for (name, bytes) in assets {
                digest.update((name.len() as u64).to_le_bytes());
                digest.update(name.as_bytes());
                digest.update((bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            }
            let identity = format!("{:x}", digest.finalize());
            let mut cache = RESOURCES
                .lock()
                .map_err(|_| internal("resource cache poisoned"))?;
            let resources = if let Some(resources) = cache.as_ref() {
                if resources.identity != identity {
                    return Err(blocked(
                        "configuration_changed",
                        "runner resources belong to a different immutable configuration; start a new runner",
                    ));
                }
                Arc::clone(resources)
            } else {
                let source = replay_source(replay, assets, &plan.limits)?;
                let profile = match config.ocr.profile.as_str() {
                    mp::ACCEPTED_G004_PROFILE_ID => mp::OcrProviderProfile::NativeG004,
                    mp::ACCEPTED_BOUNDED_PROFILE_ID => mp::OcrProviderProfile::BoundedDetector,
                    _ => return Err(blocked("ocr_unsupported", "unsupported OCR profile")),
                };
                let ocr = mp::OcrProviderConfig::new(
                    profile,
                    mp::OcrExecutionProviderPolicy::Cpu,
                    &config.ocr.model_root,
                    &config.ocr.runtime.path,
                );
                let engine = mp::replay_engine_with_ocr_provider(
                    mp::ReplayEngineRequest::new(source),
                    &ocr,
                    &operation,
                )
                .map_err(|error| prerequisite_error("engine_initialization", error))?;
                // Recheck externally stored resources after initialization, before publication.
                validate_ocr(&config, &control)?;
                let provider = engine
                    .ocr_provider()
                    .ok_or_else(|| internal("OCR provider identity missing"))?;
                if provider.active_provider() != mp::OcrExecutionProvider::Cpu
                    || provider.initialization_fell_back()
                {
                    return Err(blocked(
                        "ocr_provider_mismatch",
                        "configured CPU provider was not selected exactly",
                    ));
                }
                let mut package = mp::MemoryPackage::new();
                for (entry, asset) in &replay.package_entries {
                    let bytes = assets.get(asset).ok_or_else(|| {
                        blocked(
                            "template_asset_missing",
                            "template package references an uncaptured asset",
                        )
                    })?;
                    package = package.with_entry(entry, Arc::<[u8]>::from(bytes.as_slice()));
                }
                let package = engine.load_package(&mp::PackageSource::memory(package), &operation).map_err(|error| {
                    blocked("template_validation", "MadoPilot asset package validation failed").with_context(json!({
                        "stage":"template_validation", "engine_revision":REVISION,
                        "cause":{"status":error.status().as_str(),"kind":format!("{:?}",error.kind()),"stage":format!("{:?}",error.stage())}
                    }))
                })?;
                let mut templates = BTreeMap::new();
                for (alias, template) in &replay.templates {
                    let prepared = engine
                        .prepare_from_package(&package, template, &operation)
                        .map_err(|error| {
                            prerequisite_error("template_initialization", error.into())
                        })?;
                    templates.insert(alias.clone(), prepared);
                }
                let facts = json!({"engine_revision":REVISION,"configuration_identity":identity,
                    "corpus_identity":replay.corpus_id,"backend":engine.backend().id(),
                    "ocr_model":model.model().as_str(),"ocr_profile":model.profile().as_str(),
                    "ocr_provider":"cpu","ocr_runtime_profile":provider.runtime_profile().as_str(),
                    "sink":"controlled-non-native","native_capture":false,"native_input":false});
                let resources = Arc::new(Resources {
                    identity,
                    engine,
                    templates,
                    facts,
                });
                *cache = Some(Arc::clone(&resources));
                resources
            };
            drop(cache);
            control.check()?;
            let targets = resources
                .engine
                .discover(&operation)
                .map_err(|error| engine_error("replay_discovery", error))?;
            if targets.len() != 1 {
                return Err(blocked(
                    "replay_target",
                    "replay configuration must contain exactly one target",
                ));
            }
            let session = resources
                .engine
                .open(targets[0].id(), &mp::OpenRequest::new(), &operation)
                .map_err(|error| engine_error("replay_open", error))?;
            Ok(Self {
                resources,
                state: Mutex::new(State {
                    attempt_id: attempt_id.to_owned(),
                    session: Some(session),
                    observations: BTreeMap::new(),
                    results: BTreeMap::new(),
                    queries: BTreeMap::new(),
                    active_queries: 0,
                    latest: None,
                    serial: 0,
                    cleanup: None,
                }),
                control,
                limits: plan.limits.clone(),
                run: plan.id.clone(),
                attempt,
                bridge,
                closing: AtomicBool::new(false),
                in_flight: AtomicUsize::new(0),
                handles: AtomicUsize::new(0),
            })
        }

        pub fn call(&self, method: &str, args: Value) -> Result<Value, Fault> {
            if method != "release" {
                self.check()?;
            }
            let mut state = self.lock()?;
            let result = match method {
                "observe" => {
                    empty(&args)?;
                    self.observe(&mut state, self.limits.wait_ms)
                }
                "validate_observation" => {
                    let request: ObservationRequest = decode(args)?;
                    self.observation(&state, &request.observation)?;
                    Ok(json!({"valid":true}))
                }
                "recognize" => {
                    let request: RecognitionRequest = decode(args)?;
                    if request.expected.is_some() {
                        return Err(argument("expected belongs to query"));
                    }
                    self.recognize(&mut state, &request, self.limits.wait_ms)
                }
                "query" => {
                    let mut request: RecognitionRequest = decode(args)?;
                    if request
                        .expected
                        .as_ref()
                        .is_some_and(|text| text.is_empty() || text.len() > self.limits.log_bytes)
                    {
                        return Err(argument("query expected text must be nonempty and bounded"));
                    }
                    if request.kind == "template" && request.expected.is_some() {
                        return Err(argument("template queries do not accept expected text"));
                    }
                    match request.kind.as_str() {
                        "template"
                            if request.asset.as_ref().is_some_and(|asset| {
                                self.resources.templates.contains_key(asset)
                            }) => {}
                        "ocr" if request.asset.is_none() => {}
                        _ => {
                            return Err(argument(
                                "query kind and asset must identify a configured recognizer",
                            ));
                        }
                    }
                    let source = self
                        .observation(&state, &request.observation)?
                        .frame
                        .clone();
                    request.roi.rect(&source)?;
                    self.capacity(&state, 2)?;
                    let id = next_id(&mut state, "query")?;
                    let deadline = Instant::now()
                        .checked_add(Duration::from_millis(self.limits.wait_ms))
                        .ok_or_else(|| argument("query deadline is not representable"))?;
                    request.observation = self.retain_observation(&mut state, source)?;
                    state.queries.insert(
                        id.clone(),
                        Query {
                            request,
                            deadline,
                            terminal: None,
                        },
                    );
                    Ok(json!({"id":id}))
                }
                "query_wait" => self.query_wait(&mut state, decode(args)?),
                "postcondition" => self.postcondition(&state, decode(args)?),
                "release" => {
                    let request: IdRequest = decode(args)?;
                    let query = state.queries.remove(&request.id);
                    if let Some(query) = &query {
                        if query.terminal.is_none() {
                            if let Some(id) = query.request.observation["id"].as_str() {
                                state.observations.remove(id);
                            }
                        }
                    }
                    let removed = state.observations.remove(&request.id).is_some()
                        || state.results.remove(&request.id).is_some()
                        || query.is_some();
                    if !removed {
                        return Err(Fault::new(
                            "InvalidHandle",
                            "unknown or released engine handle",
                        ));
                    }
                    Ok(json!({"released":true}))
                }
                "dispatch" => Err(blocked(
                    "native_dispatch",
                    "replay uses only the host controlled-non-native input sink",
                )),
                _ => Err(argument("unsupported engine operation")),
            };
            self.handles.store(owner_count(&state), Ordering::Release);
            result
        }

        fn check(&self) -> Result<(), Fault> {
            self.control.check()?;
            if self.closing.load(Ordering::Acquire) {
                return Err(Fault::new("Closed", "engine attempt is closing"));
            }
            Ok(())
        }

        fn lock(&self) -> Result<MutexGuard<'_, State>, Fault> {
            self.state
                .lock()
                .map_err(|_| internal("engine state poisoned"))
        }

        fn operation(&self, bound: u64) -> Result<mp::OperationContext, Fault> {
            self.check()?;
            let elapsed = self.control.elapsed_us() / 1_000;
            let remaining = self.limits.duration_ms.saturating_sub(elapsed);
            if remaining == 0 {
                return Err(Fault::new("Timeout", "engine attempt deadline expired"));
            }
            operation(&self.bridge.token, bound.min(remaining))
        }

        fn active(&self) -> InFlight<'_> {
            self.in_flight.fetch_add(1, Ordering::AcqRel);
            InFlight(&self.in_flight)
        }

        fn capacity(&self, state: &State, additional: usize) -> Result<(), Fault> {
            if owner_count(state).saturating_add(additional) > self.limits.handles {
                return Err(Fault::new(
                    "LimitExceeded",
                    "engine managed-handle limit exceeded",
                ));
            }
            Ok(())
        }

        fn observe(&self, state: &mut State, wait_ms: u64) -> Result<Value, Fault> {
            self.capacity(state, 1)?;
            let request = state
                .latest
                .map_or_else(mp::FrameRequest::latest, mp::FrameRequest::newer_than);
            let operation = self.operation(wait_ms)?;
            let _active = self.active();
            let session = state
                .session
                .as_ref()
                .ok_or_else(|| Fault::new("Closed", "session closed"))?;
            let frame = session
                .acquire_frame(&request, &operation)
                .map_err(|error| engine_error("replay_capture", error))?;
            self.check()?;
            state.latest = Some(frame.stamp());
            self.retain_observation(state, frame)
        }

        fn retain_observation(&self, state: &mut State, frame: mp::Frame) -> Result<Value, Fault> {
            let stamp = frame.stamp();
            let id = next_id(state, "observation")?;
            let extent = frame.descriptor().extent();
            let value = json!({"id":id,"run":self.run,"attempt":self.attempt,
                "process_lifetime":state.attempt_id,
                "session":format!("{}", stamp.stream()),"geometry":stamp.geometry().value(),
                "frame":stamp.sequence().value(),"epoch":stamp.epoch().value(),
                "width":extent.width(),"height":extent.height(),"coordinate_space":"capture-pixels"});
            state.observations.insert(
                id,
                Observation {
                    frame,
                    value: value.clone(),
                },
            );
            Ok(value)
        }

        fn observation<'a>(
            &self,
            state: &'a State,
            value: &Value,
        ) -> Result<&'a Observation, Fault> {
            let id = value
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| argument("observation.id is required"))?;
            let observation = state
                .observations
                .get(id)
                .ok_or_else(|| Fault::new("InvalidHandle", "unknown or released observation"))?;
            if observation.value != *value {
                return Err(Fault::new(
                    "StaleIdentity",
                    "observation provenance was changed",
                ));
            }
            if state.latest.is_some_and(|stamp| {
                stamp.stream() != observation.frame.stamp().stream()
                    || stamp.epoch() != observation.frame.stamp().epoch()
                    || stamp.geometry() != observation.frame.stamp().geometry()
            }) {
                return Err(Fault::new(
                    "StaleIdentity",
                    "observation geometry or stream generation changed",
                ));
            }
            Ok(observation)
        }

        fn recognize(
            &self,
            state: &mut State,
            request: &RecognitionRequest,
            wait_ms: u64,
        ) -> Result<Value, Fault> {
            self.capacity(state, 1)?;
            let observation = self.observation(state, &request.observation)?;
            let roi = request.roi.rect(&observation.frame)?;
            let operation = self.operation(wait_ms)?;
            let _active = self.active();
            let session = state
                .session
                .as_ref()
                .ok_or_else(|| Fault::new("Closed", "session closed"))?;
            let (retained, mut compact) = match request.kind.as_str() {
                "template" => {
                    let alias = request
                        .asset
                        .as_ref()
                        .ok_or_else(|| argument("template asset is required"))?;
                    let template = self.resources.templates.get(alias).ok_or_else(|| {
                        argument("template asset is not declared in replay configuration")
                    })?;
                    let view = observation
                        .frame
                        .view(roi, mp::ClipPolicy::Reject)
                        .map_err(|error| engine_error("template_roi", error.into()))?;
                    let find = mp::FindRequest::view(
                        &view,
                        template,
                        mp::MatchOptions::from_defaults(template.defaults()),
                    )
                    .map_err(|error| engine_error("template_request", error))?;
                    let outcome = session
                        .find_template(&find, &operation)
                        .map_err(|error| engine_error("template_recognition", error))?;
                    let Some(found) = outcome.result().matches().first() else {
                        self.check()?;
                        return Ok(Value::Null);
                    };
                    let bounds = found.bounds();
                    let value = json!({"kind":"template","observation":request.observation,
                        "region":{"x":bounds.left(),"y":bounds.top(),"width":bounds.width(),"height":bounds.height()},
                        "score":found.score()});
                    (Retained::Template(outcome), value)
                }
                "ocr" => {
                    if request.asset.is_some() {
                        return Err(argument("OCR forbids asset"));
                    }
                    let backend = self
                        .resources
                        .engine
                        .ocr_backend()
                        .ok_or_else(|| internal("OCR backend missing"))?;
                    let outcome = session
                        .recognize(mp::OcrRequest::new(
                            &observation.frame,
                            backend.backend_identity(),
                            backend.model_identity(),
                            mp::OcrRegion::Region {
                                rect: roi,
                                policy: mp::ClipPolicy::Reject,
                            },
                            mp::CoordinateSpace::CapturePixels,
                            &operation,
                        ))
                        .map_err(|error| engine_error("ocr_recognition", error))?;
                    let found = outcome.regions().iter().find(|region| {
                        request
                            .expected
                            .as_ref()
                            .is_none_or(|text| region.text() == text)
                    });
                    let Some(found) = found else {
                        self.check()?;
                        return Ok(Value::Null);
                    };
                    if found.text().len() > self.limits.log_bytes {
                        return Err(Fault::new(
                            "LimitExceeded",
                            "OCR compact text exceeds configured byte bound",
                        ));
                    }
                    let points = found.geometry().points();
                    let left = points
                        .iter()
                        .map(|point| point.x())
                        .fold(f64::INFINITY, f64::min);
                    let top = points
                        .iter()
                        .map(|point| point.y())
                        .fold(f64::INFINITY, f64::min);
                    let right = points
                        .iter()
                        .map(|point| point.x())
                        .fold(f64::NEG_INFINITY, f64::max);
                    let bottom = points
                        .iter()
                        .map(|point| point.y())
                        .fold(f64::NEG_INFINITY, f64::max);
                    let value = json!({"kind":"ocr","observation":request.observation,
                        "region":{"x":left,"y":top,"width":right-left,"height":bottom-top},
                        "score":found.confidence().get(),"text":found.text()});
                    (Retained::Ocr(outcome), value)
                }
                _ => return Err(argument("unknown recognition kind")),
            };
            self.check()?;
            if retained.stamp() != observation.frame.stamp() {
                return Err(internal("recognition source correlation mismatch"));
            }
            let id = next_id(state, "result")?;
            compact["id"] = Value::String(id.clone());
            state.results.insert(id, retained);
            Ok(compact)
        }

        fn query_wait(&self, state: &mut State, request: WaitRequest) -> Result<Value, Fault> {
            if request.timeout_ms == 0 || request.timeout_ms > self.limits.wait_ms {
                return Err(argument("query wait requires a finite configured timeout"));
            }
            let wait_deadline = Instant::now()
                .checked_add(Duration::from_millis(request.timeout_ms))
                .ok_or_else(|| argument("query wait deadline is not representable"))?;
            let mut query = state
                .queries
                .remove(&request.id)
                .ok_or_else(|| Fault::new("InvalidHandle", "unknown query"))?;
            if let Some(terminal) = &query.terminal {
                let result = match terminal {
                    Ok(value)
                        if value["id"]
                            .as_str()
                            .is_none_or(|id| !state.results.contains_key(id)) =>
                    {
                        Err(Fault::new("InvalidHandle", "query result was released"))
                    }
                    _ => terminal.clone(),
                };
                state.queries.insert(request.id, query);
                return result;
            }
            let deadline = wait_deadline.min(query.deadline);
            state.active_queries += 1;
            let result = (|| {
                loop {
                    self.check()?;
                    let wait_ms = remaining_ms(deadline)?;
                    let value = self.recognize(state, &query.request, wait_ms)?;
                    if !value.is_null() {
                        return Ok(value);
                    }
                    if let Some(id) = query.request.observation["id"].as_str() {
                        state.observations.remove(id);
                    }
                    query.request.observation = self.observe(state, remaining_ms(deadline)?)?;
                }
            })();
            state.active_queries -= 1;
            if result.is_err() {
                if let Some(id) = query.request.observation["id"].as_str() {
                    state.observations.remove(id);
                }
            }
            query.terminal = Some(result.clone());
            state.queries.insert(request.id, query);
            result
        }

        fn postcondition(
            &self,
            state: &State,
            request: PostconditionRequest,
        ) -> Result<Value, Fault> {
            let current = self.observation(state, &request.observation)?;
            let checkpoint = self.observation(state, &request.checkpoint)?;
            let now = current.frame.stamp();
            let before = checkpoint.frame.stamp();
            if now.stream() != before.stream()
                || now.epoch() != before.epoch()
                || now.geometry() != before.geometry()
                || now.sequence().value() <= before.sequence().value()
            {
                return Err(Fault::new(
                    "StaleIdentity",
                    "postcondition requires a strictly newer compatible recorded frame",
                ));
            }
            if request.expected.is_empty() || request.expected.len() > self.limits.log_bytes {
                return Err(argument("postcondition text must be nonempty and bounded"));
            }
            let operation = self.operation(self.limits.wait_ms)?;
            let _active = self.active();
            let backend = self
                .resources
                .engine
                .ocr_backend()
                .ok_or_else(|| internal("OCR backend missing"))?;
            let session = state
                .session
                .as_ref()
                .ok_or_else(|| Fault::new("Closed", "session closed"))?;
            let result = session
                .recognize(mp::OcrRequest::new(
                    &current.frame,
                    backend.backend_identity(),
                    backend.model_identity(),
                    mp::OcrRegion::FullFrame,
                    mp::CoordinateSpace::CapturePixels,
                    &operation,
                ))
                .map_err(|error| engine_error("postcondition_ocr", error))?;
            self.check()?;
            Ok(
                json!({"satisfied":result.regions().iter().any(|region| region.text() == request.expected),
                "frame":now.sequence().value(),"checkpoint":before.sequence().value(),"causation_claimed":false}),
            )
        }

        pub fn snapshot(&self) -> Value {
            json!({"configuration":self.resources.facts,"script_handles":self.handles.load(Ordering::Acquire),
                "in_flight":self.in_flight.load(Ordering::Acquire),"runner_engines":1,"runner_models":1,
                "closing":self.closing.load(Ordering::Acquire)})
        }

        pub fn finish(&self) -> Value {
            self.closing.store(true, Ordering::Release);
            self.bridge.token.cancel();
            let started = Instant::now();
            let budget = Duration::from_millis(self.limits.cleanup_ms);
            let mut state = loop {
                match self.state.try_lock() {
                    Ok(state) => break state,
                    Err(TryLockError::WouldBlock) if started.elapsed() < budget => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => {
                        return json!({"clean":false,"reason":"native_work_unsettled",
                        "in_flight":self.in_flight.load(Ordering::Acquire),"native_owners":self.handles.load(Ordering::Acquire),
                        "physical_cleanup_confirmed":false});
                    }
                }
            };
            if let Some(cleanup) = &state.cleanup {
                return cleanup.clone();
            }
            state.queries.clear();
            state.results.clear();
            state.observations.clear();
            self.handles.store(0, Ordering::Release);
            let close = remaining_ms_from(budget.saturating_sub(started.elapsed()))
                .and_then(|remaining| operation(&mp::CancellationToken::new(), remaining))
                .and_then(|context| match state.session.as_ref() {
                    Some(session) => {
                        let _active = self.active();
                        session
                            .close(&context)
                            .map_err(|error| engine_error("session_close", error))
                    }
                    None => Ok(()),
                });
            let clean = close.is_ok() && state.session.as_ref().is_none_or(mp::Session::is_closed);
            if clean {
                state.session = None;
            }
            let result = json!({"clean":clean,"native_owners":usize::from(!clean),
                "in_flight":self.in_flight.load(Ordering::Acquire),"script_handles":0,
                "runner_engines":1,"runner_models":1,"close_error":close.err(),
                "physical_cleanup_confirmed":clean,"native_input_release":"not_applicable_replay"});
            state.cleanup = Some(result.clone());
            result
        }
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            let _ = self.finish();
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ObservationRequest {
        observation: Value,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct IdRequest {
        id: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WaitRequest {
        id: String,
        timeout_ms: u64,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PostconditionRequest {
        observation: Value,
        checkpoint: Value,
        expected: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RecognitionRequest {
        observation: Value,
        kind: String,
        asset: Option<String>,
        roi: Roi,
        expected: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Roi {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    }

    impl Roi {
        fn rect(&self, frame: &mp::Frame) -> Result<mp::Rect, Fault> {
            let extent = frame.descriptor().extent();
            if self.width == 0
                || self.height == 0
                || self
                    .x
                    .checked_add(self.width)
                    .is_none_or(|right| right > extent.width())
                || self
                    .y
                    .checked_add(self.height)
                    .is_none_or(|bottom| bottom > extent.height())
            {
                return Err(argument(
                    "ROI must be nonempty and inside the recorded capture extent",
                ));
            }
            mp::Rect::from_origin_size(
                mp::CoordinateSpace::CapturePixels,
                f64::from(self.x),
                f64::from(self.y),
                f64::from(self.width),
                f64::from(self.height),
            )
            .map_err(|error| engine_error("roi", error.into()))
        }
    }

    fn owner_count(state: &State) -> usize {
        state.observations.len() + state.results.len() + state.queries.len() + state.active_queries
    }
    fn remaining_ms(deadline: Instant) -> Result<u64, Fault> {
        remaining_ms_from(deadline.saturating_duration_since(Instant::now()))
    }
    fn remaining_ms_from(remaining: Duration) -> Result<u64, Fault> {
        if remaining.is_zero() {
            return Err(Fault::new("Timeout", "operation deadline expired"));
        }
        u64::try_from(remaining.as_millis().max(1))
            .map_err(|_| argument("deadline is not representable"))
    }
    fn next_id(state: &mut State, kind: &str) -> Result<String, Fault> {
        state.serial = state
            .serial
            .checked_add(1)
            .ok_or_else(|| Fault::new("LimitExceeded", "engine identity exhausted"))?;
        let session = state
            .session
            .as_ref()
            .ok_or_else(|| Fault::new("Closed", "session closed"))?;
        Ok(format!(
            "{}-{kind}-{}-{}",
            state.attempt_id,
            session.stream(),
            state.serial
        ))
    }
    fn decode<T: DeserializeOwned>(value: Value) -> Result<T, Fault> {
        serde_json::from_value(value).map_err(|error| argument(&error.to_string()))
    }
    fn empty(value: &Value) -> Result<(), Fault> {
        if value.as_object().is_none_or(|object| !object.is_empty()) {
            return Err(argument("expected an empty object"));
        }
        Ok(())
    }
    fn argument(message: &str) -> Fault {
        Fault::new("Argument", message)
    }
    fn internal(message: &str) -> Fault {
        Fault::new("Internal", message)
    }
    fn operation(
        token: &mp::CancellationToken,
        timeout_ms: u64,
    ) -> Result<mp::OperationContext, Fault> {
        if timeout_ms == 0 {
            return Err(argument("operation requires a positive finite deadline"));
        }
        mp::OperationContext::new()
            .with_cancellation(token.clone())
            .with_timeout(Duration::from_millis(timeout_ms))
            .map_err(|error| engine_error("operation_context", error))
    }
    fn engine_error(stage: &str, error: mp::Error) -> Fault {
        let category = match error.status() {
            mp::Status::DeadlineExceeded => "Timeout".to_owned(),
            mp::Status::InvalidArgument => "Argument".to_owned(),
            status => format!("{status:?}"),
        };
        Fault::new(category, error.detail())
            .with_context(json!({"stage":stage,"engine_revision":REVISION,
            "cause":{"status":error.status().as_str(),"detail":error.detail()}}))
    }
    fn prerequisite_error(stage: &str, error: mp::Error) -> Fault {
        let mut fault = engine_error(stage, error);
        if fault.category != "Cancelled" && fault.category != "Timeout" {
            fault.category = "Blocked".into();
        }
        fault
    }

    fn validate_ocr(
        config: &Configuration,
        control: &Control,
    ) -> Result<mp::OcrModelIdentity, Fault> {
        let ocr = &config.ocr;
        let model = match ocr.profile.as_str() {
            mp::ACCEPTED_G004_PROFILE_ID => mp::OcrModelIdentity::accepted_g004(),
            mp::ACCEPTED_BOUNDED_PROFILE_ID => mp::OcrModelIdentity::accepted_bounded_detector(),
            _ => return Err(blocked("ocr_unsupported", "unsupported OCR profile")),
        };
        if ocr.model != model.model().as_str()
            || ocr.language != mp::ACCEPTED_G004_LANGUAGE_PROFILE_ID
            || ocr.provider != "cpu"
            || ocr.runtime_profile != mp::DEFAULT_OCR_RUNTIME_PROFILE_ID
        {
            return Err(blocked(
                "ocr_unsupported",
                "model/profile/language/runtime/provider combination is unsupported; no provider fallback is permitted",
            ));
        }
        canonical(&ocr.model_root, true, "model_root")?;
        for (relative, identity) in [
            (
                "rapidocr-v3.9.2/ch_PP-OCRv4_det_mobile.onnx",
                model.detector(),
            ),
            (
                "rapidocr-v3.9.2/PP-OCRv6_rec_small.onnx",
                model.recognizer(),
            ),
        ] {
            let path = ocr.model_root.join(relative);
            verify_file(
                &Library {
                    path,
                    sha256: hex(&identity.sha256()),
                    bytes: identity.byte_len(),
                },
                control,
                "model_validation",
            )?;
        }
        verify_file(&ocr.runtime, control, "runtime_validation")?;
        if config.native_libraries.is_empty() {
            return Err(blocked(
                "native_libraries_unset",
                "identify the linked OpenCV and other non-system native library files",
            ));
        }
        for library in &config.native_libraries {
            verify_file(library, control, "native_library_validation")?;
        }
        Ok(model)
    }

    fn canonical(path: &Path, directory: bool, stage: &str) -> Result<(), Fault> {
        if !path.is_absolute() {
            return Err(blocked(
                stage,
                "configured path must be absolute and canonical",
            ));
        }
        let resolved = path.canonicalize().map_err(|error| blocked("configuration_missing_file", "configured prerequisite is missing or unreadable")
            .with_context(json!({"stage":stage,"io_kind":format!("{:?}",error.kind()),"engine_revision":REVISION})))?;
        if resolved != path {
            return Err(blocked(
                stage,
                "configured path must be canonical; symlink aliases are not accepted",
            ));
        }
        let metadata = resolved
            .metadata()
            .map_err(|_| blocked(stage, "configured prerequisite metadata is unreadable"))?;
        if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
            return Err(blocked(
                stage,
                "configured prerequisite has the wrong file type",
            ));
        }
        Ok(())
    }

    fn verify_file(library: &Library, control: &Control, stage: &str) -> Result<(), Fault> {
        if library.bytes == 0
            || library.bytes > 1_073_741_824
            || library.sha256.len() != 64
            || !library
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(blocked(
                "configuration_validation",
                "each resource requires a finite size (at most 1 GiB) and lowercase SHA-256",
            ));
        }
        canonical(&library.path, false, stage)?;
        let mut file = File::open(&library.path)
            .map_err(|_| blocked(stage, "configured resource cannot be opened"))?;
        let before = file
            .metadata()
            .map_err(|_| blocked(stage, "resource metadata unavailable"))?;
        if before.len() != library.bytes {
            return Err(blocked(
                "configuration_changed",
                "configured resource byte length changed",
            ));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65_536];
        let mut remaining = library.bytes;
        while remaining > 0 {
            control.check()?;
            let length = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| internal("resource length overflow"))?;
            file.read_exact(&mut buffer[..length]).map_err(|_| {
                blocked(stage, "resource changed or became unreadable while hashing")
            })?;
            hash.update(&buffer[..length]);
            remaining -= length as u64;
        }
        let mut trailing = [0u8; 1];
        if file
            .read(&mut trailing)
            .map_err(|_| blocked(stage, "resource read failed"))?
            != 0
            || format!("{:x}", hash.finalize()) != library.sha256
        {
            return Err(blocked(
                "configuration_changed",
                "configured resource content does not match its immutable identity",
            ));
        }
        Ok(())
    }

    fn replay_source(
        config: &ReplayConfig,
        assets: &BTreeMap<String, Vec<u8>>,
        limits: &Limits,
    ) -> Result<mp::replay::ReplaySource, Fault> {
        if config.corpus_id.is_empty()
            || config.frames.is_empty()
            || config.frames.len() > limits.snapshot_files
            || config.package_entries.is_empty()
            || config.templates.is_empty()
            || config.templates.len() > limits.handles
        {
            return Err(blocked(
                "replay_validation",
                "recorded corpus, bounded frames, template package entries and template aliases are required",
            ));
        }
        let mut frames = Vec::with_capacity(config.frames.len());
        let mut bytes = 0usize;
        let mut timestamp = None;
        for record in &config.frames {
            let format = match record.pixel_format.as_str() {
                "rgba8" => mp::PixelFormat::Rgba8,
                "bgra8" => mp::PixelFormat::Bgra8,
                _ => {
                    return Err(blocked(
                        "replay_unsupported",
                        "replay supports packed rgba8 or bgra8 frames",
                    ));
                }
            };
            if timestamp.is_some_and(|previous| previous >= record.captured_ns) {
                return Err(blocked(
                    "replay_validation",
                    "recorded timestamps must be strictly increasing",
                ));
            }
            timestamp = Some(record.captured_ns);
            let pixels = assets.get(&record.asset).ok_or_else(|| {
                blocked(
                    "replay_asset_missing",
                    "recorded frame asset is not in the captured inventory",
                )
            })?;
            bytes = bytes
                .checked_add(pixels.len())
                .ok_or_else(|| argument("replay byte count overflow"))?;
            if bytes > limits.snapshot_bytes {
                return Err(blocked(
                    "replay_limit",
                    "replay corpus exceeds snapshot_bytes",
                ));
            }
            let descriptor = mp::FrameDescriptor::packed(
                mp::PixelExtent::new(record.width, record.height),
                format,
            )
            .map_err(|error| prerequisite_error("replay_descriptor", error.into()))?;
            let placement = record.placement.as_ref().map(placement).transpose()?;
            let continuity = if record.discontinuous {
                mp::Continuity::Discontinuous
            } else {
                mp::Continuity::Continuous
            };
            let frame = mp::replay::ReplayFrame::new(
                descriptor,
                mp::MonotonicInstant::from_origin(Duration::from_nanos(record.captured_ns)),
                continuity,
                placement,
                pixels.clone().into_boxed_slice(),
            )
            .map_err(|error| prerequisite_error("replay_frame", error.into()))?;
            frames.push(frame);
        }
        let target = mp::replay::ReplayTarget::new(&config.corpus_id, frames)
            .map_err(|error| prerequisite_error("replay_target", error.into()))?;
        mp::replay::ReplaySource::from_targets(vec![target])
            .map_err(|error| prerequisite_error("replay_source", error.into()))
    }

    fn placement(value: &Placement) -> Result<mp::TargetPlacement, Fault> {
        let scale = mp::Scale::new(value.scale[0], value.scale[1])
            .map_err(|error| prerequisite_error("geometry", error.into()))?;
        mp::TargetPlacement::new(
            (value.desktop_origin[0], value.desktop_origin[1]),
            (value.logical_size[0], value.logical_size[1]),
            scale,
        )
        .map_err(|error| prerequisite_error("geometry", error.into()))
    }

    fn validate_native(config: Option<&NativeConfig>, plan: &Plan) -> Result<(), Fault> {
        let config = config.ok_or_else(|| blocked("native_authority_unset", "native requires an operator-approved target and separate finite capture/input authority"))?;
        if !config.executable_or_bundle.is_absolute()
            || !config.permission_executable.is_absolute()
            || config.process_id == 0
            || config.process_lifetime.is_empty()
            || config.window_rule.is_empty()
            || config.hardware.is_empty()
            || config.recognition_language.is_empty()
            || config.visible_postcondition.is_empty()
        {
            return Err(blocked(
                "native_authority_validation",
                "target path, lifetime, window rule, hardware, permission executable, recognition language and visible postcondition are required",
            ));
        }
        if config.operating_system != std::env::consts::OS
            || !matches!(config.operating_system.as_str(), "windows" | "macos")
        {
            return Err(blocked(
                "native_platform_unsupported",
                "authority must identify this supported native OS",
            ));
        }
        let capture = &config.capture;
        if !capture.approved
            || capture.duration_ms == 0
            || capture.duration_ms > plan.limits.duration_ms
            || capture.max_frames == 0
            || capture.wait_ms == 0
            || capture.wait_ms > plan.limits.wait_ms
            || capture.interval_ms == 0
            || capture.interval_ms > capture.duration_ms
        {
            return Err(blocked(
                "capture_authority",
                "capture requires separate approval and finite duration/frame/wait/pacing bounds",
            ));
        }
        let input = &config.input;
        if !input.approved
            || input.duration_ms == 0
            || input.duration_ms > plan.limits.duration_ms
            || input.max_actions == 0
            || input.max_actions > plan.limits.max_actions
            || input.representative_actions.is_empty()
            || input.representative_actions.len() > input.max_actions
        {
            return Err(blocked(
                "input_authority",
                "input requires separate approval and finite duration/action bounds",
            ));
        }
        let supported_route = input.route == "system"
            || (config.operating_system == "windows" && input.route == "window_message")
            || (config.operating_system == "macos" && input.route == "process_directed");
        if !supported_route || !matches!(input.focus.as_str(), "require_focused" | "preserve") {
            return Err(blocked(
                "input_route_unsupported",
                "explicit supported route and non-focus-changing policy are required",
            ));
        }
        if config.cleanup_ms == 0
            || config.cleanup_ms > plan.limits.cleanup_ms
            || config.containment_ms == 0
            || config.containment_ms > plan.limits.containment_ms
        {
            return Err(blocked(
                "cleanup_authority",
                "finite cleanup and containment bounds must fit the plan",
            ));
        }
        placement(&config.geometry)?;
        Ok(())
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
