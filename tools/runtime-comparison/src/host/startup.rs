use super::{Host, Phase, lock, object};
use crate::desktop::{LaunchDisposition, NativePhase, NativeProgress, NativeTargetStatus};
use crate::model::Fault;
use serde_json::{Value, json};
#[cfg(feature = "engine")]
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};

pub(super) struct Startup {
    pub(super) progress: NativeProgress,
    requested: bool,
    awaiting: bool,
    target_bound: bool,
    configuration: Option<Value>,
    worker: Option<JoinHandle<()>>,
}

impl Startup {
    pub(super) fn new() -> Self {
        Self {
            progress: NativeProgress {
                status: NativeTargetStatus::NotRequested,
                phase: NativePhase::Readiness,
                launch: LaunchDisposition::NotRequested,
            },
            requested: false,
            awaiting: false,
            target_bound: false,
            configuration: None,
            worker: None,
        }
    }
}

impl Host {
    pub(super) fn capture_ready(&self) -> bool {
        lock(&self.inner.startup).progress.status == NativeTargetStatus::CaptureReady
    }

    pub(super) fn native_phase(&self, phase: NativePhase) {
        let progress = {
            let mut startup = lock(&self.inner.startup);
            startup.progress.phase = phase;
            startup.progress
        };
        crate::runner::emit_native_status(&self.inner.control, progress);
    }

    pub(super) fn reap_startup(&self) {
        let worker = {
            let mut startup = lock(&self.inner.startup);
            if startup.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                startup.worker.take()
            } else {
                None
            }
        };
        if let Some(worker) = worker {
            if worker.join().is_err() {
                self.fail(
                    Fault::new("Initialization", "native initialization worker panicked")
                        .with_context(json!({"native_cleanup":"unverified"})),
                );
            }
        }
    }

    pub(super) fn startup_active(&self) -> bool {
        lock(&self.inner.startup).worker.is_some()
    }

    pub(super) fn target_call(&self, method: &str, args: &Value) -> Result<Value, Fault> {
        object(args, &[], &[])?;
        if !self.script_startup()
            || self.inner.startup_link.get().is_none()
            || lock(&self.inner.state).phase != Phase::Readiness
        {
            return Err(Fault::new(
                "Authority",
                "target startup is available only in Native Readiness",
            ));
        }
        self.check()?;
        self.reap_startup();
        let mut startup = lock(&self.inner.startup);
        if method == "target_start" {
            if startup.requested {
                return Err(Fault::new(
                    "NativeStartRefused",
                    "target startup was already requested",
                ));
            }
            self.inner.control.check()?;
            startup.requested = true;
            startup.progress.status = NativeTargetStatus::Pending;
            startup.progress.phase = NativePhase::TargetDiscovery;
            crate::runner::emit_native_status(&self.inner.control, startup.progress);
            // Even an immediately available target must return pending on Start.
            let pending = startup.progress;
            drop(startup);
            self.schedule_target_probe()?;
            return Ok(json!(pending));
        }
        if !startup.requested || startup.progress.status == NativeTargetStatus::CaptureReady {
            return Ok(json!(startup.progress));
        }
        if startup.awaiting {
            if let Some(reply) = self.inner.startup_link.get().and_then(|link| link.take()) {
                startup.awaiting = false;
                startup.progress = reply.progress;
                crate::runner::emit_native_status(&self.inner.control, startup.progress);
                if let Some(fault) = reply.fault {
                    drop(startup);
                    self.fail(fault.clone());
                    return Err(fault);
                }
                self.inner.control.check()?;
                startup.target_bound = reply.configuration.is_some();
                startup.configuration = reply.configuration;
            }
        }
        let progress = startup.progress;
        let schedule = !startup.awaiting && startup.worker.is_none();
        drop(startup);
        self.check()?;
        if schedule {
            self.schedule_target_probe()?;
        }
        Ok(json!(progress))
    }

    fn schedule_target_probe(&self) -> Result<(), Fault> {
        self.check()?;
        let mut startup = lock(&self.inner.startup);
        if startup.awaiting
            || startup.worker.is_some()
            || startup.progress.status == NativeTargetStatus::CaptureReady
        {
            return Ok(());
        }
        if !startup.target_bound {
            let link =
                self.inner.startup_link.get().ok_or_else(|| {
                    Fault::new("Authority", "Desktop startup authority is missing")
                })?;
            link.request()?;
            startup.awaiting = true;
            return Ok(());
        }
        let configuration = startup.configuration.take();
        if startup.progress.phase != NativePhase::WaitingForWindow {
            startup.progress.phase = NativePhase::NativeInitialization;
            crate::runner::emit_native_status(&self.inner.control, startup.progress);
        }
        let host = self.clone();
        startup.worker = Some(
            thread::Builder::new()
                .name("native-capture-probe".into())
                .spawn(move || {
                    let result = host.initialize_target(configuration);
                    if let Err(fault) = result {
                        host.fail(fault);
                    }
                })
                .map_err(|error| Fault::new("Initialization", error.to_string()))?,
        );
        Ok(())
    }

    #[cfg(not(feature = "engine"))]
    fn initialize_target(&self, _configuration: Option<Value>) -> Result<(), Fault> {
        self.check()?;
        Err(Fault::new(
            "Blocked",
            "Native capture requires the engine feature",
        ))
    }

    #[cfg(feature = "engine")]
    fn initialize_target(&self, configuration: Option<Value>) -> Result<(), Fault> {
        self.check()?;
        if self.inner.engine.get().is_none() {
            let mut plan = self.inner.plan.clone();
            plan.native_config = Some(configuration.ok_or_else(|| {
                Fault::new(
                    "NativeRefused",
                    "host-owned Native target preparation is missing",
                )
            })?);
            let engine = crate::engine::Engine::probe_native(
                &plan,
                &self.inner.assets,
                self.control(),
                &self.inner.lifetime,
                self.inner.attempt,
                std::sync::Arc::clone(&self.inner.handle_budget),
                &self.inner.native_initialization_started,
            )?;
            // Even a late owner is installed for the cleanup path, never detached.
            self.inner
                .engine
                .set(engine)
                .map_err(|_| Fault::new("ReadinessContract", "Native engine already bound"))?;
        }
        self.check()?;
        if !self
            .inner
            .engine
            .get()
            .expect("initialized engine")
            .prepare_capture()?
        {
            self.inner.control.check()?;
            self.native_phase(NativePhase::WaitingForWindow);
            return Ok(());
        }
        self.inner.control.check()?;
        if self.inner.terminating.load(Ordering::Acquire) {
            return Err(Fault::new(
                "AdmissionClosed",
                "Native initialization completed after entry settlement",
            ));
        }
        let deadline = self.inner.control.native_transition(1, None)?;
        crate::runner::emit_native_transition(1, deadline)?;
        let mut startup = lock(&self.inner.startup);
        self.inner.control.check()?;
        startup.progress.status = NativeTargetStatus::CaptureReady;
        startup.progress.phase = NativePhase::Readiness;
        crate::runner::emit_native_status(&self.inner.control, startup.progress);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
