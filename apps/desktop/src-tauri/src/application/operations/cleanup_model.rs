use super::{NativeCleanup, TerminalOutcome};
use serde_json::{Value, json};
use stateright::{Checker, Model, Property};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Lane {
    Native,
    NonNative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Source {
    Result,
    Fault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Evidence {
    Clean,
    CleanWithoutForced,
    Unclean,
    Absent,
    Forced,
}

impl Evidence {
    fn view(self, source: Source) -> Value {
        let facts = match self {
            Self::Clean => json!({"cleanup":{"clean":true},"forced":false}),
            Self::CleanWithoutForced => json!({"cleanup":{"clean":true,"child_started":false}}),
            Self::Unclean => json!({"cleanup":{"clean":false},"forced":false}),
            Self::Absent => json!({"child_started":true}),
            // Forced containment must refuse reuse even if cleanup claims to be clean.
            Self::Forced => json!({"cleanup":{"clean":true},"forced":true}),
        };
        match source {
            Source::Result => json!({
                "state":"terminal","operation":"run","result":facts,
                // Result evidence owns cleanup, including its absence; a fault cannot
                // supply a missing attestation or override the result's forced fact.
                "error":{"category":"Script","context":{
                    "cleanup":{"clean":true},"forced":false}}
            }),
            Source::Fault => json!({
                "state":"terminal","operation":"run","result":null,
                "error":{"category":"ChildStartup","context":facts}
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Admit(Lane),
    Settle(Source, Evidence),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct State {
    cleanup: NativeCleanup,
    owner: Option<Lane>,
    // History is classified from semantic events, never from the production latch
    // or TerminalOutcome's decoder/predicate.
    ever_unsafe_native: bool,
    settlement_disagreed: bool,
    clean_native_settlements: u8,
    native_refusal_seen: bool,
    non_native_settled_after_refusal: bool,
}

struct CleanupModel;

impl Model for CleanupModel {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<Self::State> {
        vec![State {
            cleanup: NativeCleanup::Ready,
            owner: None,
            ever_unsafe_native: false,
            settlement_disagreed: false,
            clean_native_settlements: 0,
            native_refusal_seen: false,
            non_native_settled_after_refusal: false,
        }]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        if state.owner.is_none() {
            actions.extend([Action::Admit(Lane::Native), Action::Admit(Lane::NonNative)]);
        } else {
            for source in [Source::Result, Source::Fault] {
                for evidence in [
                    Evidence::Clean,
                    Evidence::CleanWithoutForced,
                    Evidence::Unclean,
                    Evidence::Absent,
                    Evidence::Forced,
                ] {
                    actions.push(Action::Settle(source, evidence));
                }
            }
        }
    }

    fn next_state(&self, last: &Self::State, action: Self::Action) -> Option<Self::State> {
        let mut state = last.clone();
        match action {
            Action::Admit(lane) if state.owner.is_none() => {
                // Other workspace/authoring guards are assumed ready. Production
                // consults this cleanup gate only for Native; non-native work bypasses it.
                if lane == Lane::NonNative || state.cleanup.ready().is_ok() {
                    state.owner = Some(lane);
                } else {
                    state.native_refusal_seen = true;
                }
            }
            Action::Admit(_) => return None,
            Action::Settle(source, evidence) => {
                let native = state.owner.take()? == Lane::Native;
                let view = evidence.view(source);
                let requires_reconciliation = state
                    .cleanup
                    .settle(native, &TerminalOutcome::from_view(&view));
                let clean = matches!(evidence, Evidence::Clean | Evidence::CleanWithoutForced);
                let unsafe_native = native && !clean;
                state.settlement_disagreed |= requires_reconciliation != unsafe_native;
                state.ever_unsafe_native |= unsafe_native;
                if native && clean {
                    // Two completed clean Native runs witness reuse; later runs are
                    // equivalent for these properties, not excluded by a depth limit.
                    state.clean_native_settlements = (state.clean_native_settlements + 1).min(2);
                }
                if !native && state.ever_unsafe_native && state.native_refusal_seen {
                    state.non_native_settled_after_refusal = true;
                }
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always(
                "native admission matches unsafe settlement history",
                |_, state| match state.cleanup.ready() {
                    Ok(()) => !state.ever_unsafe_native,
                    Err(fault) => {
                        state.ever_unsafe_native && fault.category == "NativeCleanupRequired"
                    }
                },
            ),
            Property::<Self>::always(
                "unsafe native settlement prevents every later native owner",
                |_, state| !state.ever_unsafe_native || state.owner != Some(Lane::Native),
            ),
            Property::<Self>::always(
                "trial reconciliation describes this settlement, not the latch",
                |_, state| !state.settlement_disagreed,
            ),
            Property::<Self>::sometimes(
                "clean native settlement permits native reuse",
                |_, state| {
                    state.clean_native_settlements == 2
                        && !state.ever_unsafe_native
                        && state.cleanup.ready().is_ok()
                },
            ),
            Property::<Self>::sometimes(
                "non-native work settles while native remains refused",
                |_, state| {
                    state.non_native_settled_after_refusal
                        && state.owner.is_none()
                        && state.cleanup.ready().is_err()
                },
            ),
        ]
    }
}

#[test]
fn stateright_native_cleanup_admission_follows_terminal_history() {
    // One serialized owner, two lanes, ten terminal evidence events, a two-valued
    // latch, boolean history, and a counter saturated at two make this graph finite.
    // Admission and collection already share the workspace lock. This abstraction
    // checks their decisions, not lock scheduling, worker synchronization, or OS cleanup.
    let checker = CleanupModel.checker().spawn_bfs().join();
    checker.assert_properties();
    eprintln!(
        "Native cleanup model: {} unique states, maximum depth {}",
        checker.unique_state_count(),
        checker.max_depth()
    );
}
