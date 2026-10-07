use serde_json::{Value, json};

/// A terminal account is authority transfer, not an estimate from diagnostic logs.
pub(crate) fn recovery_allowance(
    record: &Value,
    frames: u64,
    actions: usize,
) -> Option<(u64, usize)> {
    let observations = &record["observations"];
    let account = &observations["accounting"];
    if record["attempt"] != 1
        || record["primary"]["category"] != "TargetExited"
        || !matches!(
            record["primary"]["context"]["exit_reason"].as_str(),
            Some("absent" | "reused_pid" | "zombie")
        )
        || observations["workflow_entered"] != true
        || observations["terminal_accounted"] != true
        || observations["child_reaped"] != true
        || observations["startup_settled"] != true
        || observations["snapshot_stage"] != "post_cleanup"
        || record["cleanup"]["clean"] != true
        || record["forced"] != false
        || record["exit_code"] != 0
        || account["complete"] != true
        || account["input_uncertain"] != false
    {
        return None;
    }
    let frames = frames.checked_sub(account["frames"].as_u64()?)?;
    let actions =
        actions.checked_sub(usize::try_from(account["expanded_input_events"].as_u64()?).ok()?)?;
    Some((frames, actions))
}

pub(crate) fn attempt_summary(record: &Value) -> Value {
    let mut summary = json!({});
    for field in [
        "run",
        "attempt",
        "status",
        "reason",
        "primary",
        "entry_outcome",
        "cleanup",
        "observations",
        "forced",
        "exit_code",
        "stage",
        "completed_stages",
        "native_preparation",
    ] {
        summary[field] = record[field].clone();
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exited() -> Value {
        json!({"attempt":1,"primary":{"category":"TargetExited","context":{"exit_reason":"absent"}},
            "cleanup":{"clean":true},"forced":false,"exit_code":0,
            "observations":{"workflow_entered":true,"terminal_accounted":true,"child_reaped":true,
                "startup_settled":true,"snapshot_stage":"post_cleanup",
                "accounting":{"complete":true,"frames":12,"expanded_input_events":4,"input_uncertain":false}}})
    }

    #[test]
    fn only_complete_positive_first_workflow_exit_transfers_unused_authority() {
        for reason in ["absent", "reused_pid", "zombie"] {
            let mut record = exited();
            record["primary"]["context"]["exit_reason"] = json!(reason);
            assert_eq!(recovery_allowance(&record, 300, 64), Some((288, 60)));
        }
        for (pointer, value) in [
            ("/attempt", json!(2)),
            ("/primary/category", json!("TargetLost")),
            ("/primary/category", json!("PermissionDenied")),
            ("/primary/category", json!("ProviderFailure")),
            ("/primary/category", json!("Cancelled")),
            ("/primary/context/exit_reason", json!("unknown")),
            ("/observations/workflow_entered", json!(false)),
            ("/observations/terminal_accounted", json!(false)),
            ("/observations/child_reaped", json!(false)),
            ("/observations/startup_settled", json!(false)),
            ("/observations/snapshot_stage", json!("pre_cleanup")),
            ("/cleanup/clean", json!(false)),
            ("/forced", json!(true)),
            ("/exit_code", json!(124)),
            ("/observations/accounting", Value::Null),
            ("/observations/accounting/complete", json!(false)),
            ("/observations/accounting/input_uncertain", json!(true)),
            ("/observations/accounting/frames", json!(301)),
            ("/observations/accounting/expanded_input_events", json!(65)),
        ] {
            let mut record = exited();
            *record.pointer_mut(pointer).unwrap() = value;
            assert_eq!(recovery_allowance(&record, 300, 64), None, "{pointer}");
        }
        assert_eq!(recovery_allowance(&exited(), 12, 64), Some((0, 60)));
        assert_eq!(recovery_allowance(&exited(), 300, 4), Some((288, 0)));
    }
}
