//! Pure correspondence and revalidation policy; macOS collects the OS evidence.
use mado_runtime_comparison::model::Fault;
use serde_json::json;

pub(super) const MAX_CANDIDATES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Evidence {
    ExactPath,
    SignedApplication,
    UnsignedPath,
}

#[cfg(target_os = "macos")]
impl Evidence {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::ExactPath => "exact_path",
            Self::SignedApplication => "signed_application",
            Self::UnsignedPath => "unsigned_path",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Candidate {
    Verified(Evidence),
    Different,
    Unverifiable,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SignedIdentity {
    pub identifier: String,
    pub team: Option<String>,
    pub unique: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SigningIdentity {
    Unsigned,
    Signed(SignedIdentity),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Lifetime {
    pub pid: i32,
    pub seconds: u64,
    pub microseconds: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CandidateSnapshot {
    pub lifetime: Lifetime,
    pub executable: String,
    pub architecture: i32,
    pub bundle_id: String,
}

pub(super) fn unavailable(stage: &str) -> Fault {
    Fault::new(
        "TargetObservationEvidence",
        "application correspondence could not be established",
    )
    .with_context(json!({"stage": stage}))
}

pub(super) fn invalidation(fault: &Fault) -> bool {
    matches!(
        fault.category.as_str(),
        "TargetObservationCancelled" | "TargetObservationTimeout"
    )
}

pub(super) fn candidate_correspondence(
    selected: &SigningIdentity,
    running: &SigningIdentity,
    selected_executable: &str,
    satisfies_requirement: bool,
    before: &CandidateSnapshot,
    after: &CandidateSnapshot,
) -> Result<Candidate, Fault> {
    if before != after {
        return Err(unavailable("process_changed"));
    }
    Ok(if satisfies_requirement {
        correspondence(selected, running, before.executable == selected_executable)
    } else {
        Candidate::Different
    })
}

pub(super) fn correspondence(
    selected: &SigningIdentity,
    running: &SigningIdentity,
    exact_path: bool,
) -> Candidate {
    match (selected, running) {
        (SigningIdentity::Unsigned, SigningIdentity::Unsigned) if exact_path => {
            Candidate::Verified(Evidence::UnsignedPath)
        }
        (SigningIdentity::Signed(selected), SigningIdentity::Signed(running)) => {
            if selected.identifier != running.identifier || selected.team != running.team {
                return Candidate::Different;
            }
            if selected.unique != running.unique {
                return Candidate::Unverifiable;
            }
            if exact_path {
                Candidate::Verified(Evidence::ExactPath)
            } else if selected.team.as_ref().is_some_and(|team| !team.is_empty()) {
                Candidate::Verified(Evidence::SignedApplication)
            } else {
                Candidate::Unverifiable
            }
        }
        _ => Candidate::Unverifiable,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Summary {
    Absent,
    Unique(Evidence),
    Ambiguous,
    Unverifiable,
}

#[cfg(target_os = "macos")]
impl Summary {
    pub(super) fn observation(self) -> (&'static str, Option<Evidence>) {
        match self {
            Self::Absent => ("not_running", None),
            Self::Unique(evidence) => ("matched", Some(evidence)),
            Self::Ambiguous => ("ambiguous", None),
            Self::Unverifiable => ("unverifiable", None),
        }
    }
}

pub(super) fn summarize(candidates: &[Candidate]) -> Summary {
    if candidates.len() > MAX_CANDIDATES {
        return Summary::Unverifiable;
    }
    if candidates.is_empty() {
        return Summary::Absent;
    }
    let mut evidence = None;
    let mut matches = 0;
    let mut uncertain = false;
    for candidate in candidates {
        match candidate {
            Candidate::Verified(kind) => {
                matches += 1;
                evidence = Some(*kind);
            }
            Candidate::Unverifiable => uncertain = true,
            Candidate::Different => {}
        }
    }
    if matches > 1 {
        Summary::Ambiguous
    } else if !uncertain && matches == 1 {
        evidence.map_or(Summary::Unverifiable, Summary::Unique)
    } else {
        Summary::Unverifiable
    }
}

/// Rechecks the entire cohort, including candidates classified as different.
pub(super) fn summarize_revalidated(
    candidates: &[Candidate],
    discovered_pids: &mut [i32],
    current_pids: &mut [i32],
    lifetimes: impl IntoIterator<Item = (Lifetime, Result<Lifetime, Fault>)>,
) -> Result<Summary, Fault> {
    current_pids.sort_unstable();
    discovered_pids.sort_unstable();
    if current_pids != discovered_pids || current_pids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(unavailable("candidates_changed"));
    }
    for (before, after) in lifetimes {
        match after {
            Ok(after) if after == before => {}
            Err(fault) if invalidation(&fault) => return Err(fault),
            _ => return Err(unavailable("process_changed")),
        }
    }
    Ok(summarize(candidates))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(pid: i32) -> CandidateSnapshot {
        CandidateSnapshot {
            lifetime: Lifetime {
                pid,
                seconds: 1_000,
                microseconds: 123_456,
            },
            executable: "/selected/Contents/MacOS/Executable".into(),
            architecture: 16_777_228,
            bundle_id: "dev.example.application".into(),
        }
    }

    #[test]
    fn candidate_snapshot_changes_refuse_previously_matching_code() {
        let before = snapshot(7);
        let identity = signed(Some("publisher"), 1);
        assert_eq!(
            candidate_correspondence(
                &identity,
                &identity,
                &before.executable,
                true,
                &before,
                &snapshot(7),
            )
            .unwrap(),
            Candidate::Verified(Evidence::ExactPath)
        );
        for change in [
            "pid",
            "seconds",
            "microseconds",
            "executable",
            "architecture",
            "bundle_id",
        ] {
            let mut after = snapshot(7);
            match change {
                "pid" => after.lifetime.pid += 1,
                "seconds" => after.lifetime.seconds += 1,
                "microseconds" => after.lifetime.microseconds += 1,
                "executable" => after.executable = "/replacement/Executable".into(),
                "architecture" => after.architecture += 1,
                "bundle_id" => after.bundle_id = "dev.example.replacement".into(),
                _ => unreachable!(),
            }
            let fault = candidate_correspondence(
                &identity,
                &identity,
                &before.executable,
                true,
                &before,
                &after,
            )
            .unwrap_err();
            assert_eq!(fault.category, "TargetObservationEvidence", "{change}");
            assert_eq!(fault.context["stage"], "process_changed", "{change}");
        }
    }

    #[test]
    fn lifetime_rechecks_refuse_exit_and_pid_reuse_even_for_excluded_candidates() {
        let candidates = [
            Candidate::Verified(Evidence::ExactPath),
            Candidate::Different,
        ];
        let before = [snapshot(7).lifetime, snapshot(11).lifetime];
        assert_eq!(
            summarize_revalidated(
                &candidates,
                &mut [7, 11],
                &mut [11, 7],
                before.map(|lifetime| (lifetime, Ok(lifetime))),
            )
            .unwrap(),
            Summary::Unique(Evidence::ExactPath)
        );
        for changed in 0..before.len() {
            for after in [
                None,
                Some(Lifetime {
                    seconds: before[changed].seconds + 1,
                    ..before[changed]
                }),
                Some(Lifetime {
                    microseconds: before[changed].microseconds + 1,
                    ..before[changed]
                }),
            ] {
                let rechecks = before.iter().enumerate().map(|(index, lifetime)| {
                    let current = if index == changed {
                        after
                    } else {
                        Some(*lifetime)
                    };
                    (
                        *lifetime,
                        current.ok_or_else(|| unavailable("process_lifetime")),
                    )
                });
                let fault =
                    summarize_revalidated(&candidates, &mut [7, 11], &mut [7, 11], rechecks)
                        .unwrap_err();
                assert_eq!(fault.category, "TargetObservationEvidence");
                assert_eq!(
                    fault.context["stage"], "process_changed",
                    "{changed}: {after:?}"
                );
            }
        }
    }

    #[test]
    fn candidate_cohort_changes_cannot_publish_the_previous_unique_match() {
        let candidates = [
            Candidate::Verified(Evidence::ExactPath),
            Candidate::Different,
        ];
        for (mut discovered, mut current) in [
            (vec![7, 11], vec![7, 13]),
            (vec![7, 11], vec![7]),
            (vec![7, 11], vec![7, 11, 13]),
            (vec![7, 11], vec![7, 7]),
            (vec![7, 7], vec![7, 7]),
        ] {
            let fault = summarize_revalidated(
                &candidates,
                &mut discovered,
                &mut current,
                [snapshot(7).lifetime, snapshot(11).lifetime]
                    .map(|lifetime| (lifetime, Ok(lifetime))),
            )
            .unwrap_err();
            assert_eq!(fault.category, "TargetObservationEvidence");
            assert_eq!(fault.context["stage"], "candidates_changed");
        }
    }

    #[test]
    fn final_lifetime_invalidation_is_not_downgraded_to_missing_evidence() {
        for category in ["TargetObservationCancelled", "TargetObservationTimeout"] {
            let before = snapshot(7).lifetime;
            let fault = summarize_revalidated(
                &[Candidate::Verified(Evidence::ExactPath)],
                &mut [7],
                &mut [7],
                [(before, Err(Fault::new(category, "invalidated")))],
            )
            .unwrap_err();
            assert_eq!(fault.category, category);
        }
    }

    #[test]
    fn overflow_refuses_instead_of_truncating_to_one_match() {
        let mut candidates = vec![Candidate::Different; MAX_CANDIDATES];
        candidates[0] = Candidate::Verified(Evidence::ExactPath);
        assert_eq!(summarize(&candidates), Summary::Unique(Evidence::ExactPath));
        candidates.push(Candidate::Different);
        assert_eq!(summarize(&candidates), Summary::Unverifiable);
        assert_eq!(
            summarize(&vec![Candidate::Different; MAX_CANDIDATES + 1]),
            Summary::Unverifiable
        );
    }

    fn signed(team: Option<&str>, hash: u8) -> SigningIdentity {
        SigningIdentity::Signed(SignedIdentity {
            identifier: "dev.example.application".into(),
            team: team.map(str::to_owned),
            unique: vec![hash; 20],
        })
    }

    #[test]
    fn only_complete_unique_correspondence_can_match() {
        let matched = Candidate::Verified(Evidence::ExactPath);
        assert_eq!(summarize(&[]), Summary::Absent);
        assert_eq!(
            summarize(&[matched, Candidate::Different]),
            Summary::Unique(Evidence::ExactPath)
        );
        assert_eq!(summarize(&[matched, matched]), Summary::Ambiguous);
        assert_eq!(
            summarize(&[matched, Candidate::Unverifiable]),
            Summary::Unverifiable
        );
        assert_eq!(summarize(&[Candidate::Different]), Summary::Unverifiable);
        assert_eq!(summarize(&[Candidate::Unverifiable]), Summary::Unverifiable);
    }

    #[test]
    fn unsigned_and_adhoc_code_cannot_claim_relocated_correspondence() {
        assert_eq!(
            correspondence(&SigningIdentity::Unsigned, &SigningIdentity::Unsigned, true),
            Candidate::Verified(Evidence::UnsignedPath)
        );
        assert_eq!(
            correspondence(
                &SigningIdentity::Unsigned,
                &SigningIdentity::Unsigned,
                false
            ),
            Candidate::Unverifiable
        );
        assert_eq!(
            correspondence(&signed(None, 1), &signed(None, 1), true),
            Candidate::Verified(Evidence::ExactPath)
        );
        assert_eq!(
            correspondence(&signed(None, 1), &signed(None, 1), false),
            Candidate::Unverifiable
        );
        assert_eq!(
            correspondence(
                &signed(Some("publisher"), 1),
                &signed(Some("publisher"), 1),
                false
            ),
            Candidate::Verified(Evidence::SignedApplication)
        );
    }

    #[test]
    fn changed_build_or_signing_state_never_downgrades_to_path_only() {
        for exact in [false, true] {
            assert_eq!(
                correspondence(
                    &signed(Some("publisher"), 1),
                    &signed(Some("publisher"), 2),
                    exact
                ),
                Candidate::Unverifiable
            );
            assert_eq!(
                correspondence(
                    &signed(Some("publisher"), 1),
                    &SigningIdentity::Unsigned,
                    exact
                ),
                Candidate::Unverifiable
            );
            assert_eq!(
                correspondence(&SigningIdentity::Unsigned, &signed(None, 1), exact),
                Candidate::Unverifiable
            );
            assert_eq!(
                correspondence(
                    &signed(Some("publisher"), 1),
                    &signed(Some("different"), 1),
                    exact
                ),
                Candidate::Different
            );
        }
    }
}
