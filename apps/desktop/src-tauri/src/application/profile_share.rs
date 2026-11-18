use super::{Application, WorkspaceRef, check_webview_value, lock};
use crate::storage::{
    MAX_PROFILE_BYTES, ProfileStore, decode, resolve_profile_values, validate_profile_name,
};
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const PROFILE_SHARE_PREFIX: &str = "MMP1:";
pub const MAX_PROFILE_SHARE_BYTES: usize = 2 * MAX_PROFILE_BYTES;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSharePreview {
    pub package_id: String,
    pub schema_identity: String,
    pub values: Value,
    #[serde(
        default,
        deserialize_with = "present_name",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,
}

fn present_name<'de, D: Deserializer<'de>>(input: D) -> Result<Option<String>, D::Error> {
    String::deserialize(input).map(Some)
}

impl Application {
    pub fn export_profile_share_code(
        &self,
        workspace: &WorkspaceRef,
        id: &str,
        include_name: bool,
    ) -> Result<String, Fault> {
        let result = self.with_share_owner(workspace, |inventory, schema_identity, store| {
            let (profile, bytes) = store.read_record(id).map_err(|fault| {
                Fault::new(
                    fault.category,
                    "Saved profile is invalid or unavailable in this Tab/package",
                )
            })?;
            if profile.schema_identity != schema_identity {
                return Err(schema_mismatch());
            }
            check_source_integers(&bytes)?;
            let preview = ProfileSharePreview {
                package_id: profile.package_id,
                schema_identity: profile.schema_identity,
                values: effective_values(inventory, profile.values)?,
                name: include_name.then_some(profile.name),
            };
            let mut code = PROFILE_SHARE_PREFIX.as_bytes().to_vec();
            serde_json::to_writer(&mut code, &preview)
                .map_err(|_| Fault::new("ProfileShare", "Could not encode profile share code"))?;
            Ok(String::from_utf8(code).expect("serialized JSON is UTF-8"))
        });
        self.outcome(Some(workspace), "export_profile_share_code", None, result)
    }

    pub fn decode_profile_share_code(
        &self,
        workspace: &WorkspaceRef,
        code: &str,
    ) -> Result<ProfileSharePreview, Fault> {
        let result = self.with_share_owner(workspace, |inventory, schema_identity, _| {
            if code.len() > MAX_PROFILE_SHARE_BYTES {
                return Err(Fault::new(
                    "StorageLimit",
                    "Profile share code exceeds its byte limit",
                ));
            }
            let body = code.strip_prefix(PROFILE_SHARE_PREFIX).ok_or_else(|| {
                Fault::new(
                    "ProfileShareVersion",
                    "Expected a version 1 profile share code (MMP1:)",
                )
            })?;
            // The storage decoder requires one JSON object (not Serde's positional struct form).
            let mut preview: ProfileSharePreview = decode(body.as_bytes()).map_err(|_| {
                Fault::new(
                    "ProfileShareFormat",
                    "Malformed or unsupported profile share envelope",
                )
            })?;
            if preview.package_id != inventory.package_id {
                return Err(Fault::new(
                    "ProfileIdentity",
                    "Profile belongs to another package",
                ));
            }
            if preview.schema_identity != schema_identity {
                return Err(schema_mismatch());
            }
            if let Some(name) = &preview.name {
                validate_profile_name(name)?;
            }
            check_source_integers(body.as_bytes())?;
            preview.values = effective_values(inventory, preview.values)?;
            Ok(preview)
        });
        self.outcome(Some(workspace), "decode_profile_share_code", None, result)
    }

    // Use only the inspected selection. Ordinary inspection can reconcile profiles;
    // neither sharing direction inspects a path or invokes the runner.
    fn with_share_owner<T>(
        &self,
        workspace: &WorkspaceRef,
        read: impl FnOnce(&Inventory, &str, &ProfileStore) -> Result<T, Fault>,
    ) -> Result<T, Fault> {
        let (_command, state) = self.command_state()?;
        let selected = state.resolve(workspace)?;
        let inventory = selected.inventory.clone();
        let package = selected.package.clone();
        let internal_name = selected.internal_name.clone();
        drop(state);
        let store = lock(&self.store);
        let scoped = store.profile_store(&internal_name, &inventory.package_id)?;
        read(&inventory, &package.schema_identity, &scoped)
    }
}

fn schema_mismatch() -> Fault {
    Fault::new(
        "ProfileIdentity",
        "Profile schema does not match the inspected package",
    )
}

fn numeric_fault() -> Fault {
    Fault::new(
        "NumericPrecision",
        "Profile number cannot be shared losslessly with the desktop",
    )
}

// serde_json falls back to f64 for integers beyond i64/u64. On bounded,
// already parsed JSON, inspect the spelling without touching string contents
// or treating a decimal/exponent as an exact integer.
fn check_source_integers(bytes: &[u8]) -> Result<(), Fault> {
    const MAX_SAFE: &[u8] = b"9007199254740991";
    let mut quoted = false;
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        i += 1;
        match byte {
            b'\\' if quoted => i += 1,
            b'"' => quoted = !quoted,
            b'-' | b'0'..=b'9' if !quoted => {
                let start = i - 1;
                while matches!(
                    bytes.get(i),
                    Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                ) {
                    i += 1;
                }
                let token = &bytes[start..i];
                let digits = token.strip_prefix(b"-").unwrap_or(token);
                if (digits.len() > MAX_SAFE.len()
                    || (digits.len() == MAX_SAFE.len() && digits > MAX_SAFE))
                    && digits.iter().all(u8::is_ascii_digit)
                {
                    return Err(numeric_fault());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn effective_values(inventory: &Inventory, supplied: Value) -> Result<Value, Fault> {
    let (_, effective) = resolve_profile_values(inventory, supplied).map_err(|fault| {
        // Schema errors can quote input keys; portability faults can include a private path.
        let message = match fault.category.as_str() {
            "ProfileAuthority" => "Profile contains non-portable values",
            "StorageLimit" => "Profile values exceed the supported size or nesting limit",
            _ => "Profile values do not match the inspected option schema or portability rules",
        };
        Fault::new(fault.category, message)
    })?;
    // Resolution preserves every supplied option or refuses it. Check numbers
    // (including defaults) only after portability has bounded keys and depth.
    check_webview_value(&effective, "$").map_err(|_| numeric_fault())?;
    Ok(effective)
}

#[cfg(test)]
mod tests;
