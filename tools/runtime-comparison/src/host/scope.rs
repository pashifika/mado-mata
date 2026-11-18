//! A scoped Host is an immutable view of the same attempt. Only the registry and
//! queue hold live authority; script tokens never change the current scope.
use super::{Control, Fault, Host, Phase, State, argument, lock, object, positive, string};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const SAMPLES: usize = 64;

#[derive(Clone)]
pub(crate) struct WorkflowScope(Arc<Scope>);

struct Scope {
    id: u64,
    parent: u64,
    control: Arc<Control>,
    deadline: Option<Instant>,
    ended: OnceLock<Fault>,
}

impl WorkflowScope {
    pub(crate) fn new(
        id: u64,
        parent: u64,
        control: Arc<Control>,
        deadline: Option<Instant>,
    ) -> Self {
        Self(Arc::new(Scope {
            id,
            parent,
            control,
            deadline,
            ended: OnceLock::new(),
        }))
    }

    pub(super) fn id(&self) -> u64 {
        self.0.id
    }
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.0.deadline
    }

    pub(crate) fn check(&self) -> Result<(), Fault> {
        if let Some(error) = self.0.ended.get() {
            return Err(error.clone());
        }
        // Cancellation already latched by the Run does not erase an earlier
        // child expiry. Conversely a later child check must not backdate it.
        if self
            .0
            .control
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let stopped = Duration::from_micros(
                self.0
                    .control
                    .stop_us
                    .load(std::sync::atomic::Ordering::Acquire),
            );
            if self
                .deadline()
                .is_none_or(|at| at.saturating_duration_since(self.0.control.started()) > stopped)
                && let Err(error) = self.0.control.check()
            {
                return Err(self.0.ended.get_or_init(|| error).clone());
            }
        }
        if self.deadline().is_some_and(|at| self.0.control.now() >= at) {
            let error = self
                .0
                .ended
                .get_or_init(|| Fault::new("ScopeDeadline", "scope deadline expired"));
            return Err(error.clone());
        }
        Ok(())
    }

    pub(crate) fn fits(&self, waits: Duration) -> Result<(), Fault> {
        self.check()?;
        if self
            .deadline()
            .is_some_and(|at| waits > at.saturating_duration_since(self.0.control.now()))
        {
            return Err(Fault::new(
                "ScopeBudget",
                "scheduled input waits exceed the context's remaining time",
            ));
        }
        Ok(())
    }

    pub(crate) fn end(&self, error: Fault) {
        if self.check().is_ok() {
            let _ = self.0.ended.set(error);
        }
    }
}

pub(crate) fn local_fault(fault: &Fault) -> bool {
    matches!(
        fault.category.as_str(),
        "ScopeDeadline" | "ScopeBudget" | "ScopeCancelled" | "ScopeClosed" | "ScopeMismatch"
    )
}

pub(super) fn late_submission(receipt: &Value) -> Fault {
    Fault::new(
        "InputSubmitted",
        "input was submitted before its context ended; do not retry",
    )
    .with_context(json!({"receipt":receipt}))
}

fn cause(error: &Fault) -> &str {
    if error.category == "ScopeCancelled" {
        &error.message
    } else {
        &error.category
    }
}

fn earlier(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        _ => a.or(b),
    }
}

impl Host {
    pub(super) fn scope_descriptor(&self, id: u64, deadline: Option<Instant>) -> Value {
        json!({"id":id,"deadline_ms":deadline.map(|at| at.saturating_duration_since(self.inner.control.started()).as_millis())})
    }

    fn scoped(&self, id: u64) -> Result<Self, Fault> {
        let scope = if id == 0 {
            self.inner
                .root_scope
                .get_or_init(|| WorkflowScope::new(0, 0, self.control(), None))
                .clone()
        } else {
            lock(&self.inner.state)
                .scopes
                .get(&id)
                .cloned()
                .ok_or_else(|| Fault::new("ScopeClosed", "context has been released"))?
        };
        Ok(Self {
            inner: Arc::clone(&self.inner),
            scope: Some(scope),
        })
    }

    pub(crate) fn context_rpc(
        &self,
        id: u64,
        op: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, Fault> {
        if op == "host" {
            return self.call(method, args);
        }
        let host = self.scoped(id)?;
        match op {
            "call"
                if matches!(
                    method,
                    "wait"
                        | "observe"
                        | "query"
                        | "query_wait"
                        | "recognize"
                        | "scan_ocr_zones"
                        | "submit"
                        | "settle"
                        | "postcondition"
                        | "release"
                ) =>
            {
                host.call(method, args)
            }
            "child" => host.child(&args),
            "status" => {
                let error = host
                    .scope
                    .as_ref()
                    .and_then(|scope| scope.check().err())
                    .or_else(|| host.check().err());
                Ok(json!(error.as_ref().map(cause)))
            }
            "close" | "cancel" if id != 0 => {
                let error = if op == "close" {
                    Fault::new("ScopeClosed", "context has been released")
                } else {
                    let map = object(&args, &["cause"], &["cause"])?;
                    let text = string(map, "cause")?;
                    if text.is_empty() || text.len() > 256 {
                        return Err(argument("context cancellation cause must be 1..=256 bytes"));
                    }
                    Fault::new("ScopeCancelled", text)
                };
                host.end_scope(id, error, op == "close")
            }
            _ => Err(argument("unsupported Workflow context operation")),
        }
    }

    fn child(&self, args: &Value) -> Result<Value, Fault> {
        self.check()?;
        let map = object(args, &["timeout_ms", "deadline_ms"], &[])?;
        let control = &self.inner.control;
        let requested = match (
            map.contains_key("timeout_ms"),
            map.contains_key("deadline_ms"),
        ) {
            (true, true) => return Err(argument("choose timeout_ms or deadline_ms, not both")),
            (false, false) => None,
            (relative, _) => {
                let (origin, key) = if relative {
                    (control.now(), "timeout_ms")
                } else {
                    (control.started(), "deadline_ms")
                };
                Some(
                    origin
                        .checked_add(Duration::from_millis(positive(map, key, MAX_SAFE_INTEGER)?))
                        .ok_or_else(|| argument("context deadline overflows"))?,
                )
            }
        };
        let mut state = lock(&self.inner.state);
        if state.phase != Phase::Workflow
            || !control.admission.load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Fault::new(
                "AdmissionClosed",
                "context requires an active Workflow",
            ));
        }
        self.check_local()?;
        if state.scopes.len() >= self.inner.plan.limits.handles || state.next_id > MAX_SAFE_INTEGER
        {
            return Err(Fault::new(
                "HandleLimit",
                "Workflow context capacity exhausted",
            ));
        }
        // The Run clock remains independently terminal; don't reclassify its expiry as local.
        let local = earlier(
            requested,
            self.scope.as_ref().and_then(WorkflowScope::deadline),
        )
        .filter(|at| control.deadline_opt().is_none_or(|outer| *at < outer));
        if local.is_some_and(|at| {
            at.saturating_duration_since(control.started()).as_millis()
                > u128::from(MAX_SAFE_INTEGER)
        }) {
            return Err(argument(
                "effective context deadline exceeds the safe integer range",
            ));
        }
        let id = state.next_id;
        state.next_id += 1;
        let parent = self.scope.as_ref().map_or(0, |scope| scope.0.id);
        state.scopes.insert(
            id,
            WorkflowScope::new(id, parent, Arc::clone(control), local),
        );
        Ok(self.scope_descriptor(id, earlier(local, control.deadline_opt())))
    }

    fn end_scope(&self, id: u64, error: Fault, close: bool) -> Result<Value, Fault> {
        let mut state = lock(&self.inner.state);
        let mut subtree = BTreeSet::new();
        let mut retired = Vec::new();
        // IDs increase from parent to child. Propagate before dropping any registry entry.
        for (key, scope) in state.scopes.range(id..) {
            if *key == id || subtree.contains(&scope.0.parent) {
                subtree.insert(*key);
                scope.end(error.clone());
                if close {
                    let first = scope.0.ended.get().expect("scope ended");
                    retired.push(json!({"id":key,"cause":cause(first),"category":first.category}));
                }
            }
        }
        if state
            .held_keys
            .values()
            .any(|(_, owner)| subtree.contains(owner))
        {
            self.fail(Fault::new(
                "IncompleteCleanup",
                "cancelled context owns held input",
            ));
        }
        if close {
            state.scopes.retain(|key, _| !subtree.contains(key));
            let ids: Vec<_> = state
                .scoped_handles
                .iter()
                .filter_map(|(id, owner)| subtree.contains(owner).then(|| id.clone()))
                .collect();
            drop(state);
            for id in ids {
                // A query can consume an aliased handle; active sequences retain
                // their physical permit until dispatch publishes its receipt.
                if let Err(error) = self.release(&json!({"id":id}))
                    && error.category != "InvalidHandle"
                {
                    return Err(error);
                }
                lock(&self.inner.state).scoped_handles.remove(&id);
            }
        }
        Ok(Value::Array(retired))
    }

    pub(super) fn check_local(&self) -> Result<(), Fault> {
        self.scope.as_ref().map_or(Ok(()), WorkflowScope::check)
    }

    // A returned handle is registered under the same lock that retires scopes.
    // Refused publication uses only the restricted release path.
    pub(super) fn call_owned(&self, method: &str, args: Value) -> Result<Value, Fault> {
        // Unscoped calls own root handles, not every handle in this Run.
        let owner = self
            .scope
            .as_ref()
            .map(WorkflowScope::id)
            .filter(|id| *id != 0);
        if self.scope.is_some() && method != "release" {
            self.check()?;
        }
        let id = match method {
            "settle" | "query_wait" | "release" => args["id"].as_str(),
            _ => None,
        };
        if id.is_some() || args.get("observation").is_some() || args.get("checkpoint").is_some() {
            let state = lock(&self.inner.state);
            for handle in [
                id,
                args["observation"]["id"].as_str(),
                args["checkpoint"]["id"].as_str(),
            ]
            .into_iter()
            .flatten()
            {
                if state.scoped_handles.get(handle).copied() != owner {
                    return Err(Fault::new(
                        "ScopeMismatch",
                        "handle does not belong to this context",
                    ));
                }
            }
        }
        if method == "release" {
            let value = self.release(&args)?;
            if let (Some(_), Some(id)) = (owner, id) {
                lock(&self.inner.state).scoped_handles.remove(id);
            }
            return Ok(value);
        }
        let value = self.call_inner(method, args)?;
        if matches!(method, "fixture" | "target_start" | "target_status")
            || (self.scope.is_none()
                && (method == "settle" || !self.inner.control.continuous_workflow()))
        {
            return Ok(value);
        }
        let ids = [
            matches!(
                method,
                "observe" | "recognize" | "query" | "query_wait" | "submit"
            )
            .then(|| value["id"].as_str())
            .flatten(),
            (method == "query_wait")
                .then(|| value["observation"]["id"].as_str())
                .flatten(),
        ];
        let published = self.check().and_then(|()| {
            if let Some(owner) = owner
                && ids.iter().any(Option::is_some)
            {
                let mut state = lock(&self.inner.state);
                self.inner.control.check()?;
                self.check_local()?;
                for id in ids.into_iter().flatten() {
                    state.scoped_handles.insert(id.into(), owner);
                }
            }
            Ok(())
        });
        if let Err(error) = published {
            if local_fault(&error) && method == "settle" && value["status"] == "Submitted" {
                let error = late_submission(&value);
                self.fail(error.clone());
                return Err(error);
            }
            for id in ids.into_iter().flatten() {
                if let Err(release) = self.release(&json!({"id": id}))
                    && release.category != "InvalidHandle"
                {
                    self.fail(release.clone());
                    return Err(release);
                }
            }
            return Err(error);
        }
        Ok(value)
    }

    pub(super) fn capture_slot<'a>(
        &'a self,
        mut state: MutexGuard<'a, State>,
        until: Option<Instant>,
    ) -> Result<MutexGuard<'a, State>, Fault> {
        if !self.inner.control.continuous_workflow() {
            return Ok(state);
        }
        let until = until.unwrap_or_else(|| {
            Instant::now() + Duration::from_millis(self.inner.plan.limits.wait_ms)
        });
        let spacing = Duration::from_millis(crate::desktop::continuous_limits().interval_ms);
        loop {
            self.check_local()?;
            self.inner.control.check()?;
            if Instant::now() >= until {
                return Err(Fault::new("Timeout", "capture spacing exceeds this wait"));
            }
            let now = self.inner.control.now();
            let delay = state.last_capture.map_or(Duration::ZERO, |last| {
                spacing.saturating_sub(now.saturating_duration_since(last))
            });
            if delay.is_zero() {
                state.last_capture = Some(now);
                return Ok(state);
            }
            drop(state);
            self.check()?;
            thread::sleep(delay.min(Duration::from_millis(1)));
            state = lock(&self.inner.state);
        }
    }

    pub(super) fn record_sample(&self, samples: &mut VecDeque<Value>, value: Value) {
        if self.inner.control.continuous_workflow() && samples.len() >= SAMPLES {
            samples.pop_front();
        }
        samples.push_back(value);
    }
}

#[cfg(test)]
mod tests;
