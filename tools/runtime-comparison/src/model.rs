use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const ENGINE_REVISION: &str = "4b4f3296838a9eecdcb00e9d2bb3121a25cdc240";
pub const MAX_TRANSPORT_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;

/// Refuse excess bytes during serialization, rather than allocating an
/// unbounded intermediate buffer before checking the transport limit.
pub(crate) fn encode_bounded<T: Serialize>(value: &T, bound: usize) -> Result<Vec<u8>, Fault> {
    struct Output {
        bytes: Vec<u8>,
        bound: usize,
        exceeded: bool,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.bound.saturating_sub(self.bytes.len()) {
                self.exceeded = true;
                return Err(std::io::Error::other("JSON byte bound exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Output {
        bytes: Vec::new(),
        bound,
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut output, value) {
        return Err(if output.exceeded {
            Fault::new("LimitExceeded", "JSON exceeds its transport byte bound")
        } else {
            Fault::new("Encoding", error.to_string())
        });
    }
    Ok(output.bytes)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub category: String,
    pub message: String,
    pub context: Value,
    #[serde(skip)]
    provisional_timeout: bool,
}

impl Fault {
    pub fn new(category: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            message: message.into(),
            context: Value::Null,
            provisional_timeout: false,
        }
    }

    pub fn with_context(mut self, context: Value) -> Self {
        self.context = context;
        self
    }

    pub(crate) fn is_provisional_timeout(&self) -> bool {
        self.provisional_timeout
    }

    /// Diagnostic detail cannot displace the primary category or the separate
    /// native-cleanup obligation in the terminal protocol.
    pub(crate) fn bound_diagnostics(&mut self) {
        let original_bytes = self.message.len();
        if original_bytes > MAX_DIAGNOSTIC_BYTES {
            let mut end = MAX_DIAGNOSTIC_BYTES;
            while !self.message.is_char_boundary(end) {
                end -= 1;
            }
            self.message.truncate(end);
        }
        let context_omitted = encode_bounded(&self.context, MAX_DIAGNOSTIC_BYTES).is_err();
        if context_omitted {
            let native_unverified = self.context["native_cleanup"] == "unverified";
            self.context = serde_json::json!({});
            if native_unverified {
                self.context["native_cleanup"] = serde_json::json!("unverified");
            }
        }
        let message_bytes_dropped = original_bytes - self.message.len();
        if message_bytes_dropped != 0 || context_omitted {
            if !self.context.is_object() {
                self.context = serde_json::json!({"cause":self.context});
            }
            self.context["diagnostic_truncation"] = serde_json::json!({
                "message_bytes_dropped":message_bytes_dropped,
                "context_omitted":context_omitted,
                "field_byte_limit":MAX_DIAGNOSTIC_BYTES
            });
        }
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
            (
                "snapshot_bytes",
                self.snapshot_bytes,
                crate::images::PACKAGE_BYTES,
            ),
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

// `cause` bits: 1 Cancelled, 2 Timeout, 4 launch admitted. DERIVED marks a
// Timeout this process inferred from its own deadline reading rather than
// received as an explicit Stop; SUPERSEDED records that the supervisor's
// verdict replaced such a Timeout.
const DERIVED: u8 = 8;
const SUPERSEDED: u8 = 16;

#[derive(Debug)]
pub struct Control {
    pub cancelled: AtomicBool,
    pub admission: AtomicBool,
    pub stop_us: AtomicU64,
    pub closed_us: AtomicU64,
    cause: AtomicU8,
    // One outgoing successor: unscheduled, queued, admitted, or closed.
    transition: AtomicU8,
    started: Instant,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StopReason {
    #[serde(rename = "Stop")]
    Cancelled,
    Timeout,
}

impl StopReason {
    pub(crate) fn fault(self) -> Fault {
        match self {
            Self::Cancelled => Fault::new("Cancelled", "attempt cancellation is latched"),
            Self::Timeout => Fault::new("Timeout", "attempt deadline expired"),
        }
    }
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
            transition: AtomicU8::new(0),
            started,
            deadline: started + Duration::from_millis(limits.duration_ms),
        }
    }

    pub fn cancel(&self) {
        self.stop(StopReason::Cancelled);
    }

    pub(crate) fn stop(&self, reason: StopReason) {
        self.latch(Self::cause_bits(reason));
    }

    /// The supervising process accepted this Stop for the same operation. Its
    /// verdict supersedes only a Timeout this process derived from its own
    /// deadline reading, never an explicit stop already latched here, so a Stop
    /// accepted before the deadline keeps its attribution even when this
    /// process could not observe anything until after that deadline.
    pub(crate) fn inherit_stop(&self, reason: StopReason) {
        let cause = Self::cause_bits(reason);
        self.settle(|state| {
            if state & 3 == 0 {
                Some(state | cause)
            } else if state & DERIVED != 0 {
                Some((state & !(3 | DERIVED)) | cause | SUPERSEDED)
            } else {
                None
            }
        });
    }

    /// The supervisor verdict that superseded a Timeout derived here, if any.
    pub(crate) fn superseding_verdict(&self) -> Option<StopReason> {
        let state = self.cause.load(Ordering::Acquire);
        (state & SUPERSEDED != 0).then(|| {
            if state & 3 == 1 {
                StopReason::Cancelled
            } else {
                StopReason::Timeout
            }
        })
    }

    fn cause_bits(reason: StopReason) -> u8 {
        match reason {
            StopReason::Cancelled => 1,
            StopReason::Timeout => 2,
        }
    }

    fn latch(&self, cause: u8) {
        self.settle(|state| (state & 3 == 0).then_some(state | cause));
    }

    fn settle(&self, cause: impl FnMut(u8) -> Option<u8>) {
        // Launch admission and the first stop cause share one linearization point.
        let _ = self
            .cause
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, cause);
        self.transition.store(3, Ordering::Release);
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

    fn stop_state(&self) -> u8 {
        if self.cause.load(Ordering::Acquire) & 3 == 0 && Instant::now() >= self.deadline {
            self.latch(2 | DERIVED);
        }
        self.cause.load(Ordering::Acquire)
    }

    pub fn stop_reason(&self) -> Option<StopReason> {
        match self.stop_state() & 3 {
            2 => Some(StopReason::Timeout),
            1 => Some(StopReason::Cancelled),
            _ => None,
        }
    }

    pub fn check(&self) -> Result<(), Fault> {
        let state = self.stop_state();
        let reason = match state & 3 {
            2 => StopReason::Timeout,
            1 => StopReason::Cancelled,
            _ => return Ok(()),
        };
        let mut fault = reason.fault();
        fault.provisional_timeout = state & DERIVED != 0;
        Err(fault)
    }

    /// Admit one external launch. A later Stop cannot revoke this admission.
    pub fn admit_launch(&self) -> Result<(), Fault> {
        self.check()?;
        self.cause
            .compare_exchange(0, 4, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| {
                self.check().err().unwrap_or_else(|| {
                    Fault::new(
                        "NativeLaunchRefused",
                        "launch was already admitted for this operation",
                    )
                })
            })
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn with_deadline(limits: &Limits, deadline: Instant) -> Self {
        let mut control = Self::new(limits);
        control.deadline = control.deadline.min(deadline);
        control
    }

    // Each predecessor permits one successor. Stop and transition admission
    // linearize on one atomic, without acquiring a VM/native/dispatch lock.
    pub(crate) fn queue_transition(&self) -> Result<(), Fault> {
        self.check()?;
        self.transition
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                self.check().err().unwrap_or_else(|| {
                    Fault::new("TransitionRefused", "fresh attempt is already scheduled")
                })
            })?;
        self.check()
    }

    pub(crate) fn admit_transition(&self) -> Result<(), Fault> {
        self.check()?;
        self.transition
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                self.check().err().unwrap_or_else(|| {
                    Fault::new("TransitionRefused", "fresh attempt is not pending")
                })
            })?;
        self.check()
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
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value)
        .map_err(|error| Fault::new("Encoding", error.to_string()))?;
    Ok(format!("{:x}", writer.0.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn launch_control() -> Control {
        let plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        Control::new(&plan.limits)
    }

    #[test]
    fn launch_admission_is_one_shot_and_stop_cannot_be_undone() {
        let stopped = launch_control();
        stopped.cancel();
        assert_eq!(stopped.admit_launch().unwrap_err().category, "Cancelled");
        let admitted = launch_control();
        admitted.admit_launch().unwrap();
        assert_eq!(
            admitted.admit_launch().unwrap_err().category,
            "NativeLaunchRefused"
        );
        admitted.cancel();
        assert_eq!(admitted.admit_launch().unwrap_err().category, "Cancelled");
        assert_eq!(admitted.stop_reason(), Some(StopReason::Cancelled));
    }

    #[test]
    fn concurrent_launch_and_stop_share_the_same_linearization_point() {
        use std::sync::{Arc, Barrier};
        for _ in 0..64 {
            let control = Arc::new(launch_control());
            let barrier = Arc::new(Barrier::new(2));
            let launch_control = control.clone();
            let launch_barrier = barrier.clone();
            let launch = std::thread::spawn(move || {
                launch_barrier.wait();
                launch_control.admit_launch()
            });
            barrier.wait();
            control.cancel();
            let accepted = launch.join().unwrap().is_ok();
            assert_eq!(control.cause.load(Ordering::Acquire) & 4 != 0, accepted);
            assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
            assert_eq!(control.admit_launch().unwrap_err().category, "Cancelled");
        }
    }

    #[test]
    fn concurrent_launches_cannot_both_obtain_admission() {
        let control = launch_control();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let contender = scope.spawn(|| {
                barrier.wait();
                control.admit_launch()
            });
            barrier.wait();
            let first = control.admit_launch();
            let second = contender.join().unwrap();
            assert_ne!(first.is_ok(), second.is_ok());
            let refusal = first.err().or_else(|| second.err()).unwrap();
            assert_eq!(refusal.category, "NativeLaunchRefused");
        });
    }

    #[test]
    fn expired_operation_cannot_admit_launch_or_regrant_its_deadline() {
        let plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        let deadline = Instant::now() - Duration::from_millis(1);
        let control = Control::with_deadline(&plan.limits, deadline);
        assert_eq!(control.deadline(), deadline);
        assert_eq!(control.admit_launch().unwrap_err().category, "Timeout");
        control.cancel();
        assert_eq!(control.stop_reason(), Some(StopReason::Timeout));
    }

    #[test]
    fn inherited_stop_supersedes_only_this_process_derived_timeout() {
        let plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        let expired =
            || Control::with_deadline(&plan.limits, Instant::now() - Duration::from_millis(1));
        // Derived Timeout, then the supervisor's earlier-accepted Stop.
        let late = expired();
        assert_eq!(late.stop_reason(), Some(StopReason::Timeout));
        assert_eq!(late.superseding_verdict(), None);
        late.inherit_stop(StopReason::Cancelled);
        assert_eq!(late.stop_reason(), Some(StopReason::Cancelled));
        assert_eq!(late.check().unwrap_err().category, "Cancelled");
        assert_eq!(late.superseding_verdict(), Some(StopReason::Cancelled));
        // An explicit local Stop after a derived Timeout still does not flip it,
        // and leaves the Timeout awaiting the supervisor's verdict.
        let shown = expired();
        assert_eq!(shown.stop_reason(), Some(StopReason::Timeout));
        shown.cancel();
        assert_eq!(shown.stop_reason(), Some(StopReason::Timeout));
        assert_eq!(shown.superseding_verdict(), None);
        // Inheritance never displaces an explicit cause latched here.
        let settled = Control::new(&plan.limits);
        settled.cancel();
        settled.inherit_stop(StopReason::Timeout);
        assert_eq!(settled.stop_reason(), Some(StopReason::Cancelled));
        assert_eq!(settled.superseding_verdict(), None);
        let relayed = expired();
        relayed.inherit_stop(StopReason::Timeout);
        relayed.inherit_stop(StopReason::Cancelled);
        assert_eq!(relayed.stop_reason(), Some(StopReason::Timeout));
        // A verdict adopted as the first cause supersedes nothing.
        let prompt = Control::new(&plan.limits);
        prompt.inherit_stop(StopReason::Cancelled);
        assert_eq!(prompt.stop_reason(), Some(StopReason::Cancelled));
        assert_eq!(prompt.superseding_verdict(), None);
        // An admitted launch survives the reattribution.
        let launched = Control::new(&plan.limits);
        launched.admit_launch().unwrap();
        launched.inherit_stop(StopReason::Cancelled);
        assert_eq!(launched.cause.load(Ordering::Acquire) & 4, 4);
        assert_eq!(launched.stop_reason(), Some(StopReason::Cancelled));
        assert_eq!(launched.admit_launch().unwrap_err().category, "Cancelled");
    }

    #[test]
    fn oversized_diagnostics_preserve_classification_and_native_cleanup_obligation() {
        let message = "界".repeat(MAX_DIAGNOSTIC_BYTES);
        let original_bytes = message.len();
        let mut fault = Fault::new("Script", message).with_context(json!({
            "stack":"x".repeat(MAX_DIAGNOSTIC_BYTES * 2),
            "native_cleanup":"unverified"
        }));
        fault.bound_diagnostics();
        assert_eq!(fault.category, "Script");
        assert_eq!(fault.context["native_cleanup"], "unverified");
        assert_eq!(
            fault.context["diagnostic_truncation"]["context_omitted"],
            true
        );
        assert_eq!(
            fault.context["diagnostic_truncation"]["message_bytes_dropped"],
            original_bytes - fault.message.len()
        );
        assert!(fault.message.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(fault.message.chars().all(|character| character == '界'));
        encode_bounded(&fault, MAX_TRANSPORT_BYTES - 1)
            .expect("bounded fault remains transportable");
    }

    #[test]
    fn bounded_encoding_counts_escaped_json_bytes() {
        let error = encode_bounded(&"\0".repeat(100), 102).expect_err("escaping exceeds bound");
        assert_eq!(error.category, "LimitExceeded");
        assert_eq!(encode_bounded(&"abc", 5).expect("exact bound"), b"\"abc\"");
    }
}
