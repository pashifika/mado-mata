use super::{NativeIntent, NativeTarget, StartRequest, native_limits};
use crate::inventory::Inventory;
use crate::model::{Fault, Limits, Plan};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

fn refused(message: &str) -> Fault {
    Fault::new("NativeRefused", message)
}

fn text(value: &str, bound: usize) -> bool {
    !value.trim().is_empty() && value.len() <= bound && !value.chars().any(char::is_control)
}

pub(super) fn validate_intent(intent: &NativeIntent) -> Result<(), Fault> {
    if intent.target_revision == 0
        || !text(&intent.target_binding_id, 256)
        || !text(&intent.target_declaration_identity, 256)
        || !intent.capture_approved
        || !intent.input_approved
        || !text(&intent.operation, 4096)
        || !text(&intent.visible_postcondition, 4096)
    {
        return Err(refused(
            "Native requires current target expectations, separate capture/input approval and reviewed operation/postcondition",
        ));
    }
    let limits = &intent.limits;
    let ceiling = native_limits();
    for (value, maximum) in [
        (limits.startup_ms, ceiling.startup_ms),
        (limits.readiness_ms, ceiling.readiness_ms),
        (limits.workflow_ms, ceiling.workflow_ms),
        (limits.max_frames, ceiling.max_frames),
        (limits.wait_ms, ceiling.wait_ms),
        (limits.interval_ms, ceiling.interval_ms),
        (limits.cleanup_ms, ceiling.cleanup_ms),
    ] {
        if value == 0 || value > maximum {
            return Err(refused(
                "Native limits must be positive and within the displayed policy",
            ));
        }
    }
    if limits.max_actions == 0
        || limits.max_actions > ceiling.max_actions
        || limits.containment_ms != ceiling.containment_ms
        || limits.wait_ms < limits.interval_ms
        || limits.startup_ms < limits.wait_ms
        || limits.readiness_ms < limits.wait_ms
        || limits.workflow_ms < limits.wait_ms
        || limits.cleanup_ms > limits.containment_ms
    {
        return Err(refused(
            "Native stage limits exceed their enclosing bound or fixed containment policy",
        ));
    }
    limits.budgets().total_ms()?;
    Ok(())
}

pub(super) fn validate_target(target: Option<&NativeTarget>) -> Result<&NativeTarget, Fault> {
    let target =
        target.ok_or_else(|| refused("Native requires fresh host-owned target correspondence"))?;
    if !target.executable.is_absolute()
        || target.process_id == 0
        || target.process_lifetime.len() != 16
        || !target
            .process_lifetime
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !text(&target.window_title, 4096)
    {
        return Err(refused(
            "Native target requires the exact runtime executable, process lifetime and window",
        ));
    }
    let policy = &target.input;
    if !matches!(policy.route.as_str(), "system" | "process_directed")
        || !matches!(policy.focus.as_str(), "preserve" | "require_focused")
        || policy.click_hold_ms > 1_000
        || !matches!(
            policy.pointer_mode.as_deref(),
            None | Some("core_graphics" | "appkit_background")
        )
        || (policy.pointer_mode.as_deref() == Some("appkit_background")
            && (policy.route != "process_directed" || policy.focus != "preserve"))
    {
        return Err(refused(
            "Native requires the exact supported saved route, focus and pointer policy",
        ));
    }
    Ok(target)
}

pub(super) struct Templates {
    package_entries: BTreeMap<String, String>,
    templates: BTreeMap<String, String>,
    pub(super) images: crate::images::PayloadReservation,
}

pub(super) fn prepare_templates(
    inventory: &Inventory,
    limits: &Limits,
) -> Result<Templates, Fault> {
    inventory.validate()?;
    let mut package_entries = BTreeMap::new();
    let mut templates = BTreeMap::new();
    crate::recognition::merge_effective_template_maps(
        inventory,
        &mut package_entries,
        &mut templates,
    )?;
    if package_entries.len() > limits.snapshot_files || templates.len() > limits.handles {
        return Err(Fault::new(
            "LimitExceeded",
            "Native template mappings exceed the captured package bounds",
        ));
    }
    let images = crate::runner::reserve_native_images(inventory, &package_entries, &templates)?;
    Ok(Templates {
        package_entries,
        templates,
        images,
    })
}

pub(super) fn project(
    plan: &mut Plan,
    request: &StartRequest,
    target: &NativeTarget,
    engine: &Path,
    templates: &Templates,
    mut configuration: Value,
) -> Result<(), Fault> {
    let intent = request
        .native_intent
        .as_ref()
        .ok_or_else(|| refused("Native review is missing"))?;
    validate_intent(intent)?;
    validate_target(Some(target))?;
    let Templates {
        package_entries,
        templates,
        ..
    } = templates;
    let executable = target
        .executable
        .canonicalize()
        .map_err(|_| refused("Verified runtime executable is no longer available"))?;
    if executable != target.executable || !executable.is_file() {
        return Err(refused(
            "Verified runtime executable changed before native projection",
        ));
    }
    let permission_executable = engine.canonicalize().map_err(|_| {
        Fault::new(
            "EngineUnavailable",
            "The fixed engine artifact cannot be resolved",
        )
    })?;
    let limits = &intent.limits;
    configuration["replay"] = Value::Null;
    configuration["native"] = json!({
        "executable_or_bundle":executable,
        "process_id":target.process_id,"process_lifetime":target.process_lifetime,
        "window_rule":target.window_title,"operating_system":"macos",
        "hardware":std::env::consts::ARCH,"permission_executable":permission_executable,
        "capture":{"approved":true,"duration_ms":limits.budgets().total_ms()?,
            "max_frames":limits.max_frames,"wait_ms":limits.wait_ms,"interval_ms":limits.interval_ms},
        "input":{"approved":true,"duration_ms":limits.budgets().total_ms()?,"max_actions":limits.max_actions,
            "route":target.input.route,"focus":target.input.focus,
            "macos_process_pointer_mode":target.input.pointer_mode.as_deref().unwrap_or("core_graphics"),
            "click_hold_ms":target.input.click_hold_ms,"reviewed_operation":intent.operation},
        "geometry":null,"recognition_language":configuration["ocr"]["language"],
        "visible_postcondition":intent.visible_postcondition,
        "cleanup_ms":limits.cleanup_ms,"containment_ms":limits.containment_ms,
        "package_entries":package_entries,"templates":templates
    });
    plan.native_config = Some(configuration);
    plan.validate()?;
    Ok(())
}

#[cfg(test)]
mod tests;
