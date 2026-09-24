use super::evidence::settlement_bytes;
use super::protocol::frame;
use super::supervision::{settled_evidence, terminal_primary};
use crate::model::{Fault, MAX_TRANSPORT_BYTES};
use serde_json::{Value, json};

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
