use super::{Application, ProfileCatalog, WorkspaceRef, lock};
use crate::storage::{LegacyImport, Profile, ProfileStore};
use mado_runtime_comparison::host::{option_path, resolve_options};
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::Fault;
use serde_json::{Value, json};

impl Application {
    pub fn import_legacy_profiles(&self, workspace: &WorkspaceRef) -> Result<LegacyImport, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            lock(&self.store).import_legacy_profiles(&selected.internal_name, &selected.inventory)
        })();
        self.outcome(Some(workspace), "import_legacy_profiles", None, result)
    }

    pub fn validate(&self, workspace: &WorkspaceRef, mut values: Value) -> Result<Value, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            lock(&self.store)
                .profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            normalize_editor_numbers(&selected.inventory.schema, &mut values);
            desktop_options(&selected.inventory, values)
        })();
        self.outcome(Some(workspace), "validate", None, result)
    }

    pub fn profiles(&self, workspace: &WorkspaceRef) -> Result<ProfileCatalog, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let store = lock(&self.store);
            let scoped =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            let (profiles, profiles_error) = validated_profiles(&scoped, &selected.inventory)?;
            Ok(ProfileCatalog {
                profiles,
                profiles_error,
            })
        })();
        self.outcome(Some(workspace), "profiles", None, result)
    }

    pub fn save_profile(
        &self,
        workspace: &WorkspaceRef,
        id: Option<&str>,
        name: &str,
        mut values: Value,
    ) -> Result<Profile, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let current = self.runner.inspect(&selected.path)?;
            if current.inventory_identity != selected.inventory.identity {
                return Err(Fault::new(
                    "InventoryChanged",
                    "Package changed; inspect it again before saving",
                ));
            }
            normalize_editor_numbers(&selected.inventory.schema, &mut values);
            desktop_options(&selected.inventory, values.clone())?;
            let store = lock(&self.store);
            let store =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            if let Some(id) = id {
                checked_profile(&store, &current.package_id, &current.schema_identity, id)?;
            }
            store.save(&selected.inventory, id, name, values)
        })();
        self.outcome(
            Some(workspace),
            "save_profile",
            Some(("profile.saved", "Profile saved")),
            result,
        )
    }

    pub fn rename_profile(
        &self,
        workspace: &WorkspaceRef,
        id: &str,
        name: &str,
    ) -> Result<Profile, Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let store = lock(&self.store);
            let store =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            let profile = checked_profile(
                &store,
                &selected.inventory.package_id,
                &selected.package.schema_identity,
                id,
            )?;
            desktop_options(&selected.inventory, profile.values.clone())?;
            store.rename(&selected.inventory, id, name)
        })();
        self.outcome(
            Some(workspace),
            "rename_profile",
            Some(("profile.renamed", "Profile renamed")),
            result,
        )
    }

    pub fn delete_profile(&self, workspace: &WorkspaceRef, id: &str) -> Result<(), Fault> {
        let result = (|| {
            let (_command, state) = self.command_state()?;
            let selected = state.resolve(workspace)?.clone();
            drop(state);
            let store = lock(&self.store);
            let store =
                store.profile_store(&selected.internal_name, &selected.inventory.package_id)?;
            let profile = checked_profile(
                &store,
                &selected.inventory.package_id,
                &selected.package.schema_identity,
                id,
            )?;
            desktop_options(&selected.inventory, profile.values)?;
            store.delete(id)
        })();
        self.outcome(
            Some(workspace),
            "delete_profile",
            Some(("profile.deleted", "Profile deleted")),
            result,
        )
    }
}

// i128 avoids the saturating i64/u64 cast that would accept their rounded maxima.
fn exact_integer(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

pub(super) fn check_webview_value(value: &Value, path: &str) -> Result<(), Fault> {
    match value {
        Value::Number(number) => {
            let floating = number.as_f64().expect("JSON numbers are finite");
            if (floating == 0.0 && floating.is_sign_negative())
                || exact_integer(number).is_some_and(|integer| {
                    !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&integer)
                })
            {
                return Err(Fault::new(
                    "NumericPrecision",
                    format!("{path}: number is outside the desktop's lossless JSON contract; source data was preserved"),
                )
                .with_context(json!({"path":path,"value":number.to_string()})));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check_webview_value(item, &format!("{path}[{index}]"))?;
            }
        }
        Value::Object(fields) => {
            for (name, value) in fields {
                check_webview_value(value, &option_path(path, name))?;
            }
        }
        _ => {}
    }
    Ok(())
}

// A number editor intentionally uses f64 semantics. JSON.stringify can emit an
// integer spelling for a float (1.0, 1e18, etc.); restore that schema-known type
// only on incoming editor values, never on unchecked package or stored data.
pub(super) fn normalize_editor_numbers(schema: &Value, value: &mut Value) {
    match schema["type"].as_str() {
        Some("number") => {
            if let Some(number) = value.as_f64().and_then(serde_json::Number::from_f64) {
                *value = Value::Number(number);
            }
        }
        Some("object") => {
            if let Some(fields) = value.as_object_mut() {
                for (name, value) in fields {
                    if let Some(node) = schema["properties"].get(name) {
                        normalize_editor_numbers(node, value);
                    }
                }
            }
        }
        Some("array") => {
            if let Some(items) = value.as_array_mut() {
                for value in items {
                    normalize_editor_numbers(&schema["items"], value);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn desktop_options(inventory: &Inventory, values: Value) -> Result<Value, Fault> {
    check_webview_value(&values, "$")?;
    let effective = resolve_options(
        &inventory.schema,
        &json!({"package_id":inventory.package_id,"schema_version":inventory.schema["version"],"options":values}),
        &inventory.package_id,
    )?;
    check_webview_value(&effective, "$")?;
    Ok(effective)
}

pub(super) fn checked_profile(
    store: &ProfileStore,
    package_id: &str,
    schema_identity: &str,
    id: &str,
) -> Result<Profile, Fault> {
    let profile = store
        .list(package_id, schema_identity)?
        .profiles
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| {
            Fault::new(
                "ProfileNotFound",
                "Saved profile is unavailable; select or save it again",
            )
        })?;
    check_webview_value(&profile.values, "$").map_err(|error| {
        Fault::new(
            "ProfileRejected",
            "Saved numbers cannot be used by the desktop; the file was preserved",
        )
        .with_context(json!({"profile_id":profile.id,"cause":error}))
    })?;
    Ok(profile)
}

// JSON normalizes 1.0 to 1 in the WebView, but must never equate rounded integers.
pub(super) fn same_json_values(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            match (exact_integer(left), exact_integer(right)) {
                (Some(left), Some(right)) => left == right,
                (Some(integer), None) | (None, Some(integer)) => {
                    let floating = if left.is_f64() { left } else { right };
                    floating
                        .as_f64()
                        .is_some_and(|value| value.fract() == 0.0 && value as i128 == integer)
                }
                (None, None) => left == right,
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_json_values(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|other| same_json_values(value, other))
                })
        }
        _ => left == right,
    }
}

pub(super) fn validated_profiles(
    store: &ProfileStore,
    inventory: &Inventory,
) -> Result<(Vec<Profile>, Option<Fault>), Fault> {
    let listing = store.list(
        &inventory.package_id,
        &mado_runtime_comparison::model::identity(&inventory.schema)?,
    )?;
    let mut profiles = Vec::with_capacity(listing.profiles.len());
    let mut rejected = listing.rejected;
    for profile in listing.profiles {
        match desktop_options(inventory, profile.values.clone()) {
            Ok(_) => profiles.push(profile),
            Err(error) => rejected.push(
                Fault::new("Profile", "Saved values do not match the selected schema")
                    .with_context(json!({"profile_id":profile.id, "cause":error})),
            ),
        }
    }
    let error = (!rejected.is_empty()).then(|| {
        Fault::new(
            "ProfileRejected",
            "Some saved profiles are incompatible; their files were preserved",
        )
        .with_context(json!({"rejected":rejected}))
    });
    Ok((profiles, error))
}

#[cfg(test)]
mod tests;
