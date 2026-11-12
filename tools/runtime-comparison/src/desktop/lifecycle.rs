//! Controller decisions under the State mutex. Active still owns the real worker
//! and Control; a phase is not evidence that either has physically settled.

use super::NativePhase;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Phase {
    Idle,
    Preparing,
    Running,
    Recovering,
    Stopping,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Progress {
    Preparing,
    Running,
    Recovering,
    ChildStarted,
}

impl Progress {
    pub(super) fn native(phase: NativePhase) -> Self {
        match phase {
            NativePhase::Settling | NativePhase::Recovering => Self::Recovering,
            NativePhase::Workflow | NativePhase::Readiness | NativePhase::Function => Self::Running,
            _ => Self::Preparing,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionRefusal {
    Closed,
    Active,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StopDecision {
    Stale,
    Inactive,
    Cancel { notify: bool },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct Lifecycle {
    closed: bool,
    phase: Phase,
}

impl Lifecycle {
    pub(super) fn new() -> Self {
        Self {
            closed: false,
            phase: Phase::Idle,
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(super) fn admission(&self, worker_owned: bool) -> Result<(), AdmissionRefusal> {
        if self.closed {
            Err(AdmissionRefusal::Closed)
        } else if worker_owned {
            Err(AdmissionRefusal::Active)
        } else {
            Ok(())
        }
    }

    // Published only after spawning succeeds, while the reservation lock is held.
    pub(super) fn started(&mut self) {
        self.phase = Phase::Preparing;
    }

    pub(super) fn progress(&mut self, progress: Progress) {
        if self.phase == Phase::Stopping {
            return;
        }
        self.phase = match progress {
            Progress::Preparing => Phase::Preparing,
            Progress::Running => Phase::Running,
            Progress::Recovering => Phase::Recovering,
            Progress::ChildStarted if self.phase == Phase::Preparing => Phase::Running,
            Progress::ChildStarted => self.phase,
        };
    }

    pub(super) fn stop(
        &mut self,
        current: Option<&str>,
        requested: &str,
        worker_owned: bool,
    ) -> StopDecision {
        if current != Some(requested) {
            return StopDecision::Stale;
        }
        if !worker_owned {
            return StopDecision::Inactive;
        }
        let notify = self.phase != Phase::Stopping;
        self.phase = Phase::Stopping;
        StopDecision::Cancel { notify }
    }

    pub(super) fn close(&mut self, worker_owned: bool) {
        self.closed = true;
        if worker_owned {
            self.phase = Phase::Stopping;
        }
    }

    // Called after the real finished worker is joined and its evidence imported,
    // never merely because is_finished() was observed or Stop was requested.
    pub(super) fn settled(&mut self) {
        self.phase = Phase::Terminal;
    }

    pub(super) fn status(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "idle",
            Phase::Preparing => "preparing",
            Phase::Running => "running",
            Phase::Recovering => "recovering",
            Phase::Stopping => "stopping",
            Phase::Terminal => "terminal",
        }
    }
}

#[cfg(test)]
mod tests;
