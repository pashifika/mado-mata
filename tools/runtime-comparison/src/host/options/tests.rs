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
    assert_eq!(error.context["path"], "$.recognition.roi");
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
        (json!({"priorities":["missing"]}), "$.priorities[0]"),
        (json!({"unknown":true}), "$.unknown"),
        (
            json!({"recognition":{"threshold":"0.9","roi":{"x":0,"y":0,"width":640,"height":480}}}),
            "$.recognition.threshold",
        ),
        (
            json!({"recognition":{"threshold":2,"roi":{"x":0,"y":0,"width":640,"height":480}}}),
            "$.recognition.threshold",
        ),
        (
            json!({"actions":{"template":"A","ocr":"B","extra":"C"}}),
            "$.actions.extra",
        ),
        (json!({"priorities":[]}), "$.priorities"),
    ];
    for (options, path) in invalid_options {
        let invalid = json!({"package_id":"m0-workload","schema_version":1,"options":options});
        let error = resolve_options(&schema, &invalid, "m0-workload").expect_err("invalid option");
        assert_eq!(error.category, "Profile");
        assert_eq!(error.context["path"], path);
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
    let error =
        resolve_options(&required_schema, &missing, "m0-workload").expect_err("required missing");
    assert_eq!(error.category, "Profile");
    assert_eq!(error.context["path"], "$.actions");
}

#[test]
fn all_defaults_and_unsupported_schema_features_are_validated() {
    let mut invalid = schema();
    invalid["properties"]["recognition"]["properties"]["threshold"]["default"] =
        json!("not-a-number");
    let error = resolve_options(&invalid, &profile("ocr-first"), "m0-workload")
        .expect_err("overridden invalid default");
    assert_eq!(error.category, "SchemaDefault");
    assert_eq!(error.context["path"], "$.recognition.threshold");
    let mut unsupported = schema();
    unsupported["properties"]["actions"]["patternProperties"] = json!({});
    let error = resolve_options(&unsupported, &profile("template-first"), "m0-workload")
        .expect_err("unknown schema keyword");
    assert_eq!(error.category, "Schema");
    assert_eq!(error.context["path"], "$.actions");
}

#[test]
fn invalid_default_retains_the_missing_nested_value_path() {
    let mut schema = schema();
    let roi = &mut schema["properties"]["recognition"]["properties"]["roi"];
    roi["properties"]["label"] = json!({"type":"string","default":"new"});
    roi["required"].as_array_mut().unwrap().push(json!("label"));
    let error = resolve_options(&schema, &profile("template-first"), "m0-workload")
        .expect_err("nested defaults do not fill an existing object");
    assert_eq!(error.category, "SchemaDefault");
    assert_eq!(error.context["path"], "$.recognition.roi.label");
}

#[test]
fn structured_paths_distinguish_literal_names_from_nested_fields_and_indices() {
    let schema = json!({
        "version": 1, "type": "object", "required": [], "additionalProperties": false,
        "properties": {
            "a.b": {"type": "integer"},
            "a": {"type": "object", "required": [], "additionalProperties": false,
                  "properties": {"b": {"type": "integer"}}},
            "items[0]": {"type": "integer"},
            "items": {"type": "array", "items": {"type": "integer"}}
        }
    });
    let cases = [
        (json!({"a.b": "bad"}), "$[\"a.b\"]"),
        (json!({"a": {"b": "bad"}}), "$.a.b"),
        (json!({"items[0]": "bad"}), "$[\"items[0]\"]"),
        (json!({"items": ["bad"]}), "$.items[0]"),
        (json!({"unknown.key": 1}), "$[\"unknown.key\"]"),
    ];
    for (options, path) in cases {
        let profile = json!({"package_id": "paths", "schema_version": 1, "options": options});
        let error = resolve_options(&schema, &profile, "paths").unwrap_err();
        assert_eq!(error.category, "Profile");
        assert_eq!(error.context["path"], path);
    }
    let mut required = schema.clone();
    required["required"] = json!(["a.b"]);
    let profile = json!({"package_id": "paths", "schema_version": 1, "options": {}});
    assert_eq!(
        resolve_options(&required, &profile, "paths")
            .unwrap_err()
            .context["path"],
        "$[\"a.b\"]"
    );
    let mut invalid_default = schema;
    invalid_default["properties"]["quote\"key"] = json!({"type": "integer", "default": "bad"});
    let error = resolve_options(&invalid_default, &profile, "paths").unwrap_err();
    assert_eq!(error.category, "SchemaDefault");
    assert_eq!(error.context["path"], "$[\"quote\\\"key\"]");
}
