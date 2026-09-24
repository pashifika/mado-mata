use super::*;
use crate::desktop::test_support::{fixture, request};

#[test]
fn javascript_preflight_refuses_missing_static_dependencies_without_evaluation() {
    let limits = manual_plan().unwrap().limits;
    let mut inventory = Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &limits,
    )
    .unwrap();
    inventory
        .sources
        .get_mut("main.js")
        .unwrap()
        .push_str("\nthrow new Error('must not evaluate during inspect');\n");
    let mut inventory = inspect_javascript(inventory, &limits).unwrap();
    inventory
        .sources
        .get_mut("main.js")
        .unwrap()
        .push_str("\nexport { missing } from './missing.js';\n");
    let fault = inspect_javascript(inventory, &limits).unwrap_err();
    assert_eq!(fault.category, "ImportRefused");
    assert_eq!(fault.context["specifier"], "./missing.js");
}

#[test]
fn javascript_preflight_links_present_modules_without_evaluation() {
    let limits = manual_plan().unwrap().limits;
    let mut inventory = Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &limits,
    )
    .unwrap();
    inventory.sources.get_mut("main.js").unwrap().push_str(
        "\nexport { decide as chooseDecision } from './decisions.js';\n\
         export * from './decisions.js';\n\
         throw new Error('entry must not evaluate during inspect');\n",
    );
    inventory.sources.get_mut("decisions.js").unwrap().push_str(
        "\nimport { readiness } from './main.js';\n\
         export { readiness };\n\
         throw new Error('dependency must not evaluate during inspect');\n",
    );
    let inventory = inspect_javascript(inventory, &limits).unwrap();
    for invalid_binding in [
        "import { notExported as unavailable } from './decisions.js';",
        "export { notExported } from './decisions.js';",
    ] {
        let mut invalid = inventory.clone();
        invalid
            .sources
            .get_mut("main.js")
            .unwrap()
            .push_str(invalid_binding);
        let fault = inspect_javascript(invalid, &limits).unwrap_err();
        assert_eq!(
            fault.category, "ImportRefused",
            "{invalid_binding}: {fault:?}"
        );
        assert!(fault.message.contains("notExported"), "{fault:?}");
    }
}

#[test]
fn javascript_preflight_follows_requested_declaration_modules_only() {
    let limits = manual_plan().unwrap().limits;
    let mut inventory = Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &limits,
    )
    .unwrap();
    inventory.sources.insert(
        "runtime.d.ts".into(),
        "export { decide } from './bridge.d.ts';\n\
         import { readiness } from './main.js';\n\
         export { readiness };\n\
         throw new Error('runtime declaration must not evaluate during inspect');\n"
            .into(),
    );
    inventory.sources.insert(
        "bridge.d.ts".into(),
        "import { decide } from './decisions.js';\n\
         export { decide };\n\
         throw new Error('transitive declaration must not evaluate during inspect');\n"
            .into(),
    );
    inventory.sources.insert(
        "unused.d.ts".into(),
        "export { Unused } from './missing-types.d.ts';\n\
         export declare const unused: import('./missing-query.d.ts').Unused;\n"
            .into(),
    );
    for request in [
        "import { decide } from './runtime.d.ts';",
        "export function load() { return import('./runtime.d.ts'); }",
    ] {
        let mut selected = inventory.clone();
        selected.sources.insert(
            "main.js".into(),
            format!(
                "{request}\n\
                 export function readiness() {{ return 'Ready'; }}\n\
                 export function workflow() {{}}\n\
                 throw new Error('entry must not evaluate during inspect');\n"
            ),
        );
        let selected = inspect_javascript(selected, &limits).unwrap();

        let mut missing_export = selected.clone();
        missing_export.sources.insert(
            "bridge.d.ts".into(),
            "export { notExported as decide } from './decisions.js';".into(),
        );
        let fault = inspect_javascript(missing_export, &limits).unwrap_err();
        assert_eq!(fault.category, "ImportRefused", "{request}: {fault:?}");
        assert!(fault.message.contains("notExported"), "{fault:?}");

        let mut invalid_syntax = selected;
        invalid_syntax.sources.insert(
            "bridge.d.ts".into(),
            "export function decide() {\n  const value = ;\n}\n".into(),
        );
        let fault = inspect_javascript(invalid_syntax, &limits).unwrap_err();
        assert_eq!(fault.category, "Syntax", "{request}: {fault:?}");
        assert_eq!(fault.context["module"], "bridge.d.ts");
        assert_eq!(fault.context["line"], 2);
    }
}

#[test]
fn javascript_preflight_preserves_target_engine_syntax_location() {
    let limits = manual_plan().unwrap().limits;
    let mut inventory = Inventory::capture(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/javascript")),
        &limits,
    )
    .unwrap();
    inventory.sources.insert(
        "decisions.js".into(),
        "export function decide() {\n  const value = ;\n}\n".into(),
    );
    let fault = inspect_javascript(inventory, &limits).unwrap_err();
    assert_eq!(fault.category, "Syntax");
    assert_eq!(fault.context["module"], "decisions.js");
    assert_eq!(fault.context["line"], 2);
}

#[test]
fn profile_capture_preserves_values_and_rejects_stale_inputs() {
    let inventory = fixture();
    let mut request = request(&inventory);
    let mut selected = inventory.clone();
    select_profile(&mut selected, &request).unwrap();
    request.values["priorities"] = json!(["template", "ocr"]);
    let effective = resolve_options(
        &selected.schema,
        &selected.profiles["saved-choice"],
        &selected.package_id,
    )
    .unwrap();
    assert_eq!(effective["priorities"], json!(["ocr", "template"]));
    assert_ne!(selected.identity, inventory.identity);
    let mut changed = inventory.clone();
    changed
        .sources
        .get_mut("main.ts")
        .unwrap()
        .push_str("\n// changed\n");
    changed.refresh_identity().unwrap();
    assert_eq!(
        select_profile(&mut changed, &request).unwrap_err().category,
        "StaleIdentity"
    );
    request.schema_identity = "old-schema".into();
    assert_eq!(
        select_profile(&mut inventory.clone(), &request)
            .unwrap_err()
            .category,
        "ProfileIdentity"
    );
}
