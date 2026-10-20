//! Common workload behavior on controlled input; no native target or input is used.
use mado_runtime_comparison::check::plan;
use mado_runtime_comparison::host::{Host, resolve_options, run_rust};
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Control, Fault};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, LazyLock};

static WORKLOADS: LazyLock<Vec<(&str, Arc<Inventory>)>> = LazyLock::new(|| {
    ["rust", "javascript", "typescript", "lua"]
        .into_iter()
        .map(|candidate| {
            let limits = plan(candidate, "success", "template-first").limits;
            let fixture = if candidate == "rust" {
                "javascript"
            } else {
                candidate
            };
            let captured = Inventory::capture(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("fixtures")
                    .join(fixture),
                &limits,
            )
            .unwrap();
            let inventory = if candidate == "typescript" {
                mado_runtime_comparison::typescript::compile(&captured, &limits).unwrap()
            } else {
                captured
            };
            (candidate, Arc::new(inventory))
        })
        .collect()
});

fn options(inventory: &Inventory, profile: &str) -> Value {
    resolve_options(
        &inventory.schema,
        &inventory.profiles[profile],
        &inventory.package_id,
    )
    .unwrap()
}

fn run(candidate: &str, inventory: &Arc<Inventory>, options: Value) -> (Result<(), Fault>, Value) {
    let plan = plan(candidate, "success", "template-first");
    let profile = json!({"package_id":inventory.package_id,"schema_version":1,"options":options});
    let options = resolve_options(&inventory.schema, &profile, &inventory.package_id).unwrap();
    let control = Arc::new(Control::new(&plan.limits));
    let host = Host::new(plan, options, inventory.assets.clone(), control).unwrap();
    let outcome = match candidate {
        "rust" => run_rust(&host),
        "javascript" | "typescript" => {
            mado_runtime_comparison::javascript::run(Arc::clone(inventory), host.clone())
        }
        "lua" => mado_runtime_comparison::lua::run(Arc::clone(inventory), host.clone()),
        _ => unreachable!(),
    };
    let facts = host.snapshot();
    assert_eq!(host.finish()["clean"], true, "{candidate}: {facts}");
    assert_eq!(host.snapshot()["attempt_owners"], 0, "{candidate}");
    (outcome.map(|_| ()), facts)
}

#[test]
fn common_workloads_keep_independent_priority_and_key_oracles() {
    for (candidate, inventory) in WORKLOADS.iter() {
        for (profile, key) in [("template-first", "A"), ("ocr-first", "D")] {
            let (outcome, facts) = run(candidate, inventory, options(inventory, profile));
            outcome.unwrap_or_else(|fault| panic!("{candidate}/{profile}: {fault:?}"));
            assert_eq!(
                facts["effects"],
                json!([
                    {"order":1,"kind":"key_down","key":key},
                    {"order":1,"kind":"key_up","key":key}
                ]),
                "{candidate}/{profile}"
            );
            assert_eq!(
                facts["postconditions"][0]["satisfied"], true,
                "{candidate}/{profile}"
            );
            assert_eq!(
                facts["operation_metrics"]["wait"]["count"], 0,
                "{candidate}/{profile}"
            );
            assert_eq!(facts["live_handles"], 0, "{candidate}/{profile}");
        }
    }
}

#[test]
fn common_workloads_keep_click_order_balanced_input_and_postcondition_crop() {
    for (candidate, inventory) in WORKLOADS.iter() {
        for (x, satisfied) in [(100, true), (0, false)] {
            let mut values = options(inventory, "template-first");
            values["actions"]["template"] = json!([
                {"kind":"key_down","key":"A"},
                {"kind":"click","x":10.5,"y":20.5,"button":"left"},
                {"kind":"click","x":30.5,"y":40.5,"button":"right"},
                {"kind":"key_up","key":"A"}
            ]);
            values["postcondition"]["roi"] = json!({"x":x,"y":80,"width":40,"height":20});
            values["postcondition"]["wait_ms"] = json!(1);
            let (outcome, facts) = run(candidate, inventory, values);
            outcome.unwrap_or_else(|fault| panic!("{candidate}/{x}: {fault:?}"));
            assert_eq!(
                facts["effects"],
                json!([
                    {"order":1,"kind":"key_down","key":"A"},
                    {"order":1,"kind":"click","x":10.5,"y":20.5,"button":"left"},
                    {"order":1,"kind":"click","x":30.5,"y":40.5,"button":"right"},
                    {"order":1,"kind":"key_up","key":"A"}
                ]),
                "{candidate}/{x}"
            );
            assert_eq!(
                facts["receipts"][0]["status"], "Submitted",
                "{candidate}/{x}"
            );
            assert_eq!(facts["receipts"][0]["submitted"], 4, "{candidate}/{x}");
            assert_eq!(facts["held_keys"], json!([]), "{candidate}/{x}");
            assert_eq!(facts["held_buttons"], json!([]), "{candidate}/{x}");
            let postcondition = &facts["postconditions"][0];
            assert_eq!(postcondition["satisfied"], satisfied, "{candidate}/{x}");
            assert!(
                postcondition["frame"].as_u64().unwrap()
                    > postcondition["checkpoint"].as_u64().unwrap()
            );
            assert_eq!(
                facts["operation_metrics"]["wait"]["count"], 1,
                "{candidate}/{x}"
            );
            assert_eq!(facts["live_handles"], 0, "{candidate}/{x}");
        }
    }
}

#[test]
fn common_workloads_refuse_invalid_later_clicks_before_any_input() {
    for (candidate, inventory) in WORKLOADS.iter() {
        let mut values = options(inventory, "template-first");
        values["actions"]["template"] = json!([
            {"kind":"click","x":10.5,"y":20.5,"button":"left"},
            {"kind":"click","x":640,"y":20,"button":"left"}
        ]);
        let (outcome, facts) = run(candidate, inventory, values);
        assert_eq!(outcome.unwrap_err().category, "Argument", "{candidate}");
        assert_eq!(facts["accepted"], json!([]), "{candidate}");
        assert_eq!(facts["effects"], json!([]), "{candidate}");
        assert_eq!(facts["postconditions"], json!([]), "{candidate}");
    }
}

#[test]
fn common_workloads_guard_ocr_text_without_weakening_scores_or_template_priority() {
    for (candidate, inventory) in WORKLOADS.iter() {
        for (expected, threshold, admitted) in [
            ("READY", 0.92, true),
            ("WRONG", 0.92, false),
            ("ready", 0.92, false),
            ("READY", 0.99, false),
        ] {
            let mut values = options(inventory, "ocr-first");
            values["priorities"] = json!(["ocr"]);
            values["recognition"]["expected"] = json!(expected);
            values["recognition"]["threshold"] = json!(threshold);
            let (outcome, facts) = run(candidate, inventory, values);
            outcome.unwrap_or_else(|fault| panic!("{candidate}/{expected}/{threshold}: {fault:?}"));
            let effects = if admitted {
                json!([{"order":1,"kind":"key_down","key":"D"},
                    {"order":1,"kind":"key_up","key":"D"}])
            } else {
                json!([])
            };
            assert_eq!(
                facts["effects"], effects,
                "{candidate}/{expected}/{threshold}"
            );
            assert_eq!(
                facts["live_handles"], 0,
                "{candidate}/{expected}/{threshold}"
            );
        }
        let mut values = options(inventory, "template-first");
        values["recognition"]["expected"] = json!("WRONG");
        let (outcome, facts) = run(candidate, inventory, values);
        outcome.unwrap();
        assert_eq!(
            facts["effects"][0]["key"], "A",
            "{candidate}: template remains eligible"
        );
    }
}

#[test]
fn common_workloads_use_the_selected_template_asset_without_a_marker_fallback() {
    for (candidate, inventory) in WORKLOADS.iter() {
        let mut values = options(inventory, "template-first");
        values["recognition"]["asset"] = json!("missing-selected-asset");
        let (outcome, facts) = run(candidate, inventory, values);
        assert_eq!(outcome.unwrap_err().category, "MissingAsset", "{candidate}");
        assert_eq!(facts["effects"], json!([]), "{candidate}");
        assert_eq!(facts["postconditions"], json!([]), "{candidate}");
    }
}
