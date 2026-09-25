//! Pure correspondence policy; OS evidence is collected by the macOS module.
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

pub(super) fn summarize(candidates: &[Candidate]) -> (&'static str, Option<Evidence>) {
    if candidates.len() > MAX_CANDIDATES {
        return ("unverifiable", None);
    }
    if candidates.is_empty() {
        return ("not_running", None);
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
        ("ambiguous", None)
    } else if matches == 1 && !uncertain {
        ("matched", evidence)
    } else {
        ("unverifiable", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_refuses_instead_of_truncating_to_one_match() {
        let mut candidates = vec![Candidate::Different; MAX_CANDIDATES];
        candidates[0] = Candidate::Verified(Evidence::ExactPath);
        assert_eq!(
            summarize(&candidates),
            ("matched", Some(Evidence::ExactPath))
        );
        candidates.push(Candidate::Different);
        assert_eq!(summarize(&candidates), ("unverifiable", None));
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
        assert_eq!(summarize(&[]), ("not_running", None));
        assert_eq!(
            summarize(&[matched, Candidate::Different]),
            ("matched", Some(Evidence::ExactPath))
        );
        assert_eq!(summarize(&[matched, matched]), ("ambiguous", None));
        assert_eq!(
            summarize(&[matched, Candidate::Unverifiable]),
            ("unverifiable", None)
        );
        assert_eq!(summarize(&[Candidate::Different]), ("unverifiable", None));
        assert_eq!(
            summarize(&[Candidate::Unverifiable]),
            ("unverifiable", None)
        );
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
