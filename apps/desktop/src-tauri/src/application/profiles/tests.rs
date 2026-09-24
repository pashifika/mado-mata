use super::*;
use crate::application::test_support::*;
use std::fs;
use std::sync::Arc;

#[test]
fn unsafe_package_numbers_are_refused_before_ipc_export() {
    for (source, pointer, expected_path) in [
        (
            "schema.json",
            "/properties/amount/default",
            "$.schema.properties.amount.default",
        ),
        (
            "schema.json",
            "/properties/amount/maximum",
            "$.schema.properties.amount.maximum",
        ),
        (
            "profiles/template-first.json",
            "/options/amount",
            "$.presets[\"template-first\"].options.amount",
        ),
    ] {
        let fixture = Fixture::new();
        let package = fixture.numeric_package();
        let path = package.join(source);
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if source == "schema.json" {
            value["properties"]["amount"]["maximum"] = json!(u64::MAX);
        } else {
            value["options"]["amount"] = Value::Null;
        }
        *value.pointer_mut(pointer).unwrap() = json!(9_007_199_254_740_993_u64);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        let error = inspect_named(&fixture.application, "Main", &package)
            .err()
            .expect("unsafe source must be refused");
        assert_eq!(error.category, "NumericPrecision", "{source}");
        assert_eq!(error.context["path"], expected_path);
        assert_eq!(error.context["value"], "9007199254740993");
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn omitted_defaults_are_checked_without_replacing_explicit_values() {
    let fixture = Fixture::new();
    let path = fixture.numeric_package();
    inspect_named(&fixture.application, "Main", &path).unwrap();
    let mut selected = lock(&fixture.application.workspaces);
    let inventory = Arc::make_mut(&mut selected.open[0].selected.as_mut().unwrap().inventory);
    inventory.schema["properties"]["amount"]["default"] = json!(9_007_199_254_740_993_u64);
    let error = desktop_options(inventory, json!({})).unwrap_err();
    assert_eq!(error.category, "NumericPrecision");
    assert_eq!(error.context["path"], "$.amount");
    let explicit = desktop_options(inventory, json!({"amount":0.1})).unwrap();
    assert_eq!(explicit["amount"], json!(0.1));
}

#[test]
fn integer_precision_checks_do_not_saturate_or_conflate_values() {
    for text in [
        "9007199254740992",
        "9007199254740993",
        "-9007199254740993",
        "9223372036854775807",
        "18446744073709551615",
        "-9223372036854775808",
    ] {
        let value: Value = serde_json::from_str(text).unwrap();
        let error = check_webview_value(&json!({"items":[value]}), "$").unwrap_err();
        assert_eq!(error.context["path"], "$.items[0]");
        assert_eq!(error.context["value"], text);
    }
    let safe: Value =
        serde_json::from_str("[9007199254740991,-9007199254740991,0.1,1e18,1e100]").unwrap();
    check_webview_value(&safe, "$").unwrap();
    assert!(same_json_values(
        &json!({"items":[1.0]}),
        &json!({"items":[1]})
    ));
    assert!(!same_json_values(
        &json!(9_007_199_254_740_993_u64),
        &json!(9_007_199_254_740_992_u64)
    ));
    assert!(!same_json_values(
        &json!(9_007_199_254_740_993_u64),
        &json!(9_007_199_254_740_992_f64)
    ));
    assert!(!same_json_values(&json!(u64::MAX), &json!(u64::MAX as f64)));
}

#[test]
fn signed_zero_sources_are_refused_without_rewriting_profiles() {
    let fixture = Fixture::new();
    let application = &fixture.application;
    let path = fixture.numeric_package();
    let selection = inspect_named(application, "Main", &path).unwrap();
    let workspace = workspace_ref(&selection);
    let inventory = lock(&application.workspaces)
        .resolve(&workspace)
        .unwrap()
        .inventory
        .as_ref()
        .clone();
    let saved = lock(&application.store)
        .profile_store(&selection.internal_name, &inventory.package_id)
        .unwrap()
        .save(&inventory, None, "Signed zero", json!({"amount":-0.0}))
        .unwrap();
    let stored_path = profile_path(&fixture, "Main", &saved);
    let before = fs::read(&stored_path).unwrap();
    let reopened = application.profiles(&workspace).unwrap();
    assert!(reopened.profiles.is_empty());
    let error = reopened.profiles_error.unwrap();
    assert_eq!(
        error.context["rejected"][0]["context"]["profile_id"],
        saved.id
    );
    assert_eq!(
        error.context["rejected"][0]["context"]["cause"]["context"]["path"],
        "$.amount"
    );
    assert_eq!(
        error.context["rejected"][0]["context"]["cause"]["context"]["value"],
        "-0.0"
    );
    assert_eq!(
        application
            .rename_profile(&workspace, &saved.id, "Unsigned")
            .unwrap_err()
            .category,
        "ProfileRejected"
    );
    assert_eq!(
        application
            .validate(&workspace, json!({"amount":-0.0}))
            .unwrap_err()
            .category,
        "NumericPrecision",
    );
    let schema_path = path.join("schema.json");
    let mut schema = inventory.schema;
    schema["properties"]["amount"]["default"] = json!(-0.0);
    fs::write(&schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
    let source_before = fs::read(&schema_path).unwrap();
    let error = application
        .inspect(&path, &workspace)
        .err()
        .expect("signed-zero default must be refused");
    assert_eq!(error.category, "NumericPrecision");
    assert_eq!(error.context["path"], "$.schema.properties.amount.default");
    assert_eq!(error.context["value"], "-0.0");
    assert_eq!(fs::read(schema_path).unwrap(), source_before);
    assert_eq!(fs::read(stored_path).unwrap(), before);
}
