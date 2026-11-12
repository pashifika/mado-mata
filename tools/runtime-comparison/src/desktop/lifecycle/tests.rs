use super::*;
use stateright::{Checker, Model, Property};

// Two successful run IDs permit reuse and a predecessor's stale Stop. There is
// one owned worker, one queued accepted progress event, and success/failure
// completion. Repeated Stop, progress, and shutdown loop over finite state; no
// depth/time cutoff is used. Native progress has already passed State::progress's
// identity/attempt/status filtering, whose regressions remain in preflight_tests.
//
// Worker completion and controller refresh are separate actions. Refresh drains
// progress and then joins a finished worker, just like production; Start refreshes
// before admission, whereas Stop does not. This is a controller decision model,
// not a model of threads, channels, Control atomics, or native/child cleanup.
const RUNS: [&str; 2] = ["first", "second"];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Worker {
    finished: Option<bool>,
    cancelled: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Observation {
    None,
    Started,
    StartupFailed,
    ClosedRefusal,
    ActiveRefusal,
    StaleStop(&'static str),
    InactiveStop,
    StopRequested,
    RepeatedStop,
    Progress,
    LateProgress,
    Joined,
    Closing,
    ShutdownTimeout,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ControllerState {
    lifecycle: Lifecycle,
    run: Option<&'static str>,
    worker: Option<Worker>,
    queued: Option<Progress>,
    started: usize,
    joined: usize,
    close_requested: bool,
    ran: bool,
    recovered: bool,
    successful_terminal: bool,
    admission_contract: bool,
    stop_contract: bool,
    observation: Observation,
}

impl ControllerState {
    fn refresh(&mut self) {
        let Some(worker) = self.worker else { return };
        if let Some(progress) = self.queued.take() {
            self.lifecycle.progress(progress);
            self.ran |= self.lifecycle.status() == "running";
            self.recovered |= self.lifecycle.status() == "recovering";
            self.observation = if worker.cancelled {
                Observation::LateProgress
            } else {
                Observation::Progress
            };
        }
        if let Some(success) = worker.finished {
            // The real adapter takes and joins Active here. Merely finishing
            // above did not release ownership or publish a terminal result.
            self.worker = None;
            self.joined += 1;
            self.successful_terminal = success && !worker.cancelled;
            self.lifecycle.settled();
            self.observation = Observation::Joined;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Start { spawn_succeeds: bool },
    NativeProgress(NativePhase),
    ChildStarted,
    Stop(&'static str),
    Finish { success: bool },
    Poll,
    Shutdown,
    ShutdownDeadline,
}

struct ControllerModel;

impl Model for ControllerModel {
    type State = ControllerState;
    type Action = Action;

    fn init_states(&self) -> Vec<Self::State> {
        vec![ControllerState {
            lifecycle: Lifecycle::new(),
            run: None,
            worker: None,
            queued: None,
            started: 0,
            joined: 0,
            close_requested: false,
            ran: false,
            recovered: false,
            successful_terminal: false,
            admission_contract: true,
            stop_contract: true,
            observation: Observation::None,
        }]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        if state.started < RUNS.len() {
            actions.push(Action::Start {
                spawn_succeeds: true,
            });
            actions.push(Action::Start {
                spawn_succeeds: false,
            });
        }
        for run in [RUNS[0], RUNS[1], "unrelated"] {
            actions.push(Action::Stop(run));
        }
        actions.push(Action::Shutdown);
        if let Some(worker) = state.worker {
            actions.push(Action::Poll);
            if worker.finished.is_none() {
                actions.push(Action::Finish { success: true });
                actions.push(Action::Finish { success: false });
                if state.close_requested {
                    actions.push(Action::ShutdownDeadline);
                }
                if state.queued.is_none() {
                    actions.push(Action::ChildStarted);
                    for phase in [
                        NativePhase::Preflight,
                        NativePhase::TargetDiscovery,
                        NativePhase::LaunchSubmission,
                        NativePhase::WaitingForProcess,
                        NativePhase::WaitingForWindow,
                        NativePhase::NativeInitialization,
                        NativePhase::Readiness,
                        NativePhase::HostAttachment,
                        NativePhase::Instantiation,
                        NativePhase::Function,
                        NativePhase::Workflow,
                        NativePhase::Settling,
                        NativePhase::Recovering,
                    ] {
                        actions.push(Action::NativeProgress(phase));
                    }
                }
            }
        }
    }

    fn next_state(&self, previous: &Self::State, action: Self::Action) -> Option<Self::State> {
        let mut state = previous.clone();
        state.observation = Observation::None;
        match action {
            Action::Start { spawn_succeeds } => {
                state.refresh();
                match state.lifecycle.admission(state.worker.is_some()) {
                    Ok(()) => {
                        state.admission_contract &= !state.close_requested
                            && state.worker.is_none()
                            && state.started == state.joined;
                        if spawn_succeeds {
                            state.run = Some(RUNS[state.started]);
                            state.started += 1;
                            state.worker = Some(Worker {
                                finished: None,
                                cancelled: false,
                            });
                            state.lifecycle.started();
                            state.ran = false;
                            state.recovered = false;
                            state.successful_terminal = false;
                            state.observation = Observation::Started;
                        } else {
                            state.observation = Observation::StartupFailed;
                        }
                    }
                    Err(AdmissionRefusal::Closed) => {
                        state.admission_contract &= state.close_requested;
                        state.observation = Observation::ClosedRefusal;
                    }
                    Err(AdmissionRefusal::Active) => {
                        state.admission_contract &=
                            !state.close_requested && state.worker.is_some();
                        state.observation = Observation::ActiveRefusal;
                    }
                }
            }
            Action::NativeProgress(phase) => state.queued = Some(Progress::native(phase)),
            Action::ChildStarted => state.queued = Some(Progress::ChildStarted),
            Action::Stop(run) => {
                let decision = state.lifecycle.stop(state.run, run, state.worker.is_some());
                match decision {
                    StopDecision::Stale => state.observation = Observation::StaleStop(run),
                    StopDecision::Inactive => state.observation = Observation::InactiveStop,
                    StopDecision::Cancel { notify } => {
                        if let Some(worker) = &mut state.worker {
                            worker.cancelled = true;
                        }
                        state.observation = if notify {
                            Observation::StopRequested
                        } else {
                            Observation::RepeatedStop
                        };
                    }
                }
                state.stop_contract &= if previous.run != Some(run) {
                    decision == StopDecision::Stale
                        && state.lifecycle == previous.lifecycle
                        && state.worker == previous.worker
                } else if let Some(worker) = previous.worker {
                    decision
                        == (StopDecision::Cancel {
                            notify: previous.lifecycle.status() != "stopping",
                        })
                        && state.worker
                            == Some(Worker {
                                cancelled: true,
                                ..worker
                            })
                        && state.lifecycle.status() == "stopping"
                } else {
                    decision == StopDecision::Inactive && state.lifecycle == previous.lifecycle
                };
            }
            Action::Finish { success } => {
                state.worker.as_mut().unwrap().finished = Some(success);
            }
            Action::Poll => state.refresh(),
            Action::Shutdown => {
                state.close_requested = true;
                state.lifecycle.close(state.worker.is_some());
                if let Some(worker) = &mut state.worker {
                    worker.cancelled = true;
                }
                state.observation = Observation::Closing;
            }
            Action::ShutdownDeadline => state.observation = Observation::ShutdownTimeout,
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always(
                "owned workers exclude successors, including after Stop",
                |_, state| {
                    state.worker.is_none()
                        || state.lifecycle.admission(true)
                            == Err(if state.close_requested {
                                AdmissionRefusal::Closed
                            } else {
                                AdmissionRefusal::Active
                            })
                },
            ),
            Property::<Self>::always(
                "terminal publication requires joined settlement",
                |_, state| {
                    state.lifecycle.status() != "terminal"
                        || (state.worker.is_none()
                            && state.joined == state.started
                            && state.joined > 0)
                },
            ),
            Property::<Self>::always(
                "late progress cannot reopen a stopping owner",
                |_, state| {
                    !state.worker.is_some_and(|worker| worker.cancelled)
                        || state.lifecycle.status() == "stopping"
                },
            ),
            Property::<Self>::always("closing is permanent and refuses Start", |_, state| {
                !state.close_requested
                    || (state.lifecycle.is_closed()
                        && state.lifecycle.admission(state.worker.is_some())
                            == Err(AdmissionRefusal::Closed))
            }),
            Property::<Self>::always(
                "Start preserves the reservation and refusal contract",
                |_, state| state.admission_contract,
            ),
            Property::<Self>::always(
                "Stop preserves identity, ownership and repeated-stop notification",
                |_, state| state.stop_contract,
            ),
            Property::<Self>::sometimes(
                "successful running reaches joined terminal",
                |_, state| {
                    state.ran && state.successful_terminal && state.lifecycle.status() == "terminal"
                },
            ),
            Property::<Self>::sometimes("recovery can return to running", |_, state| {
                state.recovered && state.lifecycle.status() == "running"
            }),
            Property::<Self>::sometimes(
                "joined settlement permits a fresh successor",
                |_, state| {
                    state.started == 2
                        && state.joined == 1
                        && state.observation == Observation::Started
                },
            ),
            Property::<Self>::sometimes(
                "stale predecessor Stop leaves the successor uncancelled",
                |_, state| {
                    state.started == 2
                        && state.observation == Observation::StaleStop(RUNS[0])
                        && state.worker.is_some_and(|worker| !worker.cancelled)
                },
            ),
            Property::<Self>::sometimes(
                "finished worker remains owned before refresh",
                |_, state| {
                    state.worker.is_some_and(|worker| worker.finished.is_some())
                        && state.lifecycle.status() != "terminal"
                },
            ),
            Property::<Self>::sometimes(
                "Stop can repeat while the worker remains owned",
                |_, state| state.observation == Observation::RepeatedStop && state.worker.is_some(),
            ),
            Property::<Self>::sometimes("a stopped owner rejects a successor", |_, state| {
                state.observation == Observation::ActiveRefusal
                    && state.worker.is_some_and(|worker| worker.cancelled)
            }),
            Property::<Self>::sometimes("queued progress can arrive after Stop", |_, state| {
                state.observation == Observation::LateProgress
                    && state.lifecycle.status() == "stopping"
            }),
            Property::<Self>::sometimes("shutdown timeout retains the owner", |_, state| {
                state.observation == Observation::ShutdownTimeout && state.worker.is_some()
            }),
            Property::<Self>::sometimes(
                "failed spawn leaves the idle controller reusable",
                |_, state| {
                    state.observation == Observation::StartupFailed
                        && state.lifecycle.status() == "idle"
                        && state.lifecycle.admission(state.worker.is_some()).is_ok()
                },
            ),
        ]
    }
}

#[test]
fn stateright_controller_lifecycle() {
    let checker = ControllerModel.checker().spawn_bfs().join();
    println!(
        "controller lifecycle: {} states, {} unique, depth {}",
        checker.state_count(),
        checker.unique_state_count(),
        checker.max_depth(),
    );
    checker.assert_properties();
}
