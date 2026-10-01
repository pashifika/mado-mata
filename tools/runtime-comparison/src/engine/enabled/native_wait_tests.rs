use super::*;
use crate::model::StopReason;
use mado_pilot_core::{IdentityIssuer, ProviderId};
use std::cell::Cell;
use std::num::NonZeroU32;

fn config() -> NativeConfig {
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    serde_json::from_value(json!({
        "executable_or_bundle":executable,"process_id":42,
        "process_lifetime":"0000000000000007","window_rule":"Selected window",
        "operating_system":"macos","hardware":"test",
        "permission_executable":executable,
        "capture":{"approved":true,"duration_ms":5000,"max_frames":10,"wait_ms":500,"interval_ms":50},
        "input":{"approved":true,"duration_ms":5000,"max_actions":10,
            "route":"process_directed","focus":"preserve","reviewed_operation":"test"},
        "geometry":null,"recognition_language":"test","visible_postcondition":"test",
        "cleanup_ms":1000,"containment_ms":2000
    }))
    .unwrap()
}

fn control() -> Control {
    let plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    Control::new(&plan.limits)
}

fn window(
    issuer: &IdentityIssuer,
    config: &NativeConfig,
    process: Option<(u32, u64)>,
) -> mp::TargetDescription {
    let target = mp::TargetDescription::new(
        issuer
            .issue_target(ProviderId::new("native-wait-test"))
            .unwrap(),
        &config.window_rule,
        mp::PixelExtent::new(8, 8),
        mp::PixelFormat::Bgra8,
        mp::CoordinateSupport::with_target_placement(),
    )
    .with_capability(mp::TargetCapability::capture_only(mp::TargetKind::Window));
    match process {
        Some((pid, lifetime)) => target.with_process_identity(
            mp::TargetProcessIdentity::new(
                NonZeroU32::new(pid).unwrap(),
                lifetime,
                config.executable_or_bundle.clone(),
            )
            .unwrap(),
        ),
        None => target,
    }
}

#[test]
fn window_probes_return_pending_without_discovering_until_the_next_poll() {
    let config = config();
    let control = control();
    let issuer = IdentityIssuer::new();
    let selected = window(&issuer, &config, Some((42, 7)));
    let selected_id = selected.id();
    let mut snapshots = [
        vec![window(&issuer, &config, Some((99, 7)))],
        vec![selected],
    ]
    .into_iter();
    let discoveries = Cell::new(0);
    let mut discover = || {
        discoveries.set(discoveries.get() + 1);
        Ok(snapshots.next().expect("one discovery per Script poll"))
    };
    assert_eq!(
        probe_native_target(&config, &control, &mut discover, || Ok(())).unwrap(),
        None
    );
    assert_eq!(discoveries.get(), 1);
    assert_eq!(
        probe_native_target(&config, &control, &mut discover, || Ok(())).unwrap(),
        Some(selected_id)
    );
    assert_eq!(discoveries.get(), 2);
}

#[test]
fn an_already_present_window_needs_no_startup_wait() {
    let config = config();
    let control = control();
    let selected = window(&IdentityIssuer::new(), &config, Some((42, 7)));
    let selected_id = selected.id();
    let mut snapshot = Some(vec![selected]);
    let target = probe_native_target(
        &config,
        &control,
        || Ok(snapshot.take().expect("one discovery")),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(target, Some(selected_id));
}

#[test]
fn ambiguous_unverifiable_and_recycled_pid_windows_are_not_retried() {
    let config = config();
    let issuer = IdentityIssuer::new();
    for (candidates, category, stage) in [
        (
            vec![
                window(&issuer, &config, Some((42, 7))),
                window(&issuer, &config, Some((42, 7))),
            ],
            "Blocked",
            "target_identity_ambiguous",
        ),
        (
            vec![window(&issuer, &config, None)],
            "Blocked",
            "target_identity_unavailable",
        ),
        (
            vec![window(&issuer, &config, Some((42, 8)))],
            "TargetLost",
            "target_identity_mismatch",
        ),
    ] {
        let mut candidates = Some(candidates);
        let fault = probe_native_target(
            &config,
            &control(),
            || Ok(candidates.take().expect("refusal must not rediscover")),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(fault.category, category);
        assert_eq!(fault.context["stage"], stage);
    }
}

#[test]
fn overflowing_window_candidates_never_become_a_truncated_unique_match() {
    let config = config();
    let issuer = IdentityIssuer::new();
    let mut candidates = vec![window(&issuer, &config, Some((42, 7)))];
    candidates.extend(
        (0..NATIVE_WINDOW_CANDIDATE_LIMIT).map(|_| window(&issuer, &config, Some((99, 7)))),
    );
    let fault = select_native_target(&candidates, &config, 7).unwrap_err();
    assert_eq!(fault.context["stage"], "target_candidate_limit");
}

#[test]
fn windowless_process_loss_or_replacement_refuses_before_discovering_a_successor() {
    let config = config();
    for replacement in [None, Some((42, 8)), Some((43, 7))] {
        let control = control();
        assert_eq!(
            probe_native_target(
                &config,
                &control,
                || Ok(Vec::new()),
                || { require_selected_lifetime(42, 7, &config, 7) }
            )
            .unwrap(),
            None
        );
        let fault = probe_native_target(
            &config,
            &control,
            || panic!("a successor must not be discovered"),
            || {
                let (pid, lifetime) = replacement.ok_or_else(selected_process_lost)?;
                require_selected_lifetime(pid, lifetime, &config, 7)
            },
        )
        .unwrap_err();
        assert_eq!(fault.category, "TargetLost");
    }
}

#[test]
fn process_loss_during_discovery_cannot_publish_a_late_window() {
    let config = config();
    let alive = Cell::new(true);
    let selected = window(&IdentityIssuer::new(), &config, Some((42, 7)));
    let mut snapshot = Some(vec![selected]);
    let fault = probe_native_target(
        &config,
        &control(),
        || {
            alive.set(false);
            Ok(snapshot.take().unwrap())
        },
        || {
            if alive.get() {
                Ok(())
            } else {
                Err(selected_process_lost())
            }
        },
    )
    .unwrap_err();
    assert_eq!(fault.category, "TargetLost");
}

#[test]
fn cancellation_and_timeout_between_polls_never_admit_another_discovery() {
    let config = config();
    for reason in [StopReason::Cancelled, StopReason::Timeout] {
        let control = control();
        assert_eq!(
            probe_native_target(&config, &control, || Ok(Vec::new()), || Ok(())).unwrap(),
            None
        );
        control.stop(reason);
        let fault = probe_native_target(
            &config,
            &control,
            || panic!("Stop forbids a later Script probe"),
            || panic!("Stop forbids process checks"),
        )
        .unwrap_err();
        assert_eq!(fault.category, reason.fault().category);
        assert_eq!(fault.context["stage"], "waiting_for_window");
    }
}

#[test]
fn a_window_returned_after_stop_never_becomes_target_authority() {
    let config = config();
    for reason in [StopReason::Cancelled, StopReason::Timeout] {
        let control = control();
        let mut snapshot = Some(vec![window(&IdentityIssuer::new(), &config, Some((42, 7)))]);
        let fault = probe_native_target(
            &config,
            &control,
            || {
                control.stop(reason);
                Ok(snapshot.take().unwrap())
            },
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(fault.category, reason.fault().category);
    }
}

#[test]
fn an_expired_supplied_budget_never_starts_window_or_process_discovery() {
    let config = config();
    let mut plan: Plan =
        serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
    plan.limits.duration_ms = 0;
    let control = Control::new(&plan.limits);
    let fault = probe_native_target(
        &config,
        &control,
        || panic!("window discovery must not receive a renewed deadline"),
        || panic!("process discovery must not receive a renewed deadline"),
    )
    .unwrap_err();
    assert_eq!(fault.category, "Timeout");
    assert_eq!(fault.context["stage"], "waiting_for_window");
}

#[test]
fn os_failure_and_unavailable_process_evidence_are_not_window_absence() {
    let config = config();
    let failure = Fault::new("PermissionDenied", "discovery denied")
        .with_context(json!({"stage":"target_discovery"}));
    let fault =
        probe_native_target(&config, &control(), || Err(failure.clone()), || Ok(())).unwrap_err();
    assert_eq!(fault.category, failure.category);
    assert_eq!(fault.context, failure.context);

    let fault = probe_native_target(
        &config,
        &control(),
        || panic!("unverifiable process must refuse before window discovery"),
        || {
            Err(blocked(
                "target_identity_unavailable",
                "unavailable process evidence",
            ))
        },
    )
    .unwrap_err();
    assert_eq!(fault.context["stage"], "target_identity_unavailable");
}

#[test]
fn a_selected_window_is_not_replaced_by_matching_metadata_or_a_new_title() {
    let config = config();
    let issuer = IdentityIssuer::new();
    let selected = window(&issuer, &config, Some((42, 7)));
    validate_retained_native_target(selected.id(), &selected, &config, 7).unwrap();
    let replacement = window(&issuer, &config, Some((42, 7)));
    assert_eq!(
        validate_retained_native_target(selected.id(), &replacement, &config, 7)
            .unwrap_err()
            .category,
        "TargetLost"
    );
    let renamed = mp::TargetDescription::new(
        selected.id(),
        "Different window",
        mp::PixelExtent::new(8, 8),
        mp::PixelFormat::Bgra8,
        mp::CoordinateSupport::with_target_placement(),
    )
    .with_capability(mp::TargetCapability::capture_only(mp::TargetKind::Window))
    .with_process_identity(selected.process_identity().unwrap().clone());
    assert_eq!(
        validate_retained_native_target(selected.id(), &renamed, &config, 7)
            .unwrap_err()
            .category,
        "TargetLost"
    );
}

#[test]
fn cancellation_during_process_validation_does_not_begin_window_discovery() {
    let config = config();
    let control = control();
    let fault = probe_native_target(
        &config,
        &control,
        || panic!("late process validation cannot start window discovery"),
        || {
            control.cancel();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(fault.category, "Cancelled");
    assert_eq!(fault.context["stage"], "waiting_for_window");
}
