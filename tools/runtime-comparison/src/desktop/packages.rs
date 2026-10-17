use super::{DesktopController, PackageInfo, StartRequest, manual_plan};
use crate::host::resolve_options;
use crate::inventory::{Inventory, TargetDeclaration};
use crate::model::{Control, Fault, Limits, encode_bounded, identity};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

impl DesktopController {
    /// Capture and statically validate only; no package module is evaluated.
    pub fn inspect(&self, package: &Path) -> Result<PackageInfo, Fault> {
        self.check_open()?;
        let limits = manual_plan()?.limits;
        let inventory = Inventory::capture(package, &limits)?;
        inspect_captured(inventory, &limits, &Arc::new(Control::new(&limits)))
    }

    /// Inspect captured bytes; an owning operation supplies its Stop/deadline.
    pub fn inspect_inventory(
        &self,
        inventory: Inventory,
        control: Option<&Arc<Control>>,
    ) -> Result<PackageInfo, Fault> {
        self.check_open()?;
        let limits = manual_plan()?.limits;
        let bounds = json!({"files": limits.snapshot_files, "bytes": limits.snapshot_bytes});
        if inventory.metadata.get("capture_limits") != Some(&bounds) {
            return Err(Fault::new(
                "Inventory",
                "inventory capture bounds differ from desktop inspection bounds",
            ));
        }
        inventory.validate()?;
        let owned_control;
        let control = if let Some(control) = control {
            control
        } else {
            owned_control = Arc::new(Control::new(&limits));
            &owned_control
        };
        inspect_captured(inventory, &limits, control)
    }

    fn check_open(&self) -> Result<(), Fault> {
        if self.state().closed {
            return Err(Fault::new(
                "ControllerClosed",
                "controller is shutting down",
            ));
        }
        Ok(())
    }
}

fn inspect_captured(
    inventory: Inventory,
    limits: &Limits,
    control: &Arc<Control>,
) -> Result<PackageInfo, Fault> {
    control.check()?;
    let runtime = runtime(&inventory)?;
    let inventory = match runtime.as_str() {
        "typescript" => {
            crate::typescript::compile_with_control(&inventory, limits, control)?;
            inventory
        }
        "javascript" => inspect_javascript(inventory, limits, control)?,
        _ => unreachable!("runtime() admits only QuickJS languages"),
    };
    let defaults = profile(&inventory.package_id, &inventory.schema, json!({}));
    let effective_defaults =
        resolve_options(&inventory.schema, &defaults, &inventory.package_id).ok();
    let target = inventory.target()?;
    let target_identity = target
        .as_ref()
        .map(TargetDeclaration::identity)
        .transpose()?;
    let info = PackageInfo {
        schema_identity: identity(&inventory.schema)?,
        package_id: inventory.package_id,
        inventory_identity: inventory.identity,
        runtime,
        schema: inventory.schema,
        profiles: inventory.profiles,
        effective_defaults,
        target,
        target_identity,
    };
    encode_bounded(&info, 2 * 1024 * 1024)?;
    Ok(info)
}

fn inspect_javascript(
    inventory: Inventory,
    limits: &Limits,
    control: &Arc<Control>,
) -> Result<Inventory, Fault> {
    let inventory = Arc::new(inventory);
    crate::typescript::validate_javascript_modules(&inventory, limits, control)?;
    Arc::try_unwrap(inventory)
        .map_err(|_| Fault::new("Runtime", "inspection retained the captured inventory"))
}

pub(super) fn runtime(inventory: &Inventory) -> Result<String, Fault> {
    match inventory.metadata["runtime"].as_str() {
        Some(value @ ("javascript" | "typescript")) => Ok(value.into()),
        _ => Err(Fault::new(
            "RuntimeRefused",
            "desktop M1 supports JavaScript and TypeScript through QuickJS only",
        )),
    }
}

fn profile(package_id: &str, schema: &Value, values: Value) -> Value {
    json!({"package_id":package_id,"schema_version":schema["version"],"options":values})
}

pub(super) fn select_profile(
    inventory: &mut Inventory,
    request: &StartRequest,
) -> Result<(), Fault> {
    if inventory.identity != request.inventory_identity
        || inventory.package_id != request.package_id
    {
        return Err(Fault::new(
            "StaleIdentity",
            "package changed since inspection; inspect it again",
        )
        .with_context(
            json!({"expected_inventory":request.inventory_identity,"inventory":inventory.identity}),
        ));
    }
    if identity(&inventory.schema)? != request.schema_identity {
        return Err(Fault::new(
            "ProfileIdentity",
            "profile schema identity no longer matches the package",
        ));
    }
    let selected = profile(
        &inventory.package_id,
        &inventory.schema,
        request.values.clone(),
    );
    resolve_options(&inventory.schema, &selected, &inventory.package_id)?;
    inventory.metadata["desktop_profile"] = json!({
        "id":request.profile_id,"package_inventory_identity":inventory.identity,
        "schema_identity":request.schema_identity
    });
    // The external profile is a captured application input, never a package file read.
    inventory.metadata["manifest"]["profiles"][&request.profile_id] =
        json!("application-profile.json");
    inventory
        .profiles
        .insert(request.profile_id.clone(), selected);
    inventory.refresh_identity()
}

#[cfg(test)]
mod tests;
