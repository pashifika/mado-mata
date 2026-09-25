use super::*;

pub(super) fn schema() -> Value {
    serde_json::from_str(include_str!("../../fixtures/common/schema.json")).expect("fixture schema")
}

pub(super) fn profile(name: &str) -> Value {
    let source = match name {
        "template-first" => include_str!("../../fixtures/common/profiles/template-first.json"),
        "ocr-first" => include_str!("../../fixtures/common/profiles/ocr-first.json"),
        _ => panic!("unknown test profile"),
    };
    serde_json::from_str(source).expect("fixture profile")
}

pub(super) fn make_host(scenario: &str, selected_profile: &str) -> Host {
    let limits = Limits {
        duration_ms: 5_000,
        readiness_ms: 1_000,
        wait_ms: 500,
        cleanup_ms: 30,
        containment_ms: 1_000,
        queue_capacity: 2,
        handles: 32,
        log_records: 2,
        log_bytes: 64,
        vm_bytes: 1024 * 1024,
        max_actions: 32,
        snapshot_files: 32,
        snapshot_bytes: 64 * 1024,
    };
    let control = Arc::new(Control::new(&limits));
    let plan = Plan {
        version: 1,
        id: "host-regression".into(),
        candidate: "rust".into(),
        lane: "controlled".into(),
        scenario: scenario.into(),
        profile: selected_profile.into(),
        limits,
        samples: 1,
        warmups: 0,
        repetitions: 1,
        budgets: BTreeMap::new(),
        native_config: None,
    };
    let options = resolve_options(&schema(), &profile(selected_profile), "m0-workload")
        .expect("resolved fixture");
    Host::new(
        plan,
        options,
        BTreeMap::from([("marker".into(), vec![255; 16])]),
        control,
    )
    .expect("controlled host")
}

pub(super) fn ready_host(scenario: &str) -> Host {
    let host = make_host(scenario, "template-first");
    host.begin_readiness().expect("readiness starts");
    host.begin_workflow()
        .expect("explicit Ready accepted by adapter");
    host
}

pub(super) fn observe(host: &Host) -> Value {
    host.call("observe", json!({})).expect("observation")
}

pub(super) fn submit(host: &Host, observation: &Value, key: &str) -> Result<Value, Fault> {
    host.call(
        "submit",
        json!({"observation":observation,"actions":[
            {"kind":"key_down","key":key},{"kind":"key_up","key":key}
        ]}),
    )
}

pub(super) fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "fixture handshake deadline expired"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
