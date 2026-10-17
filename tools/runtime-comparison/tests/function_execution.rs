use mado_runtime_comparison::desktop::{
    DesktopController, FunctionInvocation, NativeProgress, NativeTarget, StartPreparation,
    StartRequest,
};
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Control, Fault, Plan, identity};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

fn inventory(source: &str) -> Inventory {
    let plan: Plan = serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
    let mut inventory = Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/typescript")),
        &plan.limits,
    )
    .unwrap();
    inventory.sources.insert("main.ts".into(),
        "export function readiness() { throw new Error('readiness invoked'); }\nexport function workflow() { throw new Error('workflow invoked'); }\nthrow new Error('unrelated module instantiated');".into());
    inventory
        .sources
        .insert("decisions.ts".into(), source.into());
    inventory.refresh_identity().unwrap();
    inventory
}

fn run(inventory: Inventory) -> Value {
    let executable = env!("CARGO_BIN_EXE_mado-runtime-comparison");
    let controller = DesktopController::new(executable.into(), "no-native-authority".into());
    let request = StartRequest {
        package_path: "captured-source-not-a-live-path".into(),
        inventory_identity: inventory.identity.clone(),
        package_id: inventory.package_id.clone(),
        schema_identity: identity(&inventory.schema).unwrap(),
        profile_id: "template-first".into(),
        values: inventory.profiles["template-first"]["options"].clone(),
        lane: "controlled".into(),
        scenario: "workflow".into(),
        replay_descriptor_path: None,
        native_intent: None,
        function: Some(FunctionInvocation {
            module: "decisions.ts".into(),
            function: "battle".into(),
            arguments: vec![json!(4)],
        }),
    };
    controller
        .start_with_preparation(
            request,
            move |_, _| {
                Ok(StartPreparation {
                    inventory: Some(inventory),
                    environment: None,
                    native: (),
                })
            },
            |_, _| {
                |_: &Control,
                 _: &dyn Fn(NativeProgress),
                 _: &dyn Fn() -> Result<(), Fault>|
                 -> Result<Option<NativeTarget>, Fault> {
                    panic!("controlled Function must not resolve a native target")
                }
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let view = controller.poll();
        if view.state == "terminal" {
            assert!(view.error.is_none(), "{:?}", view.error);
            return view.result.expect("retained result");
        }
        assert!(
            Instant::now() < deadline,
            "supervised function failed to settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn supervised_function_uses_captured_source_returns_summary_and_starts_fresh() {
    let captured = inventory(
        "let count = 0; async function battle(value: number) { count++; await Promise.resolve(); return count + value; }",
    );
    for _ in 0..2 {
        let result = run(captured.clone());
        assert_eq!(result["status"], "PASS", "{result}");
        assert_eq!(result["entry_outcome"], "Returned");
        assert_eq!(result["cleanup"]["clean"], true);
        assert_eq!(result["package_readiness"], "not_invoked");
        assert_eq!(result["function"]["module"], "decisions.ts");
        assert_eq!(
            result["metrics"]["runtime"]["return_summary"],
            json!({"kind":"number","value":5.0})
        );
        assert_eq!(result["recovery_count"], 0);
    }
}

#[test]
fn supervised_function_failure_maps_original_source_and_keeps_cleanup() {
    let result = run(inventory(
        "async function battle(value: number) {\n    await Promise.resolve(value);\n    throw new Error('battle failed');\n}",
    ));
    assert_eq!(result["status"], "FAIL", "{result}");
    assert_eq!(result["primary"]["category"], "Script");
    assert_eq!(result["cleanup"]["clean"], true);
    assert!(
        result["primary"]["context"]["typescript"]["frames"]
            .as_array()
            .unwrap()
            .iter()
            .any(|frame| frame["original"]["module"] == "decisions.ts"
                && frame["original"]["line"] == 3),
        "{result}"
    );
}
