use crate::model::{ENGINE_REVISION, Fault};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::LazyLock;

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
    static IDENTITY: LazyLock<Value> = LazyLock::new(|| {
        // Enumerate non-sensitive hardware once, without process lists,
        // hostnames, serial numbers or private machine paths.
        let mut system = sysinfo::System::new();
        system.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
        system.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
        let cpu_brands: BTreeSet<_> = system.cpus().iter().map(|cpu| cpu.brand().trim()).collect();
        json!({
            "tool_version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "rustc":env!("COMPARISON_RUSTC"),"target":env!("COMPARISON_TARGET"),
            "profile":env!("COMPARISON_PROFILE"),"engine_revision":ENGINE_REVISION,
            "source_sha256":env!("COMPARISON_SOURCE_SHA256"),"os_version":sysinfo::System::long_os_version(),
            "kernel_version":sysinfo::System::kernel_version(),"engine_enabled":cfg!(feature="engine"),
            "hardware":{"cpu_brands":cpu_brands,"logical_cpus":system.cpus().len(),
                "total_memory_bytes":system.total_memory()},
            "executable_sha256":executable_identity(),
            "cargo_lock_sha256":format!("{:x}",Sha256::digest(include_bytes!("../Cargo.lock"))),
            "javascript":{"crate":"rquickjs","version":"0.14.0","engine":"QuickJS-ng 0.16.2","features":["std","loader"],"allocator":"QuickJS default"},
            "lua":{"crate":"mlua","version":"0.12.1","engine":"Lua 5.4.9","abi":"Lua 5.4","features":["lua54","vendored","serde"]},
            "typescript":{"version":"5.9.3","role":"authoring into JavaScript VM"},
            "sampling":{"clock":"monotonic","quantile":"nearest-rank","minimum_samples":{"p50":2,"p95":20,"p99":100}}
        })
    });
    IDENTITY.clone()
}

fn executable_identity() -> Option<String> {
    let file = std::fs::File::open(std::env::current_exe().ok()?).ok()?;
    let metadata = file.metadata().ok()?;
    let length = metadata.len();
    if !metadata.is_file() || length == 0 {
        return None;
    }
    // A growing executable cannot turn hashing into an unbounded read.
    // No path or OS error is exported into public output.
    let mut input = file.take(length.checked_add(1)?);
    let mut hash = Sha256::new();
    let mut observed = 0;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        observed += count as u64;
        hash.update(&buffer[..count]);
    }
    (observed == length).then(|| format!("{:x}", hash.finalize()))
}

/// Only the built-in controlled check may attest this executable-defined suite.
/// Bind observed compiler/parser identities as well as authoring inputs: static
/// version strings do not identify a Node patch, installed compiler or emitted code.
pub(crate) fn controlled_qualification(
    rows: &[Value],
    inventories: &BTreeMap<&str, &str>,
) -> Result<Value, Fault> {
    let mut configurations: Vec<_> = rows
        .iter()
        .map(|row| json!([row["id"], row["candidate"], row["plan_identity"]]))
        .collect();
    let mut corpus: Vec<_> = rows
        .iter()
        .map(|row| {
            json!([
                row["id"],
                row["candidate"],
                row["inventory_identity"],
                row["metrics"]["compiled_inventory_identity"],
                row["metrics"]["runtime"]["source_diagnostic"]["import_parser"]["identity"],
                row["primary"]["context"]["parser"],
                row["primary"]["context"]["compiler"],
                row["observed"]["context"]["compiler"],
                row["observed"]["context"]["typescript"]["compiler_sha256"]
            ])
        })
        .collect();
    configurations.sort_by(|left, right| left[0].as_str().cmp(&right[0].as_str()));
    corpus.sort_by(|left, right| left[0].as_str().cmp(&right[0].as_str()));
    let source = env!("COMPARISON_SOURCE_SHA256");
    Ok(json!({
        "version":1,
        "configuration_sha256":crate::model::identity(&json!({
            "source":source,"controlled_configurations":configurations
        }))?,
        "corpus_sha256":crate::model::identity(&json!({
            "source":source,"inventories":inventories,"controlled_corpus":corpus
        }))?
    }))
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

fn known_text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.trim().is_empty()
            && !["unknown", "unavailable", "unspecified"]
                .iter()
                .any(|unknown| text.trim().eq_ignore_ascii_case(unknown))
    })
}

fn hex_identity(value: &Value, length: usize) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == length
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

// Qualification identity is suite-scoped, not a single plan or package hash.
// The producer must bind its complete, immutable check-to-configuration manifest
// (including limits, budgets and any native engine/target configuration) and
// corpus manifest (including all package variants, assets, replay frames/models).
// Different declared profiles/scenarios may then contribute to the SAME suite.
// A label, timestamp, path or absent digest cannot establish compatibility.
// These are evidence-producer attestations, not authentication of imported JSON.
fn qualification_identity(row: &Value) -> Result<String, &'static str> {
    let build = &row["build"];
    if ![
        "/tool_version",
        "/os",
        "/arch",
        "/rustc",
        "/target",
        "/profile",
        "/os_version",
        "/kernel_version",
        "/javascript/crate",
        "/javascript/version",
        "/javascript/engine",
        "/javascript/allocator",
        "/lua/crate",
        "/lua/version",
        "/lua/engine",
        "/lua/abi",
        "/typescript/version",
        "/typescript/role",
    ]
    .iter()
    .all(|path| build.pointer(path).is_some_and(known_text))
        || !hex_identity(&build["source_sha256"], 64)
        || !hex_identity(&build["cargo_lock_sha256"], 64)
        || !hex_identity(&build["executable_sha256"], 64)
        || !hex_identity(&build["engine_revision"], 40)
        || !build["engine_enabled"].is_boolean()
        || !build["hardware"]["cpu_brands"]
            .as_array()
            .is_some_and(|brands| !brands.is_empty() && brands.iter().all(known_text))
        || !["logical_cpus", "total_memory_bytes"].iter().all(|field| {
            build["hardware"][field]
                .as_u64()
                .is_some_and(|value| value > 0)
        })
        || !["javascript", "lua"].iter().all(|runtime| {
            build[runtime]["features"]
                .as_array()
                .is_some_and(|features| !features.is_empty() && features.iter().all(known_text))
        })
    {
        return Err("missing or unknown build, runtime, engine or environment identity");
    }
    if os(row) != build["os"].as_str().unwrap_or("")
        || !["controlled", "replay", "native"].contains(&row["lane"].as_str().unwrap_or(""))
        || (row["lane"] != "controlled" && build["engine_enabled"] != true)
    {
        return Err("inconsistent OS, evidence lane or engine-enabled identity");
    }
    let qualification = &row["qualification"];
    if qualification["version"] != 1
        || !hex_identity(&qualification["configuration_sha256"], 64)
        || !hex_identity(&qualification["corpus_sha256"], 64)
    {
        return Err("missing immutable qualification configuration/corpus SHA-256 identity");
    }
    let identity = json!([build, qualification, os(row), row["lane"]]);
    Ok(format!("{:x}", Sha256::digest(identity.to_string())))
}

/// Missing execution remains visible; a catalog entry alone is never evidence.
pub fn coverage(rows: &[Value]) -> Result<Vec<Value>, Fault> {
    let mut matrix = Vec::new();
    let identities: Vec<_> = rows.iter().map(qualification_identity).collect();
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
                    let mut cohorts = BTreeMap::<&str, Vec<&Value>>::new();
                    let mut unqualified = Vec::new();
                    let mut failed = false;
                    let mut blocked = false;
                    let mut unexecuted = false;
                    for check in checks.into_iter().flatten() {
                        for (row, identity) in rows.iter().zip(&identities).filter(|(row, _)| {
                            row["id"] == *check && os(row) == *system && row["lane"] == *lane
                        }) {
                            match row["status"].as_str() {
                                Some("PASS") => {}
                                Some("FAIL") => failed = true,
                                Some("BLOCKED") => blocked = true,
                                _ => unexecuted = true,
                            }
                            evidence.push(reference(row).clone());
                            match identity {
                                Ok(identity) => cohorts.entry(identity).or_default().push(row),
                                Err(reason) => unqualified.push(json!({
                                    "evidence":reference(row),"reason":reason
                                })),
                            }
                        }
                    }
                    let cohorts: Vec<_> = cohorts
                        .into_iter()
                        .map(|(identity, rows)| {
                            let missing: Vec<_> = checks
                                .into_iter()
                                .flatten()
                                .filter(|check| {
                                    !rows
                                        .iter()
                                        .any(|row| row["id"] == **check && row["status"] == "PASS")
                                })
                                .collect();
                            let status = if rows.iter().any(|row| row["status"] == "FAIL") {
                                "FAIL"
                            } else if rows.iter().any(|row| row["status"] == "BLOCKED") {
                                "BLOCKED"
                            } else if rows.iter().any(|row| row["status"] != "PASS") {
                                "UNEXECUTED"
                            } else if missing.is_empty()
                                && checks.is_some_and(|checks| !checks.is_empty())
                            {
                                "PASS"
                            } else if lane == "controlled" {
                                "UNEXECUTED"
                            } else {
                                "BLOCKED"
                            };
                            let evidence: Vec<_> = rows.iter().map(|row| reference(row)).collect();
                            json!({"identity":identity,"status":status,"missing_checks":missing,
                                "evidence":evidence})
                        })
                        .collect();
                    let complete = cohorts.iter().any(|cohort| cohort["status"] == "PASS");
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
                            "all named oracles passed within one compatible build/runtime/environment/configuration/corpus cohort; historical failures remain authoritative",
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
                            "no compatible, sufficiently identified qualification cohort establishes every named oracle on this OS",
                        )
                    };
                    matrix.push(json!({"scenario":requirement["id"],"candidate":candidate,"lane":lane,"os":system,
                        "status":status,"reason":reason,"oracle":requirement["oracle"],"checks":checks,
                        "evidence":evidence,"cohorts":cohorts,"unqualified_evidence":unqualified}));
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
        "coverage_counts":coverage_counts,"coverage":matrix,"budgets":budgets,
        "qualification":{"version":1,
            "required_row_metadata":["build (complete build/runtime/engine/OS/hardware identity)",
                "qualification.version = 1","qualification.configuration_sha256","qualification.corpus_sha256"],
            "identity":"exact build and immutable suite configuration/corpus digests; profile/scenario plans may differ within that suite",
            "hardware":"CPU brands, logical CPU count and total physical memory; no hostnames, serial numbers or private paths",
            "artifact":"SHA-256 of the actual executable; cached bounded streaming read, unavailable on read/hash failure",
            "configuration":"SHA-256 of the complete check-to-configuration manifest, including prospective limits/budgets and applicable native engine/target configuration",
            "corpus":"SHA-256 of the complete immutable corpus manifest, including package variants/assets, observed effective compiler/parser and emitted-inventory identities, and applicable recorded frames/models",
            "trust":"producer-attested content identities, not authentication of imported evidence; absent/unknown metadata cannot contribute to PASS"},
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

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE_SCENARIO: &str =
        "runtime-comparison-workload/profiles-select-different-declared-actions";

    fn workflow_row(profile: &str) -> Value {
        json!({
            "id":format!("javascript-{profile}"),
            "run":format!("run-{profile}"),
            "candidate":"javascript","lane":"controlled","os":"macos",
            "scenario":"success","profile":profile,"status":"PASS","execution_status":"PASS",
            "plan_identity":format!("{:x}", Sha256::digest(profile)),
            "inventory_identity":"a".repeat(64),
            "build":{
                "tool_version":"0.1.0","os":"macos","arch":"aarch64",
                "rustc":"rustc 1.97.1","target":"aarch64-apple-darwin","profile":"debug",
                "source_sha256":"b".repeat(64),"cargo_lock_sha256":"c".repeat(64),
                "engine_revision":ENGINE_REVISION,"engine_enabled":false,
                "executable_sha256":"f".repeat(64),
                "os_version":"macOS 27.0","kernel_version":"27.0.0",
                "hardware":{"cpu_brands":["Apple M1 Pro"],"logical_cpus":10,
                    "total_memory_bytes":17179869184_u64},
                "javascript":{"crate":"rquickjs","version":"0.14.0","engine":"QuickJS-ng 0.16.2",
                    "features":["std","loader"],"allocator":"QuickJS default"},
                "lua":{"crate":"mlua","version":"0.12.1","engine":"Lua 5.4.9",
                    "abi":"Lua 5.4","features":["lua54","vendored","serde"]},
                "typescript":{"version":"5.9.3","role":"authoring into JavaScript VM"}
            },
            "qualification":{
                "version":1,"configuration_sha256":"d".repeat(64),"corpus_sha256":"e".repeat(64)
            }
        })
    }

    fn profile_coverage(rows: &[Value]) -> Value {
        coverage(rows)
            .unwrap()
            .into_iter()
            .find(|row| {
                row["scenario"] == PROFILE_SCENARIO
                    && row["candidate"] == "javascript"
                    && row["os"] == "macos"
                    && row["lane"] == "controlled"
            })
            .unwrap()
    }

    #[test]
    fn compatible_profiles_qualify_without_identical_plans() {
        let rows = vec![workflow_row("template-first"), workflow_row("ocr-first")];
        assert_ne!(rows[0]["plan_identity"], rows[1]["plan_identity"]);
        let covered = profile_coverage(&rows);
        assert_eq!(covered["status"], "PASS");
        assert_eq!(covered["cohorts"][0]["status"], "PASS");
        assert_eq!(covered["cohorts"][0]["missing_checks"], json!([]));
        assert_eq!(
            covered["evidence"],
            json!(["run-template-first", "run-ocr-first"])
        );
        let summary = summarize(&json!({"version":1,"rows":rows})).unwrap();
        assert_eq!(summary["decision"]["kind"], "Blocked");
    }

    #[test]
    fn incompatible_partial_cohorts_never_manufacture_coverage() {
        for (field, replacement) in [
            ("/build/source_sha256", json!("1".repeat(64))),
            ("/build/executable_sha256", json!("9".repeat(64))),
            ("/build/cargo_lock_sha256", json!("2".repeat(64))),
            ("/build/engine_revision", json!("3".repeat(40))),
            ("/build/engine_enabled", json!(true)),
            ("/build/profile", json!("release")),
            ("/build/javascript/engine", json!("QuickJS-ng different")),
            ("/build/lua/version", json!("0.12.2")),
            ("/build/typescript/version", json!("5.9.4")),
            ("/build/kernel_version", json!("27.1.0")),
            ("/build/hardware/cpu_brands", json!(["Apple M2 Pro"])),
            ("/build/hardware/logical_cpus", json!(12)),
            ("/build/hardware/total_memory_bytes", json!(34359738368_u64)),
            ("/qualification/configuration_sha256", json!("4".repeat(64))),
            ("/qualification/corpus_sha256", json!("5".repeat(64))),
        ] {
            let first = workflow_row("template-first");
            let mut second = workflow_row("ocr-first");
            *second.pointer_mut(field).unwrap() = replacement;
            assert_eq!(
                profile_coverage(std::slice::from_ref(&first))["status"],
                "UNEXECUTED"
            );
            assert_eq!(
                profile_coverage(std::slice::from_ref(&second))["status"],
                "UNEXECUTED"
            );
            let covered = profile_coverage(&[first, second]);
            assert_eq!(covered["status"], "UNEXECUTED", "{field}");
            let cohorts = covered["cohorts"].as_array().unwrap();
            assert_eq!(cohorts.len(), 2, "{field}");
            for cohort in cohorts {
                assert_eq!(cohort["status"], "UNEXECUTED", "{field}");
                assert!(matches!(
                    cohort["missing_checks"].as_array().unwrap().as_slice(),
                    [missing] if missing == "javascript-template-first" || missing == "javascript-ocr-first"
                ));
            }
        }
    }

    #[test]
    fn missing_or_unknown_identity_cannot_complete_a_cohort() {
        for (field, replacement) in [
            ("/build", Value::Null),
            ("/build/source_sha256", json!("unknown")),
            ("/build/engine_revision", json!("a")),
            ("/build/executable_sha256", Value::Null),
            ("/build/javascript/version", Value::Null),
            ("/build/typescript/version", json!("unknown")),
            ("/build/os_version", Value::Null),
            ("/build/os", json!("windows")),
            ("/build/hardware", Value::Null),
            ("/build/hardware/cpu_brands", json!([])),
            ("/build/hardware/cpu_brands", json!(["unknown"])),
            ("/build/hardware/logical_cpus", json!(0)),
            ("/build/hardware/total_memory_bytes", Value::Null),
            ("/qualification", Value::Null),
            ("/qualification/version", json!(2)),
            ("/qualification/configuration_sha256", json!("")),
            ("/qualification/corpus_sha256", json!("mutable/path")),
        ] {
            let mut second = workflow_row("ocr-first");
            *second.pointer_mut(field).unwrap() = replacement;
            let covered = profile_coverage(&[workflow_row("template-first"), second]);
            assert_eq!(covered["status"], "UNEXECUTED", "{field}");
            assert_eq!(
                covered["cohorts"][0]["missing_checks"],
                json!(["javascript-ocr-first"]),
                "{field}"
            );
            assert_eq!(
                covered["unqualified_evidence"][0]["evidence"], "run-ocr-first",
                "{field}"
            );
        }
    }

    #[test]
    fn complete_cohort_keeps_incompatible_partial_evidence_visible() {
        let mut partial = workflow_row("template-first");
        partial["run"] = json!("other-build");
        partial["build"]["source_sha256"] = json!("1".repeat(64));
        let covered = profile_coverage(&[
            workflow_row("template-first"),
            workflow_row("ocr-first"),
            partial,
        ]);
        assert_eq!(covered["status"], "PASS");
        let cohorts = covered["cohorts"].as_array().unwrap();
        assert!(cohorts.iter().any(|cohort| cohort["status"] == "PASS"));
        assert!(cohorts.iter().any(|cohort| {
            cohort["status"] == "UNEXECUTED"
                && cohort["missing_checks"] == json!(["javascript-ocr-first"])
                && cohort["evidence"] == json!(["other-build"])
        }));
    }

    #[test]
    fn effective_toolchain_changes_cannot_supply_complementary_profiles() {
        let mut baseline = vec![workflow_row("template-first"), workflow_row("ocr-first")];
        for row in &mut baseline {
            row["metrics"] = json!({
                "compiled_inventory_identity":"a".repeat(64),
                "runtime":{"source_diagnostic":{"import_parser":{"identity":{
                    "version":"5.9.3","node":"24.18.0",
                    "compiler_sha256":"1".repeat(64),"driver_sha256":"2".repeat(64),
                    "worker_sha256":"3".repeat(64),"sdk_sha256":"4".repeat(64),
                    "lock_sha256":"5".repeat(64)
                }}}}
            });
        }
        let inventories = BTreeMap::from([("javascript", "captured-fixture-identity")]);
        let qualification = controlled_qualification(&baseline, &inventories).unwrap();
        for row in &mut baseline {
            row["qualification"] = qualification.clone();
        }
        assert_eq!(profile_coverage(&baseline)["status"], "PASS");

        for (field, replacement) in [
            (
                "/metrics/compiled_inventory_identity",
                json!("6".repeat(64)),
            ),
            (
                "/metrics/runtime/source_diagnostic/import_parser/identity/node",
                json!("24.19.0"),
            ),
            (
                "/metrics/runtime/source_diagnostic/import_parser/identity/compiler_sha256",
                json!("7".repeat(64)),
            ),
            (
                "/metrics/runtime/source_diagnostic/import_parser/identity/driver_sha256",
                json!("8".repeat(64)),
            ),
            (
                "/metrics/runtime/source_diagnostic/import_parser/identity/worker_sha256",
                json!("9".repeat(64)),
            ),
            (
                "/metrics/runtime/source_diagnostic/import_parser/identity/sdk_sha256",
                json!("a".repeat(64)),
            ),
        ] {
            let mut changed = baseline.clone();
            for row in &mut changed {
                *row.pointer_mut(field).unwrap() = replacement.clone();
            }
            let qualification = controlled_qualification(&changed, &inventories).unwrap();
            for row in &mut changed {
                row["qualification"] = qualification.clone();
            }
            assert_eq!(profile_coverage(&changed)["status"], "PASS", "{field}");
            let mixed = [baseline[0].clone(), changed[1].clone()];
            assert_eq!(profile_coverage(&mixed)["status"], "UNEXECUTED", "{field}");
        }
    }

    #[test]
    fn historical_failures_survive_a_complete_passing_cohort() {
        for qualified in [true, false] {
            let mut failure = workflow_row("template-first");
            failure["run"] = json!("historical-failure");
            failure["status"] = json!("FAIL");
            failure["execution_status"] = json!("FAIL");
            failure["build"]["source_sha256"] = json!("1".repeat(64));
            if !qualified {
                failure["qualification"] = Value::Null;
            }
            let rows = vec![
                workflow_row("template-first"),
                workflow_row("ocr-first"),
                failure,
            ];
            let covered = profile_coverage(&rows);
            assert_eq!(covered["status"], "FAIL");
            assert!(
                covered["evidence"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("historical-failure"))
            );
            assert!(
                covered["cohorts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|cohort| cohort["status"] == "PASS")
            );
            let summary = summarize(&json!({"version":1,"rows":rows})).unwrap();
            assert_eq!(summary["counts"]["FAIL"], 1);
            assert_eq!(summary["decision"]["kind"], "Blocked");
        }
    }
}
