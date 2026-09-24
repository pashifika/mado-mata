use super::{Host, Phase, ScheduledAttempt, lock};
use crate::model::{Control, Fault};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// One attempt-wide ceiling. Reservations never acquire host or native work locks.
#[derive(Debug)]
pub(crate) struct HandleBudget {
    limit: usize,
    pub(super) live: AtomicUsize,
}

impl HandleBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            live: AtomicUsize::new(0),
        }
    }

    pub(crate) fn reserve(self: &Arc<Self>, count: usize) -> Result<HandlePermit, Fault> {
        self.live
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                live.checked_add(count).filter(|next| *next <= self.limit)
            })
            .map_err(|_| Fault::new("HandleLimit", "managed handle capacity exhausted"))?;
        Ok(HandlePermit {
            budget: Arc::clone(self),
            count,
        })
    }
}

#[derive(Debug)]
pub(crate) struct HandlePermit {
    budget: Arc<HandleBudget>,
    count: usize,
}

impl HandlePermit {
    #[cfg(feature = "engine")]
    pub(crate) fn split_one(&mut self) -> Self {
        assert!(self.count > 0, "a reservation must own the split handle");
        self.count -= 1;
        Self {
            budget: Arc::clone(&self.budget),
            count: 1,
        }
    }
}

impl Drop for HandlePermit {
    fn drop(&mut self) {
        self.budget.live.fetch_sub(self.count, Ordering::AcqRel);
    }
}

/// The permit follows the logical owner, including moves into active work.
pub(crate) struct Managed<T> {
    pub(crate) owner: T,
    _permit: HandlePermit,
}

impl<T> Managed<T> {
    pub(crate) fn new(value: T, permit: HandlePermit) -> Self {
        Self {
            owner: value,
            _permit: permit,
        }
    }
}

impl<T> std::ops::Deref for Managed<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.owner
    }
}

impl<T> std::ops::DerefMut for Managed<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.owner
    }
}

impl ScheduledAttempt {
    pub(crate) fn control(&self) -> Arc<Control> {
        Arc::clone(&self.control)
    }

    pub(crate) fn admit(self) -> Result<Host, Fault> {
        self.control.check()?;
        self.predecessor.fresh_predecessor()?;
        self.predecessor.inner.control.admit_transition()?;
        self.control.check()?;
        let old = &self.predecessor.inner;
        let fresh = Host::new(
            old.plan.clone(),
            old.options.clone(),
            old.assets.clone(),
            Arc::clone(&self.control),
        )?;
        {
            let previous = lock(&old.state);
            let mut state = lock(&fresh.inner.state);
            state.session = previous.session.checked_add(1).ok_or_else(|| {
                Fault::new("TransitionRefused", "target session generation exhausted")
            })?;
            if previous.current_process_lifetime != previous.retained_process_lifetime {
                // Only the explicit new attempt binds the injected replacement.
                state.retained_process_lifetime = previous.current_process_lifetime.clone();
                state.current_process_lifetime = previous.current_process_lifetime.clone();
            }
        }
        if let Err(error) = self.control.check() {
            fresh.finish();
            return Err(error);
        }
        Ok(fresh)
    }
}

impl Host {
    fn fresh_predecessor(&self) -> Result<(), Fault> {
        if self.inner.plan.lane != "controlled" {
            return Err(Fault::new(
                "Authority",
                "fresh-attempt scheduling is restricted to the controlled harness",
            ));
        }
        self.inner.control.check()?;
        // Target loss terminates the predecessor, not an explicitly scheduled
        // controlled successor after verified cleanup. Preserve its first fault;
        // Stop remains independently latched by the control check above.
        if let Some(fault) = self
            .failure()
            .filter(|fault| fault.category != "TargetLost")
        {
            return Err(fault);
        }
        let state = lock(&self.inner.state);
        if state.phase != Phase::Finished {
            return Err(Fault::new(
                "TransitionRefused",
                "predecessor has not terminated",
            ));
        }
        if state
            .cleanup
            .as_ref()
            .is_none_or(|cleanup| cleanup["clean"] != true)
            || !state.handles.is_empty()
            || !state.queue.is_empty()
            || state.active.is_some()
            || !state.held_keys.is_empty()
            || state.receipts.len() != state.released_receipts.len()
            || self.inner.physical.load(Ordering::Acquire) != 0
            || !lock(&self.inner.workers).is_empty()
        {
            return Err(Fault::new(
                "IncompleteCleanup",
                "fresh attempt requires a settled predecessor ownership baseline",
            ));
        }
        Ok(())
    }

    pub(crate) fn schedule_fresh(&self) -> Result<ScheduledAttempt, Fault> {
        self.fresh_predecessor()?;
        self.inner.control.queue_transition()?;
        let control = Arc::new(Control::new(&self.inner.plan.limits));
        Ok(ScheduledAttempt {
            predecessor: self.clone(),
            control,
        })
    }

    pub(super) fn close_admission(&self) {
        self.inner.control.admission.store(false, Ordering::Release);
        let _ = self.inner.control.closed_us.compare_exchange(
            0,
            self.inner.control.elapsed_us().max(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(super) fn check(&self) -> Result<(), Fault> {
        if let Some(failure) = self.failure() {
            return Err(failure);
        }
        if let Err(error) = self.inner.control.check() {
            if error.category == "Timeout" {
                self.fail(error.clone());
            }
            return Err(error);
        }
        if self.inner.terminating.load(Ordering::Acquire) {
            return Err(Fault::new("AdmissionClosed", "attempt is terminating"));
        }
        let state = lock(&self.inner.state);
        if state.phase == Phase::Finished {
            return Err(Fault::new("AdmissionClosed", "attempt has finished"));
        }
        if !state.alive || state.current_process_lifetime != state.retained_process_lifetime {
            return Err(Fault::new(
                "TargetLost",
                "authorized process lifetime ended",
            ));
        }
        if state.phase == Phase::Workflow && !self.inner.control.admission.load(Ordering::Acquire) {
            return Err(Fault::new(
                "AdmissionClosed",
                "ordinary work admission is closed",
            ));
        }
        if state.phase == Phase::Readiness
            && state.readiness_started.is_some_and(|started| {
                started.elapsed() >= Duration::from_millis(self.inner.plan.limits.readiness_ms)
            })
        {
            drop(state);
            let fault = Fault::new("Timeout", "readiness deadline expired");
            self.fail(fault.clone());
            return Err(fault);
        }
        Ok(())
    }

    pub fn begin_readiness(&self) -> Result<(), Fault> {
        self.check()?;
        {
            let mut state = lock(&self.inner.state);
            if state.phase != Phase::Instantiating {
                return Err(Fault::new("ReadinessContract", "readiness may start once"));
            }
            state.phase = Phase::Readiness;
            state.readiness_started = Some(Instant::now());
        }
        // Establish an eligible current observation before entering package readiness.
        let observation = self.call("observe", json!({}))?;
        self.call("release", json!({"id":observation["id"]}))?;
        Ok(())
    }

    pub fn begin_workflow(&self) -> Result<(), Fault> {
        self.check()?;
        let mut state = lock(&self.inner.state);
        if state.phase != Phase::Readiness {
            return Err(Fault::new(
                "ReadinessContract",
                "workflow requires explicit readiness",
            ));
        }
        state.phase = Phase::Workflow;
        self.inner.control.admission.store(true, Ordering::Release);
        drop(state);
        if let Err(error) = self.check() {
            self.close_admission();
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn input_check(&self, observation: &Value) -> Result<(), Fault> {
        self.check()?;
        if !self.inner.control.admission.load(Ordering::Acquire) {
            return Err(Fault::new(
                "AdmissionClosed",
                "ordinary input admission is closed",
            ));
        }
        {
            let state = lock(&self.inner.state);
            if state.phase != Phase::Workflow {
                return Err(Fault::new(
                    "AdmissionClosed",
                    "input requires workflow readiness",
                ));
            }
            if !state.route {
                return Err(Fault::new(
                    "RouteRefused",
                    "configured input route is no longer authorized",
                ));
            }
            if !state.focus {
                return Err(Fault::new(
                    "FocusRefused",
                    "configured focus requirement is not satisfied",
                ));
            }
            if self.inner.plan.lane == "controlled" {
                self.observation(&state, observation)?;
            }
        }
        #[cfg(feature = "engine")]
        if let Some(engine) = &self.inner.engine {
            engine.call("validate_observation", json!({"observation":observation}))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
