//! System-wide monotonic clock shared by a supervisor and its owned child.
//!
//! `Instant` values are process-local, so the supervisor's deadline cannot cross
//! the payload boundary directly. Both processes instead read the OS clock that
//! `Instant` itself is built on, and every conversion rounds against the child:
//! whatever spawn, payload transfer and validation cost, the child's bound is
//! never later than the supervisor's original deadline.
use crate::model::Fault;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// The supervisor's absolute operation deadline in whole microseconds of the
/// shared clock. Its origin is OS-defined and meaningless across boots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct SharedDeadline(u64);

impl SharedDeadline {
    /// Supervisor side. Rounds down, so the wire value never passes `deadline`.
    pub(super) fn from_instant(deadline: Instant) -> Result<Self, Fault> {
        // Shared clock first: the later Instant read only shrinks the remainder.
        let shared = now_ns()? / 1_000;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let remaining = u64::try_from(remaining.as_micros()).unwrap_or(u64::MAX);
        Ok(Self(shared.saturating_add(remaining)))
    }

    /// Child side. Rounds its own reading up, so the local deadline never passes
    /// the supervisor's; an already-elapsed deadline is simply now.
    pub(super) fn instant(self) -> Result<Instant, Fault> {
        // Instant first: the later shared read only shrinks the remainder.
        let local = Instant::now();
        let shared = now_ns()?.div_ceil(1_000);
        let remaining = Duration::from_micros(self.0.saturating_sub(shared));
        // An unrepresentable far deadline refuses rather than extends.
        Ok(local.checked_add(remaining).unwrap_or(local))
    }
}

/// Nanoseconds on the clock `Instant::now` reads on this target.
#[cfg(unix)]
fn now_ns() -> Result<u64, Fault> {
    #[cfg(target_vendor = "apple")]
    const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_vendor = "apple"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut now = std::mem::MaybeUninit::<libc::timespec>::zeroed();
    // SAFETY: `now` is valid writable storage for exactly one timespec, which the
    // call fills synchronously before returning 0; zeroed padding stays valid.
    #[expect(
        unsafe_code,
        reason = "reading the shared monotonic clock into local storage"
    )]
    let now = unsafe {
        if libc::clock_gettime(CLOCK, now.as_mut_ptr()) != 0 {
            return Err(Fault::new(
                "Clock",
                std::io::Error::last_os_error().to_string(),
            ));
        }
        now.assume_init()
    };
    let negative = || Fault::new("Clock", "monotonic clock reading is negative");
    let seconds = u64::try_from(now.tv_sec).map_err(|_| negative())?;
    let nanos = u64::try_from(now.tv_nsec).map_err(|_| negative())?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|nanoseconds| nanoseconds.checked_add(nanos))
        .ok_or_else(|| Fault::new("Clock", "monotonic clock exceeds the transport range"))
}

/// Nanoseconds on the clock `Instant::now` reads on this target.
#[cfg(windows)]
fn now_ns() -> Result<u64, Fault> {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let mut ticks = 0i64;
    let mut frequency = 0i64;
    // SAFETY: both out-pointers address live, writable i64 locals for the calls.
    #[expect(
        unsafe_code,
        reason = "reading the shared performance counter into local storage"
    )]
    let succeeded = unsafe {
        QueryPerformanceFrequency(&raw mut frequency) != 0
            && QueryPerformanceCounter(&raw mut ticks) != 0
    };
    if !succeeded || frequency <= 0 || ticks < 0 {
        return Err(Fault::new("Clock", "performance counter unavailable"));
    }
    // ticks × 1e9 overflows u64 within hours at common frequencies.
    let nanoseconds =
        u128::from(ticks.unsigned_abs()) * 1_000_000_000 / u128::from(frequency.unsigned_abs());
    u64::try_from(nanoseconds)
        .map_err(|_| Fault::new("Clock", "performance counter exceeds the transport range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Control, Limits, Plan, StopReason};

    fn limits() -> Limits {
        serde_json::from_str::<Plan>(include_str!("../../fixtures/manual-plan.json"))
            .unwrap()
            .limits
    }

    #[test]
    fn transferred_deadline_never_passes_the_supervisor_deadline() {
        let limits = limits();
        let supervisor = Control::new(&limits);
        let wire = SharedDeadline::from_instant(supervisor.deadline()).unwrap();
        let child = Control::with_deadline(&limits, wire.instant().unwrap());
        // Every spawn/transfer/validation microsecond is charged to the child.
        assert!(child.deadline() <= supervisor.deadline());
        // The bound is the supervisor's remaining time, not a collapsed value;
        // half of the 10 s manual budget tolerates any realistic test stall.
        assert!(child.deadline() > Instant::now() + Duration::from_secs(5));
        assert_eq!(child.stop_reason(), None);
        child.cancel();
        assert_eq!(child.stop_reason(), Some(StopReason::Cancelled));
    }

    #[test]
    fn elapsed_supervisor_deadline_reaches_the_child_as_timeout_not_a_fresh_budget() {
        let limits = limits();
        let elapsed = Instant::now() - Duration::from_millis(1);
        let supervisor = Control::with_deadline(&limits, elapsed);
        let wire = SharedDeadline::from_instant(supervisor.deadline()).unwrap();
        let child = Control::with_deadline(&limits, wire.instant().unwrap());
        assert!(child.deadline() <= Instant::now());
        assert_eq!(child.stop_reason(), Some(StopReason::Timeout));
        assert_eq!(child.admit_launch().unwrap_err().category, "Timeout");
        child.cancel();
        assert_eq!(child.stop_reason(), Some(StopReason::Timeout));
    }
}
