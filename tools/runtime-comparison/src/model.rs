use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::sync::Mutex;
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

    /// Diagnostic detail cannot displace the primary category, validated exit
    /// evidence, or the separate native-cleanup obligation in the terminal protocol.
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
            let exit_reason = match (self.category.as_str(), self.context["exit_reason"].as_str()) {
                ("TargetExited", Some("absent" | "reused_pid" | "zombie")) => {
                    Some(self.context["exit_reason"].take())
                }
                _ => None,
            };
            let exit_stage =
                exit_reason.is_some() && self.context["stage"] == "target_process_exit";
            self.context = serde_json::json!({});
            if native_unverified {
                self.context["native_cleanup"] = serde_json::json!("unverified");
            }
            if let Some(reason) = exit_reason {
                self.context["exit_reason"] = reason;
                if exit_stage {
                    self.context["stage"] = serde_json::json!("target_process_exit");
                }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeBudgets {
    pub startup_ms: u64,
    pub readiness_ms: u64,
    pub workflow_ms: u64,
}

impl NativeBudgets {
    pub const DEFAULT: Self = Self {
        startup_ms: 60_000,
        readiness_ms: 30_000,
        workflow_ms: 30_000,
    };

    pub const CEILINGS: Self = Self {
        workflow_ms: 900_000,
        ..Self::DEFAULT
    };

    pub fn operation_ms(self, recoveries: u8) -> Result<u64, Fault> {
        if recoveries > 1 {
            return Err(Fault::new(
                "NativeRefused",
                "At most one exit recovery is supported",
            ));
        }
        self.total_ms()?
            .checked_mul(1 + u64::from(recoveries))
            .and_then(|total| total.checked_add(u64::from(recoveries) * 3_000))
            .ok_or_else(|| Fault::new("NativeRefused", "Native operation deadline overflows"))
    }

    pub fn total_ms(self) -> Result<u64, Fault> {
        for (value, ceiling) in [
            (self.startup_ms, Self::CEILINGS.startup_ms),
            (self.readiness_ms, Self::CEILINGS.readiness_ms),
            (self.workflow_ms, Self::CEILINGS.workflow_ms),
        ] {
            if value == 0 || value > ceiling {
                return Err(Fault::new(
                    "NativeRefused",
                    "Native phase budgets exceed host policy",
                ));
            }
        }
        self.startup_ms
            .checked_add(self.readiness_ms)
            .and_then(|sum| sum.checked_add(self.workflow_ms))
            .ok_or_else(|| Fault::new("NativeRefused", "Native budget sum overflows"))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_budgets: Option<NativeBudgets>,
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
        if let Some(budgets) = self.native_budgets {
            if self.lane != "native"
                || budgets.total_ms()? != self.limits.duration_ms
                || budgets.readiness_ms != self.limits.readiness_ms
            {
                return Err(Fault::new(
                    "InvalidPlan",
                    "Native phase budgets do not match the plan",
                ));
            }
        }
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
const PHASE_TIMEOUT: u8 = 32;

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
    native_clock: Mutex<Option<NativeClock>>,
    owner: Option<std::sync::Arc<Control>>,
    admission_gate: Mutex<()>,
}

#[derive(Clone, Copy, Debug)]
struct NativeClock {
    budgets: NativeBudgets,
    phase: u8,
    deadline: Instant,
    settled: bool,
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
        Self::from_deadline(started, started + Duration::from_millis(limits.duration_ms))
    }

    fn from_deadline(started: Instant, deadline: Instant) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            admission: AtomicBool::new(false),
            stop_us: AtomicU64::new(0),
            closed_us: AtomicU64::new(0),
            cause: AtomicU8::new(0),
            transition: AtomicU8::new(0),
            started,
            deadline,
            native_clock: Mutex::new(None),
            owner: None,
            admission_gate: Mutex::new(()),
        }
    }

    pub fn cancel(&self) {
        self.stop(StopReason::Cancelled);
    }

    pub(crate) fn stop(&self, reason: StopReason) {
        let _gate = self
            .admission_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        if let Some(reason) = self.owner.as_ref().and_then(|owner| owner.stop_reason()) {
            self.latch(Self::cause_bits(reason));
        }
        if self.cause.load(Ordering::Acquire) & 3 == 0 {
            let clock = self.native_clock.lock().unwrap_or_else(|e| e.into_inner());
            if clock.is_some_and(|clock| !clock.settled && Instant::now() >= clock.deadline) {
                // A phase deadline is an independently earned failure.
                self.latch(2 | PHASE_TIMEOUT);
            }
        }
        if self.cause.load(Ordering::Acquire) & 3 == 0 && Instant::now() >= self.deadline {
            // The absolute deadline is provisional until the supervisor verdict.
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
        if state & PHASE_TIMEOUT != 0 {
            let clock = self.native_clock.lock().unwrap_or_else(|e| e.into_inner());
            let phase = clock.map_or("startup", |clock| match clock.phase {
                0 => "startup",
                1 => "readiness",
                _ => "workflow",
            });
            fault.message = format!("{phase} deadline expired");
            fault.context = serde_json::json!({"stage":phase});
        }
        Err(fault)
    }

    /// Admit one external launch. A later Stop cannot revoke this admission.
    pub fn admit_launch(&self) -> Result<(), Fault> {
        let authority = self.owner.as_deref().unwrap_or(self);
        let _gate = authority
            .admission_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        self.native_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .filter(|clock| !clock.settled)
            .map_or(self.deadline, |clock| clock.deadline.min(self.deadline))
    }

    pub(crate) fn outer_deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn with_deadline(limits: &Limits, deadline: Instant) -> Self {
        let mut control = Self::new(limits);
        control.deadline = control.deadline.min(deadline);
        control
    }

    pub(crate) fn reserved(limits: &Limits) -> Result<Self, Fault> {
        let started = Instant::now();
        let deadline = started
            .checked_add(Duration::from_millis(limits.duration_ms))
            .ok_or_else(|| Fault::new("Clock", "operation deadline overflows"))?;
        Ok(Self::from_deadline(started, deadline))
    }

    pub(crate) fn for_attempt(owner: std::sync::Arc<Self>, limits: &Limits, initial: bool) -> Self {
        let mut control = Self::new(limits);
        control.deadline = owner.deadline;
        if initial {
            control.started = owner.started;
        }
        control.owner = Some(owner);
        control
    }

    /// Stop and the single successor admission share this short-held gate.
    pub(crate) fn admit_recovery(&self) -> Result<(), Fault> {
        let _gate = self
            .admission_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.check()?;
        self.cause
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cause| {
                (cause & (3 | 64) == 0).then_some(cause | 64)
            })
            .map(|_| ())
            .map_err(|_| {
                self.check().err().unwrap_or_else(|| {
                    Fault::new("RecoveryRefused", "Recovery credit already consumed")
                })
            })
    }

    pub(crate) fn start_native(
        &self,
        budgets: NativeBudgets,
        inherited: Option<Instant>,
    ) -> Result<(), Fault> {
        budgets.total_ms()?;
        let mut clock = self.native_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.is_some() {
            return Err(Fault::new(
                "ReadinessContract",
                "Native clock already started",
            ));
        }
        let deadline = self
            .started
            .checked_add(Duration::from_millis(budgets.startup_ms))
            .ok_or_else(|| Fault::new("Clock", "startup deadline overflows"))?
            .min(self.deadline);
        *clock = Some(NativeClock {
            budgets,
            phase: 0,
            deadline: inherited.map_or(deadline, |at| at.min(deadline)),
            settled: false,
        });
        Ok(())
    }

    /// Authenticated entry settlement ends ordinary stage time, not the outer
    /// deadline or any already-earned Stop/timeout. Keep the clock for attribution.
    pub(crate) fn settle_native(&self) {
        let mut guard = self.native_clock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(clock) = guard.as_mut() {
            clock.settled = true;
        }
    }

    /// Transition once, with a conservative absolute deadline from the child
    /// when called by its supervisor. No preceding stage's unused time is lent.
    pub(crate) fn native_transition(
        &self,
        phase: u8,
        inherited: Option<Instant>,
    ) -> Result<Instant, Fault> {
        self.check()?;
        let mut guard = self.native_clock.lock().unwrap_or_else(|e| e.into_inner());
        let clock = guard
            .as_mut()
            .ok_or_else(|| Fault::new("ReadinessContract", "Native clock missing"))?;
        if clock.settled {
            return Err(Fault::new(
                "ReadinessContract",
                "Native entry has already settled",
            ));
        }
        if Instant::now() >= clock.deadline {
            self.latch(2 | PHASE_TIMEOUT);
        }
        if self.cause.load(Ordering::Acquire) & 3 != 0 {
            drop(guard);
            return Err(self.check().expect_err("latched stop cannot reopen"));
        }
        if phase != clock.phase + 1 || phase > 2 {
            return Err(Fault::new(
                "ReadinessContract",
                "Native phase transition is not a successor",
            ));
        }
        let budget = if phase == 1 {
            clock.budgets.readiness_ms
        } else {
            clock.budgets.workflow_ms
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(budget))
            .ok_or_else(|| Fault::new("Clock", "phase deadline overflows"))?
            .min(self.deadline);
        clock.phase = phase;
        clock.deadline = inherited.map_or(deadline, |at| at.min(deadline));
        Ok(clock.deadline)
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

    fn native_control(budgets: NativeBudgets) -> Control {
        let mut plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        plan.limits.duration_ms = budgets.total_ms().unwrap();
        Control::new(&plan.limits)
    }

    #[test]
    fn recovery_deadline_is_fixed_and_default_disabled() {
        for (budgets, total, recovered) in [
            (NativeBudgets::DEFAULT, 120_000, 243_000),
            (NativeBudgets::CEILINGS, 990_000, 1_983_000),
        ] {
            assert_eq!(budgets.operation_ms(0).unwrap(), total);
            assert_eq!(budgets.operation_ms(1).unwrap(), recovered);
            for recoveries in [2, u8::MAX] {
                assert_eq!(
                    budgets.operation_ms(recoveries).unwrap_err().category,
                    "NativeRefused"
                );
            }
            let mut plan: Plan =
                serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
            plan.limits.duration_ms = recovered;
            plan.limits.validate().unwrap();
            let owner = std::sync::Arc::new(Control::reserved(&plan.limits).unwrap());
            let outer = owner.started + Duration::from_millis(recovered);
            assert_eq!(owner.outer_deadline(), outer);
            let initial = Control::for_attempt(owner.clone(), &plan.limits, true);
            initial.start_native(budgets, None).unwrap();
            initial.admit_launch().unwrap();
            initial.cancel();
            assert!(
                owner.check().is_ok(),
                "attempt settlement must not cancel its owner"
            );
            owner.admit_recovery().unwrap();
            let fresh = Control::for_attempt(owner.clone(), &plan.limits, false);
            fresh.start_native(budgets, None).unwrap();
            fresh.native_transition(1, None).unwrap();
            fresh.native_transition(2, None).unwrap();
            assert_eq!(fresh.outer_deadline(), outer);
            assert_eq!(owner.outer_deadline(), outer);
            fresh.admit_launch().unwrap();
            assert!(owner.admit_recovery().is_err());
            owner.cancel();
            assert_eq!(fresh.check().unwrap_err().category, "Cancelled");
            assert_eq!(fresh.admit_launch().unwrap_err().category, "Cancelled");
        }
    }

    #[test]
    fn native_budget_arithmetic_refuses_invalid_phases_before_clock_construction() {
        for (field, ceiling) in [
            ("startup_ms", 60_000),
            ("readiness_ms", 30_000),
            ("workflow_ms", 900_000),
        ] {
            for value in [0, ceiling + 1, u64::MAX] {
                let mut raw = serde_json::to_value(NativeBudgets::DEFAULT).unwrap();
                raw[field] = json!(value);
                let budgets: NativeBudgets = serde_json::from_value(raw).unwrap();
                assert_eq!(budgets.total_ms().unwrap_err().category, "NativeRefused");
                assert_eq!(
                    budgets.operation_ms(1).unwrap_err().category,
                    "NativeRefused"
                );
                assert_eq!(
                    launch_control()
                        .start_native(budgets, None)
                        .unwrap_err()
                        .category,
                    "NativeRefused"
                );
            }
        }
    }

    #[test]
    fn recovery_and_launch_never_clear_outer_stop_or_expiry() {
        let plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        for expired in [false, true] {
            let mut owner = Control::new(&plan.limits);
            if expired {
                owner.deadline = Instant::now() - Duration::from_millis(1);
            } else {
                owner.cancel();
            }
            let owner = std::sync::Arc::new(owner);
            let fresh = Control::for_attempt(owner.clone(), &plan.limits, false);
            assert!(owner.admit_recovery().is_err());
            assert!(fresh.admit_launch().is_err());
            assert_eq!(fresh.outer_deadline(), owner.outer_deadline());
        }
    }

    #[test]
    fn stop_and_recovery_share_one_admission_decision() {
        use std::sync::{Arc, Barrier};
        for _ in 0..32 {
            let owner = Arc::new(launch_control());
            let barrier = Arc::new(Barrier::new(2));
            let contender = Arc::clone(&owner);
            let gate = Arc::clone(&barrier);
            let admission = std::thread::spawn(move || {
                gate.wait();
                contender.admit_recovery()
            });
            barrier.wait();
            owner.cancel();
            let admitted = admission.join().unwrap().is_ok();
            assert_eq!(owner.cause.load(Ordering::Acquire) & 64 != 0, admitted);
            assert_eq!(owner.stop_reason(), Some(StopReason::Cancelled));
            assert!(owner.admit_recovery().is_err());
        }
    }

    #[test]
    fn native_stage_transitions_never_renew_or_borrow_unused_budgets() {
        for budgets in [NativeBudgets::DEFAULT, NativeBudgets::CEILINGS] {
            for capped in [false, true] {
                let mut control = native_control(budgets);
                if capped {
                    control.deadline = Instant::now() + Duration::from_secs(10);
                }
                let outer = control.outer_deadline();
                control.start_native(budgets, None).unwrap();
                let startup = control.deadline();
                assert!(control.start_native(budgets, None).is_err());
                assert_eq!(control.deadline(), startup);
                let before = Instant::now();
                let ready = control.native_transition(1, None).unwrap();
                let readiness = Duration::from_millis(budgets.readiness_ms);
                assert!(ready >= (before + readiness).min(outer));
                assert!(ready <= (Instant::now() + readiness).min(outer));
                if !capped {
                    assert!(ready < startup);
                }
                assert!(control.native_transition(1, None).is_err());
                assert_eq!(control.deadline(), ready);
                let before = Instant::now();
                let workflow = control.native_transition(2, None).unwrap();
                let duration = Duration::from_millis(budgets.workflow_ms);
                assert!(workflow >= (before + duration).min(outer));
                assert!(workflow <= (Instant::now() + duration).min(outer));
                assert!(control.native_transition(2, None).is_err());
                for _ in 0..3 {
                    control.check().unwrap();
                    assert_eq!(control.deadline(), workflow);
                    assert_eq!(control.outer_deadline(), outer);
                }
                // Advance the selected stage deterministically, not wall-clock time.
                control
                    .native_clock
                    .get_mut()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_mut()
                    .unwrap()
                    .deadline = Instant::now() - Duration::from_millis(1);
                let fault = control.check().unwrap_err();
                assert_eq!(fault.category, "Timeout");
                assert_eq!(fault.context["stage"], "workflow");
                assert!(!fault.is_provisional_timeout());
                assert!(control.native_transition(2, None).is_err());
                control.settle_native();
                assert_eq!(control.check().unwrap_err().context["stage"], "workflow");
                assert_eq!(control.outer_deadline(), outer);
            }
        }
    }

    #[test]
    fn native_startup_charges_reservation_time_and_phase_timeout_is_not_provisional() {
        let budgets = NativeBudgets {
            startup_ms: 100,
            readiness_ms: 1_000,
            workflow_ms: 1_000,
        };
        let mut control = native_control(budgets);
        control.started -= Duration::from_millis(101);
        control.start_native(budgets, None).unwrap();
        let fault = control.admit_launch().unwrap_err();
        assert_eq!(fault.category, "Timeout");
        assert_eq!(fault.context["stage"], "startup");
        assert!(!fault.is_provisional_timeout());
        control.inherit_stop(StopReason::Cancelled);
        assert_eq!(control.check().unwrap_err().context["stage"], "startup");
        assert!(control.native_transition(1, None).is_err());
    }

    #[test]
    fn transferred_phase_deadlines_can_only_shorten_the_admitted_phase() {
        let budgets = NativeBudgets {
            startup_ms: 1_000,
            readiness_ms: 1_000,
            workflow_ms: 1_000,
        };
        let control = native_control(budgets);
        let outer = control.outer_deadline();
        control.start_native(budgets, None).unwrap();
        let inherited = Instant::now() - Duration::from_millis(1);
        assert_eq!(
            control.native_transition(1, Some(inherited)).unwrap(),
            inherited
        );
        let fault = control.check().unwrap_err();
        assert_eq!(fault.context["stage"], "readiness");
        assert!(!fault.is_provisional_timeout());
        assert_eq!(control.outer_deadline(), outer);
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
    fn settled_native_stage_cannot_expire_or_restart_during_cleanup() {
        let budgets = NativeBudgets {
            startup_ms: 5_000,
            readiness_ms: 5_000,
            workflow_ms: 5_000,
        };
        let mut control = native_control(budgets);
        control.start_native(budgets, None).unwrap();
        control.native_transition(1, None).unwrap();
        control.native_transition(2, None).unwrap();
        let outer = control.outer_deadline();
        control.settle_native();
        // Advance only the old stage past expiry; settlement must not poll it.
        control
            .native_clock
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .unwrap()
            .deadline = Instant::now() - Duration::from_millis(1);
        control.settle_native();
        assert!(control.check().is_ok());
        assert_eq!(control.deadline(), outer);
        assert!(control.native_transition(2, None).is_err());
        assert!(control.start_native(budgets, None).is_err());
        control.deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(control.check().unwrap_err().category, "Timeout");
    }

    #[test]
    fn native_settlement_preserves_earlier_stage_timeout_and_owner_stop() {
        let budgets = NativeBudgets {
            startup_ms: 5_000,
            readiness_ms: 5_000,
            workflow_ms: 5_000,
        };
        let plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        let owner = std::sync::Arc::new(native_control(budgets));
        let attempt = Control::for_attempt(owner.clone(), &plan.limits, true);
        attempt.start_native(budgets, None).unwrap();
        attempt.native_transition(1, None).unwrap();
        attempt
            .native_transition(2, Some(Instant::now() - Duration::from_millis(1)))
            .unwrap();
        let earlier = attempt.check().unwrap_err();
        assert_eq!(earlier.category, "Timeout");
        assert_eq!(earlier.context["stage"], "workflow");
        attempt.settle_native();
        let retained = attempt.check().unwrap_err();
        assert_eq!(retained.category, "Timeout");
        assert_eq!(retained.context["stage"], "workflow");
        assert!(!retained.is_provisional_timeout());
        assert!(owner.check().is_ok());

        let fresh = Control::for_attempt(owner.clone(), &plan.limits, false);
        fresh.start_native(budgets, None).unwrap();
        fresh.settle_native();
        owner.cancel();
        assert_eq!(fresh.check().unwrap_err().category, "Cancelled");
        assert_eq!(owner.admit_recovery().unwrap_err().category, "Cancelled");
        assert_eq!(fresh.outer_deadline(), owner.outer_deadline());
    }

    #[test]
    fn oversized_context_retains_only_validated_target_exit_evidence() {
        for reason in ["absent", "reused_pid", "zombie"] {
            let mut fault =
                Fault::new("TargetExited", "bound lifetime ended").with_context(json!({
                    "exit_reason":reason,"stage":"target_process_exit",
                    "stack":"x".repeat(MAX_DIAGNOSTIC_BYTES),
                    "native_cleanup":"unverified","unknown_authority":{"recover":true}
                }));
            fault.bound_diagnostics();
            assert_eq!(fault.context["exit_reason"], reason);
            assert_eq!(fault.context["stage"], "target_process_exit");
            assert_eq!(fault.context["native_cleanup"], "unverified");
            assert_eq!(
                fault.context["diagnostic_truncation"]["context_omitted"],
                true
            );
            assert!(fault.context.get("unknown_authority").is_none());
            encode_bounded(&fault.context, MAX_DIAGNOSTIC_BYTES).unwrap();
        }
        for (category, reason, stage, retained) in [
            (
                "TargetLost",
                json!("absent"),
                json!("target_process_exit"),
                false,
            ),
            (
                "TargetExited",
                json!("unknown"),
                json!("target_process_exit"),
                false,
            ),
            (
                "TargetExited",
                json!({"absent":true}),
                json!("target_process_exit"),
                false,
            ),
            (
                "TargetExited",
                json!("absent"),
                json!("untrusted-stage"),
                true,
            ),
        ] {
            let mut fault = Fault::new(category, "diagnostic detail").with_context(json!({
                "exit_reason":reason,"stage":stage,"stack":"x".repeat(MAX_DIAGNOSTIC_BYTES)
            }));
            fault.bound_diagnostics();
            assert_eq!(fault.context.get("exit_reason").is_some(), retained);
            assert!(fault.context.get("stage").is_none());
        }
    }

    #[test]
    fn bounded_encoding_counts_escaped_json_bytes() {
        let error = encode_bounded(&"\0".repeat(100), 102).expect_err("escaping exceeds bound");
        assert_eq!(error.category, "LimitExceeded");
        assert_eq!(encode_bounded(&"abc", 5).expect("exact bound"), b"\"abc\"");
    }
}
