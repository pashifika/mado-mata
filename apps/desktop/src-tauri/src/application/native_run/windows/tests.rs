use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn control() -> Control {
    super::super::tests::control()
}

fn discover(
    entries: impl IntoIterator<Item = Result<Candidate<u32>, Fault>>,
) -> Result<Option<u32>, Fault> {
    let mut entries = entries.into_iter();
    select_process(&control(), || Ok(entries.next()), |entry| entry)
}

#[test]
fn only_complete_unique_discovery_can_return_a_process_or_absence() {
    assert_eq!(discover([Ok(Candidate::Unrelated)]).unwrap(), None);
    assert_eq!(
        discover([
            Ok(Candidate::Unrelated),
            Ok(Candidate::Matching(7)),
            Ok(Candidate::Unrelated),
        ])
        .unwrap(),
        Some(7),
    );
    assert_eq!(
        discover([Ok(Candidate::Matching(7)), Ok(Candidate::Matching(8))])
            .unwrap_err()
            .category,
        "NativeTargetAmbiguous",
    );
    for entries in [
        vec![Err(unavailable())],
        vec![Ok(Candidate::Matching(7)), Err(unavailable())],
        vec![Err(unavailable()), Ok(Candidate::Matching(7))],
    ] {
        assert_eq!(
            discover(entries).unwrap_err().category,
            "NativeTargetUnverifiable"
        );
    }
}

#[test]
fn an_incomplete_or_overflowed_snapshot_never_certifies_uniqueness() {
    let mut calls = 0;
    let failure = select_process(
        &control(),
        || {
            calls += 1;
            if calls == 1 {
                Ok(Some(7))
            } else {
                Err(unavailable())
            }
        },
        |pid| Ok(Candidate::Matching(pid)),
    )
    .unwrap_err();
    assert_eq!(failure.category, "NativeTargetUnverifiable");
    assert_eq!(calls, 2);

    for length in [MAX_PROCESSES, MAX_PROCESSES + 1] {
        let entries = (0..length).map(|index| {
            Ok(if index == 0 {
                Candidate::Matching(7)
            } else {
                Candidate::Unrelated
            })
        });
        let result = discover(entries);
        if length == MAX_PROCESSES {
            assert_eq!(result.unwrap(), Some(7));
        } else {
            assert_eq!(result.unwrap_err().category, "NativeTargetUnverifiable");
        }
    }
}

#[test]
fn cancellation_prevents_os_entry_and_rejects_late_discovery() {
    let cancelled = control();
    cancelled.cancel();
    let result: Result<Option<u32>, Fault> = select_process(
        &cancelled,
        || panic!("cancelled work must not enter process enumeration"),
        |(): ()| panic!("cancelled work must not inspect a process"),
    );
    assert_eq!(result.unwrap_err().category, "Cancelled");

    let active = control();
    let result: Result<Option<u32>, Fault> = select_process(
        &active,
        || {
            active.cancel();
            Ok(Some(()))
        },
        |()| panic!("late enumeration must not inspect a process"),
    );
    assert_eq!(result.unwrap_err().category, "Cancelled");
}

#[derive(Debug)]
struct Retained(Arc<AtomicUsize>);

impl Drop for Retained {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn retained_evidence_is_released_on_ambiguity_cancellation_and_owner_drop() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut entries = [0, 1].into_iter();
    let result = select_process(
        &control(),
        || Ok(entries.next()),
        |_| Ok(Candidate::Matching(Retained(Arc::clone(&dropped)))),
    );
    assert_eq!(result.unwrap_err().category, "NativeTargetAmbiguous");
    assert_eq!(dropped.load(Ordering::Acquire), 2);

    let active = control();
    let result = select_process(
        &active,
        || Ok(Some(())),
        |()| {
            active.cancel();
            Ok(Candidate::Matching(Retained(Arc::clone(&dropped))))
        },
    );
    assert_eq!(result.unwrap_err().category, "Cancelled");
    assert_eq!(dropped.load(Ordering::Acquire), 3);

    let mut entries = [()].into_iter();
    let selected = select_process(
        &control(),
        || Ok(entries.next()),
        |()| Ok(Candidate::Matching(Retained(Arc::clone(&dropped)))),
    )
    .unwrap();
    assert_eq!(dropped.load(Ordering::Acquire), 3);
    drop(selected);
    assert_eq!(dropped.load(Ordering::Acquire), 4);
}

#[test]
fn saved_canonical_resolution_cannot_silently_follow_a_new_destination() {
    let canonical = std::path::Path::new("C:/Games/Original.exe");
    let original = crate::target::ResolvedLocation {
        path: canonical.to_str().unwrap().into(),
        executable: canonical.to_str().unwrap().into(),
    };
    require_saved_resolution(canonical, &original).unwrap();
    for field in ["path", "executable"] {
        let mut changed = original.clone();
        if field == "path" {
            changed.path = "C:/Games/Replacement.exe".into();
        } else {
            changed.executable = "C:/Games/Replacement.exe".into();
        }
        assert_eq!(
            require_saved_resolution(canonical, &changed)
                .unwrap_err()
                .category,
            "StaleNativeSelection",
        );
    }
}
