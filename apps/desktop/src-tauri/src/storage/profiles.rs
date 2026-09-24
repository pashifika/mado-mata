use super::fs::{
    bounded_entries, check_alias, check_directory, checked_file, limit, malformed, storage,
};
use super::{
    MAX_NAME_BYTES, MAX_PROFILE_BYTES, MAX_PROFILES, MAX_TOTAL_BYTES, MAX_VALUE_DEPTH,
    MAX_VALUE_NODES, ProfileStore, Store, VERSION, check_budget, decode, encode, exists,
    filesystem_key, new_id, private_directory, read_bytes, validate_id, write_atomic,
};
use crate::configuration::publish_no_replace;
use mado_runtime_comparison::host::resolve_options;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, identity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub package_id: String,
    pub schema_identity: String,
    pub values: Value,
}
#[derive(Debug)]
pub struct ProfileListing {
    pub profiles: Vec<Profile>,
    pub rejected: Vec<Fault>,
}
#[derive(Clone, Debug, Serialize)]
pub struct LegacyImport {
    pub imported: Vec<String>,
    pub unchanged: Vec<String>,
    pub fault: Option<Fault>,
}

impl Store {
    pub fn import_legacy_profiles(
        &self,
        tab_name: &str,
        inventory: &Inventory,
    ) -> Result<LegacyImport, Fault> {
        let scoped = self.profile_store(tab_name, &inventory.package_id)?;
        let schema_identity = identity(&inventory.schema)?;
        validate_identity(&inventory.package_id, &schema_identity)?;
        let mut result = LegacyImport {
            imported: Vec::new(),
            unchanged: Vec::new(),
            fault: None,
        };
        let import = (|| {
            let directory = self.root.join("profiles");
            check_alias(&self.root, "profiles")?;
            if !exists(&directory)? {
                return Ok(());
            }
            check_directory(&directory)?;
            let mut entries = bounded_entries(&directory)?;
            entries.sort_by_key(|entry| entry.file_name());
            let mut count = 0;
            let mut total = 0;
            for entry in entries {
                let filename = entry.file_name();
                let Some(name) = filename.to_str() else {
                    if filename.as_encoded_bytes().ends_with(b".json") {
                        return Err(malformed());
                    }
                    continue;
                };
                if name.ends_with(".pending") {
                    return Err(Fault::new(
                        "StoragePending",
                        "legacy profile import has an unresolved pending file",
                    ));
                }
                let Some(id) = name.strip_suffix(".json") else {
                    continue;
                };
                validate_id(id)?;
                count += 1;
                let bytes = read_bytes(&entry.path(), MAX_PROFILE_BYTES)
                    .map_err(|fault| profile_fault(fault, id))?;
                total += bytes.len();
                if count > MAX_PROFILES || total > MAX_TOTAL_BYTES {
                    return Err(limit(
                        "legacy profiles exceed their count or aggregate byte bound",
                    ));
                }
                let profile: Profile = decode(&bytes).map_err(|fault| profile_fault(fault, id))?;
                validate_profile(&profile).map_err(|fault| profile_fault(fault, id))?;
                if profile.id != id {
                    return Err(profile_fault(
                        Fault::new(
                            "ProfileIdentity",
                            "legacy profile ID does not match its filename",
                        ),
                        id,
                    ));
                }
                if profile.package_id != inventory.package_id
                    || profile.schema_identity != schema_identity
                {
                    continue;
                }
                validate_values(inventory, profile.values)
                    .map_err(|fault| profile_fault(fault, id))?;
                scoped.check_owner()?;
                let destination = scoped.profile_path(id);
                if exists(&destination)? {
                    scoped.read_profile(id)?;
                    if read_bytes(&destination, MAX_PROFILE_BYTES)? != bytes {
                        return Err(profile_fault(
                            Fault::new(
                                "LegacyConflict",
                                "legacy profile conflicts with an existing owner profile; neither file was changed",
                            ),
                            id,
                        ));
                    }
                    result.unchanged.push(id.to_owned());
                    continue;
                }
                let profiles = scoped.profiles()?;
                if profiles.len() >= MAX_PROFILES
                    || profiles.iter().map(|(_, size)| size).sum::<usize>() + bytes.len()
                        > MAX_TOTAL_BYTES
                {
                    return Err(limit("import exceeds this Tab/package profile budget"));
                }
                scoped.write_profile(id, &bytes, false)?;
                result.imported.push(id.to_owned());
            }
            Ok(())
        })();
        if let Err(fault) = import {
            result.fault = Some(scoped.owner_fault(fault));
        }
        Ok(result)
    }
}

impl ProfileStore {
    pub fn list(&self, package_id: &str, schema_identity: &str) -> Result<ProfileListing, Fault> {
        self.check_package(package_id)?;
        validate_identity(package_id, schema_identity)?;
        let mut profiles = Vec::new();
        let mut rejected = Vec::new();
        for (profile, _) in self.profiles()? {
            match check_binding(&profile, package_id, schema_identity) {
                Ok(()) => profiles.push(profile),
                Err(fault) => rejected.push(self.owner_fault(fault)),
            }
        }
        profiles.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        Ok(ProfileListing { profiles, rejected })
    }

    pub fn save(
        &self,
        inventory: &Inventory,
        id: Option<&str>,
        name: &str,
        values: Value,
    ) -> Result<Profile, Fault> {
        self.check_package(&inventory.package_id)?;
        validate_name(name)?;
        let schema_identity = identity(&inventory.schema)?;
        validate_identity(&inventory.package_id, &schema_identity)?;
        let mut profiles = self.profiles()?;
        let replacing = id.is_some();
        let id = match id {
            Some(id) => {
                validate_id(id)?;
                let index = profiles
                    .iter()
                    .position(|(profile, _)| profile.id == id)
                    .ok_or_else(|| {
                        self.owner_fault(profile_fault(
                            Fault::new(
                                "ProfileNotFound",
                                "saved profile no longer exists in this Tab/package",
                            ),
                            id,
                        ))
                    })?;
                let (old, _) = profiles.swap_remove(index);
                check_binding(&old, &inventory.package_id, &schema_identity)?;
                validate_values(inventory, old.values).map_err(|fault| profile_fault(fault, id))?;
                id.to_owned()
            }
            None => {
                if profiles.len() >= MAX_PROFILES {
                    return Err(limit("profile count exceeds 64"));
                }
                self.next_id()?
            }
        };
        let profile = Profile {
            version: VERSION,
            id,
            name: name.to_owned(),
            package_id: inventory.package_id.clone(),
            schema_identity,
            values: validate_values(inventory, values)?,
        };
        let bytes = encode(&profile, MAX_PROFILE_BYTES)?;
        if profiles.iter().map(|(_, size)| size).sum::<usize>() + bytes.len() > MAX_TOTAL_BYTES {
            return Err(limit("stored profiles exceed the 1 MiB aggregate limit"));
        }
        self.write_profile(&profile.id, &bytes, replacing)?;
        Ok(profile)
    }

    pub fn rename(&self, inventory: &Inventory, id: &str, name: &str) -> Result<Profile, Fault> {
        validate_id(id)?;
        let profile = self.read_profile(id)?.0;
        self.save(inventory, Some(id), name, profile.values)
    }

    pub fn delete(&self, id: &str) -> Result<(), Fault> {
        validate_id(id)?;
        self.read_profile(id)?;
        // Refuse this owner's unresolved write; unrelated owners are not this store's concern.
        self.profiles()?;
        fs::remove_file(self.profile_path(id))
            .map_err(|error| self.owner_fault(storage("delete profile", error)))
    }

    pub(super) fn profile_path(&self, id: &str) -> PathBuf {
        self.directory().join(format!("{id}.config"))
    }

    fn next_id(&self) -> Result<String, Fault> {
        for _ in 0..16 {
            let id = new_id()?;
            let path = self.profile_path(&id);
            if !exists(&path)? && !exists(&path.with_extension("pending"))? {
                return Ok(id);
            }
        }
        Err(Fault::new(
            "Storage",
            "could not allocate a unique profile ID",
        ))
    }

    pub(super) fn read_profile(&self, id: &str) -> Result<(Profile, usize), Fault> {
        let result = (|| {
            validate_id(id)?;
            self.check_owner()?;
            check_alias(&self.directory(), &format!("{id}.config"))?;
            let bytes = read_bytes(&self.profile_path(id), MAX_PROFILE_BYTES)?;
            let profile: Profile = decode(&bytes)?;
            validate_profile(&profile)?;
            if profile.id != id || profile.package_id != self.package_id {
                return Err(Fault::new(
                    "ProfileIdentity",
                    "profile identity does not match its containing Tab/package/file",
                ));
            }
            Ok((profile, bytes.len()))
        })();
        result.map_err(|fault| self.owner_fault(profile_fault(fault, id)))
    }

    fn profiles(&self) -> Result<Vec<(Profile, usize)>, Fault> {
        self.check_owner()?;
        let directory = self.directory();
        if !exists(&directory)? {
            return Ok(Vec::new());
        }
        let entries = bounded_entries(&directory)?;
        let mut result = Vec::new();
        let mut bytes = 0;
        for entry in entries {
            let filename = entry.file_name();
            let name_bytes = filename.as_encoded_bytes();
            if !name_bytes.ends_with(b".config") && !name_bytes.ends_with(b".pending") {
                continue;
            }
            let with_filename = |mut fault: Fault| {
                fault.context["file"] = json!(name_bytes.escape_ascii().to_string());
                self.owner_fault(fault)
            };
            let name = filename
                .to_str()
                .ok_or_else(|| with_filename(malformed()))?;
            // Target failures belong to the Target view, never profile admission.
            if matches!(
                filesystem_key(name).as_str(),
                "target.config" | "target.pending"
            ) {
                continue;
            }
            if name.ends_with(".pending") {
                checked_file(&entry.path(), MAX_PROFILE_BYTES).map_err(with_filename)?;
                return Err(with_filename(Fault::new(
                    "StoragePending",
                    "package configuration has an unresolved pending file",
                )));
            }
            let id = name.strip_suffix(".config").ok_or_else(malformed)?;
            validate_id(id).map_err(with_filename)?;
            if result.len() >= MAX_PROFILES {
                return Err(limit("profile count exceeds 64"));
            }
            let (profile, size) = self.read_profile(id)?;
            bytes += size;
            if bytes > MAX_TOTAL_BYTES {
                return Err(limit("stored profiles exceed the 1 MiB aggregate limit"));
            }
            result.push((profile, size));
        }
        Ok(result)
    }

    fn write_profile(&self, id: &str, bytes: &[u8], replacing: bool) -> Result<(), Fault> {
        self.check_owner()?;
        let relative = format!("tabs/{}/{}/{id}.config", self.tab_name, self.package_id);
        check_budget(&self.root, &relative, bytes.len())?;
        private_directory(&self.directory())?;
        check_alias(&self.directory(), &format!("{id}.config"))?;
        write_atomic(&self.profile_path(id), bytes, |from, to| {
            if replacing {
                fs::rename(from, to)
            } else {
                publish_no_replace(from, to)
            }
        })
        .map_err(|fault| self.owner_fault(profile_fault(fault, id)))
    }
}

fn profile_fault(mut fault: Fault, id: &str) -> Fault {
    fault.context["profile_id"] = json!(id);
    fault
}
fn validate_name(name: &str) -> Result<(), Fault> {
    if name.trim().is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.chars().any(char::is_control)
        || machine_path(name)
    {
        return Err(Fault::new(
            "Profile",
            "profile name must be nonempty portable text of at most 128 bytes",
        ));
    }
    Ok(())
}

fn validate_identity(package_id: &str, schema_identity: &str) -> Result<(), Fault> {
    if package_id.is_empty()
        || package_id.len() > 240
        || package_id == "."
        || package_id == ".."
        || package_id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        || schema_identity.len() != 64
        || !schema_identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Fault::new(
            "ProfileIdentity",
            "invalid package or schema identity",
        ));
    }
    Ok(())
}

pub(crate) fn validate_profile(profile: &Profile) -> Result<(), Fault> {
    if profile.version != VERSION {
        return Err(Fault::new(
            "ProfileVersion",
            "unsupported saved profile version; original data was preserved",
        ));
    }
    validate_id(&profile.id)?;
    validate_name(&profile.name)?;
    validate_identity(&profile.package_id, &profile.schema_identity)?;
    portable_values(&profile.values)
}

fn check_binding(profile: &Profile, package_id: &str, schema_identity: &str) -> Result<(), Fault> {
    if profile.package_id != package_id || profile.schema_identity != schema_identity {
        return Err(Fault::new(
            "ProfileIdentity",
            "saved profile belongs to a different package or schema; select a compatible package or save a new profile",
        )
        .with_context(json!({"profile_id": profile.id})));
    }
    Ok(())
}

fn validate_values(inventory: &Inventory, values: Value) -> Result<Value, Fault> {
    portable_values(&values)?;
    encode(&values, MAX_PROFILE_BYTES)?;
    let mut wrapper = json!({
        "package_id": inventory.package_id,
        "schema_version": inventory.schema.get("version"),
    });
    wrapper["options"] = values;
    let resolved = resolve_options(&inventory.schema, &wrapper, &inventory.package_id)?;
    portable_values(&resolved)?;
    encode(&resolved, MAX_PROFILE_BYTES)?;
    Ok(wrapper["options"].take())
}

fn portable_values(values: &Value) -> Result<(), Fault> {
    if !values.is_object() {
        return Err(Fault::new("Profile", "profile values must be an object"));
    }
    visit_values(values, "$", 0, &mut 0)
}

fn visit_values(value: &Value, path: &str, depth: usize, nodes: &mut usize) -> Result<(), Fault> {
    *nodes += 1;
    if depth > MAX_VALUE_DEPTH || *nodes > MAX_VALUE_NODES {
        return Err(limit("profile values exceed their nesting or item bound"));
    }
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key.len() > MAX_NAME_BYTES || key.chars().any(char::is_control) {
                    return Err(Fault::new(
                        "Profile",
                        "profile option name exceeds its portable text bound",
                    ));
                }
                if authority_field(key) {
                    return Err(Fault::new(
                        "ProfileAuthority",
                        "machine authority and credentials are not portable profile options",
                    )
                    .with_context(json!({"field": format!("{path}.{key}")})));
                }
                visit_values(value, &format!("{path}.{key}"), depth + 1, nodes)?;
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                visit_values(value, &format!("{path}[{index}]"), depth + 1, nodes)?;
            }
        }
        Value::String(text) if text.len() > MAX_PROFILE_BYTES => {
            return Err(limit("profile string exceeds its byte bound"));
        }
        Value::String(text) if text.contains('\0') || machine_path(text) => {
            return Err(Fault::new(
                "ProfileAuthority",
                "machine-local paths are not portable profile values",
            )
            .with_context(json!({"field": path})));
        }
        _ => {}
    }
    Ok(())
}

fn authority_field(field: &str) -> bool {
    let normalized: String = field
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    matches!(
        normalized.as_str(),
        "password"
            | "passwd"
            | "credential"
            | "credentials"
            | "secret"
            | "secrets"
            | "apikey"
            | "apitoken"
            | "accesstoken"
            | "refreshtoken"
            | "authtoken"
            | "authorization"
            | "clientsecret"
            | "privatekey"
            | "bearertoken"
            | "token"
            | "auth"
            | "authentication"
            | "sessiontoken"
            | "sessioncookie"
            | "accesskey"
            | "secretkey"
            | "executable"
            | "executablepath"
            | "targetexecutable"
            | "targetexecutablepath"
            | "targetpath"
            | "gameexecutable"
            | "gameexecutablepath"
            | "gamepath"
            | "gamedirectory"
            | "ocrmodel"
            | "ocrmodelpath"
            | "ocrpath"
            | "modelpath"
            | "modeldirectory"
            | "ocrmodelfile"
            | "runtimepath"
            | "runtimedirectory"
            | "ocrruntime"
            | "ocrruntimepath"
            | "packagepath"
            | "packageroot"
            | "permission"
            | "permissions"
            | "permissiongrant"
            | "permissiongrants"
            | "ospermission"
            | "ospermissions"
            | "inputauthority"
            | "nativeauthority"
            | "inputbackend"
            | "targetbinding"
            | "processid"
            | "windowhandle"
            | "nativeconfig"
            | "nativeconfiguration"
            | "machineconfig"
            | "machineconfiguration"
            | "targetconfig"
            | "ocrconfig"
            | "inputconfig"
            | "inputpermission"
            | "inputpermissions"
            | "ospermissiongrants"
            | "runtimeexecutable"
            | "runtimeexecutablepath"
            | "enginepath"
            | "engineroot"
            | "engineexecutable"
            | "engineexecutablepath"
            | "targetwindow"
            | "windowid"
            | "targetprocess"
            | "targetprocessid"
            | "processhandle"
    )
}

fn machine_path(text: &str) -> bool {
    let text = text.trim();
    let bytes = text.as_bytes();
    text.starts_with('/')
        || text.starts_with('\\')
        || text.starts_with("~/")
        || text.starts_with("~\\")
        || text == ".."
        || text.starts_with("../")
        || text.starts_with("..\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || text
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
        || text.starts_with("$HOME/")
        || text.starts_with("${HOME}/")
        || text
            .get(..14)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("%USERPROFILE%\\"))
}

#[cfg(test)]
mod tests;
