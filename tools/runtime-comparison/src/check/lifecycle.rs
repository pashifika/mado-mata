use super::{case, host_case, host_for, plan, replace_entry};
use crate::host::{Host, resolve_options};
use crate::inventory::Inventory;
use crate::model::{Control, Fault, Plan, RuntimeMetrics};
use crate::runner::run_once;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) fn run(
    rows: &mut Vec<Value>,
    inventories: &BTreeMap<&str, Inventory>,
) -> Result<(), Fault> {
    profile_cases(rows, &inventories["rust"])?;
    finite_wait_cases(rows, &inventories["rust"])?;
    identity_cases(rows, &inventories["rust"])?;
    held_cases(rows, &inventories["rust"])?;
    for candidate in ["javascript", "lua"] {
        let inventory = &inventories[candidate];
        invalid_profile_cases(rows, candidate, inventory)?;
        release_cases(rows, candidate, inventory)?;
        normal_return_case(rows, candidate, inventory)?;
        retention_case(rows, candidate, inventory)?;
        fresh_vm_case(rows, candidate, inventory)?;
        incomplete_return_case(rows, candidate, inventory)?;
        stop_cases(rows, candidate, inventory)?;
    }
    Ok(())
}

fn fault_is<T>(result: &Result<T, Fault>, category: &str) -> bool {
    result
        .as_ref()
        .is_err_and(|fault| fault.category == category)
}

fn no_owners(snapshot: &Value) -> bool {
    snapshot["live_handles"] == 0
        && snapshot["attempt_owners"] == 0
        && snapshot["in_flight_native"] == 0
        && snapshot["queue_depth"] == 0
        && snapshot["active_sequence"].is_null()
        && snapshot["held_keys"] == json!([])
}

fn new_host(inventory: &Inventory, scenario: Plan) -> Result<Host, Fault> {
    let profile = inventory
        .profiles
        .get(&scenario.profile)
        .ok_or_else(|| Fault::new("Fixture", "missing lifecycle profile"))?;
    let options = resolve_options(&inventory.schema, profile, &inventory.package_id)?;
    let control = Arc::new(Control::new(&scenario.limits));
    Host::new(scenario, options, inventory.assets.clone(), control)
}

fn run_vm(candidate: &str, inventory: &Inventory, host: &Host) -> Result<RuntimeMetrics, Fault> {
    let inventory = Arc::new(inventory.clone());
    match candidate {
        "javascript" => crate::javascript::run(inventory, host.clone()),
        "lua" => crate::lua::run(inventory, host.clone()),
        _ => Err(Fault::new("Fixture", "unsupported lifecycle VM candidate")),
    }
}

fn profile_cases(rows: &mut Vec<Value>, inventory: &Inventory) -> Result<(), Fault> {
    let mut omitted = inventory.clone();
    omitted.profiles.insert(
        "template-first".into(),
        json!({"package_id":inventory.package_id,"schema_version":1,"options":{}}),
    );
    omitted.refresh_identity()?;
    let host = host_for(&omitted, "success")?;
    let options = host.options();
    let partial_profile = json!({
        "package_id":inventory.package_id,"schema_version":1,
        "options":{"recognition":{"threshold":0.8}}
    });
    let partial = resolve_options(&inventory.schema, &partial_profile, &inventory.package_id);
    let passed = options["recognition"]
        == json!({"threshold":0.9,"roi":{"x":0,"y":0,"width":640,"height":480}})
        && fault_is(&partial, "Profile")
        && partial
            .as_ref()
            .is_err_and(|fault| fault.message.starts_with("$.recognition.roi:"));
    let index = rows.len();
    host_case(
        rows,
        "lifecycle-profile-omission-versus-replacement",
        &host,
        passed,
        "omitted recognition receives the whole default; a partial top-level replacement fails at $.recognition.roi without inheriting the missing field",
    );
    rows[index]["proof"] = json!({
        "omitted_profile":omitted.profiles["template-first"],"effective_options":options,
        "replacement_profile":partial_profile,"replacement_result":partial
    });
    Ok(())
}

fn invalid_profile_cases(
    rows: &mut Vec<Value>,
    candidate: &str,
    inventory: &Inventory,
) -> Result<(), Fault> {
    let sentinel = if candidate == "javascript" {
        "throw Error('profile guard reached package code'); export function readiness(){return 'Ready';} export function workflow(){host.call('observe',{});}"
    } else {
        "error('profile guard reached package code'); return {readiness=function() return 'Ready' end,workflow=function() host.call('observe',{}) end}"
    };
    let base = replace_entry(inventory, sentinel)?;
    for (name, profile, category, field) in [
        (
            "identity",
            json!({"package_id":"foreign-package","schema_version":1,"options":{}}),
            "ProfileIdentity",
            None,
        ),
        (
            "schema-version",
            json!({"package_id":inventory.package_id,"schema_version":2,"options":{}}),
            "ProfileIdentity",
            None,
        ),
        (
            "unknown-option",
            json!({"package_id":inventory.package_id,"schema_version":1,"options":{"unknown":true}}),
            "Profile",
            Some("unknown"),
        ),
        (
            "coercion",
            json!({"package_id":inventory.package_id,"schema_version":1,
                "options":{"recognition":{"threshold":"0.9","roi":{"x":0,"y":0,"width":640,"height":480}}}}),
            "Profile",
            Some("$.recognition.threshold:"),
        ),
        (
            "missing-field",
            json!({"package_id":inventory.package_id,"schema_version":1,
                "options":{"recognition":{"threshold":0.8}}}),
            "Profile",
            Some("$.recognition.roi:"),
        ),
        (
            "out-of-range",
            json!({"package_id":inventory.package_id,"schema_version":1,
                "options":{"recognition":{"threshold":1.1,"roi":{"x":0,"y":0,"width":640,"height":480}}}}),
            "Profile",
            Some("$.recognition.threshold:"),
        ),
    ] {
        let mut invalid = base.clone();
        invalid
            .profiles
            .insert("template-first".into(), profile.clone());
        // Invalid content must fail the real supervisor preflight, before a child
        // is started. Refreshing its identity would itself reject the profile.
        let result = run_once(
            &plan(candidate, "success", "template-first"),
            &invalid,
            None,
            false,
        );
        let passed = result.as_ref().is_err_and(|fault| {
            fault.category == category && field.is_none_or(|field| fault.message.contains(field))
        });
        rows.push(json!({
            "id":format!("{candidate}-lifecycle-invalid-profile-{name}"),
            "candidate":candidate,"lane":"controlled","os":std::env::consts::OS,
            "status":if passed {"PASS"} else {"FAIL"},
            "execution_status":result.as_ref().map_or("FAIL", |record| record.status.as_str()),
            "oracle":"run_once preflight returns the applicable profile identity or field error instead of entering package code or producing an execution record",
            "profile":profile,"result":result,"build":crate::report::build_identity()
        }));
    }
    Ok(())
}

fn finite_wait_cases(rows: &mut Vec<Value>, inventory: &Inventory) -> Result<(), Fault> {
    let host = host_for(inventory, "success")?;
    let observation = host.call("observe", json!({}))?;
    let query = host.call(
        "query",
        json!({"observation":observation,"kind":"ocr",
        "roi":{"x":0,"y":0,"width":640,"height":480},"expected":"READY"}),
    )?;
    let before = host.snapshot();
    let mut refusals = Vec::new();
    let mut passed = true;
    for (name, bound) in [
        ("omitted", None),
        ("zero", Some(json!(0))),
        ("negative", Some(json!(-1))),
        ("fractional", Some(json!(0.5))),
        ("string", Some(json!("100"))),
        ("null", Some(Value::Null)),
        ("above-limit", Some(json!(host.limits().wait_ms + 1))),
    ] {
        let mut delay = json!({});
        let mut wait = json!({"id":query["id"]});
        if let Some(bound) = bound {
            delay["duration_ms"] = bound.clone();
            wait["timeout_ms"] = bound;
        }
        let delay_result = host.call("wait", delay.clone());
        let query_result = host.call("query_wait", wait.clone());
        passed &= fault_is(&delay_result, "Argument") && fault_is(&query_result, "Argument");
        refusals.push(
            json!({"case":name,"delay_arguments":delay,"query_arguments":wait,
            "delay_result":delay_result,"query_result":query_result}),
        );
    }
    let after = host.snapshot();
    passed &= before["live_handles"] == after["live_handles"]
        && before["in_flight_native"] == after["in_flight_native"]
        && before["observations"] == after["observations"]
        && after["accepted"] == json!([]);
    let finite = host.call("query_wait", json!({"id":query["id"],"timeout_ms":100}));
    passed &= finite
        .as_ref()
        .is_ok_and(|result| result["text"] == "READY");
    let index = rows.len();
    host_case(
        rows,
        "lifecycle-finite-wait-arguments",
        &host,
        passed,
        "unbounded or invalid delay/query bounds return Argument without scheduling or consuming the query; a subsequent finite query still settles",
    );
    rows[index]["proof"] = json!({"before":before,"after_refusals":after,
        "refusals":refusals,"finite_result":finite});
    Ok(())
}

fn identity_cases(rows: &mut Vec<Value>, inventory: &Inventory) -> Result<(), Fault> {
    let old = host_for(inventory, "success")?;
    let observation = old.call("observe", json!({}))?;
    let recognition = old.call(
        "recognize",
        json!({"observation":observation,"kind":"ocr",
        "roi":{"x":0,"y":0,"width":640,"height":480}}),
    )?;
    let actions = json!([{"kind":"key_down","key":"A"},{"kind":"key_up","key":"A"}]);
    let sequence = old.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    )?;
    old.call("fixture", json!({"event":"target_exit"}))?;
    let old_receipt = old.call("settle", json!({"id":sequence["id"]}));
    let old_after_exit = old.call("observe", json!({}));
    let old_cleanup = old.finish();
    let old_snapshot = old.snapshot();

    let fresh = new_host(inventory, plan("rust", "success", "template-first"))?;
    let premature = fresh.begin_workflow();
    fresh.begin_readiness()?;
    let ready_observation = fresh.call("observe", json!({}))?;
    let during_readiness = fresh.call(
        "submit",
        json!({"observation":ready_observation,"actions":actions}),
    );
    fresh.begin_workflow()?;
    let fresh_query = fresh.call(
        "query",
        json!({"observation":ready_observation,"kind":"ocr",
        "roi":{"x":0,"y":0,"width":640,"height":480},"expected":"READY"}),
    )?;
    let before = fresh.snapshot();
    let stale_observation = fresh.call(
        "submit",
        json!({"observation":observation,"actions":actions}),
    );
    let stale_recognition = fresh.call(
        "submit",
        json!({"observation":recognition["observation"],"actions":actions}),
    );
    let stale_query = fresh.call(
        "query_wait",
        json!({"id":recognition["id"],"timeout_ms":100}),
    );
    let stale_sequence = fresh.call("settle", json!({"id":sequence["id"]}));
    let after = fresh.snapshot();
    let current_result = fresh.call(
        "query_wait",
        json!({"id":fresh_query["id"],"timeout_ms":100}),
    );
    let passed = old_receipt.as_ref().is_ok_and(|receipt| {
        receipt["status"] == "Refused"
            && receipt["reason"] == "TargetLost"
            && receipt["submitted"] == 0
    }) && fault_is(&old_after_exit, "TargetLost")
        && old_cleanup["clean"] == true
        && no_owners(&old_snapshot)
        && observation["attempt"] != ready_observation["attempt"]
        && observation["process_lifetime"] != ready_observation["process_lifetime"]
        && fault_is(&premature, "ReadinessContract")
        && fault_is(&during_readiness, "AdmissionClosed")
        && fault_is(&stale_observation, "StaleIdentity")
        && fault_is(&stale_recognition, "StaleIdentity")
        && fault_is(&stale_query, "InvalidHandle")
        && fault_is(&stale_sequence, "InvalidHandle")
        && [
            "accepted",
            "receipts",
            "effects",
            "live_handles",
            "attempt_owners",
            "observations",
            "recognitions",
            "queue_depth",
            "active_sequence",
        ]
        .iter()
        .all(|key| before[*key] == after[*key])
        && current_result.as_ref().is_ok_and(|result| {
            result["id"] == fresh_query["id"]
                && result["text"] == "READY"
                && result["observation"]["attempt"] == ready_observation["attempt"]
        });
    let index = rows.len();
    host_case(
        rows,
        "lifecycle-old-callback-identity",
        &fresh,
        passed,
        "old observation, recognition, and sequence identities are refused without changing the fresh attempt or completing its query; new readiness and query remain independent",
    );
    rows[index]["proof"] = json!({
        "identity_scope":"controlled host lifetime generations in one harness process; not OS PID reuse",
        "old_observation":observation,"old_recognition":recognition,"old_sequence":sequence,
        "old_receipt":old_receipt,"old_late_access":old_after_exit,
        "old_cleanup":old_cleanup,"old_observations":old_snapshot,
        "premature_workflow":premature,"readiness_input":during_readiness,
        "fresh_observation":ready_observation,"before_old_callbacks":before,"after_old_callbacks":after,
        "stale_observation":stale_observation,"stale_recognition":stale_recognition,
        "stale_query":stale_query,"stale_sequence":stale_sequence,"current_result":current_result
    });
    Ok(())
}

// A failed oracle must not leave the controlled worker alive in the check process.
// The original cleanup outcome is captured before this fixture-only final release.
struct HeldHost(Host);

impl Drop for HeldHost {
    fn drop(&mut self) {
        let _ = self.0.call("fixture", json!({"event":"release_hold"}));
        self.0.finish();
    }
}

fn held_cases(rows: &mut Vec<Value>, inventory: &Inventory) -> Result<(), Fault> {
    for cause in ["stop", "target-loss"] {
        let held = HeldHost(host_for(inventory, "success")?);
        let host = &held.0;
        let observation = host.call("observe", json!({}))?;
        let sequence = host.call(
            "submit",
            json!({"observation":observation,
            "actions":[{"kind":"key_down","key":"A"}]}),
        )?;
        let receipt = host.call("settle", json!({"id":sequence["id"]}))?;
        host.call("fixture", json!({"event":"hold"}))?;
        let query = host.call(
            "query",
            json!({"observation":observation,"kind":"ocr",
            "roi":{"x":0,"y":0,"width":640,"height":480},"expected":"READY"}),
        )?;
        let before = host.snapshot();
        if cause == "stop" {
            host.control().cancel();
        } else {
            host.call("fixture", json!({"event":"target_exit"}))?;
        }
        let category = if cause == "stop" {
            "Cancelled"
        } else {
            "TargetLost"
        };
        let cancelled = host.call("query_wait", json!({"id":query["id"],"timeout_ms":100}));
        let after_logical_end = host.snapshot();
        let released = host.call("fixture", json!({"event":"release_hold"}));
        let released_at = Instant::now();
        while host.snapshot()["in_flight_native"] != 0
            && released_at.elapsed() < Duration::from_millis(host.limits().cleanup_ms)
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        let after_physical_end = host.snapshot();
        let completed_result = host.call("query_wait", json!({"id":query["id"],"timeout_ms":100}));
        let cleanup = host.finish();
        let late = host.call("query_wait", json!({"id":query["id"],"timeout_ms":100}));
        let forbidden_input = host.call(
            "submit",
            json!({"observation":observation,
            "actions":[{"kind":"key_down","key":"B"}]}),
        );
        let snapshot = host.snapshot();
        let passed = receipt["status"] == "Submitted"
            && receipt["submitted"] == 1
            && before["in_flight_native"] == 1
            && before["held_keys"] == json!(["A"])
            && fault_is(&cancelled, category)
            && after_logical_end["in_flight_native"] == 1
            && released
                .as_ref()
                .is_ok_and(|value| value["applied"] == true)
            && after_physical_end["in_flight_native"] == 0
            && fault_is(&completed_result, category)
            && cleanup["clean"] == true
            && cleanup["status"] == "CleanupFinished"
            && no_owners(&snapshot)
            && snapshot["effects"] == json!([{"order":1,"kind":"key_down","key":"A"}])
            && cleanup["release_outcomes"]
                == json!([{"key":"A","order":1,"released":true,"sink":"controlled-non-native"}])
            && fault_is(
                &late,
                if cause == "stop" {
                    "Cancelled"
                } else {
                    "AdmissionClosed"
                },
            )
            && forbidden_input.is_err();
        rows.push(json!({
            "id":format!("lifecycle-held-{cause}-release"),"candidate":"rust","lane":"controlled",
            "os":std::env::consts::OS,"status":if passed {"PASS"} else {"FAIL"},
            "oracle":"logical cancellation or target loss preserves the physical owner until explicit hold release; late results cannot resume work and clean cleanup requires owner settlement plus held-input release",
            "receipt":receipt,"before":before,"logical_result":cancelled,
            "after_logical_end":after_logical_end,"hold_release":released,
            "after_physical_end":after_physical_end,"completed_result":completed_result,"late_result":late,
            "late_input":forbidden_input,"cleanup":cleanup,"observations":snapshot,
            "build":crate::report::build_identity()
        }));
    }
    Ok(())
}

fn release_cases(
    rows: &mut Vec<Value>,
    candidate: &str,
    inventory: &Inventory,
) -> Result<(), Fault> {
    for explicit in [false, true] {
        let source = if candidate == "javascript" {
            format!(
                r#"
export function readiness() {{ return 'Ready'; }}
export function workflow() {{
    const observation = host.call('observe', {{}});
    const query = host.call('recognize', {{observation,kind:'ocr',roi:{{x:0,y:0,width:640,height:480}}}});
    const sequence = host.call('submit', {{observation,actions:[{{kind:'key_down',key:'A'}}]}});
    const receipt = host.call('settle', {{id:sequence.id}});
    if (receipt.status !== 'Submitted') throw Error('input did not dispatch');
    if ({explicit}) {{
        for (const value of [observation,query,sequence]) host.call('release', {{id:value.id}});
    }}
    host.call('log', {{message:'entry-return'}});
}}
"#
            )
        } else {
            format!(
                r#"
return {{
    readiness=function() return 'Ready' end,
    workflow=function()
        local observation=host.call('observe',{{}})
        local query=host.call('recognize',{{observation=observation,kind='ocr',roi={{x=0,y=0,width=640,height=480}}}})
        local sequence=host.call('submit',{{observation=observation,actions={{{{kind='key_down',key='A'}}}}}})
        local receipt=host.call('settle',{{id=sequence.id}})
        if receipt.status~='Submitted' then error('input did not dispatch') end
        if {explicit} then
            for _,value in ipairs({{observation,query,sequence}}) do host.call('release',{{id=value.id}}) end
        end
        host.call('log',{{message='entry-return'}})
    end
}}
"#
            )
        };
        let inventory = replace_entry(inventory, &source)?;
        let host = new_host(&inventory, plan(candidate, "success", "template-first"))?;
        let entry = run_vm(candidate, &inventory, &host);
        let before = host.snapshot();
        let cleanup = host.finish();
        let late_observe = host.call("observe", json!({}));
        let late_receipt = host.call("settle", json!({"id":before["accepted"][0]["id"]}));
        let after = host.snapshot();
        let passed = entry.is_ok()
            && before["logs"] == json!(["entry-return"])
            && before["live_handles"] == (if explicit { 0 } else { 3 })
            && before["attempt_owners"] == (if explicit { 1 } else { 3 })
            && before["held_keys"] == json!(["A"])
            && before["receipts"][0]["status"] == "Submitted"
            && cleanup["clean"] == true
            && no_owners(&after)
            && cleanup["release_outcomes"]
                == json!([{"key":"A","order":1,"released":true,"sink":"controlled-non-native"}])
            && fault_is(&late_observe, "AdmissionClosed")
            && fault_is(&late_receipt, "InvalidHandle");
        rows.push(json!({
            "id":format!("{candidate}-lifecycle-{}-release",if explicit {"explicit"} else {"omitted"}),
            "candidate":candidate,"lane":"controlled","os":std::env::consts::OS,
            "status":if passed {"PASS"} else {"FAIL"},
            "execution_status":if entry.is_ok() && cleanup["clean"] == true {"PASS"} else {"FAIL"},
            "oracle":"actual VM entry retains or explicitly releases observation/query/receipt handles; host cleanup releases remaining owners and held input, and rejects late observation and receipt access",
            "entry_result":entry,"before_cleanup":before,"late_observation":late_observe,
            "late_receipt":late_receipt,"cleanup":cleanup,"observations":after,
            "inventory_identity":inventory.identity,"build":crate::report::build_identity()
        }));
    }
    Ok(())
}

fn retention_case(
    rows: &mut Vec<Value>,
    candidate: &str,
    inventory: &Inventory,
) -> Result<(), Fault> {
    let source = if candidate == "javascript" {
        "export function readiness(){return 'Ready';} export function workflow(){host.state.retained=[];for(let i=0;i<5;i++)host.state.retained.push(host.call('observe',{}));host.call('log',{message:'overflow was admitted'});}"
    } else {
        "return {readiness=function() return 'Ready' end,workflow=function() host.state.retained={} for i=1,5 do host.state.retained[i]=host.call('observe',{}) end host.call('log',{message='overflow was admitted'}) end}"
    };
    let inventory = replace_entry(inventory, source)?;
    let mut scenario = plan(candidate, "success", "template-first");
    scenario.limits.handles = 4;
    let host = new_host(&inventory, scenario)?;
    let entry = run_vm(candidate, &inventory, &host);
    let retained = host.snapshot();
    let cleanup = host.finish();
    let after = host.snapshot();
    let passed = fault_is(&entry, "HandleLimit")
        && retained["failure"]["category"] == "HandleLimit"
        && retained["live_handles"] == 4
        && retained["attempt_owners"] == 4
        && retained["in_flight_native"] == 0
        && retained["operation_metrics"]["observe"]["count"] == 6
        && retained["operation_metrics"]["observe"]["failures"] == 1
        && retained["logs"] == json!([])
        && retained["accepted"] == json!([])
        && cleanup["clean"] == true
        && no_owners(&after);
    rows.push(json!({
        "id":format!("{candidate}-lifecycle-retention-bound"),"candidate":candidate,
        "lane":"controlled","os":std::env::consts::OS,"status":if passed {"PASS"} else {"FAIL"},
        "execution_status":if entry.is_err() {"FAIL"} else {"PASS"},
        "oracle":"the fifth retained observation fails HandleLimit at capacity four; the failing VM result and live observation owners are retained before cleanup, never replaced by a fresh process or RSS claim",
        "owner_category":"observation","declared_handle_bound":4,"entry_result":entry,
        "before_cleanup":retained,"cleanup":cleanup,"observations":after,
        "inventory_identity":inventory.identity,"build":crate::report::build_identity()
    }));
    Ok(())
}

fn fresh_vm_case(
    rows: &mut Vec<Value>,
    candidate: &str,
    inventory: &Inventory,
) -> Result<(), Fault> {
    let source = if candidate == "javascript" {
        r#"
let entered = false;
export function readiness() {
    if (entered || host.state.ready !== undefined) throw Error('attempt state survived');
    entered = true;
    host.state.ready = true;
    host.call('log',{message:'fresh Ready'});
    return 'Ready';
}
export function workflow() {
    if (!entered || host.state.ready !== true) throw Error('workflow preceded readiness');
    host.state.ready = false;
    const observation=host.call('observe',{});
    const sequence=host.call('submit',{observation,actions:[{kind:'key_down',key:'A'},{kind:'key_up',key:'A'}]});
    host.call('settle',{id:sequence.id});
    host.call('log',{message:'fresh workflow'});
    if (host.call('fixture',{event:'target_exit'}).applied !== true) throw Error('target exit not applied');
}
"#
    } else {
        r#"
local entered=false
return {
    readiness=function()
        if entered or host.state.ready~=nil then error('attempt state survived') end
        entered=true
        host.state.ready=true
        host.call('log',{message='fresh Ready'})
        return 'Ready'
    end,
    workflow=function()
        if not entered or host.state.ready~=true then error('workflow preceded readiness') end
        host.state.ready=false
        local observation=host.call('observe',{})
        local sequence=host.call('submit',{observation=observation,actions={{kind='key_down',key='A'},{kind='key_up',key='A'}}})
        host.call('settle',{id=sequence.id})
        host.call('log',{message='fresh workflow'})
        if host.call('fixture',{event='target_exit'}).applied~=true then error('target exit not applied') end
    end
}
"#
    };
    let inventory = replace_entry(inventory, source)?;
    let identity = inventory.identity.clone();
    let old = new_host(&inventory, plan(candidate, "success", "template-first"))?;
    let old_entry = run_vm(candidate, &inventory, &old);
    let old_late = old.call("observe", json!({}));
    let old_cleanup = old.finish();
    let old_snapshot = old.snapshot();
    let fresh = new_host(&inventory, plan(candidate, "success", "template-first"))?;
    let fresh_entry = run_vm(candidate, &inventory, &fresh);
    let fresh_cleanup = fresh.finish();
    let fresh_snapshot = fresh.snapshot();
    let logs = json!(["fresh Ready", "fresh workflow"]);
    let identity_changed = old_snapshot["accepted"][0]["id"].is_string()
        && fresh_snapshot["accepted"][0]["id"].is_string()
        && old_snapshot["accepted"][0]["id"] != fresh_snapshot["accepted"][0]["id"];
    let passed = old_entry.is_ok()
        && fresh_entry.is_ok()
        && old_snapshot["operation_metrics"]["fixture"]["count"] == 1
        && old_snapshot["operation_metrics"]["fixture"]["failures"] == 0
        && fresh_snapshot["operation_metrics"]["fixture"]["count"] == 1
        && fresh_snapshot["operation_metrics"]["fixture"]["failures"] == 0
        && (fault_is(&old_late, "TargetLost") || fault_is(&old_late, "AdmissionClosed"))
        && identity_changed
        && old_snapshot["logs"] == logs
        && fresh_snapshot["logs"] == logs
        && old_snapshot["receipts"][0]["status"] == "Submitted"
        && fresh_snapshot["receipts"][0]["status"] == "Submitted"
        && old_snapshot["effects"] == fresh_snapshot["effects"]
        && old_cleanup["clean"] == true
        && fresh_cleanup["clean"] == true
        && no_owners(&old_snapshot)
        && no_owners(&fresh_snapshot)
        && inventory.identity == identity
        && inventory.validate().is_ok();
    rows.push(json!({
        "id":format!("{candidate}-lifecycle-target-exit-fresh-vm"),"candidate":candidate,
        "lane":"controlled","os":std::env::consts::OS,"status":if passed {"PASS"} else {"FAIL"},
        "oracle":"after controlled target exit, an explicitly constructed new VM uses the same immutable inventory, fresh module/host state, and literal Ready before its workflow; this is not an automatic recovery scheduler",
        "inventory_identity":identity,"old_entry_result":old_entry,
        "old_late_access":old_late,"old_cleanup":old_cleanup,
        "old_observations":old_snapshot,"fresh_entry_result":fresh_entry,
        "cleanup":fresh_cleanup,"observations":fresh_snapshot,
        "build":crate::report::build_identity()
    }));
    Ok(())
}

fn incomplete_return_case(
    rows: &mut Vec<Value>,
    candidate: &str,
    inventory: &Inventory,
) -> Result<(), Fault> {
    let source = if candidate == "javascript" {
        "export function readiness(){return 'Ready';} export function workflow(){const observation=host.call('observe',{});host.call('query',{observation,kind:'ocr',roi:{x:0,y:0,width:640,height:480},expected:'READY'});host.call('log',{message:'entry-return'});}"
    } else {
        "return {readiness=function() return 'Ready' end,workflow=function() local observation=host.call('observe',{});host.call('query',{observation=observation,kind='ocr',roi={x=0,y=0,width=640,height=480},expected='READY'});host.call('log',{message='entry-return'}) end}"
    };
    let inventory = replace_entry(inventory, source)?;
    let held = HeldHost(new_host(
        &inventory,
        plan(candidate, "held-work", "template-first"),
    )?);
    let host = &held.0;
    let entry = run_vm(candidate, &inventory, host);
    let before = host.snapshot();
    let incomplete = host.finish();
    let retained = host.snapshot();
    let late = host.call("observe", json!({}));
    let released = host.call("fixture", json!({"event":"release_hold"}));
    let harness_cleanup = host.finish();
    let after = host.snapshot();
    let passed = entry.is_ok()
        && before["logs"] == json!(["entry-return"])
        && before["in_flight_native"] == 1
        && incomplete["clean"] == false
        && incomplete["status"] == "IncompleteCleanup"
        && incomplete["cleanup_finished_us"].is_null()
        && incomplete["remaining"]["in_flight_native"] == 1
        && retained["in_flight_native"] == 1
        && retained["live_handles"] == 0
        && fault_is(&late, "AdmissionClosed")
        && released.is_ok()
        && harness_cleanup["clean"] == true
        && no_owners(&after);
    rows.push(json!({
        "id":format!("{candidate}-lifecycle-return-incomplete-cleanup"),"candidate":candidate,
        "lane":"controlled","os":std::env::consts::OS,"status":if passed {"PASS"} else {"FAIL"},
        "execution_status":if entry.is_ok() && incomplete["clean"] == true {"PASS"} else {"FAIL"},
        "oracle":"successful actual VM entry return is retained separately from IncompleteCleanup with a live physical owner; later fixture teardown does not rewrite the original terminal cleanup outcome",
        "entry_result":entry,"before_cleanup":before,"cleanup":incomplete,"observations":retained,
        "late_access":late,"fixture_hold_release":released,"fixture_cleanup":harness_cleanup,
        "after_fixture_cleanup":after,"inventory_identity":inventory.identity,
        "build":crate::report::build_identity()
    }));
    Ok(())
}

fn stop_cases(rows: &mut Vec<Value>, candidate: &str, inventory: &Inventory) -> Result<(), Fault> {
    for kind in ["delay", "query", "detached"] {
        let source = match (candidate, kind) {
            ("javascript", "delay") => {
                r#"
export function readiness(){return 'Ready';}
export function workflow(){
    host.call('log',{message:'wait-enter'});
    host.call('wait',{duration_ms:1000});
    host.call('log',{message:'forbidden-continuation'});
    host.call('observe',{});
}
"#
            }
            ("lua", "delay") => {
                r#"
return {readiness=function() return 'Ready' end,workflow=function()
    host.call('log',{message='wait-enter'})
    host.call('wait',{duration_ms=1000})
    host.call('log',{message='forbidden-continuation'})
    host.call('observe',{})
end}
"#
            }
            ("javascript", "query") => {
                r#"
export function readiness(){return 'Ready';}
export function workflow(){
    const observation=host.call('observe',{});
    const query=host.call('query',{observation,kind:'ocr',roi:{x:0,y:0,width:640,height:480},expected:'MISSING'});
    host.call('log',{message:'wait-enter'});
    host.call('query_wait',{id:query.id,timeout_ms:1000});
    host.call('log',{message:'forbidden-continuation'});
    host.call('submit',{observation,actions:[{kind:'key_down',key:'B'}]});
}
"#
            }
            ("lua", "query") => {
                r#"
return {readiness=function() return 'Ready' end,workflow=function()
    local observation=host.call('observe',{})
    local query=host.call('query',{observation=observation,kind='ocr',roi={x=0,y=0,width=640,height=480},expected='MISSING'})
    host.call('log',{message='wait-enter'})
    host.call('query_wait',{id=query.id,timeout_ms=1000})
    host.call('log',{message='forbidden-continuation'})
    host.call('submit',{observation=observation,actions={{kind='key_down',key='B'}}})
end}
"#
            }
            ("javascript", "detached") => {
                r#"
export function readiness(){return 'Ready';}
export async function workflow(){
    const observation=host.call('observe',{});
    const sequence=host.call('submit',{observation,actions:[{kind:'key_down',key:'A'}]});
    host.call('settle',{id:sequence.id});
    queueMicrotask(()=>{
        host.call('log',{message:'detached-enter'});
        host.call('wait',{duration_ms:1000});
        host.call('submit',{observation,actions:[{kind:'key_down',key:'B'}]});
    });
    queueMicrotask(()=>host.call('submit',{observation,actions:[{kind:'key_down',key:'C'}]}));
    await new Promise(()=>{});
}
"#
            }
            ("lua", "detached") => {
                r#"
return {readiness=function() return 'Ready' end,workflow=function()
    local observation=host.call('observe',{})
    local sequence=host.call('submit',{observation=observation,actions={{kind='key_down',key='A'}}})
    host.call('settle',{id=sequence.id})
    local pending=coroutine.create(function()
        coroutine.yield()
        host.call('submit',{observation=observation,actions={{kind='key_down',key='C'}}})
    end)
    coroutine.resume(pending)
    local active=coroutine.create(function()
        host.call('log',{message='detached-enter'})
        host.call('wait',{duration_ms=1000})
        host.call('submit',{observation=observation,actions={{kind='key_down',key='B'}}})
    end)
    coroutine.resume(active)
    coroutine.resume(pending)
end}
"#
            }
            _ => return Err(Fault::new("Fixture", "unknown lifecycle scheduling case")),
        };
        let inventory = replace_entry(inventory, source)?;
        let scenario = plan(
            candidate,
            if kind == "query" {
                "query-absent"
            } else {
                "success"
            },
            "template-first",
        );
        let record = run_once(&scenario, &inventory, Some(250), false)?;
        let operation = if kind == "query" {
            "query_wait"
        } else {
            "wait"
        };
        let observations = &record.observations;
        let mut passed = record.status == "FAIL"
            && record
                .primary
                .as_ref()
                .is_some_and(|fault| fault.category == "Cancelled")
            && record.exit_code == Some(0)
            && !record.forced
            && record.cleanup["clean"] == true
            && no_owners(observations)
            && observations["operation_metrics"][operation]["count"] == 1
            && observations["operation_metrics"][operation]["failures"] == 1
            && observations["operation_metrics"][operation]["total_us"]
                .as_u64()
                .is_some_and(|elapsed| {
                    elapsed >= 1_000 && elapsed < scenario.limits.wait_ms * 1000
                })
            && record
                .milestones
                .iter()
                .any(|event| event["event"] == "StopRequested" && event["reason"] == "Stop")
            && record
                .milestones
                .iter()
                .any(|event| event["event"] == "AdmissionClosed")
            && record.metrics["containment_us"]
                .as_u64()
                .is_some_and(|elapsed| elapsed < scenario.limits.containment_ms * 1000);
        if kind == "detached" {
            passed &= observations["logs"] == json!(["detached-enter"])
                && observations["effects"] == json!([{"order":1,"kind":"key_down","key":"A"}])
                && observations["accepted"]
                    .as_array()
                    .is_some_and(|values| values.len() == 1)
                && observations["receipts"][0]["status"] == "Submitted"
                && record.cleanup["release_outcomes"]
                    == json!([{"key":"A","order":1,"released":true,"sink":"controlled-non-native"}]);
        } else {
            passed &= observations["logs"] == json!(["wait-enter"])
                && observations["accepted"] == json!([])
                && observations["effects"] == json!([]);
        }
        case(
            rows,
            &format!("{candidate}-lifecycle-stop-{kind}"),
            record,
            passed,
            "actual VM delay/query or scheduled callback is interrupted by external Stop; no continuation input is admitted, dispatched work remains distinct, and bounded clean cleanup reaps the child",
        );
    }
    Ok(())
}

fn normal_return_case(
    rows: &mut Vec<Value>,
    candidate: &str,
    inventory: &Inventory,
) -> Result<(), Fault> {
    let source = if candidate == "javascript" {
        r#"
export function readiness(){return 'Ready';}
export function workflow(){
    const observation=host.call('observe',{});
    const query=host.call('query',{observation,kind:'ocr',roi:{x:0,y:0,width:640,height:480},expected:'READY'});
    host.call('log',{message:JSON.stringify(observation)});
    host.call('log',{message:query.id});
}
"#
    } else {
        r#"
return {readiness=function() return 'Ready' end,workflow=function()
    local observation=host.call('observe',{})
    local query=host.call('query',{observation=observation,kind='ocr',roi={x=0,y=0,width=640,height=480},expected='READY'})
    local encoded=string.format('{"id":%q,"run":%q,"attempt":%d,"process_lifetime":%q,"session":%d,"geometry":%d,"frame":%d,"width":%d,"height":%d,"coordinate_space":%q}',
        observation.id,observation.run,observation.attempt,observation.process_lifetime,observation.session,
        observation.geometry,observation.frame,observation.width,observation.height,observation.coordinate_space)
    host.call('log',{message=encoded})
    host.call('log',{message=query.id})
end}
"#
    };
    let inventory = replace_entry(inventory, source)?;
    let host = new_host(&inventory, plan(candidate, "success", "template-first"))?;
    let entry = run_vm(candidate, &inventory, &host);
    let before = host.snapshot();
    let observation = before["logs"][0]
        .as_str()
        .and_then(|encoded| serde_json::from_str::<Value>(encoded).ok())
        .unwrap_or(Value::Null);
    let query = before["logs"][1].clone();
    let admission = host
        .control()
        .admission
        .load(std::sync::atomic::Ordering::Acquire);
    // Exercise the still-live host immediately after normal VM return. Calling
    // finish first would conceal an admission gap at the entry boundary.
    let late_observe = host.call("observe", json!({}));
    let late_query = host.call(
        "query",
        json!({"observation":observation,"kind":"ocr",
        "roi":{"x":0,"y":0,"width":640,"height":480},"expected":"READY"}),
    );
    let late_query_wait = host.call("query_wait", json!({"id":query,"timeout_ms":100}));
    let late_wait = host.call("wait", json!({"duration_ms":1}));
    let after_late_work = host.snapshot();
    let observation_release = host.call("release", json!({"id":observation["id"]}));
    let query_release = host.call("release", json!({"id":query}));
    let mut unexpected_releases = Vec::new();
    for result in [&late_observe, &late_query] {
        if let Ok(value) = result {
            if value["id"].is_string() {
                unexpected_releases.push(host.call("release", json!({"id":value["id"]})));
            }
        }
    }
    let cleanup = host.finish();
    let after = host.snapshot();
    let passed = entry.is_ok()
        && observation.is_object()
        && query.is_string()
        && !admission
        && before["live_handles"] == 2
        && fault_is(&late_observe, "AdmissionClosed")
        && fault_is(&late_query, "AdmissionClosed")
        && fault_is(&late_query_wait, "AdmissionClosed")
        && fault_is(&late_wait, "AdmissionClosed")
        && observation_release
            .as_ref()
            .is_ok_and(|value| value["released"] == true)
        && query_release
            .as_ref()
            .is_ok_and(|value| value["released"] == true)
        && after_late_work["live_handles"] == before["live_handles"]
        && after_late_work["observations"] == before["observations"]
        && after_late_work["recognitions"] == before["recognitions"]
        && cleanup["clean"] == true
        && no_owners(&after);
    rows.push(json!({
        "id":format!("{candidate}-lifecycle-return-closes-host-work"),"candidate":candidate,
        "lane":"controlled","os":std::env::consts::OS,"status":if passed {"PASS"} else {"FAIL"},
        "execution_status":if entry.is_ok() && cleanup["clean"] == true {"PASS"} else {"FAIL"},
        "oracle":"normal VM entry return closes observe/query/query_wait/wait before finish while explicit release remains available; any unexpected admitted work fails the oracle and is cleaned up",
        "entry_result":entry,"before_late_work":before,"admission_after_return":admission,
        "late_observation":late_observe,"late_query":late_query,"late_query_wait":late_query_wait,
        "late_wait":late_wait,"after_late_work":after_late_work,
        "observation_release":observation_release,"query_release":query_release,
        "unexpected_handle_releases":unexpected_releases,"cleanup":cleanup,"observations":after,
        "inventory_identity":inventory.identity,"build":crate::report::build_identity()
    }));
    Ok(())
}
