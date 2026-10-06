use super::evidence::settlement_bytes;
use super::protocol::frame;
use super::recognition::child_exit_primary;
use super::supervision::{comparison_identity, settled_evidence, terminal_primary};
use crate::images::PayloadBytes;
use crate::inventory::Inventory;
use crate::model::{Fault, MAX_TRANSPORT_BYTES};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

#[test]
fn forced_eof_preserves_a_returned_entry_without_claiming_cleanup() {
    let entry = json!({"event":"EntrySettled","entry_outcome":"Returned","primary":null});
    let evidence = settled_evidence(None, &[entry]);
    let error = frame(
        &mut br#"{"event":"Terminal""#.as_slice(),
        MAX_TRANSPORT_BYTES,
    )
    .expect_err("forced exit interrupted the terminal frame");
    assert!(terminal_primary(&evidence, Some(&error), true).is_none());
    assert_eq!(evidence["entry_outcome"], "Returned");
    assert!(evidence.get("cleanup").is_none());
}

#[test]
fn forced_exit_does_not_excuse_other_protocol_failures() {
    let entry = json!({"event":"EntrySettled","entry_outcome":"Returned","primary":null});
    let partial = frame(&mut b"{".as_slice(), MAX_TRANSPORT_BYTES).unwrap_err();
    let oversized = frame(&mut b"123456789\n".as_slice(), 8).unwrap_err();
    for (scenario, evidence, error, forced) in [
        ("unexpected EOF", &entry, &partial, false),
        ("unobserved entry", &Value::Null, &partial, true),
        ("oversized frame", &entry, &oversized, true),
    ] {
        let primary = terminal_primary(evidence, Some(error), forced)
            .unwrap_or_else(|| panic!("{scenario} must remain a protocol failure"));
        assert_eq!(primary.category, "Transport", "{scenario}");
    }
    let failed = json!({"event":"EntrySettled","entry_outcome":"FailedOrNotStarted",
        "primary":Fault::new("Script", "entry failed")});
    assert_eq!(
        terminal_primary(&failed, Some(&partial), true)
            .unwrap()
            .category,
        "Script"
    );
}

#[test]
fn recognition_child_exit_before_entry_has_a_primary_despite_clean_pipe_eof() {
    assert!(
        frame(&mut b"".as_slice(), MAX_TRANSPORT_BYTES)
            .unwrap()
            .is_none()
    );
    let started = json!({"event":"ChildStarted","run":"recognition","attempt":1});
    let evidence = settled_evidence(None, &[started]);
    let primary = child_exit_primary(
        terminal_primary(&evidence, None, false),
        &evidence,
        false,
        None,
    )
    .expect("a signaled child exit before EntrySettled must be attributable");
    assert_eq!(primary.category, "Transport");
    assert_eq!(primary.context["stage"], "child_exit");
    assert_eq!(primary.context["terminal_received"], false);
    assert!(primary.context["exit_code"].is_null());
}

#[test]
fn recognition_child_exit_requires_both_terminal_evidence_and_successful_exit() {
    let entry = json!({"event":"EntrySettled","primary":null,"result":{"matched":false}});
    let terminal = json!({"event":"Terminal","primary":null,"result":{"matched":false},
        "cleanup":{"clean":true,"status":"CleanupFinished","session_closed":true}});
    for (scenario, terminal, exit_success, exit_code, expected_terminal) in [
        ("missing terminal", None, true, Some(0), false),
        (
            "nonzero exit after terminal",
            Some(terminal.clone()),
            false,
            Some(1),
            true,
        ),
    ] {
        let evidence = settled_evidence(terminal, std::slice::from_ref(&entry));
        let primary = child_exit_primary(
            terminal_primary(&evidence, None, false),
            &evidence,
            exit_success,
            exit_code,
        )
        .unwrap_or_else(|| panic!("{scenario} must not become a successful trial"));
        assert_eq!(primary.category, "Transport", "{scenario}");
        assert_eq!(
            primary.context["terminal_received"], expected_terminal,
            "{scenario}"
        );
        assert_eq!(primary.context["exit_code"], json!(exit_code), "{scenario}");
    }
    let evidence = settled_evidence(Some(terminal), &[entry]);
    assert!(
        child_exit_primary(
            terminal_primary(&evidence, None, false),
            &evidence,
            true,
            Some(0),
        )
        .is_none()
    );
}

#[test]
fn recognition_child_exit_preserves_the_settled_primary_and_cancellation_context() {
    let cancelled = Fault::new("Cancelled", "recognition was stopped").with_context(
        json!({"stage":"ocr_recognition","status":"cancelled","native_cleanup":"unverified"}),
    );
    let entry = json!({"event":"EntrySettled","primary":cancelled,"result":null});
    let evidence = settled_evidence(None, &[entry]);
    let primary = child_exit_primary(
        terminal_primary(&evidence, None, false),
        &evidence,
        false,
        None,
    )
    .expect("the child cancellation remains primary despite an incomplete exchange");
    assert_eq!(json!(primary), json!(cancelled));
}

#[test]
fn bounded_fallback_retains_receipts_and_owners_without_inventing_cleanup() {
    let facts = json!({
        "accepted":[{"id":"sequence-1","order":1,"actions":[{"kind":"key_down","key":"A"}]}],
        "receipts":[{"id":"sequence-1","order":1,"status":"Submitted","submitted":1,
            "total":1,"cleanup_required":["A"],"sink":"controlled-non-native"}],
        "held_keys":["A"],"attempt_owners":2,"in_flight_native":1,
        "postconditions":[{"satisfied":false}],
        "snapshot_stage":"pre_cleanup"
    });
    let mut observations = facts.clone();
    observations["logs"] = json!([{"message":"x".repeat(MAX_TRANSPORT_BYTES)}]);
    let mut bytes = settlement_bytes(json!({
        "event":"EntrySettled","run":"retained","attempt":1,
        "primary":Fault::new("Script", "workflow failed"),
        "entry_outcome":"FailedOrNotStarted","observations":observations
    }))
    .expect("diagnostic fallback fits");
    bytes.push(b'\n');
    let frame = frame(&mut bytes.as_slice(), MAX_TRANSPORT_BYTES)
        .expect("bounded protocol frame")
        .expect("entry frame");
    let milestone = serde_json::from_slice(&frame).expect("settled evidence");
    let retained = settled_evidence(None, &[milestone]);
    assert_eq!(retained["primary"]["category"], "Script");
    for field in [
        "accepted",
        "receipts",
        "held_keys",
        "attempt_owners",
        "in_flight_native",
        "postconditions",
        "snapshot_stage",
    ] {
        assert_eq!(retained["observations"][field], facts[field], "{field}");
    }
    assert_eq!(retained["diagnostic_details_omitted"], true);
    assert!(retained.get("cleanup").is_none());
}

#[test]
fn comparison_cohort_spans_candidate_sources_but_binds_asset_names_lengths_and_bytes() {
    let capture = |candidate: &str| {
        let plan = crate::check::plan(candidate, "success", "template-first");
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(candidate);
        let inventory = Inventory::capture(&root, &plan.limits).unwrap();
        (plan, inventory)
    };
    let (plan, javascript) = capture("javascript");
    let (lua_plan, lua) = capture("lua");
    assert_ne!(
        javascript.identity, lua.identity,
        "candidate sources split whole-package identity"
    );
    let common = comparison_identity(&plan, &javascript).unwrap();
    assert_eq!(
        comparison_identity(&lua_plan, &lua).unwrap(),
        common,
        "candidates over the same data and configuration share one cohort"
    );

    let with_assets = |assets: &[(&str, &[u8])]| {
        let mut inventory = javascript.clone();
        inventory.assets = assets
            .iter()
            .map(|&(name, bytes)| (name.to_owned(), PayloadBytes::new(bytes.to_vec()).unwrap()))
            .collect();
        comparison_identity(&plan, &inventory).unwrap()
    };
    let marker = javascript.assets["marker"].as_slice();
    assert_eq!(
        with_assets(&[("marker", marker)]),
        common,
        "identical bytes without a cached digest stay in the cohort"
    );
    let mut changed = marker.to_vec();
    changed[0] ^= 1;
    let mut extended = marker.to_vec();
    extended.push(0);
    let other = vec![7_u8; marker.len()];
    let mut schema = javascript.clone();
    schema.schema["version"] = json!(2);
    let mut distinct = BTreeSet::from([common]);
    for (case, identity) in [
        ("changed byte", with_assets(&[("marker", &changed)])),
        ("extended length", with_assets(&[("marker", &extended)])),
        ("renamed asset", with_assets(&[("renamed", marker)])),
        (
            "added asset",
            with_assets(&[("marker", marker), ("other", &other)]),
        ),
        (
            "swapped bytes",
            with_assets(&[("marker", &other), ("other", marker)]),
        ),
        (
            "changed schema",
            comparison_identity(&plan, &schema).unwrap(),
        ),
    ] {
        assert!(distinct.insert(identity), "{case} must split the cohort");
    }
}
