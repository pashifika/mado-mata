mod admission;
mod options;
mod sequence;

pub(crate) use admission::{HandleBudget, HandlePermit, Managed};
pub use options::{option_path, resolve_options};

#[cfg(test)]
mod test_support;

use crate::images::PayloadBytes;
use crate::model::{Control, Fault, Limits, Plan, RuntimeMetrics};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_ATTEMPT: AtomicU64 = AtomicU64::new(1);
// A synthetic fixture identifier, never an OS-selected or operated-on process.
const CONTROLLED_PROCESS_ID: u32 = 4242;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn argument(message: impl Into<String>) -> Fault {
    Fault::new("Argument", message)
}

fn object<'a>(
    value: &'a Value,
    allowed: &[&str],
    required: &[&str],
) -> Result<&'a Map<String, Value>, Fault> {
    let map = value
        .as_object()
        .ok_or_else(|| argument("expected an object"))?;
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(argument(format!("unknown field: {key}")));
        }
    }
    for key in required {
        if !map.contains_key(*key) {
            return Err(argument(format!("missing field: {key}")));
        }
    }
    Ok(map)
}

fn string<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str, Fault> {
    map.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| argument(format!("{key} must be a string")))
}

fn positive(map: &Map<String, Value>, key: &str, maximum: u64) -> Result<u64, Fault> {
    let value = map
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| argument(format!("{key} must be an integer")))?;
    if value == 0 || value > maximum {
        return Err(argument(format!("{key} must be in 1..={maximum}")));
    }
    Ok(value)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Region {
    fn validate(&self, width: u32, height: u32) -> Result<(), Fault> {
        if self.width == 0
            || self.height == 0
            || self.x.checked_add(self.width).is_none_or(|end| end > width)
            || self
                .y
                .checked_add(self.height)
                .is_none_or(|end| end > height)
        {
            return Err(argument(
                "ROI must be nonempty and contained in capture-pixel extent",
            ));
        }
        Ok(())
    }

    fn contains(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && u64::from(other.x) + u64::from(other.width)
                <= u64::from(self.x) + u64::from(self.width)
            && u64::from(other.y) + u64::from(other.height)
                <= u64::from(self.y) + u64::from(self.height)
    }
}

#[derive(Clone, Debug)]
struct Frame {
    value: Value,
    visible: String,
}

#[derive(Clone)]
struct Query {
    frame: Arc<Frame>,
    kind: String,
    roi: Region,
    expected: Option<String>,
    deadline: Instant,
    terminal: Option<Value>,
    physical: Option<Arc<PhysicalWork>>,
}

#[derive(Clone)]
enum Handle {
    Observation(Arc<Frame>),
    Query(Query),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Action {
    KeyDown {
        key: String,
    },
    KeyUp {
        key: String,
    },
    Click {
        x: f64,
        y: f64,
        button: PointerButton,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PointerButton {
    Left,
    Right,
    Middle,
}

// The same permit moves from queued to active input, then to the retained receipt.
struct Sequence {
    id: String,
    order: u64,
    observation: Value,
    actions: Vec<Action>,
    permit: HandlePermit,
}

struct Receipt {
    value: Value,
    permit: Option<HandlePermit>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Instantiating,
    Readiness,
    Workflow,
    Finished,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Instantiating => "Instantiating",
            Self::Readiness => "Readiness",
            Self::Workflow => "Workflow",
            Self::Finished => "Finished",
        }
    }
}

struct State {
    phase: Phase,
    readiness_started: Option<Instant>,
    next_id: u64,
    session: u64,
    geometry: u64,
    frame: u64,
    retained_process_lifetime: String,
    current_process_lifetime: String,
    alive: bool,
    focus: bool,
    route: bool,
    visible: String,
    handles: BTreeMap<String, Managed<Handle>>,
    queue: VecDeque<Sequence>,
    active: Option<String>,
    accepted: Vec<Value>,
    receipts: BTreeMap<String, Receipt>,
    released_receipts: BTreeSet<String>,
    effects: Vec<Value>,
    held_keys: BTreeMap<String, u64>,
    release_outcomes: Vec<Value>,
    postconditions: Vec<Value>,
    observations: u64,
    recognitions: u64,
    admitted_actions: usize,
    dispatches: u64,
    queue_high_water: usize,
    queue_rejections: u64,
    logs: Vec<String>,
    log_bytes: usize,
    dropped_logs: u64,
    cleanup: Option<Value>,
}

struct PhysicalWork {
    released: AtomicBool,
    completed: AtomicBool,
    // The physical worker retains its source independently of logical handles.
    frame: Arc<Frame>,
    work_lock: Mutex<()>,
}

struct Worker {
    work: Arc<PhysicalWork>,
    thread: JoinHandle<()>,
}

const OPERATIONS: [&str; 13] = [
    "asset",
    "observe",
    "recognize",
    "query",
    "query_wait",
    "submit",
    "settle",
    "postcondition",
    "release",
    "wait",
    "log",
    "fixture",
    "unknown",
];

#[derive(Default)]
struct OperationMetrics {
    count: AtomicU64,
    failures: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
}

struct Inner {
    plan: Plan,
    options: Value,
    assets: BTreeMap<String, PayloadBytes>,
    control: Arc<Control>,
    attempt: u64,
    lifetime: String,
    state: Mutex<State>,
    failure: Mutex<Option<Fault>>,
    dispatch: Mutex<()>,
    workers: Mutex<Vec<Worker>>,
    physical: Arc<AtomicUsize>,
    handle_budget: Arc<HandleBudget>,
    held: AtomicBool,
    terminating: AtomicBool,
    metrics: [OperationMetrics; OPERATIONS.len()],
    #[cfg(feature = "engine")]
    engine: Option<crate::engine::Engine>,
}

#[derive(Clone)]
pub struct Host {
    inner: Arc<Inner>,
}

/// One explicitly queued controlled successor. Its control exists before its VM,
/// so Stop can cancel queued work without reopening the predecessor's latch.
pub(crate) struct ScheduledAttempt {
    predecessor: Host,
    control: Arc<Control>,
}

impl Host {
    pub fn new(
        plan: Plan,
        options: Value,
        assets: BTreeMap<String, PayloadBytes>,
        control: Arc<Control>,
    ) -> Result<Self, Fault> {
        plan.limits.validate()?;
        if !options.is_object() {
            return Err(Fault::new("Profile", "resolved options must be an object"));
        }
        let attempt = NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Fault::new("Clock", "system clock precedes Unix epoch"))?
            .as_nanos();
        let lifetime = format!("controlled-{}-{nonce}-{attempt}", std::process::id());
        let handle_budget = Arc::new(HandleBudget::new(plan.limits.handles));
        #[cfg(feature = "engine")]
        let engine = if plan.lane == "controlled" {
            None
        } else {
            Some(crate::engine::Engine::new(
                &plan,
                &assets,
                Arc::clone(&control),
                &lifetime,
                attempt,
                Arc::clone(&handle_budget),
            )?)
        };
        #[cfg(not(feature = "engine"))]
        if plan.lane != "controlled" {
            return Err(Fault::new(
                "Blocked",
                "replay/native requires the engine feature and explicit configuration",
            ));
        }
        if plan.lane != "controlled" && plan.lane != "replay" && plan.lane != "native" {
            return Err(Fault::new("InvalidPlan", "unknown environment lane"));
        }
        let held = plan.scenario == "held-work";
        Ok(Self {
            inner: Arc::new(Inner {
                plan,
                options,
                assets,
                control,
                attempt,
                lifetime: lifetime.clone(),
                state: Mutex::new(State {
                    phase: Phase::Instantiating,
                    readiness_started: None,
                    next_id: 1,
                    session: 1,
                    geometry: 1,
                    frame: 0,
                    retained_process_lifetime: lifetime.clone(),
                    current_process_lifetime: lifetime,
                    alive: true,
                    focus: true,
                    route: true,
                    visible: "READY".into(),
                    handles: BTreeMap::new(),
                    queue: VecDeque::new(),
                    active: None,
                    accepted: Vec::new(),
                    receipts: BTreeMap::new(),
                    released_receipts: BTreeSet::new(),
                    effects: Vec::new(),
                    held_keys: BTreeMap::new(),
                    release_outcomes: Vec::new(),
                    postconditions: Vec::new(),
                    observations: 0,
                    recognitions: 0,
                    admitted_actions: 0,
                    dispatches: 0,
                    queue_high_water: 0,
                    queue_rejections: 0,
                    logs: Vec::new(),
                    log_bytes: 0,
                    dropped_logs: 0,
                    cleanup: None,
                }),
                failure: Mutex::new(None),
                dispatch: Mutex::new(()),
                workers: Mutex::new(Vec::new()),
                physical: Arc::new(AtomicUsize::new(0)),
                handle_budget,
                held: AtomicBool::new(held),
                terminating: AtomicBool::new(false),
                metrics: std::array::from_fn(|_| OperationMetrics::default()),
                #[cfg(feature = "engine")]
                engine,
            }),
        })
    }

    pub fn options(&self) -> Value {
        self.inner.options.clone()
    }
    pub fn control(&self) -> Arc<Control> {
        Arc::clone(&self.inner.control)
    }
    pub fn limits(&self) -> Limits {
        self.inner.plan.limits.clone()
    }

    pub fn fail(&self, failure: Fault) {
        self.close_admission();
        let mut first = lock(&self.inner.failure);
        if first.is_none() {
            *first = Some(failure);
        }
    }

    pub fn failure(&self) -> Option<Fault> {
        lock(&self.inner.failure).clone()
    }

    pub fn set_failure_stack(&self, stack: Option<String>) {
        let Some(stack) = stack else {
            return;
        };
        if let Some(fault) = lock(&self.inner.failure).as_mut() {
            if !fault.context.is_object() {
                fault.context = json!({"cause":fault.context});
            }
            if let Some(context) = fault.context.as_object_mut() {
                context.entry("stack").or_insert(json!(stack));
            }
        }
    }

    pub fn call(&self, method: &str, args: Value) -> Result<Value, Fault> {
        let index = OPERATIONS
            .iter()
            .position(|name| *name == method)
            .unwrap_or(OPERATIONS.len() - 1);
        let started = Instant::now();
        let result = self.call_inner(method, args);
        if let Err(error) = &result {
            self.retain_terminal_fault(error);
        }
        let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let metrics = &self.inner.metrics[index];
        metrics.count.fetch_add(1, Ordering::Relaxed);
        if result.is_err() {
            metrics.failures.fetch_add(1, Ordering::Relaxed);
        }
        metrics.total_us.fetch_add(elapsed, Ordering::Relaxed);
        metrics.max_us.fetch_max(elapsed, Ordering::Relaxed);
        result
    }

    fn retain_terminal_fault(&self, error: &Fault) {
        if matches!(error.category.as_str(), "TargetLost" | "Closed") {
            self.fail(error.clone());
        }
    }

    fn call_inner(&self, method: &str, args: Value) -> Result<Value, Fault> {
        if method == "release" {
            return self.release(&args);
        }
        if method == "fixture" {
            return self.fixture(&args);
        }
        if method == "settle" {
            return self.settle(&args);
        }
        self.check()?;
        if method != "asset"
            && method != "log"
            && lock(&self.inner.state).phase == Phase::Instantiating
        {
            return Err(Fault::new(
                "AdmissionClosed",
                "host work is unavailable during module instantiation",
            ));
        }
        #[cfg(feature = "engine")]
        if let Some(engine) = &self.inner.engine {
            if matches!(
                method,
                "observe" | "recognize" | "query" | "query_wait" | "postcondition"
            ) {
                if method == "postcondition"
                    && lock(&self.inner.state).postconditions.len()
                        >= self.inner.plan.limits.max_actions
                {
                    return Err(Fault::new(
                        "ResultLimit",
                        "postcondition evidence capacity exhausted",
                    ));
                }
                let value = engine.call(method, args)?;
                // Checks the harness latch, not the SDK's unexposed capture terminal cause.
                self.check()?;
                let mut state = lock(&self.inner.state);
                match method {
                    "observe" => state.observations += 1,
                    "recognize" | "query_wait" => state.recognitions += 1,
                    "postcondition"
                        if state.postconditions.len() < self.inner.plan.limits.max_actions =>
                    {
                        state.postconditions.push(value.clone())
                    }
                    _ => {}
                }
                return Ok(value);
            }
        }
        let result = match method {
            "asset" => {
                let map = object(&args, &["id"], &["id"])?;
                let id = string(map, "id")?;
                let bytes = self.inner.assets.get(id).ok_or_else(|| {
                    Fault::new("MissingAsset", "asset is not in the immutable inventory")
                })?;
                Ok(json!({"id":id,"bytes":bytes.len()}))
            }
            "observe" => {
                object(&args, &[], &[])?;
                self.observe()
            }
            "recognize" => self.recognize(&args, false),
            "query" => self.recognize(&args, true),
            "query_wait" => self.query_wait(&args),
            "submit" => self.submit(&args),
            "postcondition" => self.postcondition(&args),
            "wait" => {
                let map = object(&args, &["duration_ms"], &["duration_ms"])?;
                let duration = positive(map, "duration_ms", self.inner.plan.limits.wait_ms)?;
                let started = Instant::now();
                crate::runner::emit_host_wait_entered(self);
                while started.elapsed() < Duration::from_millis(duration) {
                    self.check()?;
                    thread::sleep(
                        Duration::from_millis(1)
                            .min(Duration::from_millis(duration).saturating_sub(started.elapsed())),
                    );
                }
                self.check()?;
                Ok(json!({"elapsed_ms":started.elapsed().as_millis() as u64}))
            }
            "log" => self.log(&args),
            _ => Err(argument(format!("unknown host method: {method}"))),
        };
        if result.is_ok() {
            self.check()?;
        }
        result
    }

    fn next_id(&self, state: &mut State, kind: &str) -> String {
        let id = format!("{}:{kind}:{}", self.inner.lifetime, state.next_id);
        state.next_id += 1;
        id
    }

    fn frame(&self, state: &mut State) -> Arc<Frame> {
        state.frame += 1;
        state.observations += 1;
        let id = self.next_id(state, "observation");
        Arc::new(Frame {
            value: json!({
                "id":id,"run":self.inner.plan.id,"attempt":self.inner.attempt,
                "process_id":CONTROLLED_PROCESS_ID,"process_lifetime":state.retained_process_lifetime,
                "session":state.session,"geometry":state.geometry,"frame":state.frame,
                "width":640,"height":480,"coordinate_space":"capture-pixels"
            }),
            visible: state.visible.clone(),
        })
    }

    fn observe(&self) -> Result<Value, Fault> {
        self.check()?;
        let mut state = lock(&self.inner.state);
        let permit = self.inner.handle_budget.reserve(1)?;
        let frame = self.frame(&mut state);
        let value = frame.value.clone();
        state.handles.insert(
            value["id"].as_str().unwrap_or_default().into(),
            Managed::new(Handle::Observation(frame), permit),
        );
        Ok(value)
    }

    fn validate_identity(&self, state: &State, value: &Value) -> Result<(), Fault> {
        if value["run"].as_str() != Some(self.inner.plan.id.as_str())
            || value["attempt"].as_u64() != Some(self.inner.attempt)
            || value["process_id"].as_u64() != Some(u64::from(CONTROLLED_PROCESS_ID))
            || value["process_lifetime"].as_str() != Some(state.retained_process_lifetime.as_str())
            || value["session"].as_u64() != Some(state.session)
            || value["geometry"].as_u64() != Some(state.geometry)
        {
            return Err(Fault::new(
                "StaleIdentity",
                "observation belongs to a different run, process lifetime, session, or geometry",
            ));
        }
        if !state.alive || state.current_process_lifetime != state.retained_process_lifetime {
            return Err(Fault::new(
                "TargetLost",
                "authorized process lifetime ended",
            ));
        }
        Ok(())
    }

    fn observation(&self, state: &State, value: &Value) -> Result<Arc<Frame>, Fault> {
        self.validate_identity(state, value)?;
        let id = value["id"]
            .as_str()
            .ok_or_else(|| argument("observation.id must be a string"))?;
        let frame = match state.handles.get(id).map(|handle| &handle.owner) {
            Some(Handle::Observation(frame)) => Some(Arc::clone(frame)),
            Some(Handle::Query(query)) if query.frame.value["id"] == id => {
                Some(Arc::clone(&query.frame))
            }
            _ => None,
        }
        .ok_or_else(|| {
            Fault::new(
                "InvalidHandle",
                "observation has been released or is unknown",
            )
        })?;
        if frame.value != *value {
            return Err(Fault::new(
                "StaleIdentity",
                "observation provenance was modified",
            ));
        }
        Ok(frame)
    }

    fn recognition_value(
        &self,
        frame: &Frame,
        kind: &str,
        roi: &Region,
        id: &str,
    ) -> Result<Value, Fault> {
        if self.inner.plan.scenario == "backend-failure" {
            return Err(Fault::new("Backend", "controlled recognition backend failure").with_context(json!({"operation":kind,"observation":frame.value["id"],"cause":"injected-backend-failure"})));
        }
        if matches!(
            self.inner.plan.scenario.as_str(),
            "no-match" | "query-absent"
        ) {
            return Ok(Value::Null);
        }
        let region = Region {
            x: 100,
            y: 80,
            width: 40,
            height: 20,
        };
        if !roi.contains(&region) {
            return Ok(Value::Null);
        }
        let mut result = json!({"id":id,"observation":frame.value,"kind":kind,"region":region,"score":if kind == "template" {0.95} else {0.98}});
        if kind == "ocr" {
            result["text"] = json!(frame.visible);
        }
        Ok(result)
    }

    fn recognize(&self, args: &Value, waitable: bool) -> Result<Value, Fault> {
        let map = object(
            args,
            if waitable {
                &["observation", "kind", "asset", "roi", "expected"]
            } else {
                &["observation", "kind", "asset", "roi"]
            },
            &["observation", "kind", "roi"],
        )?;
        let kind = string(map, "kind")?;
        if !["template", "ocr"].contains(&kind) {
            return Err(argument("kind must be template or ocr"));
        }
        if kind == "template" {
            let asset = string(map, "asset")?;
            if !self.inner.assets.contains_key(asset) {
                return Err(Fault::new(
                    "MissingAsset",
                    "template is not in captured assets",
                ));
            }
        } else if map.contains_key("asset") {
            return Err(argument("OCR does not accept a template asset"));
        }
        let roi: Region = serde_json::from_value(map["roi"].clone())
            .map_err(|error| argument(error.to_string()))?;
        roi.validate(640, 480)?;
        let expected = map
            .get("expected")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| argument("expected must be a string"))
            })
            .transpose()?;
        if expected
            .as_ref()
            .is_some_and(|text| text.is_empty() || text.len() > self.inner.plan.limits.log_bytes)
        {
            return Err(argument(
                "expected must be nonempty and within the configured text bound",
            ));
        }
        if kind == "template" && expected.is_some() {
            return Err(argument("template queries do not accept expected text"));
        }
        self.check()?;
        let mut state = lock(&self.inner.state);
        let frame = self.observation(&state, &map["observation"])?;
        let permit = self.inner.handle_budget.reserve(1)?;
        let id = self.next_id(&mut state, "query");
        state.recognitions += 1;
        let result = self.recognition_value(&frame, kind, &roi, &id)?;
        if !waitable && result.is_null() {
            return Ok(Value::Null);
        }
        let physical = if waitable && self.inner.held.load(Ordering::Acquire) {
            Some(self.start_held_work(Arc::clone(&frame))?)
        } else {
            None
        };
        let query = Query {
            frame,
            kind: kind.into(),
            roi,
            expected,
            physical,
            deadline: Instant::now() + Duration::from_millis(self.inner.plan.limits.wait_ms),
            terminal: (!waitable).then(|| result.clone()),
        };
        state
            .handles
            .insert(id.clone(), Managed::new(Handle::Query(query), permit));
        if waitable {
            Ok(json!({"id":id}))
        } else {
            Ok(result)
        }
    }

    fn start_held_work(&self, frame: Arc<Frame>) -> Result<Arc<PhysicalWork>, Fault> {
        self.reap_workers();
        if !lock(&self.inner.workers).is_empty() {
            return Err(Fault::new(
                "WorkCapacity",
                "the previous physical worker has not exited",
            ));
        }
        if self
            .inner
            .physical
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Fault::new(
                "WorkCapacity",
                "the controlled native worker is occupied",
            ));
        }
        let work = Arc::new(PhysicalWork {
            released: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            frame,
            work_lock: Mutex::new(()),
        });
        let retained = Arc::clone(&work);
        let physical = Arc::clone(&self.inner.physical);
        let thread = match thread::Builder::new()
            .name("m0-held-native".into())
            .spawn(move || {
                let _work_lock = lock(&retained.work_lock);
                while !retained.released.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(1));
                }
                // Touch the retained frame only while the physical owner is alive.
                std::hint::black_box(&retained.frame);
                retained.completed.store(true, Ordering::Release);
                physical.fetch_sub(1, Ordering::AcqRel);
            }) {
            Ok(thread) => thread,
            Err(error) => {
                self.inner.physical.store(0, Ordering::Release);
                return Err(Fault::new("Backend", error.to_string()));
            }
        };
        let mut workers = lock(&self.inner.workers);
        // A release before registration must also reach this newly created worker.
        if !self.inner.held.load(Ordering::Acquire) {
            work.released.store(true, Ordering::Release);
        }
        workers.push(Worker {
            work: Arc::clone(&work),
            thread,
        });
        Ok(work)
    }

    fn reap_workers(&self) {
        let mut workers = lock(&self.inner.workers);
        let mut index = 0;
        while index < workers.len() {
            if workers[index].thread.is_finished() {
                let worker = workers.swap_remove(index);
                if worker.thread.join().is_err() {
                    self.fail(Fault::new("Backend", "controlled worker panicked"));
                }
            } else {
                index += 1;
            }
        }
    }

    fn query_wait(&self, args: &Value) -> Result<Value, Fault> {
        let map = object(args, &["id", "timeout_ms"], &["id", "timeout_ms"])?;
        let id = string(map, "id")?;
        let timeout = positive(map, "timeout_ms", self.inner.plan.limits.wait_ms)?;
        let deadline = Instant::now() + Duration::from_millis(timeout);
        crate::runner::emit_host_wait_entered(self);
        loop {
            if let Err(error) = self.check() {
                lock(&self.inner.state).handles.remove(id);
                return Err(error);
            }
            let mut state = lock(&self.inner.state);
            let query = match state.handles.get(id).map(|handle| &handle.owner) {
                Some(Handle::Query(query)) => query.clone(),
                _ => {
                    return Err(Fault::new(
                        "InvalidHandle",
                        "query has been released or is unknown",
                    ));
                }
            };
            if let Err(error) = self.validate_identity(&state, &query.frame.value) {
                state.handles.remove(id);
                return Err(error);
            }
            if let Some(result) = query.terminal {
                return Ok(result);
            }
            if Instant::now() >= deadline.min(query.deadline) {
                state.handles.remove(id);
                return Err(Fault::new(
                    "Timeout",
                    "visual query condition was absent before its deadline",
                )
                .with_context(json!({"query":id,"timeout_ms":timeout})));
            }
            if query
                .physical
                .as_ref()
                .is_none_or(|work| work.completed.load(Ordering::Acquire))
            {
                let mut frame = self.frame(&mut state);
                Arc::make_mut(&mut frame).value["id"] = json!(id);
                let result = match self.recognition_value(&frame, &query.kind, &query.roi, id) {
                    Ok(result) => result,
                    Err(error) => {
                        state.handles.remove(id);
                        return Err(error);
                    }
                };
                state.recognitions += 1;
                let satisfied = !result.is_null()
                    && query
                        .expected
                        .as_ref()
                        .is_none_or(|expected| result["text"].as_str() == Some(expected.as_str()));
                if let Some(Handle::Query(stored)) =
                    state.handles.get_mut(id).map(|handle| &mut handle.owner)
                {
                    stored.frame = frame;
                    if satisfied {
                        stored.terminal = Some(result.clone());
                    }
                }
                if satisfied {
                    return Ok(result);
                }
            }
            drop(state);
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn postcondition(&self, args: &Value) -> Result<Value, Fault> {
        let map = object(
            args,
            &["observation", "checkpoint", "expected"],
            &["observation", "checkpoint", "expected"],
        )?;
        let expected = string(map, "expected")?;
        let mut state = lock(&self.inner.state);
        let observation = self.observation(&state, &map["observation"])?;
        let checkpoint = self.observation(&state, &map["checkpoint"])?;
        let frame = observation.value["frame"].as_u64().unwrap_or(0);
        let checkpoint_frame = checkpoint.value["frame"].as_u64().unwrap_or(u64::MAX);
        if frame <= checkpoint_frame {
            return Err(Fault::new(
                "StaleFrame",
                "postcondition requires a strictly newer accepted frame",
            ));
        }
        if state.postconditions.len() >= self.inner.plan.limits.max_actions {
            return Err(Fault::new(
                "ResultLimit",
                "postcondition evidence capacity exhausted",
            ));
        }
        let result = json!({"satisfied":observation.visible == expected,"frame":frame,"checkpoint":checkpoint_frame});
        state.postconditions.push(result.clone());
        Ok(result)
    }

    fn log(&self, args: &Value) -> Result<Value, Fault> {
        let map = object(args, &["message"], &["message"])?;
        let message = string(map, "message")?;
        let mut state = lock(&self.inner.state);
        if state.logs.len() >= self.inner.plan.limits.log_records
            || message.len()
                > self
                    .inner
                    .plan
                    .limits
                    .log_bytes
                    .saturating_sub(state.log_bytes)
        {
            state.dropped_logs += 1;
            return Ok(json!({"recorded":false}));
        }
        state.log_bytes += message.len();
        state.logs.push(message.into());
        crate::runner::emit_script_log(self.control().elapsed_us(), message);
        Ok(json!({"recorded":true}))
    }

    fn fixture(&self, args: &Value) -> Result<Value, Fault> {
        if self.inner.plan.lane != "controlled" {
            return Err(Fault::new(
                "Authority",
                "fixture events are restricted to the controlled environment",
            ));
        }
        let map = object(args, &["event"], &["event"])?;
        let event = string(map, "event")?;
        if event == "release_hold" {
            self.inner.held.store(false, Ordering::Release);
            for worker in lock(&self.inner.workers).iter() {
                worker.work.released.store(true, Ordering::Release);
            }
            return Ok(json!({"applied":true}));
        }
        self.check()?;
        let mut state = lock(&self.inner.state);
        match event {
            "geometry" => state.geometry += 1,
            "session" => state.session += 1,
            "target_exit" => {
                state.alive = false;
                self.close_admission();
            }
            "pid_reuse" => {
                // Keep PID, attempt, session, and geometry unchanged: only the
                // observed lifetime changes, while the authorized one is retained.
                state.current_process_lifetime =
                    format!("controlled-pid-reuse-{}", self.inner.lifetime);
                self.close_admission();
            }
            "focus_lost" => state.focus = false,
            "route_revoked" => state.route = false,
            "hold" => self.inner.held.store(true, Ordering::Release),
            _ => return Err(argument("unknown controlled fixture event")),
        }
        Ok(json!({"applied":true}))
    }

    pub fn snapshot(&self) -> Value {
        let state = lock(&self.inner.state);
        let live = self.inner.handle_budget.live.load(Ordering::Acquire);
        let physical = self.inner.physical.load(Ordering::Acquire);
        let mut receipts: Vec<_> = state
            .receipts
            .values()
            .map(|receipt| &receipt.value)
            .collect();
        receipts.sort_unstable_by_key(|receipt| receipt["order"].as_u64());
        let mut value = json!({
            "lane":self.inner.plan.lane,"sink":self.input_sink(),"phase":state.phase.name(),
            "accepted":state.accepted,"receipts":receipts,
            "postconditions":state.postconditions,"observations":state.observations,"recognitions":state.recognitions,
            "dispatches":state.dispatches,"effects":state.effects,"queue_depth":state.queue.len(),
            "queue_high_water":state.queue_high_water,"queue_rejections":state.queue_rejections,
            "live_handles":live,"attempt_owners":state.handles.len() + state.queue.len() + usize::from(state.active.is_some()) + state.held_keys.len(),
            "in_flight_native":physical,"runner_owners":0,"active_sequence":state.active,
            "held_keys":state.held_keys.keys().collect::<Vec<_>>(),"logs":state.logs,"dropped_logs":state.dropped_logs,
            "release_outcomes":state.release_outcomes
        });
        if self.inner.plan.lane == "controlled" {
            value["target_identity"] = json!({
                "scope":"controlled PID-reuse injection; not OS PID recycling",
                "process_id":CONTROLLED_PROCESS_ID,"attempt":self.inner.attempt,
                "session":state.session,"geometry":state.geometry,"alive":state.alive,
                "retained_process_lifetime":state.retained_process_lifetime,
                "current_process_lifetime":state.current_process_lifetime
            });
        }
        drop(state);
        value["failure"] = json!(self.failure());
        value["operation_metrics"] = Value::Object(
            OPERATIONS
                .iter()
                .zip(&self.inner.metrics)
                .map(|(name, metrics)| {
                    (
                        (*name).into(),
                        json!({
                            "count":metrics.count.load(Ordering::Relaxed),
                            "failures":metrics.failures.load(Ordering::Relaxed),
                            "total_us":metrics.total_us.load(Ordering::Relaxed),
                            "max_us":metrics.max_us.load(Ordering::Relaxed),
                        }),
                    )
                })
                .collect(),
        );
        #[cfg(feature = "engine")]
        if let Some(engine) = &self.inner.engine {
            let engine = engine.snapshot();
            value["attempt_owners"] = json!(
                value["attempt_owners"].as_u64().unwrap_or(0)
                    + engine["script_handles"].as_u64().unwrap_or(0)
            );
            value["in_flight_native"] =
                json!(physical + engine["in_flight"].as_u64().unwrap_or(0) as usize);
            value["runner_owners"] = json!(
                engine["runner_engines"].as_u64().unwrap_or(0)
                    + engine["runner_models"].as_u64().unwrap_or(0)
            );
            value["engine"] = engine;
        }
        value
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.control.admission.store(false, Ordering::Release);
        // Unfinished native owners retain their own resources; never join or unload under them.
        // Explicit finish supplies the bounded cleanup evidence. Process containment owns a hold.
    }
}

/// The control implements decisions directly, without sharing the source-package decision helper.
pub fn run_rust(host: &Host) -> Result<RuntimeMetrics, Fault> {
    host.begin_readiness()?;
    let ready = host.call("observe", json!({}))?;
    host.call("release", json!({"id":ready["id"]}))?;
    host.begin_workflow()?;
    let options = host.options();
    let roi = options["recognition"]["roi"].clone();
    let threshold = options["recognition"]["threshold"]
        .as_f64()
        .ok_or_else(|| Fault::new("Profile", "recognition.threshold must be numeric"))?;
    let before = host.call("observe", json!({}))?;
    host.call("asset", json!({"id":"marker"}))?;
    let template = host.call(
        "recognize",
        json!({"observation":before,"kind":"template","asset":"marker","roi":roi}),
    )?;
    let ocr = host.call(
        "recognize",
        json!({"observation":before,"kind":"ocr","roi":roi}),
    )?;
    let priorities = options["priorities"]
        .as_array()
        .ok_or_else(|| Fault::new("Profile", "priorities must be an array"))?;
    let mut selected = None;
    for priority in priorities {
        let kind = priority
            .as_str()
            .ok_or_else(|| Fault::new("Profile", "priority must be a string"))?;
        let result = match kind {
            "template" => &template,
            "ocr" => &ocr,
            _ => return Err(Fault::new("Profile", "unsupported recognition priority")),
        };
        if !result.is_null()
            && result["score"]
                .as_f64()
                .is_some_and(|score| score >= threshold)
        {
            selected = Some(kind);
            break;
        }
    }
    if let Some(kind) = selected {
        let key = options["actions"][kind]
            .as_str()
            .ok_or_else(|| Fault::new("Profile", "selected action key is missing"))?;
        let accepted = host.call("submit", json!({"observation":before,"actions":[{"kind":"key_down","key":key},{"kind":"key_up","key":key}]}))?;
        let receipt = host.call("settle", json!({"id":accepted["id"]}))?;
        let after = host.call("observe", json!({}))?;
        let postcondition = host.call(
            "postcondition",
            json!({"observation":after,"checkpoint":before,"expected":options["postcondition"]}),
        )?;
        host.call("log", json!({"message":format!("decision={kind};receipt={};postcondition={}", receipt["status"], postcondition["satisfied"])}))?;
        host.call("release", json!({"id":after["id"]}))?;
        host.call("release", json!({"id":accepted["id"]}))?;
    }
    for recognition in [&template, &ocr] {
        if !recognition.is_null() {
            host.call("release", json!({"id":recognition["id"]}))?;
        }
    }
    host.call("release", json!({"id":before["id"]}))?;
    Ok(RuntimeMetrics::default())
}

#[cfg(test)]
mod tests {
    use super::test_support::{make_host, observe, ready_host, submit, wait_until};
    use super::*;

    fn ready_host_with_handle_limit(limit: usize) -> Host {
        let mut host = make_host("success", "template-first");
        let inner = Arc::get_mut(&mut host.inner).expect("unshared fixture");
        inner.plan.limits.handles = limit;
        inner.handle_budget = Arc::new(HandleBudget::new(limit));
        host.begin_readiness().expect("readiness");
        host.begin_workflow().expect("workflow");
        host
    }

    #[cfg(feature = "engine")]
    fn replay_host_with_handle_limit(limit: usize) -> Host {
        let mut host = make_host("success", "template-first");
        let inner = Arc::get_mut(&mut host.inner).expect("unshared fixture");
        inner.plan.lane = "replay".into();
        inner.plan.limits.handles = limit;
        inner.handle_budget = Arc::new(HandleBudget::new(limit));
        inner.engine = Some(crate::engine::Engine::replay_for_test(
            &inner.plan,
            Arc::clone(&inner.control),
            &inner.lifetime,
            inner.attempt,
            Arc::clone(&inner.handle_budget),
        ));
        host.begin_readiness().expect("replay readiness");
        host.begin_workflow().expect("replay workflow");
        host
    }

    #[cfg(feature = "engine")]
    #[test]
    fn replay_engine_refuses_observation_while_host_owns_sequence_or_receipt() {
        let host = replay_host_with_handle_limit(2);
        let observation = observe(&host);
        let sequence = submit(&host, &observation, "A").expect("aggregate two owners");
        assert_eq!(
            host.call("observe", json!({}))
                .expect_err("queue consumes last slot")
                .category,
            "HandleLimit"
        );
        let recognition = json!({
            "observation":observation,"kind":"ocr",
            "roi":{"x":0,"y":0,"width":8,"height":8}
        });
        for method in ["recognize", "query"] {
            assert_eq!(
                host.call(method, recognition.clone())
                    .expect_err("host owns remaining capacity")
                    .category,
                "HandleLimit"
            );
        }
        let receipt = host
            .call("settle", json!({"id":sequence["id"]}))
            .expect("controlled sink");
        assert_eq!(receipt["status"], "Submitted");
        assert_eq!(
            host.call("observe", json!({}))
                .expect_err("receipt consumes last slot")
                .category,
            "HandleLimit"
        );
        assert_eq!(host.snapshot()["live_handles"], 2);
        host.call("release", json!({"id":sequence["id"]}))
            .expect("release receipt");
        assert_eq!(
            host.call("query", recognition)
                .expect_err("query and its source require two slots")
                .category,
            "HandleLimit"
        );
        let newer = observe(&host);
        assert_eq!(host.snapshot()["live_handles"], 2);
        host.call("release", json!({"id":observation["id"]}))
            .expect("release engine source");
        host.call("release", json!({"id":newer["id"]}))
            .expect("release newer engine source");
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(host.snapshot()["live_handles"], 0);
        assert_eq!(host.snapshot()["receipts"][0], receipt);
    }

    #[cfg(feature = "engine")]
    #[test]
    fn replay_engine_owners_refuse_host_sequence_until_an_engine_release() {
        let host = replay_host_with_handle_limit(2);
        let first = observe(&host);
        let second = observe(&host);
        assert_eq!(
            submit(&host, &first, "A")
                .expect_err("engine consumes both slots")
                .category,
            "HandleLimit"
        );
        assert_eq!(host.snapshot()["accepted"], json!([]));
        host.call("release", json!({"id":second["id"]}))
            .expect("release engine slot");
        let sequence = submit(&host, &first, "A").expect("host can reserve released slot");
        host.call("release", json!({"id":sequence["id"]}))
            .expect("cancel queued sequence");
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(host.snapshot()["live_handles"], 0);
        assert_eq!(host.snapshot()["receipts"][0]["status"], "Cancelled");
    }

    #[test]
    fn shared_budget_keeps_queued_and_settled_input_charged_until_release() {
        let host = ready_host_with_handle_limit(2);
        let observation = observe(&host);
        let sequence = submit(&host, &observation, "A").expect("two owners fit");
        // Exercise the same atomic reservation used by engine observations and results.
        assert_eq!(
            host.inner
                .handle_budget
                .reserve(1)
                .expect_err("queued input owns capacity")
                .category,
            "HandleLimit"
        );
        let receipt = host
            .call("settle", json!({"id":sequence["id"]}))
            .expect("settled");
        assert_eq!(receipt["status"], "Submitted");
        assert_eq!(
            host.inner
                .handle_budget
                .reserve(1)
                .expect_err("receipt still owns capacity")
                .category,
            "HandleLimit"
        );
        assert_eq!(host.snapshot()["live_handles"], 2);
        host.call("release", json!({"id":sequence["id"]}))
            .expect("release receipt");
        let engine_owner = Managed::new(
            (),
            host.inner.handle_budget.reserve(1).expect("released slot"),
        );
        host.control().cancel();
        host.call("release", json!({"id":observation["id"]}))
            .expect("release after Stop");
        assert_eq!(host.snapshot()["receipts"][0], receipt);
        drop(engine_owner);
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(host.snapshot()["live_handles"], 0);
    }

    #[test]
    fn shared_budget_counts_engine_owners_before_host_admission() {
        let host = ready_host_with_handle_limit(2);
        let engine_owner = Managed::new(
            (),
            host.inner.handle_budget.reserve(1).expect("engine owner"),
        );
        let observation = observe(&host);
        assert_eq!(
            submit(&host, &observation, "A")
                .expect_err("aggregate ceiling")
                .category,
            "HandleLimit"
        );
        assert_eq!(host.snapshot()["accepted"], json!([]));
        drop(engine_owner);
        let sequence = submit(&host, &observation, "A").expect("engine release returns capacity");
        host.call("release", json!({"id":sequence["id"]}))
            .expect("cancel queued sequence");
        let replacement = host
            .inner
            .handle_budget
            .reserve(1)
            .expect("queue release returns capacity");
        assert_eq!(host.snapshot()["receipts"][0]["status"], "Cancelled");
        drop(replacement);
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(host.snapshot()["live_handles"], 0);
    }

    #[test]
    fn shared_budget_keeps_active_input_charged_while_dispatch_waits() {
        let host = ready_host("held-work");
        let observation = observe(&host);
        let sequence = submit(&host, &observation, "A").expect("accepted");
        let other_owners = host
            .inner
            .handle_budget
            .reserve(30)
            .expect("fill shared capacity");
        let settling = host.clone();
        let id = sequence["id"].clone();
        let worker = thread::spawn(move || settling.call("settle", json!({"id":id})));
        wait_until(|| host.snapshot()["in_flight_native"] == 1);
        assert_eq!(host.snapshot()["queue_depth"], 0);
        assert_eq!(host.snapshot()["active_sequence"], sequence["id"]);
        assert_eq!(
            host.inner
                .handle_budget
                .reserve(1)
                .expect_err("active input still owns capacity")
                .category,
            "HandleLimit"
        );
        host.call("fixture", json!({"event":"release_hold"}))
            .expect("release controlled worker");
        let receipt = worker.join().expect("settler exits").expect("receipt");
        assert_eq!(receipt["status"], "Submitted");
        assert_eq!(host.snapshot()["live_handles"], 32);
        drop(other_owners);
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(host.snapshot()["live_handles"], 0);
    }

    #[test]
    fn target_loss_is_latched_without_erasing_prior_receipts_or_release() {
        let host = ready_host("success");
        let observation = observe(&host);
        let sequence = submit(&host, &observation, "A").expect("accepted");
        let receipt = host
            .call("settle", json!({"id":sequence["id"]}))
            .expect("submitted");
        host.call("fixture", json!({"event":"target_exit"}))
            .expect("controlled target loss");
        let fault = host.call("observe", json!({})).expect_err("target loss");
        assert_eq!(fault.category, "TargetLost");
        assert!(!host.control().admission.load(Ordering::Acquire));
        host.fail(Fault::new("Closed", "later cleanup failure"));
        assert_eq!(host.snapshot()["failure"], json!(fault));
        assert_eq!(
            host.call("postcondition", json!({}))
                .expect_err("no actionable result")
                .category,
            "TargetLost"
        );
        host.call("release", json!({"id":observation["id"]}))
            .expect("release remains usable");
        host.call("release", json!({"id":sequence["id"]}))
            .expect("receipt release remains usable");
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(host.snapshot()["receipts"][0], receipt);
    }

    #[test]
    fn target_loss_during_settlement_retains_fault_and_refused_receipt() {
        let host = ready_host("success");
        let observation = observe(&host);
        let sequence = submit(&host, &observation, "A").expect("accepted");
        host.call("fixture", json!({"event":"target_exit"}))
            .expect("controlled target loss");
        let receipt = host
            .call("settle", json!({"id":sequence["id"]}))
            .expect("receipt survives fault");
        assert_eq!(receipt["status"], "Refused");
        assert_eq!(receipt["error"]["category"], "TargetLost");
        assert_eq!(host.snapshot()["failure"], receipt["error"]);
        assert_eq!(host.snapshot()["effects"], json!([]));
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(host.snapshot()["receipts"][0], receipt);
    }

    fn query(host: &Host, observation: &Value) -> Result<Value, Fault> {
        host.call(
            "query",
            json!({"observation":observation,"kind":"ocr",
            "roi":{"x":0,"y":0,"width":640,"height":480},"expected":"READY"}),
        )
    }

    #[test]
    fn direct_control_matches_independent_profile_and_effect_oracles() {
        for (profile, key) in [("template-first", "A"), ("ocr-first", "D")] {
            let host = make_host("success", profile);
            run_rust(&host).expect("direct workflow");
            let facts = host.snapshot();
            assert_eq!(
                facts["effects"],
                json!([
                    {"order":1,"kind":"key_down","key":key},
                    {"order":1,"kind":"key_up","key":key}
                ])
            );
            assert_eq!(facts["receipts"][0]["status"], "Submitted");
            assert_eq!(facts["postconditions"][0]["satisfied"], true);
            assert_eq!(facts["live_handles"], 0);
            assert_eq!(host.finish()["clean"], true);
            assert_eq!(host.snapshot()["attempt_owners"], 0);
        }
    }

    #[test]
    fn input_receipts_do_not_prove_postconditions_or_replay_uncertain_actions() {
        for (scenario, expected_status, submitted) in [
            ("partial", "Partial", 1),
            ("uncertain", "Uncertain", 1),
            ("postcondition-absent", "Submitted", 2),
        ] {
            let host = make_host(scenario, "template-first");
            run_rust(&host).expect("typed receipt workflow");
            let facts = host.snapshot();
            assert_eq!(facts["receipts"][0]["status"], expected_status);
            assert_eq!(facts["receipts"][0]["submitted"], submitted);
            assert_eq!(facts["accepted"].as_array().expect("accepted").len(), 1);
            assert_eq!(facts["postconditions"][0]["satisfied"], false);
            let cleanup = host.finish();
            assert_eq!(cleanup["clean"], true);
            assert_eq!(host.snapshot()["held_keys"], json!([]));
            assert_eq!(host.snapshot()["receipts"][0]["status"], expected_status);
        }
    }

    #[test]
    fn absence_timeout_and_backend_failure_remain_distinct() {
        let absent = make_host("no-match", "template-first");
        run_rust(&absent).expect("absence is not a backend failure");
        assert_eq!(absent.snapshot()["accepted"], json!([]));
        assert_eq!(absent.finish()["clean"], true);

        let timed = ready_host("query-absent");
        let observation = observe(&timed);
        let pending = query(&timed, &observation).expect("query accepted");
        assert_eq!(
            timed
                .call("query_wait", json!({"id":pending["id"],"timeout_ms":5}))
                .expect_err("absent condition")
                .category,
            "Timeout"
        );
        assert_eq!(
            timed
                .call("release", json!({"id":pending["id"]}))
                .expect_err("wait released its ownership")
                .category,
            "InvalidHandle"
        );
        assert_eq!(timed.finish()["clean"], true);

        let failed = make_host("backend-failure", "template-first");
        let error = run_rust(&failed).expect_err("backend failure");
        assert_eq!(error.category, "Backend");
        assert_eq!(error.context["cause"], "injected-backend-failure");
        assert_eq!(failed.snapshot()["accepted"], json!([]));
        assert_eq!(failed.finish()["clean"], true);
    }

    #[test]
    fn released_forged_and_old_attempt_observations_cannot_admit_actions() {
        let old = ready_host("success");
        let observation = observe(&old);
        let retained = query(&old, &observation).expect("query retains source physically");
        old.call("release", json!({"id":observation["id"]}))
            .expect("logical observation release");
        assert_eq!(
            submit(&old, &observation, "A")
                .expect_err("released observation")
                .category,
            "InvalidHandle"
        );
        old.call("release", json!({"id":retained["id"]}))
            .expect("query release");
        assert_eq!(old.finish()["clean"], true);
        let fresh = ready_host("success");
        assert_eq!(
            submit(&fresh, &observation, "A")
                .expect_err("old attempt")
                .category,
            "StaleIdentity"
        );
        let mut forged = observe(&fresh);
        forged["width"] = json!(1);
        assert_eq!(
            submit(&fresh, &forged, "A")
                .expect_err("modified provenance")
                .category,
            "StaleIdentity"
        );
        assert_eq!(fresh.snapshot()["effects"], json!([]));
        assert_eq!(fresh.finish()["clean"], true);
    }

    #[test]
    fn postconditions_require_newer_frames_and_actual_visible_content() {
        let host = ready_host("success");
        let before = observe(&host);
        assert_eq!(
            host.call(
                "postcondition",
                json!({"observation":before,"checkpoint":before,"expected":"DONE"})
            )
            .expect_err("old checkpoint")
            .category,
            "StaleFrame"
        );
        let merely_new = observe(&host);
        assert_eq!(
            host.call(
                "postcondition",
                json!({"observation":merely_new,"checkpoint":before,"expected":"DONE"})
            )
            .expect("fresh but unsatisfied")["satisfied"],
            false
        );
        let accepted = submit(&host, &before, "A").expect("submit");
        host.call("settle", json!({"id":accepted["id"]}))
            .expect("receipt");
        assert_eq!(
            host.call(
                "postcondition",
                json!({"observation":merely_new,"checkpoint":before,"expected":"DONE"})
            )
            .expect("captured content is immutable")["satisfied"],
            false
        );
        let changed = observe(&host);
        assert_eq!(
            host.call(
                "postcondition",
                json!({"observation":changed,"checkpoint":before,"expected":"DONE"})
            )
            .expect("visible effect")["satisfied"],
            true
        );
        assert_eq!(host.finish()["clean"], true);
    }

    #[test]
    fn stop_does_not_wait_for_the_physical_work_lock_or_drop_its_owner() {
        let host = ready_host("held-work");
        let observation = observe(&host);
        let pending = query(&host, &observation).expect("held query");
        assert_eq!(host.snapshot()["in_flight_native"], 1);
        assert_eq!(
            query(&host, &observation)
                .expect_err("one worker only")
                .category,
            "WorkCapacity"
        );
        let waiter = host.clone();
        let id = pending["id"].clone();
        let thread =
            thread::spawn(move || waiter.call("query_wait", json!({"id":id,"timeout_ms":500})));
        host.control().cancel();
        assert!(!host.control().admission.load(Ordering::Acquire));
        assert_eq!(
            thread
                .join()
                .expect("waiter exits")
                .expect_err("logical cancellation")
                .category,
            "Cancelled"
        );
        assert_eq!(host.finish()["clean"], false);
        assert_eq!(host.snapshot()["in_flight_native"], 1);
        host.call("fixture", json!({"event":"release_hold"}))
            .expect("release remains available");
        wait_until(|| host.snapshot()["in_flight_native"] == 0);
        assert_eq!(host.finish()["clean"], true);
        assert_eq!(
            host.call("query_wait", json!({"id":pending["id"],"timeout_ms":5}))
                .expect_err("late result cannot revive")
                .category,
            "Cancelled"
        );
        assert_eq!(host.snapshot()["effects"], json!([]));
    }

    #[test]
    fn held_worker_registration_cannot_miss_a_prior_release() {
        let host = ready_host("held-work");
        let observation = observe(&host);
        let frame = host
            .observation(&lock(&host.inner.state), &observation)
            .expect("retained observation");
        // Dispatch can observe the hold before release, then register its worker afterward.
        host.call("fixture", json!({"event":"release_hold"}))
            .expect("release before worker registration");
        let work = host.start_held_work(frame).expect("physical worker");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !work.completed.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        let completed_without_second_release = work.completed.load(Ordering::Acquire);
        // Reap the worker even when the regression fails.
        host.call("fixture", json!({"event":"release_hold"}))
            .expect("release for test cleanup");
        wait_until(|| host.snapshot()["in_flight_native"] == 0);
        drop(work);
        assert_eq!(host.finish()["clean"], true);
        assert!(
            completed_without_second_release,
            "a release before registration must not leave physical work held"
        );
    }

    #[test]
    fn return_cancels_unawaited_sequences_and_failure_survives_log_pressure() {
        let host = ready_host("success");
        let observation = observe(&host);
        submit(&host, &observation, "A").expect("unawaited sequence");
        for _ in 0..10 {
            host.call("log", json!({"message":"bounded diagnostic"}))
                .expect("bounded log");
        }
        host.fail(
            Fault::new("ImportRefused", "forbidden module")
                .with_context(json!({"specifier":"blocked"})),
        );
        host.fail(Fault::new("Cancelled", "secondary cleanup cause"));
        assert_eq!(
            submit(&host, &observation, "B")
                .expect_err("fatal failure latched")
                .category,
            "ImportRefused"
        );
        assert_eq!(host.finish()["clean"], true);
        let facts = host.snapshot();
        assert_eq!(facts["failure"]["category"], "ImportRefused");
        assert_eq!(facts["failure"]["context"]["specifier"], "blocked");
        assert_eq!(facts["dropped_logs"], 8);
        assert_eq!(facts["receipts"][0]["status"], "Cancelled");
        assert_eq!(facts["effects"], json!([]));
        assert_eq!(facts["live_handles"], 0);
        assert_eq!(facts["attempt_owners"], 0);
    }

    #[test]
    fn operation_metrics_include_errors_without_unbounded_method_labels() {
        let host = ready_host("success");
        host.call("wait", json!({"duration_ms":5}))
            .expect("bounded delay");
        assert_eq!(
            host.call("wait", json!({}))
                .expect_err("failed call recorded")
                .category,
            "Argument"
        );
        assert_eq!(
            host.call("unknown-one", json!({}))
                .expect_err("unknown method")
                .category,
            "Argument"
        );
        assert_eq!(
            host.call("unknown-two", json!({}))
                .expect_err("another unknown method")
                .category,
            "Argument"
        );
        let facts = host.snapshot();
        assert_eq!(facts["operation_metrics"]["wait"]["count"], 2);
        assert_eq!(facts["operation_metrics"]["wait"]["failures"], 1);
        assert!(
            facts["operation_metrics"]["wait"]["total_us"]
                .as_u64()
                .expect("monotonic duration")
                >= 5_000
        );
        assert_eq!(facts["operation_metrics"]["unknown"]["count"], 2);
        assert!(facts["operation_metrics"].get("unknown-one").is_none());
        assert_eq!(host.finish()["clean"], true);
    }
}
