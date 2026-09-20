use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const ENGINE_REVISION: &str = "2c9d57a53e44ffc97315975c3ca46a766d6c8539";
pub const MAX_TRANSPORT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub category: String,
    pub message: String,
    pub context: Value,
}

impl Fault {
    pub fn new(category: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            message: message.into(),
            context: Value::Null,
        }
    }

    pub fn with_context(mut self, context: Value) -> Self {
        self.context = context;
        self
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.category, self.message)
    }
}

impl std::error::Error for Fault {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub duration_ms: u64,
    pub readiness_ms: u64,
    pub wait_ms: u64,
    pub cleanup_ms: u64,
    pub containment_ms: u64,
    pub queue_capacity: usize,
    pub handles: usize,
    pub log_records: usize,
    pub log_bytes: usize,
    pub vm_bytes: usize,
    pub max_actions: usize,
    pub snapshot_files: usize,
    pub snapshot_bytes: usize,
}

impl Limits {
    pub fn validate(&self) -> Result<(), Fault> {
        for (name, value) in [
            ("duration_ms", self.duration_ms),
            ("readiness_ms", self.readiness_ms),
            ("wait_ms", self.wait_ms),
            ("cleanup_ms", self.cleanup_ms),
            ("containment_ms", self.containment_ms),
        ] {
            if value == 0 || value > 3_600_000 {
                return Err(Fault::new(
                    "InvalidPlan",
                    format!("{name} must be in 1..=3600000"),
                ));
            }
        }
        if self.readiness_ms > self.duration_ms
            || self.wait_ms > self.duration_ms
            || self.cleanup_ms > self.containment_ms
        {
            return Err(Fault::new(
                "InvalidPlan",
                "stage bounds exceed their enclosing bound",
            ));
        }
        for (name, value, ceiling) in [
            ("queue_capacity", self.queue_capacity, 1024),
            ("handles", self.handles, 4096),
            ("log_records", self.log_records, 4096),
            ("log_bytes", self.log_bytes, 262_144),
            ("vm_bytes", self.vm_bytes, 256 * 1024 * 1024),
            ("max_actions", self.max_actions, 4096),
            ("snapshot_files", self.snapshot_files, 1024),
            ("snapshot_bytes", self.snapshot_bytes, 2 * 1024 * 1024),
        ] {
            if value == 0 || value > ceiling {
                return Err(Fault::new(
                    "InvalidPlan",
                    format!("{name} must be in 1..={ceiling}"),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub id: String,
    pub candidate: String,
    pub lane: String,
    pub scenario: String,
    pub profile: String,
    pub limits: Limits,
    pub samples: usize,
    pub warmups: usize,
    pub repetitions: usize,
    pub budgets: BTreeMap<String, f64>,
    pub native_config: Option<Value>,
}

impl Plan {
    pub fn validate(&self) -> Result<(), Fault> {
        if self.version != 1
            || !portable_label(&self.id)
            || !portable_label(&self.scenario)
            || !portable_label(&self.profile)
        {
            return Err(Fault::new(
                "InvalidPlan",
                "unsupported version or nonportable plan label",
            ));
        }
        if !["rust", "javascript", "lua", "typescript"].contains(&self.candidate.as_str())
            || !["controlled", "replay", "native"].contains(&self.lane.as_str())
        {
            return Err(Fault::new(
                "InvalidPlan",
                "unknown candidate or evidence lane",
            ));
        }
        if self.lane == "controlled"
            && ![
                "success",
                "no-match",
                "partial",
                "uncertain",
                "postcondition-absent",
                "backend-failure",
                "query-absent",
                "held-work",
            ]
            .contains(&self.scenario.as_str())
        {
            return Err(Fault::new("InvalidPlan", "unknown controlled scenario"));
        }
        self.limits.validate()?;
        if !(1..=1000).contains(&self.samples)
            || self.warmups > 100
            || !(1..=1000).contains(&self.repetitions)
            || self.samples.saturating_mul(self.repetitions) > 1000
        {
            return Err(Fault::new(
                "InvalidPlan",
                "sample/repetition bounds exceeded",
            ));
        }
        for name in [
            "startup_us",
            "preflight_us",
            "host_call_us",
            "workflow_us",
            "stop_receipt_us",
            "admission_close_us",
            "cleanup_us",
            "containment_us",
            "cpu_percent",
            "supervisor_rss_bytes",
            "child_rss_bytes",
            "vm_bytes",
            "live_owners",
        ] {
            let Some(value) = self.budgets.get(name) else {
                return Err(Fault::new(
                    "InvalidPlan",
                    format!("missing prospective budget: {name}"),
                ));
            };
            if !value.is_finite() || *value <= 0.0 {
                return Err(Fault::new(
                    "InvalidPlan",
                    format!("budget {name} must be positive and finite"),
                ));
            }
        }
        if self.budgets.len() != 13 {
            return Err(Fault::new("InvalidPlan", "unrecognized budget name"));
        }
        if self.lane == "controlled" && self.native_config.is_some() {
            return Err(Fault::new(
                "InvalidPlan",
                "controlled plans cannot carry native authority",
            ));
        }
        Ok(())
    }
}

fn portable_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

#[derive(Debug)]
pub struct Control {
    pub cancelled: AtomicBool,
    pub admission: AtomicBool,
    pub stop_us: AtomicU64,
    pub closed_us: AtomicU64,
    cause: AtomicU8,
    started: Instant,
    deadline: Instant,
}

impl Control {
    pub fn new(limits: &Limits) -> Self {
        let started = Instant::now();
        Self {
            cancelled: AtomicBool::new(false),
            admission: AtomicBool::new(false),
            stop_us: AtomicU64::new(0),
            closed_us: AtomicU64::new(0),
            cause: AtomicU8::new(0),
            started,
            deadline: started + Duration::from_millis(limits.duration_ms),
        }
    }

    pub fn cancel(&self) {
        self.latch(1);
    }

    fn latch(&self, cause: u8) {
        let _ = self
            .cause
            .compare_exchange(0, cause, Ordering::AcqRel, Ordering::Acquire);
        let now = self.elapsed_us().max(1);
        let _ = self
            .stop_us
            .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
        self.cancelled.store(true, Ordering::Release);
        self.admission.store(false, Ordering::Release);
        let _ = self.closed_us.compare_exchange(
            0,
            self.elapsed_us().max(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub fn check(&self) -> Result<(), Fault> {
        if self.cause.load(Ordering::Acquire) == 0 && Instant::now() >= self.deadline {
            self.latch(2);
        }
        match self.cause.load(Ordering::Acquire) {
            2 => Err(Fault::new("Timeout", "attempt deadline expired")),
            1 => Err(Fault::new("Cancelled", "attempt cancellation is latched")),
            _ => Ok(()),
        }
    }

    pub fn elapsed_us(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RuntimeMetrics {
    pub vm_bytes: Option<usize>,
    pub jobs_executed: u64,
    pub source_diagnostic: Option<Value>,
}

pub fn identity<T: Serialize>(value: &T) -> Result<String, Fault> {
    let bytes = serde_json::to_vec(value).map_err(|e| Fault::new("Encoding", e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
