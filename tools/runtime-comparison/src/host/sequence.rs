use super::{
    Action, DragPoint, Host, Phase, PointerButton, Receipt, Sequence, State, argument, lock,
    object, string,
};
use crate::model::Fault;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DragEvent {
    Move { x: f64, y: f64 },
    Press { button: PointerButton },
    Delay { ms: u64 },
    Release { button: PointerButton },
}

// The validated path is borrowed; counting never needs an expanded event allocation.
pub(crate) fn drag_events(
    button: PointerButton,
    path: &[DragPoint],
) -> impl Iterator<Item = DragEvent> + '_ {
    path.iter()
        .enumerate()
        .flat_map(move |(index, point)| {
            [
                Some(DragEvent::Move {
                    x: point.x,
                    y: point.y,
                }),
                (index == 0).then_some(DragEvent::Press { button }),
                (point.hold_ms != 0).then_some(DragEvent::Delay { ms: point.hold_ms }),
            ]
            .into_iter()
            .flatten()
        })
        .chain(std::iter::once(DragEvent::Release { button }))
}

impl Action {
    // Call only after validation, which bounds each drag to at most 130 events.
    pub(crate) fn event_count(&self) -> usize {
        match self {
            Self::Click { .. } => 3,
            Self::KeyDown { .. } | Self::KeyUp { .. } => 1,
            Self::Drag { path, .. } => {
                path.len() + 2 + path.iter().filter(|point| point.hold_ms != 0).count()
            }
        }
    }

    pub(crate) fn scheduled_wait_ms(&self) -> u64 {
        match self {
            Self::Drag { path, .. } => path.iter().map(|point| point.hold_ms).sum(),
            _ => 0,
        }
    }

    pub(crate) fn validate(&self, extent: Option<(f64, f64)>) -> Result<(), Fault> {
        let point = |x: f64, y: f64| {
            if !x.is_finite()
                || !y.is_finite()
                || x < 0.0
                || y < 0.0
                || extent.is_some_and(|(width, height)| x >= width || y >= height)
            {
                Err(argument(
                    "pointer point must lie within the retained capture-pixel extent",
                ))
            } else {
                Ok(())
            }
        };
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
            Self::Click { x, y, .. } => point(*x, *y)?,
            Self::Drag { path, .. } => {
                if !(2..=64).contains(&path.len()) {
                    return Err(argument("drag path must contain 2..=64 points"));
                }
                let mut wait_ms = 0u64;
                for waypoint in path {
                    point(waypoint.x, waypoint.y)?;
                    if waypoint.hold_ms > 1000 {
                        return Err(argument("drag point hold_ms must be in 0..=1000"));
                    }
                    wait_ms = wait_ms
                        .checked_add(waypoint.hold_ms)
                        .ok_or_else(|| argument("drag wait sum overflow"))?;
                }
                if wait_ms > 5000 {
                    return Err(argument("drag holds must total at most 5000 ms"));
                }
            }
        }
        Ok(())
    }
}

/// Charge a whole expanded batch at dispatch, including a subsequently partial
/// submission. Expiry, not release or scope churn, restores capacity.
#[derive(Debug, Default)]
pub(crate) struct EventWindow {
    batches: std::collections::VecDeque<(Instant, usize)>,
    used: usize,
}

impl EventWindow {
    pub(crate) fn charge(
        &mut self,
        now: Instant,
        count: usize,
        max: usize,
        interval_ms: u64,
    ) -> Result<(), Fault> {
        if count == 0 || count > max {
            return Err(Fault::new(
                "ActionLimit",
                "expanded input batch exceeds its limit",
            ));
        }
        let interval = Duration::from_millis(interval_ms);
        while let Some(&(at, events)) = self.batches.front() {
            if now.saturating_duration_since(at) < interval {
                break;
            }
            self.used -= events;
            self.batches.pop_front();
        }
        if self.used > max - count {
            let mut needed = self.used - (max - count);
            for &(at, events) in &self.batches {
                if events >= needed {
                    let retry = interval.saturating_sub(now.saturating_duration_since(at));
                    return Err(Fault::new("InputRate", "expanded input batch exceeds the rolling rate")
                        .with_context(json!({"stage":"input_rate","retry_after_ms":retry.as_nanos().div_ceil(1_000_000)})));
                }
                needed -= events;
            }
            unreachable!("charged events belong to retained batches");
        }
        self.batches.push_back((now, count));
        self.used += count;
        Ok(())
    }
}

impl Host {
    fn scope_id(&self) -> u64 {
        self.scope.as_ref().map_or(0, super::WorkflowScope::id)
    }

    // The native engine charges immediately before send_input. The supervised
    // non-native sink uses the same rolling-window rule at its dispatch edge.
    fn charge_dispatch(&self, sequence: &Sequence) -> Result<(), Fault> {
        if self.inner.plan.lane == "native" || !self.inner.control.continuous_workflow() {
            return Ok(());
        }
        let limits = crate::desktop::continuous_limits();
        let max_held = limits.max_batch_events;
        let mut state = lock(&self.inner.state);
        self.check_local()?;
        // Native batches are balanced by the SDK. Bound the controlled sink's
        // separately owned held keys before dispatching even one action.
        if state.held_keys.len() + sequence.actions.len() > max_held {
            let mut held: std::collections::BTreeSet<_> = state.held_keys.keys().collect();
            for action in &sequence.actions {
                match action {
                    Action::KeyDown { key } => {
                        held.insert(key);
                    }
                    Action::KeyUp { key } => {
                        held.remove(key);
                    }
                    _ => {}
                }
                if held.len() > max_held {
                    return Err(Fault::new("HandleLimit", "held input capacity exhausted"));
                }
            }
        }
        state.event_window.charge(
            self.inner.control.now(),
            sequence.event_count,
            limits.max_batch_events,
            limits.event_interval_ms,
        )?;
        state.admitted_actions = state.admitted_actions.saturating_add(sequence.event_count);
        Ok(())
    }

    fn archive_receipt(&self, state: &mut State, id: String, value: Value) {
        if self.inner.control.continuous_workflow() {
            self.record_sample(&mut state.archived_receipts, value);
        } else {
            state.receipts.insert(
                id,
                Receipt {
                    value,
                    permit: None,
                },
            );
        }
    }

    fn check_input_time(&self, wait_ms: u64) -> Result<(), Fault> {
        let control = &self.inner.control;
        control.check()?;
        let waits = Duration::from_millis(wait_ms);
        if control
            .deadline_opt()
            .is_some_and(|at| waits > at.saturating_duration_since(control.now()))
        {
            return Err(Fault::new(
                "Timeout",
                "scheduled input waits exceed remaining execution time",
            ));
        }
        self.scope
            .as_ref()
            .map_or(Ok(()), |scope| scope.fits(waits))
    }

    fn controlled_effect(
        &self,
        sequence: &Sequence,
        effect: impl FnOnce(&mut State) -> Result<(), Fault>,
    ) -> Result<(), Fault> {
        self.input_check(&sequence.observation)?;
        self.inner.control.with_input_admission(|| {
            let mut state = lock(&self.inner.state);
            self.check_local()?;
            if self.inner.plan.lane == "controlled" {
                self.observation(&state, &sequence.observation)?;
            }
            if self.inner.terminating.load(Ordering::Acquire)
                || state.phase != Phase::Workflow
                || !self.inner.control.admission.load(Ordering::Acquire)
            {
                return Err(Fault::new(
                    "AdmissionClosed",
                    "ordinary input admission is closed",
                ));
            }
            if !state.route {
                return Err(Fault::new("RouteRefused", "input route was revoked"));
            }
            if !state.focus {
                return Err(Fault::new("FocusRefused", "input focus was lost"));
            }
            effect(&mut state)
        })
    }

    #[cfg(test)]
    fn drag_checkpoint(&self, submitted_events: usize, waiting: bool) {
        let hook = lock(&self.inner.drag_hook).clone();
        if let Some(hook) = hook {
            hook(submitted_events, waiting);
        }
    }

    fn controlled_drag_event(
        &self,
        sequence: &Sequence,
        event: DragEvent,
        submitted_events: usize,
    ) -> Result<(), Fault> {
        if let DragEvent::Delay { ms } = event {
            self.controlled_effect(sequence, |state| {
                state.pointer_waiting = true;
                Ok(())
            })?;
            #[cfg(test)]
            self.drag_checkpoint(submitted_events, true);
            let deadline = Instant::now() + Duration::from_millis(ms);
            let waited = loop {
                if let Err(error) = self.input_check(&sequence.observation) {
                    break Err(error);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break Ok(());
                }
                thread::sleep(remaining.min(Duration::from_millis(1)));
            };
            lock(&self.inner.state).pointer_waiting = false;
            waited?;
        }
        self.controlled_effect(sequence, |state| {
            let mut effect = match event {
                DragEvent::Move { x, y } => {
                    state.pointer_position = Some((x, y));
                    json!({"kind":"pointer_move","x":x,"y":y,
                        "held_buttons":state.held_buttons.keys().collect::<Vec<_>>()})
                }
                DragEvent::Press { button } => {
                    if !state.held_buttons.is_empty() {
                        return Err(Fault::new(
                            "IncompleteCleanup",
                            "a pointer button is already owned",
                        ));
                    }
                    state.held_buttons.insert(button, sequence.order);
                    json!({"kind":"button_down","button":button})
                }
                DragEvent::Delay { ms } => json!({"kind":"delay","ms":ms}),
                DragEvent::Release { button } => {
                    if state.held_buttons.get(&button) != Some(&sequence.order) {
                        return Err(Fault::new(
                            "IncompleteCleanup",
                            "drag does not own its button",
                        ));
                    }
                    state.held_buttons.remove(&button);
                    if self.inner.plan.lane == "controlled"
                        && self.inner.plan.scenario != "postcondition-absent"
                    {
                        state.visible = "DONE".into();
                    }
                    json!({"kind":"button_up","button":button})
                }
            };
            if submitted_events == 0 {
                state.dispatches += 1;
            }
            effect["order"] = json!(sequence.order);
            self.record_sample(&mut state.effects, effect);
            Ok(())
        })
    }

    fn cleanup_buttons(&self, state: &mut State, owner: Option<u64>) {
        let released = true;
        #[cfg(test)]
        let released = released && !self.inner.refuse_button_cleanup.load(Ordering::Acquire);
        state.held_buttons.retain(|button, order| {
            if owner.is_some_and(|owner| owner != *order) {
                return true;
            }
            // A failed release remains owed; termination must not silently retry it.
            let name = match button {
                PointerButton::Left => "left",
                PointerButton::Right => "right",
                PointerButton::Middle => "middle",
            };
            if state.release_outcomes.iter().any(|outcome| {
                outcome["button"].as_str() == Some(name) && outcome["order"] == *order
            }) {
                return true;
            }
            state.release_outcomes.push(json!({
                "button":button,"order":order,"released":released,"sink":"controlled-non-native"
            }));
            !released
        });
    }

    fn dispatch_drag_batch(&self, sequence: &Sequence) -> Value {
        let mut submitted = 0usize;
        let mut submitted_events = 0usize;
        let mut failure = self
            .check_input_time(sequence.scheduled_wait_ms.unwrap_or(0))
            .err();
        if failure.is_none() && !lock(&self.inner.state).held_buttons.is_empty() {
            failure = Some(Fault::new(
                "IncompleteCleanup",
                "earlier pointer cleanup is unresolved",
            ));
        }
        'actions: for action in &sequence.actions {
            if failure.is_some() {
                break;
            }
            if let Action::Drag { button, path } = action {
                for event in drag_events(*button, path) {
                    if let Err(error) =
                        self.controlled_drag_event(sequence, event, submitted_events)
                    {
                        failure = Some(error);
                        break 'actions;
                    }
                    submitted_events += 1;
                    if matches!(event, DragEvent::Release { .. }) {
                        submitted += 1;
                    }
                    #[cfg(test)]
                    self.drag_checkpoint(submitted_events, false);
                    if let Some(error) = self.controlled_partial_fault() {
                        failure = Some(error);
                        break 'actions;
                    }
                }
            } else {
                let result = self.controlled_effect(sequence, |state| {
                    if submitted_events == 0 {
                        state.dispatches += 1;
                    }
                    match action {
                        Action::KeyDown { key } => {
                            state
                                .held_keys
                                .entry(key.clone())
                                .or_insert((sequence.order, self.scope_id()));
                        }
                        Action::KeyUp { key } => {
                            if state.held_keys.remove(key).is_some()
                                && self.inner.plan.lane == "controlled"
                                && self.inner.plan.scenario != "postcondition-absent"
                            {
                                state.visible = "DONE".into();
                            }
                        }
                        Action::Click { .. } => {
                            if self.inner.plan.lane == "controlled"
                                && self.inner.plan.scenario != "postcondition-absent"
                            {
                                state.visible = "DONE".into();
                            }
                        }
                        Action::Drag { .. } => unreachable!("drag handled above"),
                    }
                    let mut effect = json!(action);
                    effect["order"] = json!(sequence.order);
                    self.record_sample(&mut state.effects, effect);
                    Ok(())
                });
                if let Err(error) = result {
                    failure = Some(error);
                    break;
                }
                submitted += 1;
                submitted_events += action.event_count();
                if let Some(error) = self.controlled_partial_fault() {
                    failure = Some(error);
                    break;
                }
            }
        }
        let status = match &failure {
            None => "Submitted",
            Some(error) if error.category == "ControlledUncertain" => "Uncertain",
            Some(_) if submitted_events != 0 => "Partial",
            Some(error) if error.category == "Cancelled" => "Cancelled",
            Some(_) => "Refused",
        };
        if let Some(error) = &failure {
            // Receipt outcomes end the gesture, not the attempt; target loss still latches.
            self.retain_terminal_fault(error);
        }
        let mut state = lock(&self.inner.state);
        if failure.is_some() && submitted_events != 0 {
            // No ordinary input of any kind follows an interrupted gesture.
            state.input_closed = true;
        }
        self.cleanup_buttons(&mut state, Some(sequence.order));
        let mut cleanup: Vec<String> = state
            .held_keys
            .iter()
            .filter(|(_, (order, _))| *order == sequence.order)
            .map(|(key, _)| key.clone())
            .collect();
        let released = state
            .release_outcomes
            .iter()
            .filter(|outcome| outcome["order"] == sequence.order && outcome["released"] == true)
            .count();
        let owed = state
            .held_buttons
            .values()
            .filter(|owner| **owner == sequence.order)
            .count();
        if owed != 0 {
            cleanup.push("controlled_button_release_unconfirmed".into());
        }
        let mut receipt = self.receipt(
            sequence,
            status,
            submitted,
            failure.as_ref().map(|error| error.category.as_str()),
            cleanup,
        );
        receipt["submitted_events"] = json!(submitted_events);
        receipt["last_submitted_event"] = json!(submitted_events.checked_sub(1));
        receipt["cleanup"] = json!({
            "state":if owed != 0 {"Incomplete"} else if released != 0 {"Released"} else {"NotRequired"},
            "released":released,"owed":owed,"may_leave_state_held":owed != 0
        });
        if let Some(error) = failure {
            receipt["error"] = json!(error);
        }
        receipt
    }

    fn controlled_partial_fault(&self) -> Option<Fault> {
        match self.inner.plan.scenario.as_str() {
            "partial" => Some(Fault::new("ControlledPartial", "controlled partial input")),
            "uncertain" => Some(Fault::new(
                "ControlledUncertain",
                "controlled uncertain input",
            )),
            _ => None,
        }
    }

    pub(super) fn submit(&self, args: &Value) -> Result<Value, Fault> {
        let map = object(
            args,
            &["observation", "actions"],
            &["observation", "actions"],
        )?;
        let raw = map["actions"]
            .as_array()
            .ok_or_else(|| argument("actions must be an array"))?;
        let continuous = self.inner.control.continuous_workflow();
        let batch_limit = if continuous && self.inner.plan.lane != "native" {
            crate::desktop::continuous_limits().max_batch_events
        } else {
            self.inner.plan.limits.max_actions
        };
        if continuous && raw.len() > batch_limit {
            return Err(Fault::new(
                "ActionLimit",
                "expanded input batch exceeds its limit",
            ));
        }
        if raw.is_empty() || raw.len() > batch_limit {
            return Err(argument("action sequence must be nonempty and bounded"));
        }
        // Bound nested storage before cloning/deserializing caller-owned arrays.
        for action in raw {
            if action["kind"] == "drag" {
                let path = action["path"]
                    .as_array()
                    .ok_or_else(|| argument("drag path must be an array"))?;
                if !(2..=64).contains(&path.len()) {
                    return Err(argument("drag path must contain 2..=64 points"));
                }
            }
        }
        let actions: Vec<Action> = serde_json::from_value(map["actions"].clone())
            .map_err(|error| argument(error.to_string()))?;
        let extent = Some((
            map["observation"]["width"].as_u64().unwrap_or(0) as f64,
            map["observation"]["height"].as_u64().unwrap_or(0) as f64,
        ));
        for action in &actions {
            action.validate(extent)?;
        }
        let contains_drag = actions
            .iter()
            .any(|action| matches!(action, Action::Drag { .. }));
        let mut event_count = 0usize;
        let mut wait_ms = 0u64;
        for action in &actions {
            #[cfg(not(feature = "engine"))]
            let (expanded, wait) = (action.event_count(), action.scheduled_wait_ms());
            #[cfg(feature = "engine")]
            let (expanded, wait) = self.inner.engine.get().map_or_else(
                || (action.event_count(), action.scheduled_wait_ms()),
                |engine| {
                    (
                        engine.action_event_count(action),
                        engine.action_wait_ms(action),
                    )
                },
            );
            event_count = event_count
                .checked_add(expanded)
                .ok_or_else(|| Fault::new("ActionLimit", "expanded input event count overflow"))?;
            wait_ms = wait_ms
                .checked_add(wait)
                .ok_or_else(|| argument("scheduled input wait overflow"))?;
        }
        if contains_drag {
            if event_count > 256 {
                return Err(Fault::new(
                    "ActionLimit",
                    "drag batch exceeds 256 input events",
                ));
            }
            if wait_ms > self.inner.plan.limits.wait_ms {
                return Err(argument("scheduled input waits exceed wait_ms"));
            }
        }
        self.input_check(&map["observation"])?;
        #[cfg(feature = "engine")]
        if self.inner.plan.lane == "native" {
            let engine = self
                .inner
                .engine
                .get()
                .ok_or_else(|| Fault::new("Blocked", "native engine is unavailable"))?;
            engine.call_scoped("validate_input", args.clone(), self.scope.as_ref())?;
        }
        #[cfg(test)]
        {
            let hook = lock(&self.inner.before_input_enqueue).take();
            if let Some(hook) = hook {
                hook();
            }
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
        let action_limit = batch_limit;
        #[cfg(feature = "engine")]
        let action_limit = self.inner.engine.get().map_or(action_limit, |engine| {
            action_limit.min(engine.action_limit())
        });
        if event_count
            > if continuous {
                action_limit
            } else {
                action_limit.saturating_sub(state.admitted_actions)
            }
        {
            return Err(Fault::new(
                "ActionLimit",
                "attempt action budget is exhausted",
            ));
        }
        // Recheck control while holding admission state, without acquiring any work lock.
        if contains_drag {
            self.check_input_time(wait_ms)?;
        }
        self.inner.control.check()?;
        self.check_local()?;
        if !self.inner.control.admission.load(Ordering::Acquire)
            || self.inner.terminating.load(Ordering::Acquire)
            || state.input_closed
        {
            return Err(Fault::new(
                "AdmissionClosed",
                "admission closed before enqueue",
            ));
        }
        let id = self.next_id(&mut state, "sequence");
        state.sequence_count += 1;
        let order = state.sequence_count;
        if !continuous {
            state.admitted_actions += event_count;
        }
        self.record_sample(
            &mut state.accepted,
            json!({"id":id,"order":order,"actions":actions}),
        );
        state.queue.push_back(Sequence {
            id: id.clone(),
            scope: self.scope.clone(),
            order,
            observation: map["observation"].clone(),
            actions,
            event_count,
            scheduled_wait_ms: contains_drag.then_some(wait_ms),
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
        if sequence.scheduled_wait_ms.is_some() {
            receipt["submitted_events"] = json!(0);
            receipt["total_events"] = json!(sequence.event_count);
            receipt["last_submitted_event"] = Value::Null;
        }
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
            self.check_local()?;
            thread::sleep(Duration::from_millis(1));
        };
        loop {
            self.check_local()?;
            let sequence = {
                let mut state = lock(&self.inner.state);
                if state
                    .receipts
                    .get(id)
                    .is_some_and(|receipt| receipt.permit.is_none())
                {
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
                if !state.held_keys.is_empty() && !state.scopes.is_empty() {
                    let owner = sequence.scope.as_ref().map_or(0, super::WorkflowScope::id);
                    if sequence.actions.iter().any(|action| match action {
                        Action::KeyDown { key } | Action::KeyUp { key } => state
                            .held_keys
                            .get(key)
                            .is_some_and(|(_, held)| *held != owner),
                        _ => false,
                    }) {
                        let value = self.receipt(
                            &sequence,
                            "Refused",
                            0,
                            Some("ScopeMismatch"),
                            Vec::new(),
                        );
                        state.receipts.insert(
                            sequence.id,
                            Receipt {
                                value,
                                permit: Some(sequence.permit),
                            },
                        );
                        continue;
                    }
                }
                state.active = Some(sequence.id.clone());
                sequence
            };
            // Settling B may drain an earlier sequence A. Only A's captured
            // context authorizes its dispatch, never B's context or a new root.
            let dispatcher = Host {
                inner: self.inner.clone(),
                scope: sequence.scope.clone(),
            };
            let receipt = match dispatcher
                .input_check(&sequence.observation)
                .and_then(|()| {
                    if let Some(wait_ms) = sequence.scheduled_wait_ms {
                        dispatcher.check_input_time(wait_ms)?;
                    }
                    dispatcher.charge_dispatch(&sequence)
                }) {
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
                Ok(()) => dispatcher.dispatch_sequence(&sequence),
            };
            let mut state = lock(&self.inner.state);
            state.active = None;
            let uncertain = matches!(receipt["status"].as_str(), Some("Partial" | "Uncertain"));
            state.input_uncertain |= uncertain;
            if (sequence.scope.is_some() || self.inner.control.continuous_workflow())
                && (uncertain || receipt["cleanup"]["may_leave_state_held"] == true)
            {
                self.fail(
                    Fault::new(
                        "InputUncertain",
                        "input did not settle under a live context",
                    )
                    .with_context(json!({"receipt":receipt})),
                );
            } else if receipt["status"] == "Submitted"
                && dispatcher
                    .check_local()
                    .is_err_and(|error| super::local_fault(&error))
            {
                self.fail(super::scope::late_submission(&receipt));
            }
            let closed = sequence
                .scope
                .as_ref()
                .is_some_and(|scope| scope.id() != 0 && !state.scopes.contains_key(&scope.id()));
            if closed {
                self.archive_receipt(&mut state, sequence.id, receipt);
                continue;
            }
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
                .get()
                .ok_or_else(|| Fault::new("Blocked", "native engine is unavailable"))
                .and_then(|engine| {
                    engine.call_scoped(
                        "dispatch",
                        json!({
                            "observation":sequence.observation,"actions":sequence.actions
                        }),
                        self.scope.as_ref(),
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
                    if !super::local_fault(&error) && error.category != "InputRate" {
                        self.fail(error);
                    }
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
        if sequence.scheduled_wait_ms.is_some() {
            return self.dispatch_drag_batch(sequence);
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
            if let Err(error) = self.check_local() {
                status = if submitted > 0 { "Partial" } else { "Refused" };
                reason = Some(error.category);
                break;
            }
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
                    state
                        .held_keys
                        .entry(key.clone())
                        .or_insert((sequence.order, self.scope_id()));
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
                Action::Drag { .. } => unreachable!("drag batches use event-level dispatch"),
            }
            let mut effect = json!(action);
            effect["order"] = json!(sequence.order);
            self.record_sample(&mut state.effects, effect);
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
            .filter(|(_, (order, _))| *order == sequence.order)
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
        if let Some(receipt) = state.receipts.get_mut(id) {
            if receipt.permit.take().is_none() {
                return Err(Fault::new("InvalidHandle", "sequence handle was released"));
            }
            if self.inner.control.continuous_workflow() {
                let receipt = state.receipts.remove(id).expect("receipt present");
                self.record_sample(&mut state.archived_receipts, receipt.value);
            }
            return Ok(json!({"released":true}));
        }
        if let Some(index) = state.queue.iter().position(|sequence| sequence.id == id) {
            let sequence = state
                .queue
                .remove(index)
                .ok_or_else(|| Fault::new("InvalidHandle", "sequence was already removed"))?;
            let receipt = self.receipt(&sequence, "Cancelled", 0, Some("Released"), Vec::new());
            self.archive_receipt(&mut state, id.into(), receipt);
            return Ok(json!({"released":true}));
        }
        drop(state);
        // Native handle identities are opaque; route by ownership, not spelling.
        #[cfg(feature = "engine")]
        if let Some(engine) = self.inner.engine.get() {
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
        if self.script_startup() {
            self.inner.control.cancel();
        }
        let started = Instant::now();
        let cleanup_ms = self.inner.plan.limits.cleanup_ms;
        #[cfg(feature = "engine")]
        let cleanup_ms = self
            .inner
            .engine
            .get()
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
            for scope in state.scopes.values().chain(self.inner.root_scope.get()) {
                scope.end(Fault::new("AdmissionClosed", "attempt has finished"));
            }
            state.scopes.clear();
            state.scoped_handles.clear();
        }
        while self.startup_active() && started.elapsed() < Duration::from_millis(cleanup_ms) {
            self.reap_startup();
            thread::sleep(Duration::from_millis(1));
        }
        self.reap_startup();
        #[cfg(feature = "engine")]
        let engine_cleanup = self.inner.engine.get().map(crate::engine::Engine::finish);
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
            for (key, (order, _)) in held {
                state.release_outcomes.push(
                    json!({"key":key,"order":order,"released":true,"sink":"controlled-non-native"}),
                );
            }
            self.cleanup_buttons(&mut state, None);
        }
        let physical = self
            .inner
            .physical
            .load(Ordering::Acquire)
            .max(lock(&self.inner.workers).len())
            + usize::from(self.startup_active());
        let native_cleanup_unverified =
            self.inner.native_cleanup_unverified.load(Ordering::Acquire);
        let clean = physical == 0
            && !active
            && state.held_keys.is_empty()
            && state.held_buttons.is_empty()
            && !native_cleanup_unverified;
        #[cfg(feature = "engine")]
        let clean = clean
            && engine_cleanup
                .as_ref()
                .is_none_or(|engine| engine["clean"].as_bool() == Some(true));
        let mut result = json!({"clean":clean,"status":if clean {"CleanupFinished"} else {"IncompleteCleanup"},
            "cleanup_finished_us":if clean {Some(self.inner.control.elapsed_us())} else {None},
            "elapsed_us":started.elapsed().as_micros() as u64,
            "remaining":{"live_handles":state.handles.len(),"queued":state.queue.len(),"in_flight_native":physical,"active_sequences":usize::from(active),"held_keys":state.held_keys.len(),"held_buttons":state.held_buttons.len()},
            "release_outcomes":state.release_outcomes});
        if native_cleanup_unverified {
            result["native_cleanup"] = json!("unverified");
        }
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
        for receipt in state.receipts.values_mut() {
            receipt.permit = None;
        }
        state.cleanup = Some(result.clone());
        result
    }
}

#[cfg(test)]
mod tests;
