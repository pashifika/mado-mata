use super::{Action, Host, Phase, Receipt, Sequence, argument, lock, object, string};
use crate::model::Fault;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

impl Action {
    pub(crate) fn event_count(&self) -> usize {
        match self {
            Self::Click { .. } => 3,
            Self::KeyDown { .. } | Self::KeyUp { .. } => 1,
        }
    }

    fn validate(&self, observation: &Value) -> Result<(), Fault> {
        match self {
            Self::KeyDown { key } | Self::KeyUp { key } => {
                if key.is_empty()
                    || key.len() > 32
                    || !key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err(argument("invalid portable key name"));
                }
            }
            Self::Click { x, y, .. } => {
                let width = observation["width"].as_u64().unwrap_or(0) as f64;
                let height = observation["height"].as_u64().unwrap_or(0) as f64;
                if !x.is_finite()
                    || !y.is_finite()
                    || *x < 0.0
                    || *y < 0.0
                    || *x >= width
                    || *y >= height
                {
                    return Err(argument(
                        "click must lie within the retained capture-pixel extent",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Host {
    pub(super) fn submit(&self, args: &Value) -> Result<Value, Fault> {
        let map = object(
            args,
            &["observation", "actions"],
            &["observation", "actions"],
        )?;
        let raw = map["actions"]
            .as_array()
            .ok_or_else(|| argument("actions must be an array"))?;
        if raw.is_empty() || raw.len() > self.inner.plan.limits.max_actions {
            return Err(argument("action sequence must be nonempty and bounded"));
        }
        let actions: Vec<Action> = serde_json::from_value(map["actions"].clone())
            .map_err(|error| argument(error.to_string()))?;
        let mut event_count = 0usize;
        for action in &actions {
            action.validate(&map["observation"])?;
            #[cfg(not(feature = "engine"))]
            let expanded = action.event_count();
            #[cfg(feature = "engine")]
            let expanded = self.inner.engine.as_ref().map_or_else(
                || action.event_count(),
                |engine| engine.action_event_count(action),
            );
            event_count = event_count
                .checked_add(expanded)
                .ok_or_else(|| argument("expanded input event count overflow"))?;
        }
        self.input_check(&map["observation"])?;
        #[cfg(feature = "engine")]
        if self.inner.plan.lane == "native" {
            let engine = self
                .inner
                .engine
                .as_ref()
                .ok_or_else(|| Fault::new("Blocked", "native engine is unavailable"))?;
            engine.call("validate_input", args.clone())?;
        }
        let mut state = lock(&self.inner.state);
        if self.inner.plan.lane == "controlled" {
            self.observation(&state, &map["observation"])?;
        }
        if !state.route {
            return Err(Fault::new(
                "RouteRefused",
                "route authority changed before enqueue",
            ));
        }
        if !state.focus {
            return Err(Fault::new("FocusRefused", "focus changed before enqueue"));
        }
        if state.queue.len() >= self.inner.plan.limits.queue_capacity {
            state.queue_rejections += 1;
            return Err(Fault::new("QueueCapacity", "bounded input queue is full"));
        }
        let permit = self.inner.handle_budget.reserve(1)?;
        let action_limit = self.inner.plan.limits.max_actions;
        #[cfg(feature = "engine")]
        let action_limit = self.inner.engine.as_ref().map_or(action_limit, |engine| {
            action_limit.min(engine.action_limit())
        });
        if event_count > action_limit.saturating_sub(state.admitted_actions) {
            return Err(Fault::new(
                "ActionLimit",
                "attempt action budget is exhausted",
            ));
        }
        // Recheck control while holding admission state, without acquiring any work lock.
        self.inner.control.check()?;
        if !self.inner.control.admission.load(Ordering::Acquire)
            || self.inner.terminating.load(Ordering::Acquire)
        {
            return Err(Fault::new(
                "AdmissionClosed",
                "admission closed before enqueue",
            ));
        }
        let id = self.next_id(&mut state, "sequence");
        let order = state.accepted.len() as u64 + 1;
        state.admitted_actions += event_count;
        state
            .accepted
            .push(json!({"id":id,"order":order,"actions":actions}));
        state.queue.push_back(Sequence {
            id: id.clone(),
            order,
            observation: map["observation"].clone(),
            actions,
            permit,
        });
        state.queue_high_water = state.queue_high_water.max(state.queue.len());
        Ok(json!({"id":id,"order":order}))
    }

    fn receipt(
        &self,
        sequence: &Sequence,
        status: &str,
        submitted: usize,
        reason: Option<&str>,
        cleanup: Vec<String>,
    ) -> Value {
        let mut receipt = json!({"id":sequence.id,"order":sequence.order,"status":status,"submitted":submitted,
            "total":sequence.actions.len(),"cleanup_required":cleanup,"sink":self.input_sink()});
        if let Some(reason) = reason {
            receipt["reason"] = json!(reason);
        }
        receipt
    }

    pub(super) fn input_sink(&self) -> &'static str {
        if self.inner.plan.lane == "native" {
            "native"
        } else {
            "controlled-non-native"
        }
    }

    pub(super) fn settle(&self, args: &Value) -> Result<Value, Fault> {
        let map = object(args, &["id"], &["id"])?;
        let id = string(map, "id")?;
        let started = Instant::now();
        let _serial = loop {
            if let Ok(guard) = self.inner.dispatch.try_lock() {
                break guard;
            }
            if started.elapsed() >= Duration::from_millis(self.inner.plan.limits.wait_ms) {
                return Err(Fault::new("Timeout", "input settlement wait expired"));
            }
            self.inner.control.check()?;
            thread::sleep(Duration::from_millis(1));
        };
        loop {
            let sequence = {
                let mut state = lock(&self.inner.state);
                if state.released_receipts.contains(id) {
                    return Err(Fault::new("InvalidHandle", "sequence handle was released"));
                }
                if let Some(receipt) = state.receipts.get(id) {
                    return Ok(receipt.value.clone());
                }
                if !state.queue.iter().any(|sequence| sequence.id == id) {
                    return Err(Fault::new(
                        "InvalidHandle",
                        "sequence is not accepted by this attempt",
                    ));
                }
                let sequence = state
                    .queue
                    .pop_front()
                    .ok_or_else(|| Fault::new("InvalidHandle", "sequence queue is empty"))?;
                state.active = Some(sequence.id.clone());
                sequence
            };
            let receipt = match self.input_check(&sequence.observation) {
                Err(error) => {
                    self.retain_terminal_fault(&error);
                    let mut receipt = self.receipt(
                        &sequence,
                        if error.category == "Cancelled" {
                            "Cancelled"
                        } else {
                            "Refused"
                        },
                        0,
                        Some(&error.category),
                        Vec::new(),
                    );
                    receipt["error"] = json!(error);
                    receipt
                }
                Ok(()) => self.dispatch_sequence(&sequence),
            };
            let mut state = lock(&self.inner.state);
            state.active = None;
            state.receipts.insert(
                sequence.id,
                Receipt {
                    value: receipt,
                    permit: Some(sequence.permit),
                },
            );
        }
    }

    fn dispatch_sequence(&self, sequence: &Sequence) -> Value {
        #[cfg(feature = "engine")]
        if self.inner.plan.lane == "native" {
            let result = self
                .inner
                .engine
                .as_ref()
                .ok_or_else(|| Fault::new("Blocked", "native engine is unavailable"))
                .and_then(|engine| {
                    engine.call(
                        "dispatch",
                        json!({
                            "observation":sequence.observation,"actions":sequence.actions
                        }),
                    )
                });
            return match result {
                Ok(mut receipt) => {
                    receipt["id"] = json!(sequence.id);
                    receipt["order"] = json!(sequence.order);
                    if receipt["possible_native_effect"] == true {
                        lock(&self.inner.state).dispatches += 1;
                    }
                    if receipt["status"] != "Submitted" {
                        let category = match receipt["fault"]["status"].as_str() {
                            Some(status) if status == mado_pilot::Status::TargetLost.as_str() => {
                                "TargetLost"
                            }
                            Some(status) if status == mado_pilot::Status::Closed.as_str() => {
                                "Closed"
                            }
                            _ => "NativeInput",
                        };
                        self.fail(
                            Fault::new(category, "native input did not complete")
                                .with_context(json!({"receipt":receipt})),
                        );
                    }
                    receipt
                }
                Err(error) => {
                    let mut receipt = self.receipt(
                        sequence,
                        if error.category == "Cancelled" {
                            "Cancelled"
                        } else {
                            "Refused"
                        },
                        0,
                        Some(&error.category),
                        Vec::new(),
                    );
                    receipt["error"] = json!(error);
                    self.fail(error);
                    receipt
                }
            };
        }
        if self.inner.held.load(Ordering::Acquire) && self.inner.plan.lane == "controlled" {
            let frame = {
                let state = lock(&self.inner.state);
                match self.observation(&state, &sequence.observation) {
                    Ok(frame) => frame,
                    Err(error) => {
                        self.retain_terminal_fault(&error);
                        return self.receipt(
                            sequence,
                            "Refused",
                            0,
                            Some(&error.category),
                            Vec::new(),
                        );
                    }
                }
            };
            let work = match self.start_held_work(frame) {
                Ok(work) => work,
                Err(error) => {
                    return self.receipt(sequence, "Refused", 0, Some(&error.category), Vec::new());
                }
            };
            let deadline = Instant::now() + Duration::from_millis(self.inner.plan.limits.wait_ms);
            while !work.completed.load(Ordering::Acquire) {
                if let Err(error) = self.check() {
                    self.retain_terminal_fault(&error);
                    return self.receipt(
                        sequence,
                        "Cancelled",
                        0,
                        Some(&error.category),
                        Vec::new(),
                    );
                }
                if Instant::now() >= deadline {
                    return self.receipt(sequence, "Uncertain", 0, Some("Timeout"), Vec::new());
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        let mut submitted = 0;
        let mut status = "Submitted";
        let mut reason = None;
        for action in &sequence.actions {
            if let Err(error) = self.input_check(&sequence.observation) {
                self.retain_terminal_fault(&error);
                status = if submitted > 0 {
                    "Partial"
                } else if error.category == "Cancelled" {
                    "Cancelled"
                } else {
                    "Refused"
                };
                reason = Some(error.category);
                break;
            }
            let mut state = lock(&self.inner.state);
            // Fixture invalidation and effects linearize on the same short-held state lock.
            if self.inner.plan.lane == "controlled" {
                if let Err(error) = self.observation(&state, &sequence.observation) {
                    self.retain_terminal_fault(&error);
                    status = if submitted > 0 { "Partial" } else { "Refused" };
                    reason = Some(error.category);
                    break;
                }
            }
            if !state.route
                || !state.focus
                || self.inner.control.cancelled.load(Ordering::Acquire)
                || !self.inner.control.admission.load(Ordering::Acquire)
            {
                status = if submitted > 0 { "Partial" } else { "Refused" };
                reason = Some("AdmissionClosed".into());
                break;
            }
            if submitted == 0 {
                state.dispatches += 1;
            }
            match action {
                Action::KeyDown { key } => {
                    state.held_keys.entry(key.clone()).or_insert(sequence.order);
                }
                Action::KeyUp { key } => {
                    if state.held_keys.remove(key).is_some()
                        && self.inner.plan.scenario != "postcondition-absent"
                    {
                        state.visible = "DONE".into();
                    }
                }
                Action::Click { .. } => {
                    if self.inner.plan.scenario != "postcondition-absent" {
                        state.visible = "DONE".into();
                    }
                }
            }
            let mut effect = json!(action);
            effect["order"] = json!(sequence.order);
            state.effects.push(effect);
            submitted += 1;
            if self.inner.plan.scenario == "partial" {
                status = "Partial";
                reason = Some("ControlledPartial".into());
                break;
            }
            if self.inner.plan.scenario == "uncertain" {
                status = "Uncertain";
                reason = Some("ControlledUncertain".into());
                break;
            }
        }
        let cleanup = lock(&self.inner.state)
            .held_keys
            .iter()
            .filter(|(_, order)| **order == sequence.order)
            .map(|(key, _)| key.clone())
            .collect();
        self.receipt(sequence, status, submitted, reason.as_deref(), cleanup)
    }

    pub(super) fn release(&self, args: &Value) -> Result<Value, Fault> {
        let map = object(args, &["id"], &["id"])?;
        let id = string(map, "id")?;
        let mut state = lock(&self.inner.state);
        if state.handles.remove(id).is_some() {
            return Ok(json!({"released":true}));
        }
        if state.receipts.contains_key(id) && state.released_receipts.insert(id.into()) {
            if let Some(receipt) = state.receipts.get_mut(id) {
                receipt.permit = None;
            }
            return Ok(json!({"released":true}));
        }
        if let Some(index) = state.queue.iter().position(|sequence| sequence.id == id) {
            let sequence = state
                .queue
                .remove(index)
                .ok_or_else(|| Fault::new("InvalidHandle", "sequence was already removed"))?;
            let receipt = self.receipt(&sequence, "Cancelled", 0, Some("Released"), Vec::new());
            state.receipts.insert(
                id.into(),
                Receipt {
                    value: receipt,
                    permit: None,
                },
            );
            state.released_receipts.insert(id.into());
            return Ok(json!({"released":true}));
        }
        drop(state);
        // Native handle identities are opaque; route by ownership, not spelling.
        #[cfg(feature = "engine")]
        if let Some(engine) = &self.inner.engine {
            return engine.call("release", args.clone());
        }
        Err(Fault::new(
            "InvalidHandle",
            "handle is released or belongs to another attempt",
        ))
    }

    pub fn finish(&self) -> Value {
        // Closure is observable before touching dispatch/native work locks.
        self.inner.terminating.store(true, Ordering::Release);
        self.close_admission();
        let started = Instant::now();
        let cleanup_ms = self.inner.plan.limits.cleanup_ms;
        #[cfg(feature = "engine")]
        let cleanup_ms = self
            .inner
            .engine
            .as_ref()
            .map_or(cleanup_ms, crate::engine::Engine::cleanup_limit_ms);
        {
            let mut state = lock(&self.inner.state);
            if let Some(cleanup) = &state.cleanup {
                if cleanup["clean"] == true {
                    return cleanup.clone();
                }
            }
            state.phase = Phase::Finished;
            while let Some(sequence) = state.queue.pop_front() {
                let receipt = self.receipt(
                    &sequence,
                    "Cancelled",
                    0,
                    Some("EntryTerminated"),
                    Vec::new(),
                );
                state.receipts.insert(
                    sequence.id,
                    Receipt {
                        value: receipt,
                        permit: Some(sequence.permit),
                    },
                );
            }
            state.handles.clear();
        }
        #[cfg(feature = "engine")]
        let engine_cleanup = self
            .inner
            .engine
            .as_ref()
            .map(crate::engine::Engine::finish);
        while started.elapsed() < Duration::from_millis(cleanup_ms) {
            self.reap_workers();
            if self.inner.physical.load(Ordering::Acquire) == 0
                && lock(&self.inner.workers).is_empty()
                && lock(&self.inner.state).active.is_none()
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.reap_workers();
        let mut state = lock(&self.inner.state);
        let active = state.active.is_some();
        if !active {
            let held = std::mem::take(&mut state.held_keys);
            for (key, order) in held {
                state.release_outcomes.push(
                    json!({"key":key,"order":order,"released":true,"sink":"controlled-non-native"}),
                );
            }
        }
        let physical = self
            .inner
            .physical
            .load(Ordering::Acquire)
            .max(lock(&self.inner.workers).len());
        let clean = physical == 0 && !active && state.held_keys.is_empty();
        #[cfg(feature = "engine")]
        let clean = clean
            && engine_cleanup
                .as_ref()
                .is_none_or(|engine| engine["clean"].as_bool() == Some(true));
        let result = json!({"clean":clean,"status":if clean {"CleanupFinished"} else {"IncompleteCleanup"},
            "cleanup_finished_us":if clean {Some(self.inner.control.elapsed_us())} else {None},
            "elapsed_us":started.elapsed().as_micros() as u64,
            "remaining":{"live_handles":state.handles.len(),"queued":state.queue.len(),"in_flight_native":physical,"active_sequences":usize::from(active),"held_keys":state.held_keys.len()},
            "release_outcomes":state.release_outcomes});
        #[cfg(feature = "engine")]
        let result = if let Some(engine) = engine_cleanup {
            let mut result = result;
            result["remaining"]["in_flight_native"] =
                json!(physical + engine["in_flight"].as_u64().unwrap_or(0) as usize);
            result["engine"] = engine;
            result
        } else {
            result
        };
        state.released_receipts = state.receipts.keys().cloned().collect();
        for receipt in state.receipts.values_mut() {
            receipt.permit = None;
        }
        state.cleanup = Some(result.clone());
        result
    }
}

#[cfg(test)]
mod tests;
