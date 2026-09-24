use super::*;
use crate::host::test_support::{profile, schema};
use serde_json::json;

#[test]
fn profiles_replace_complete_top_level_values_without_nested_inheritance() {
    let schema = schema();
    let omitted = profile("template-first");
    let resolved = resolve_options(&schema, &omitted, "m0-workload").expect("whole default");
    assert_eq!(resolved["recognition"]["roi"]["width"], 640);
    let mut partial = omitted.clone();
    partial["options"]["recognition"] = json!({"threshold":0.8});
    let error = resolve_options(&schema, &partial, "m0-workload")
        .expect_err("partial object must not inherit ROI");
    assert_eq!(error.category, "Profile");
    assert!(error.message.contains("$.recognition.roi"));
    partial["options"]["recognition"] =
        json!({"threshold":0.8,"roi":{"x":0,"y":0,"width":320,"height":240}});
    partial["options"]["priorities"] = json!(["ocr"]);
    let replacement =
        resolve_options(&schema, &partial, "m0-workload").expect("complete replacement");
    assert_eq!(replacement["priorities"], json!(["ocr"]));
    assert_eq!(replacement["recognition"]["roi"]["width"], 320);
}

#[test]
fn profile_identity_unknown_fields_and_coercion_are_refused() {
    let schema = schema();
    let cases = [
        ("package_id", json!("another-package"), "ProfileIdentity"),
        ("schema_version", json!(2), "ProfileIdentity"),
        ("permissions", json!({"input":true}), "Profile"),
    ];
    for (field, value, category) in cases {
        let mut invalid = profile("template-first");
        invalid[field] = value;
        assert_eq!(
            resolve_options(&schema, &invalid, "m0-workload")
                .expect_err(field)
                .category,
            category
        );
    }
    let invalid_options = [
        json!({"priorities":["missing"]}),
        json!({"unknown":true}),
        json!({"recognition":{"threshold":"0.9","roi":{"x":0,"y":0,"width":640,"height":480}}}),
        json!({"recognition":{"threshold":2,"roi":{"x":0,"y":0,"width":640,"height":480}}}),
        json!({"actions":{"template":"A","ocr":"B","extra":"C"}}),
    ];
    for options in invalid_options {
        let invalid = json!({"package_id":"m0-workload","schema_version":1,"options":options});
        assert_eq!(
            resolve_options(&schema, &invalid, "m0-workload")
                .expect_err("invalid option")
                .category,
            "Profile"
        );
    }
    let mut required_schema = schema.clone();
    required_schema["properties"]["actions"]
        .as_object_mut()
        .expect("object")
        .remove("default");
    let mut missing = profile("template-first");
    missing["options"]
        .as_object_mut()
        .expect("options")
        .remove("actions");
    assert_eq!(
        resolve_options(&required_schema, &missing, "m0-workload")
            .expect_err("required missing")
            .category,
        "Profile"
    );
}

#[test]
fn all_defaults_and_unsupported_schema_features_are_validated() {
    let mut invalid = schema();
    invalid["properties"]["recognition"]["properties"]["threshold"]["default"] =
        json!("not-a-number");
    assert_eq!(
        resolve_options(&invalid, &profile("ocr-first"), "m0-workload")
            .expect_err("overridden invalid default")
            .category,
        "SchemaDefault"
    );
    let mut unsupported = schema();
    unsupported["properties"]["actions"]["patternProperties"] = json!({});
    assert_eq!(
        resolve_options(&unsupported, &profile("template-first"), "m0-workload")
            .expect_err("unknown schema keyword")
            .category,
        "Schema"
    );
}
