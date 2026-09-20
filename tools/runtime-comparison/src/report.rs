use crate::model::{ENGINE_REVISION, Fault};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const METRICS: [&str; 13] = [
    "startup_us",
    "preflight_us",
    "host_call_us",
    "workflow_us",
    "stop_receipt_us",
    "admission_close_us",
    "cleanup_us",
    "containment_us",
    "cpu_percent",
    "supervisor_rss_bytes",
    "child_rss_bytes",
    "vm_bytes",
    "live_owners",
];

pub fn build_identity() -> Value {
    json!({
        "tool_version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "rustc":env!("COMPARISON_RUSTC"),"target":env!("COMPARISON_TARGET"),
        "profile":env!("COMPARISON_PROFILE"),"engine_revision":ENGINE_REVISION,
        "source_sha256":env!("COMPARISON_SOURCE_SHA256"),"os_version":sysinfo::System::long_os_version(),
        "kernel_version":sysinfo::System::kernel_version(),"engine_enabled":cfg!(feature="engine"),
        "cargo_lock_sha256":format!("{:x}",Sha256::digest(include_bytes!("../Cargo.lock"))),
        "javascript":{"crate":"rquickjs","version":"0.14.0","engine":"QuickJS-ng 0.16.2","features":["std","loader"],"allocator":"QuickJS default"},
        "lua":{"crate":"mlua","version":"0.12.1","engine":"Lua 5.4.9","abi":"Lua 5.4","features":["lua54","vendored","serde"]},
        "typescript":{"version":"5.9.3","role":"authoring into JavaScript VM"},
        "sampling":{"clock":"monotonic","quantile":"nearest-rank","minimum_samples":{"p50":2,"p95":20,"p99":100}}
    })
}

fn catalog() -> Result<Vec<Value>, Fault> {
    serde_json::from_str(include_str!("../fixtures/coverage.json"))
        .map_err(|error| Fault::new("Report", error.to_string()))
}

fn os(row: &Value) -> &str {
    row["os"]
        .as_str()
        .or_else(|| row["build"]["os"].as_str())
        .unwrap_or("unknown")
}

fn reference(row: &Value) -> &Value {
    row.get("run")
        .or_else(|| row.get("id"))
        .unwrap_or(&Value::Null)
}

/// Missing execution remains visible; a catalog entry alone is never evidence.
pub fn coverage(rows: &[Value]) -> Result<Vec<Value>, Fault> {
    let mut matrix = Vec::new();
    let mut systems = BTreeSet::from(["windows", "macos"]);
    systems.extend(rows.iter().map(os));
    for requirement in catalog()? {
        for candidate in ["javascript", "lua"] {
            let applicable = requirement["candidates"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == candidate));
            let lanes = requirement["lanes"]
                .as_array()
                .ok_or_else(|| Fault::new("Report", "catalog lane missing"))?;
            for lane in lanes {
                for system in &systems {
                    if lane != "controlled" && !["windows", "macos"].contains(system) {
                        continue;
                    }
                    let checks_key = match lane.as_str() {
                        Some("controlled") => "controlled_checks",
                        Some("replay") => "replay_checks",
                        Some("native") => "native_checks",
                        _ => return Err(Fault::new("Report", "unknown catalog evidence lane")),
                    };
                    let checks = requirement[checks_key][candidate].as_array();
                    let mut evidence = Vec::new();
                    let mut complete = checks.is_some_and(|checks| !checks.is_empty());
                    let mut failed = false;
                    let mut blocked = false;
                    let mut unexecuted = false;
                    for check in checks.into_iter().flatten() {
                        let matches: Vec<_> = rows
                            .iter()
                            .filter(|row| {
                                row["id"] == *check && os(row) == *system && row["lane"] == *lane
                            })
                            .collect();
                        if matches.is_empty() {
                            complete = false;
                        }
                        for row in matches {
                            match row["status"].as_str() {
                                Some("PASS") => {}
                                Some("FAIL") => failed = true,
                                Some("BLOCKED") => blocked = true,
                                _ => unexecuted = true,
                            }
                            evidence.push(reference(row).clone());
                        }
                    }
                    let (status, reason) = if !applicable {
                        (
                            "NOT_APPLICABLE",
                            "language-specific mechanism only; shared host and lifecycle requirements remain applicable",
                        )
                    } else if failed {
                        ("FAIL", "at least one required behavioral oracle failed")
                    } else if blocked {
                        (
                            "BLOCKED",
                            "at least one required oracle has an unmet prerequisite",
                        )
                    } else if unexecuted {
                        (
                            "UNEXECUTED",
                            "at least one required oracle has not executed",
                        )
                    } else if complete {
                        (
                            "PASS",
                            "all named same-OS, same-lane oracles passed; shared checks retain their actual implementation identity",
                        )
                    } else if lane == "native" {
                        (
                            "BLOCKED",
                            "this catalog/result set does not establish complete per-scenario native qualification with explicit per-OS authority",
                        )
                    } else if lane == "replay" {
                        (
                            "BLOCKED",
                            "this result set lacks the named real recorded-replay oracles on this OS",
                        )
                    } else {
                        (
                            "UNEXECUTED",
                            "this result set lacks direct evidence for the complete scenario on this OS",
                        )
                    };
                    matrix.push(json!({"scenario":requirement["id"],"candidate":candidate,"lane":lane,"os":system,
                        "status":status,"reason":reason,"oracle":requirement["oracle"],"checks":checks,"evidence":evidence}));
                }
            }
        }
    }
    Ok(matrix)
}

fn budget(row: &Value, metric: &str) -> Value {
    let observed = row["metrics"][metric].as_f64();
    let bound = row["metrics"]["budgets"][metric].as_f64();
    let ordinary_return = row["metrics"]["stop_receipt_us"].is_null() && row["primary"].is_null();
    let (status, reason) = match (observed, bound) {
        (Some(value), Some(bound))
            if value.is_finite() && value >= 0.0 && bound.is_finite() && bound > 0.0 =>
        {
            if value <= bound {
                ("PASS", "observed value is within the prospective bound")
            } else {
                ("FAIL", "observed value exceeds the prospective bound")
            }
        }
        (None, _) if metric == "vm_bytes" && row["candidate"] == "rust" => {
            ("NOT_APPLICABLE", "direct Rust has no embedded VM")
        }
        (None, _)
            if ordinary_return
                && ["stop_receipt_us", "admission_close_us", "containment_us"]
                    .contains(&metric) =>
        {
            (
                "NOT_APPLICABLE",
                "ordinary return has no external Stop sample",
            )
        }
        _ => (
            "UNEXECUTED",
            "measurement or prospective numeric bound unavailable; not a budget pass",
        ),
    };
    json!({"evidence":reference(row),"candidate":row["candidate"],"os":os(row),"lane":row["lane"],
        "plan_identity":row["plan_identity"],"metric":metric,"observed":observed,"bound":bound,"status":status,"reason":reason})
}

pub fn summarize(data: &Value) -> Result<Value, Fault> {
    if data["version"] != 1 {
        return Err(Fault::new("Report", "unsupported result format"));
    }
    let rows = data
        .get("rows")
        .or_else(|| data.get("runs"))
        .and_then(Value::as_array)
        .ok_or_else(|| Fault::new("Report", "result set requires rows or runs"))?;
    let mut counts = BTreeMap::<String, usize>::new();
    let mut unique = BTreeSet::new();
    let mut groups = BTreeMap::<String, Vec<f64>>::new();
    let mut comparisons = BTreeMap::<String, BTreeMap<String, Vec<f64>>>::new();
    let mut budgets = Vec::new();
    for row in rows {
        let status = row["status"]
            .as_str()
            .ok_or_else(|| Fault::new("Report", "row has no status"))?;
        if !["PASS", "FAIL", "BLOCKED", "UNEXECUTED", "NOT_APPLICABLE"].contains(&status) {
            return Err(Fault::new("Report", "invalid result status"));
        }
        let key = reference(row)
            .as_str()
            .ok_or_else(|| Fault::new("Report", "row has no evidence identity"))?;
        if !unique.insert(key) {
            return Err(Fault::new("Report", "duplicate evidence identity"));
        }
        *counts.entry(status.into()).or_default() += 1;
        if row["metrics"]["warmup"] == true {
            continue;
        }
        if row["metrics"].is_object() {
            budgets.extend(METRICS.iter().map(|metric| budget(row, metric)));
        }
        if let Some(value) = row["metrics"]["workflow_us"]
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            let group = json!([
                row["candidate"],
                row["lane"],
                row["scenario"],
                row["profile"],
                row["plan_identity"],
                row["inventory_identity"],
                row["build"],
                row["execution_status"]
            ])
            .to_string();
            groups.entry(group).or_default().push(value);
            if status == "PASS"
                && row["execution_status"] != "FAIL"
                && row["metrics"]["comparison_identity"].is_string()
            {
                let cohort = json!([
                    row["metrics"]["comparison_identity"],
                    row["build"],
                    row["lane"],
                    row["profile"]
                ])
                .to_string();
                if let Some(candidate) = row["candidate"].as_str() {
                    comparisons
                        .entry(cohort)
                        .or_default()
                        .entry(candidate.into())
                        .or_default()
                        .push(value);
                }
            }
        }
    }
    let timings: Vec<Value> = groups.into_iter().map(|(group,mut samples)| {
        samples.sort_by(f64::total_cmp);
        json!({"identity":serde_json::from_str::<Value>(&group).ok(),"samples":samples.len(),
            "workflow_us":{"p50":quantile(&samples,50,2),"p95":quantile(&samples,95,20),"p99":quantile(&samples,99,100)},
            "method":"nearest-rank; null means insufficient samples"})
    }).collect();
    let deltas: Vec<Value> = comparisons.into_iter().map(|(cohort,mut candidates)| {
        for samples in candidates.values_mut() { samples.sort_by(f64::total_cmp); }
        let baseline = candidates.get("rust").and_then(|samples|quantile(samples,50,2));
        let values: BTreeMap<_,_> = candidates.iter().filter(|(name,_)|name.as_str()!="rust").map(|(name,samples)| {
            (name.clone(),json!({"samples":samples.len(),"workflow_p50_delta_us":baseline.zip(quantile(samples,50,2)).map(|(rust,candidate)|candidate-rust)}))
        }).collect();
        json!({"identity":serde_json::from_str::<Value>(&cohort).ok(),"direct_rust_p50_us":baseline,"candidates":values,
            "method":"matched data/configuration/build cohort; unavailable control or insufficient samples gives null"})
    }).collect();
    let matrix = coverage(rows)?;
    let mut coverage_counts = BTreeMap::<String, usize>::new();
    for row in &matrix {
        let key = format!(
            "{}/{}/{}/{}",
            row["candidate"].as_str().unwrap_or(""),
            row["os"].as_str().unwrap_or(""),
            row["lane"].as_str().unwrap_or(""),
            row["status"].as_str().unwrap_or("")
        );
        *coverage_counts.entry(key).or_default() += 1;
    }
    let missing: Vec<Value> = ["javascript","lua"].into_iter().map(|candidate| {
        let unmet = matrix.iter().filter(|row|row["candidate"]==candidate && ["windows","macos"].contains(&os(row))
            && row["status"]!="PASS" && row["status"]!="NOT_APPLICABLE").count();
        let unmet_budgets = ["windows","macos"].into_iter().flat_map(|system|METRICS.map(|metric|(system,metric)))
            .filter(|(system,metric)|!budgets.iter().any(|row|row["candidate"]==candidate && row["os"]==*system
                && row["lane"]=="native" && row["metric"]==*metric && row["status"]=="PASS")).count();
        json!({"candidate":candidate,"unmet_required_rows":unmet,"unmet_budget_rows":unmet_budgets})
    }).collect();
    // Summary rows do not authorize native operations or constitute a reviewed
    // runtime-adoption decision.
    Ok(
        json!({"version":1,"counts":counts,"timings":timings,"direct_rust_deltas":deltas,
        "coverage_counts":coverage_counts,"budgets":budgets,
        "decision":{"kind":"Blocked","reason":"both-OS native qualification and an explicit runtime decision remain outstanding","coverage":missing},
        "claims":{"controlled":"non-native sink only","replay":"recognition only; no native input",
            "native":"no native execution authority inferred","artifact_completion_is_adoption":false}}),
    )
}

fn quantile(values: &[f64], percentile: usize, minimum: usize) -> Option<f64> {
    if values.len() < minimum {
        return None;
    }
    values
        .get((values.len() * percentile).div_ceil(100) - 1)
        .copied()
}
