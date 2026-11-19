use super::*;
use crate::application::{Selection, test_support::*};
use serde_json::json;
use std::fs;
use std::path::Path;

fn envelope(selection: &Selection, values: Value) -> Value {
    json!({
        "package_id": selection.package.package_id,
        "schema_identity": selection.package.schema_identity,
        "values": values,
    })
}

fn wire(body: &Value) -> String {
    format!("MMP1:{body}")
}

#[test]
fn saved_typescript_settings_round_trip_in_both_orders_without_writes() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tools/runtime-comparison/fixtures/typescript");
    let source = inspect_named(app, "Sender", &path).unwrap();
    let receiver = workspace_ref(&inspect_named(app, "Receiver", &path).unwrap());
    let sender = workspace_ref(&source);
    let mut expected = source.package.profiles["ocr-first"]["options"].clone();
    expected["recognition"]["roi"]["x"] = json!(23);
    for (priorities, include_name) in [
        (json!(["ocr", "template"]), false),
        (json!(["template", "ocr"]), true),
    ] {
        expected["priorities"] = priorities;
        if include_name {
            expected["recognition"]["expected"] = json!("Sender-written fixture text");
        }
        let mut sparse = expected.clone();
        sparse.as_object_mut().unwrap().remove("postcondition");
        let saved = app
            .save_profile(&sender, None, "Fixture variant", sparse.clone())
            .unwrap();
        assert_eq!(saved.values, sparse);
        let before = app.capture_configuration().unwrap();

        let code = app
            .export_profile_share_code(&sender, &saved.id, include_name)
            .unwrap();
        let mut wanted = envelope(&source, expected.clone());
        if include_name {
            wanted["name"] = json!("Fixture variant");
        }
        let body = code.strip_prefix("MMP1:").unwrap();
        assert_eq!(serde_json::from_str::<Value>(body).unwrap(), wanted);
        let preview = app.decode_profile_share_code(&receiver, &code).unwrap();
        assert_eq!(serde_json::to_value(preview).unwrap(), wanted);
        // An independently authored sparse code resolves the same defaults on receipt.
        let preview = app
            .decode_profile_share_code(&receiver, &wire(&envelope(&source, sparse)))
            .unwrap();
        assert_eq!(preview.values, expected);
        assert_eq!(app.capture_configuration().unwrap(), before);
    }
    assert!(app.profiles(&receiver).unwrap().profiles.is_empty());
    assert_eq!(app.poll().controller["state"], "idle");
}

#[test]
fn ssr_package_validates_and_shares_ordered_profile_settings_between_tabs() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .join("packages/ssr-gameplay");
    let workspace = app.create_workspace("edit", "Edit").unwrap();
    let editor = app.authoring_open(&view_ref(&workspace), &path).unwrap();
    let validation = app.authoring_validate(&editor.owner, &editor.revision).unwrap();
    assert!(validation.valid, "{:?}", validation.diagnostics);
    app.authoring_exit(&editor.owner).unwrap();

    let sender = workspace_ref(&inspect_named(app, "sender", &path).unwrap());
    let receiver = workspace_ref(&inspect_named(app, "receiver", &path).unwrap());
    let values = json!({"mode":"sortie","cardPriority":["Beta","Alpha"],"excludedCards":["Beta"]});
    let saved = app.save_profile(&sender, None, "Battle", values).unwrap();
    let code = app.export_profile_share_code(&sender, &saved.id, false).unwrap();
    let preview = app.decode_profile_share_code(&receiver, &code).unwrap();
    let imported = app.save_profile(&receiver, None, "Imported", preview.values).unwrap();
    assert_eq!(imported.values["mode"], "sortie");
    assert_eq!(imported.values["cardPriority"], json!(["Beta", "Alpha"]));
    assert_eq!(imported.values["excludedCards"], json!(["Beta"]));
    assert_ne!(imported.id, saved.id);

    let decision = std::process::Command::new("node")
        .current_dir(path.parent().unwrap())
        .args(["--input-type=module", "--eval", r#"
          import { chooseHand } from './ssr-gameplay/hand.ts';
          import { hand } from './tests/hand-fixture.mjs';
          const scan = hand();
          console.log(JSON.stringify(process.argv.slice(1).map(arg => {
            const p = JSON.parse(arg);
            return chooseHand(scan, p.cardPriority, p.excludedCards).title.text;
          })));
        "#])
        .arg(saved.values.to_string())
        .arg(imported.values.to_string())
        .output()
        .unwrap();
    assert!(decision.status.success(), "{}", String::from_utf8_lossy(&decision.stderr));
    assert_eq!(serde_json::from_slice::<Value>(&decision.stdout).unwrap(), json!(["Alpha", "Alpha"]));
}

#[test]
fn share_requires_the_inspected_workspace_and_its_own_current_record() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let path = fixture.package_at("source");
    let source = inspect_named(app, "Sender", &path).unwrap();
    let sender = workspace_ref(&source);
    let receiver = workspace_ref(&inspect_named(app, "Receiver", &path).unwrap());
    let unbound = view_ref(&app.create_workspace("Empty", "Empty").unwrap());
    let saved = app
        .save_profile(&sender, None, "Same name", json!({}))
        .unwrap();
    app.save_profile(&receiver, None, "Same name", json!({"priorities":["ocr"]}))
        .unwrap();
    let valid_code = app
        .export_profile_share_code(&sender, &saved.id, false)
        .unwrap();
    assert!(
        app.export_profile_share_code(&receiver, &saved.id, true)
            .is_err()
    );
    assert!(
        app.export_profile_share_code(&sender, "template-first", false)
            .is_err()
    );
    assert_eq!(
        app.decode_profile_share_code(&unbound, &valid_code)
            .unwrap_err()
            .category,
        "WorkspaceUnbound"
    );
    let stale = WorkspaceRef {
        revision: receiver.revision - 1,
        ..receiver
    };
    assert_eq!(
        app.decode_profile_share_code(&stale, &valid_code)
            .unwrap_err()
            .category,
        "StaleIdentity"
    );

    // Decode uses the inspected selection, not a package location in the code or on disk.
    fs::rename(&path, fixture.root.join("moved-source")).unwrap();
    app.decode_profile_share_code(&sender, &valid_code).unwrap();

    // Change the owned saved record, not the inspected package or the request ID.
    let file = profile_path(&fixture, "Sender", &saved);
    for (field, invalid, category) in [
        ("schema_identity", json!("0".repeat(64)), "ProfileIdentity"),
        (
            "values",
            json!({"recognition":{"asset":"marker"}}),
            "Profile",
        ),
    ] {
        let mut document = serde_json::to_value(&saved).unwrap();
        document[field] = invalid;
        fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
        let before = app.capture_configuration().unwrap();
        let error = app
            .export_profile_share_code(&sender, &saved.id, false)
            .unwrap_err();
        assert_eq!(error.category, category);
        assert_eq!(app.capture_configuration().unwrap(), before);
    }
}

#[test]
fn envelope_identity_and_option_refusals_do_not_disclose_pasted_text() {
    const MARKER: &str = "private-fixture-marker";
    let fixture = Fixture::new();
    let app = &fixture.application;
    let selection = inspect_named(app, "Receiver", &package_path()).unwrap();
    let owner = workspace_ref(&selection);
    let valid = envelope(&selection, json!({}));
    let before = app.capture_configuration().unwrap();
    let refused = |code: &str, category: &str| {
        let error = app.decode_profile_share_code(&owner, code).unwrap_err();
        assert_eq!(error.category, category);
        assert!(!serde_json::to_string(&error).unwrap().contains(MARKER));
    };
    refused(&format!("MMP2:{valid}"), "ProfileShareVersion");
    refused(&format!("MMP1:{{{MARKER}"), "ProfileShareFormat");
    refused(&format!("{}{valid}", wire(&valid)), "ProfileShareFormat");
    refused(
        &wire(&json!([valid["package_id"], valid["schema_identity"], {}])),
        "ProfileShareFormat",
    );
    refused(
        &format!("MMP1:{{\"values\":{{}},{}", &valid.to_string()[1..]),
        "ProfileShareFormat",
    );
    for (field, invalid, category) in [
        (MARKER, json!(true), "ProfileShareFormat"),
        ("package_id", json!(MARKER), "ProfileIdentity"),
        ("schema_identity", json!("a".repeat(64)), "ProfileIdentity"),
        ("values", json!({(MARKER):true}), "Profile"),
        (
            "values",
            json!({"recognition":{"password":MARKER}}),
            "ProfileAuthority",
        ),
        (
            "values",
            json!({"recognition":{"asset":format!("/{MARKER}")}}),
            "ProfileAuthority",
        ),
        (
            "values",
            json!({"recognition":{"asset":MARKER,"threshold":0.5}}),
            "Profile",
        ),
        ("values", json!([MARKER]), "Profile"),
    ] {
        let mut body = valid.clone();
        body[field] = invalid;
        refused(&wire(&body), category);
    }
    let mut nested = json!(MARKER);
    for _ in 0..33 {
        nested = json!([nested]);
    }
    refused(
        &wire(&envelope(&selection, json!({"recognition":nested}))),
        "StorageLimit",
    );
    let poll = app.poll();
    assert_eq!(poll.controller["state"], "idle");
    assert!(!serde_json::to_string(&poll.logs).unwrap().contains(MARKER));
    assert_eq!(app.capture_configuration().unwrap(), before);
}

#[test]
fn a_present_name_must_be_valid_portable_display_text() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let selection = inspect_named(app, "Receiver", &package_path()).unwrap();
    let owner = workspace_ref(&selection);
    let mut body = envelope(&selection, json!({}));
    let at_limit = "é".repeat(64);
    body["name"] = json!(at_limit);
    assert_eq!(
        app.decode_profile_share_code(&owner, &wire(&body))
            .unwrap()
            .name
            .as_deref(),
        Some(at_limit.as_str())
    );
    for (invalid, category) in [
        (Value::Null, "ProfileShareFormat"),
        (json!(17), "ProfileShareFormat"),
        (json!("   "), "Profile"),
        (json!(format!("{at_limit}x")), "Profile"),
        (json!("secret-name-marker\n"), "Profile"),
        (json!("/secret-name-marker"), "Profile"),
    ] {
        body["name"] = invalid;
        let error = app
            .decode_profile_share_code(&owner, &wire(&body))
            .unwrap_err();
        assert_eq!(error.category, category);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("secret-name-marker")
        );
    }
}

#[test]
fn envelope_byte_limit_and_portable_value_limit_apply_independently() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let path = fixture.package_at("package");
    let schema_file = path.join("schema.json");
    let mut schema: Value = serde_json::from_slice(&fs::read(&schema_file).unwrap()).unwrap();
    schema["properties"]["note"] = json!({"type":"string"});
    fs::write(schema_file, serde_json::to_vec(&schema).unwrap()).unwrap();
    let selection = inspect_named(app, "Receiver", &path).unwrap();
    let owner = workspace_ref(&selection);
    let mut body = envelope(&selection, json!({}));
    body["name"] = json!("é");
    let mut code = wire(&body);
    code.extend(std::iter::repeat_n(' ', 128 * 1024 - code.len()));
    app.decode_profile_share_code(&owner, &code).unwrap();
    code.push(' ');
    assert_eq!(
        app.decode_profile_share_code(&owner, &code)
            .unwrap_err()
            .category,
        "StorageLimit"
    );

    // Under the wire bound in both cases: first the supplied projection, then
    // only its effective projection with the package defaults exceeds 64 KiB.
    for size in [64 * 1024, 64 * 1024 - 32] {
        body["values"] = json!({"note":"x".repeat(size)});
        let code = wire(&body);
        assert!(code.len() < 128 * 1024);
        assert_eq!(
            app.decode_profile_share_code(&owner, &code)
                .unwrap_err()
                .category,
            "StorageLimit"
        );
    }
}

#[test]
fn plain_integer_and_signed_zero_sources_cannot_be_rounded_into_a_share() {
    let fixture = Fixture::new();
    let app = &fixture.application;
    let selection = inspect_named(app, "Numeric", &fixture.numeric_package()).unwrap();
    let owner = workspace_ref(&selection);
    let mut values = selection.package.profiles["ocr-first"]["options"].clone();
    values["recognition"]["expected"] = json!(r#"quote \"18446744073709551617\""#);
    values["amount"] = json!("number-slot");
    let template = wire(&envelope(&selection, values));
    for text in [
        "-0",
        "-0.0",
        "9007199254740993",
        "18446744073709551617",
        "-9223372036854775809",
    ] {
        let code = template.replace("\"number-slot\"", text);
        let error = app.decode_profile_share_code(&owner, &code).unwrap_err();
        assert_eq!(error.category, "NumericPrecision");
        assert!(!serde_json::to_string(&error).unwrap().contains(text));
    }
    for text in [
        "9007199254740991",
        "18446744073709551617.0",
        "1.8446744073709552e19",
    ] {
        let code = template.replace("\"number-slot\"", text);
        let preview = app.decode_profile_share_code(&owner, &code).unwrap();
        assert_eq!(
            preview.values["amount"].as_f64(),
            Some(text.parse::<f64>().unwrap())
        );
    }
    let saved = app
        .save_profile(&owner, None, "Test", json!({"amount":0.25}))
        .unwrap();
    let file = profile_path(&fixture, "Numeric", &saved);
    let bytes = fs::read_to_string(&file)
        .unwrap()
        .replace("0.25", "18446744073709551617");
    fs::write(&file, &bytes).unwrap();
    assert_eq!(
        app.export_profile_share_code(&owner, &saved.id, false)
            .unwrap_err()
            .category,
        "NumericPrecision"
    );
    assert_eq!(fs::read_to_string(file).unwrap(), bytes);
}
