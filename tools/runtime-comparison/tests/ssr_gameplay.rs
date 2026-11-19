//! Run the shipped Script on a non-game frame; it must hand control back without input.
use mado_runtime_comparison::{
    check::plan,
    host::{Host, resolve_options},
    inventory::Inventory,
    javascript,
    model::Control,
    typescript,
};
use serde_json::json;
use std::{path::Path, sync::Arc};

#[test]
fn unrelated_controlled_frame_cannot_trigger_game_input() {
    let plan = plan("typescript", "success", "default");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("packages/ssr-gameplay");
    let captured = Inventory::capture(&path, &plan.limits).unwrap();
    let inv = Arc::new(typescript::compile(&captured, &plan.limits).unwrap());
    let opts = resolve_options(&inv.schema, &inv.profiles["default"], &inv.package_id).unwrap();
    let control = Arc::new(Control::new(&plan.limits));
    let host = Host::new(plan, opts, inv.assets.clone(), control).unwrap();
    javascript::run(inv, host.clone()).unwrap();
    let facts = host.snapshot();
    assert_eq!(facts["effects"], json!([]));
    assert!(facts["logs"].as_array().unwrap().iter().any(|log| {
        log.as_str().is_some_and(|s| s.contains("unsupported hand layout"))
    }));
    assert_eq!(host.finish()["clean"], true);
}
