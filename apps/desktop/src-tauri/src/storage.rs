use mado_runtime_comparison::environment::OcrEnvironment;
use mado_runtime_comparison::host::resolve_options;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, identity};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

const VERSION: u32 = 1;
const MAX_PROFILES: usize = 64;
const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 128;
const MAX_SETTINGS_BYTES: usize = 32 * 1024;
const MAX_NAME_BYTES: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_VALUE_NODES: usize = 8192;
const MAX_VALUE_DEPTH: usize = 32;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotificationPreferences {
    pub visible_count: usize,
    pub timeout_seconds: u64,
    pub show_success: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            visible_count: 2,
            timeout_seconds: 8,
            show_success: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub enum Locale {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "ja")]
    Japanese,
}

impl<'de> Deserialize<'de> for Locale {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LocaleVisitor;

        impl serde::de::Visitor<'_> for LocaleVisitor {
            type Value = Locale;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("\"en\" or \"ja\"")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Locale, E> {
                match value {
                    "en" => Ok(Locale::English),
                    "ja" => Ok(Locale::Japanese),
                    _ => Err(E::unknown_variant(value, &["en", "ja"])),
                }
            }
        }

        // Enum deserialization also accepts objects; settings require a string.
        deserializer.deserialize_str(LocaleVisitor)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditableSettings {
    pub locale: Locale,
    pub gui_log_limit: usize,
    pub ocr_environment: Option<OcrEnvironment>,
    pub notifications: NotificationPreferences,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    #[serde(default)]
    pub locale: Locale,
    pub gui_log_limit: usize,
    pub package_path: Option<String>,
    #[serde(default)]
    pub ocr_environment: Option<OcrEnvironment>,
    #[serde(default)]
    pub notifications: NotificationPreferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            locale: Locale::default(),
            gui_log_limit: 1000,
            package_path: None,
            ocr_environment: None,
            notifications: NotificationPreferences::default(),
        }
    }
}

#[derive(Debug)]
pub struct ProfileListing {
    pub profiles: Vec<Profile>,
    pub rejected: Vec<Fault>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Result<Self, Fault> {
        private_directory(&root)?;
        private_directory(&root.join("profiles"))?;
        Ok(Self { root })
    }

    pub fn list(&self, package_id: &str, schema_identity: &str) -> Result<ProfileListing, Fault> {
        validate_identity(package_id, schema_identity)?;
        let mut result = Vec::new();
        let mut rejected = Vec::new();
        for (profile, _) in self.profiles()? {
            if profile.package_id == package_id {
                match check_binding(&profile, package_id, schema_identity) {
                    Ok(()) => result.push(profile),
                    Err(error) => rejected.push(error),
                }
            }
        }
        result.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        Ok(ProfileListing {
            profiles: result,
            rejected,
        })
    }

    pub fn save(
        &self,
        inventory: &Inventory,
        id: Option<&str>,
        name: &str,
        values: Value,
    ) -> Result<Profile, Fault> {
        validate_name(name)?;
        let schema_identity = identity(&inventory.schema)?;
        validate_identity(&inventory.package_id, &schema_identity)?;
        let mut profiles = self.profiles()?;
        let id = match id {
            Some(id) => {
                validate_id(id)?;
                let index = profiles
                    .iter()
                    .position(|(profile, _)| profile.id == id)
                    .ok_or_else(|| {
                        Fault::new("ProfileNotFound", "saved profile no longer exists")
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
        let values = validate_values(inventory, values)?;
        let profile = Profile {
            version: VERSION,
            id,
            name: name.to_owned(),
            package_id: inventory.package_id.clone(),
            schema_identity,
            values,
        };
        let bytes = encode(&profile, MAX_PROFILE_BYTES)?;
        let retained_bytes: usize = profiles.iter().map(|(_, size)| size).sum();
        if retained_bytes + bytes.len() > MAX_TOTAL_BYTES {
            return Err(limit("stored profiles exceed the 1 MiB aggregate limit"));
        }
        write_atomic(&self.profile_path(&profile.id), &bytes, |from, to| {
            fs::rename(from, to)
        })?;
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
        fs::remove_file(self.profile_path(id)).map_err(|error| storage("delete profile", error))
    }

    pub fn settings(&self) -> Result<Settings, Fault> {
        match self.read_settings()? {
            None => Ok(Settings::default()),
            Some(bytes) => {
                let settings: Settings = decode(&bytes)?;
                validate_settings(&settings)?;
                Ok(settings)
            }
        }
    }

    pub fn save_preferences(&self, preferences: EditableSettings) -> Result<Settings, Fault> {
        let mut settings = self.settings()?;
        settings.locale = preferences.locale;
        settings.gui_log_limit = preferences.gui_log_limit;
        settings.ocr_environment = preferences.ocr_environment;
        settings.notifications = preferences.notifications;
        validate_settings(&settings)?;
        self.write_settings(&settings)?;
        Ok(settings)
    }

    /// Remembers the inspected package location by changing only `package_path` in the stored
    /// document. Optional preferences that are absent stay absent until an explicit Save, so an
    /// older binary can still read the file after a rollback.
    pub fn save_package_hint(&self, package_path: String) -> Result<(), Fault> {
        let (mut stored, mut settings) = match self.read_settings()? {
            None => (required_settings(), Settings::default()),
            Some(bytes) => {
                // The typed parse of the captured bytes is the gate, exactly as in `settings()`:
                // it refuses duplicate keys, unknown fields and out-of-range numbers. A generic
                // `Value` parsed alone keeps the last duplicate, so validating it would accept a
                // document nobody wrote and rewrite the file with it.
                let settings: Settings = decode(&bytes)?;
                validate_settings(&settings)?;
                let stored: Value = decode(&bytes)?;
                (stored, settings)
            }
        };
        if settings.package_path.as_deref() == Some(package_path.as_str()) {
            return Ok(());
        }
        settings.package_path = Some(package_path);
        validate_settings(&settings)?;
        let Some(fields) = stored.as_object_mut() else {
            return Err(malformed());
        };
        fields.insert("package_path".to_owned(), settings.package_path.into());
        self.write_settings(&stored)
    }

    /// Reads the bounded bytes of the stored settings document, or `None` when none has been
    /// written yet. Callers decode this one capture, so no path reads the file twice.
    fn read_settings(&self) -> Result<Option<Vec<u8>>, Fault> {
        check_directory(&self.root)?;
        let path = self.root.join("settings.json");
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(storage("inspect settings", error)),
            Ok(_) => read_bytes(&path, MAX_SETTINGS_BYTES).map(Some),
        }
    }

    fn write_settings(&self, document: &impl Serialize) -> Result<(), Fault> {
        let bytes = encode(document, MAX_SETTINGS_BYTES)?;
        write_atomic(&self.root.join("settings.json"), &bytes, |from, to| {
            fs::rename(from, to)
        })
    }

    fn profile_path(&self, id: &str) -> PathBuf {
        self.root.join("profiles").join(format!("{id}.json"))
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

    fn read_profile(&self, id: &str) -> Result<(Profile, usize), Fault> {
        let result = (|| {
            check_directory(&self.root)?;
            check_directory(&self.root.join("profiles"))?;
            let bytes = read_bytes(&self.profile_path(id), MAX_PROFILE_BYTES)?;
            let profile: Profile = decode(&bytes)?;
            validate_profile(&profile)?;
            if profile.id != id {
                return Err(Fault::new(
                    "ProfileIdentity",
                    "profile ID does not match its filename",
                ));
            }
            Ok((profile, bytes.len()))
        })();
        result.map_err(|fault| profile_fault(fault, id))
    }

    fn profiles(&self) -> Result<Vec<(Profile, usize)>, Fault> {
        let directory = self.root.join("profiles");
        check_directory(&self.root)?;
        check_directory(&directory)?;
        let entries = fs::read_dir(&directory).map_err(|error| storage("list profiles", error))?;
        let mut result = Vec::new();
        let mut bytes = 0;
        for (index, entry) in entries.enumerate() {
            if index >= MAX_DIRECTORY_ENTRIES - 1 {
                return Err(limit(
                    "profile directory has too many files; retain space for an atomic write",
                ));
            }
            let entry = entry.map_err(|error| storage("read profile entry", error))?;
            let filename = entry.file_name();
            let name_bytes = filename.as_encoded_bytes();
            // Other entries are not stored profiles, but still consume directory capacity.
            if !name_bytes.ends_with(b".json") && !name_bytes.ends_with(b".pending") {
                continue;
            }
            let with_filename = |mut fault: Fault| {
                // Escape only the basename; never expose the private storage path.
                fault.context["file"] = json!(name_bytes.escape_ascii().to_string());
                fault
            };
            let name = filename.to_str().ok_or_else(|| {
                with_filename(Fault::new(
                    "Storage",
                    "profile directory contains a non-UTF-8 filename",
                ))
            })?;
            if let Some(id) = name.strip_suffix(".pending") {
                validate_id(id).map_err(with_filename)?;
                checked_file(&entry.path(), MAX_PROFILE_BYTES).map_err(with_filename)?;
                continue;
            }
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
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
}

fn profile_fault(mut fault: Fault, id: &str) -> Fault {
    fault.context["profile_id"] = json!(id);
    fault
}

fn new_id() -> Result<String, Fault> {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Fault::new("Storage", "system clock precedes the profile ID epoch"))?;
    Ok(format!(
        "p-{:032x}-{:08x}-{:016x}",
        time.as_nanos(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

fn validate_id(id: &str) -> Result<(), Fault> {
    if id.len() != 60
        || !id.starts_with("p-")
        || id.as_bytes()[34] != b'-'
        || id.as_bytes()[43] != b'-'
        || !id.as_bytes()[2..].iter().enumerate().all(|(index, byte)| {
            matches!(index, 32 | 41) || byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
        })
    {
        return Err(Fault::new("ProfileIdentity", "invalid profile ID"));
    }
    Ok(())
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

fn validate_profile(profile: &Profile) -> Result<(), Fault> {
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

fn validate_settings(settings: &Settings) -> Result<(), Fault> {
    if settings.version != VERSION {
        return Err(Fault::new(
            "SettingsVersion",
            "unsupported application settings version; original data was preserved",
        ));
    }
    if !(1..=10_000).contains(&settings.gui_log_limit) {
        return Err(Fault::new(
            "Settings",
            "GUI log limit must be an integer from 1 to 10000",
        ));
    }
    if settings.package_path.as_ref().is_some_and(|path| {
        path.trim().is_empty() || path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control)
    }) {
        return Err(Fault::new(
            "Settings",
            "remembered package path must be nonempty text of at most 4096 bytes",
        ));
    }
    if let Some(environment) = &settings.ocr_environment {
        environment.validate()?;
    }
    if !matches!(settings.notifications.visible_count, 1 | 2)
        || !matches!(settings.notifications.timeout_seconds, 5 | 8 | 12)
    {
        return Err(Fault::new(
            "Settings",
            "notification count must be 1 or 2 and timeout must be 5, 8, or 12 seconds",
        ));
    }
    // This is a location hint, not a captured inventory or permission grant.
    Ok(())
}

/// The stored fields every supported settings version requires. A file created by an implicit
/// hint write carries only these plus the hint; optional preferences wait for an explicit Save.
fn required_settings() -> Value {
    let defaults = Settings::default();
    json!({"version": defaults.version, "gui_log_limit": defaults.gui_log_limit})
}

fn private_directory(path: &Path) -> Result<(), Fault> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(Fault::new(
                "Storage",
                "application storage must be a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder
                .create(path)
                .map_err(|error| storage("create storage directory", error))?;
        }
        Err(error) => return Err(storage("inspect storage directory", error)),
    }
    check_directory(path)
}

fn check_directory(path: &Path) -> Result<(), Fault> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| storage("inspect storage directory", error))?;
    if !metadata.file_type().is_dir() {
        return Err(Fault::new(
            "Storage",
            "application storage must be a real directory",
        ));
    }
    #[cfg(unix)]
    if metadata.mode() & 0o077 != 0 {
        return Err(Fault::new(
            "Storage",
            "application storage directory is not private",
        ));
    }
    Ok(())
}

fn checked_file(path: &Path, maximum: usize) -> Result<fs::Metadata, Fault> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| storage("inspect stored file", error))?;
    if !metadata.file_type().is_file() {
        return Err(Fault::new(
            "Storage",
            "stored data must be a regular file, not a link or directory",
        ));
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
        return Err(Fault::new(
            "Storage",
            "stored data must be private and have no hard links",
        ));
    }
    if metadata.len() > maximum as u64 {
        return Err(limit("stored file exceeds its byte bound"));
    }
    Ok(metadata)
}

/// Reads a private regular file of at most `maximum` bytes, refusing one that changes between
/// the metadata check and the open. Decoding is separate so a caller can parse one capture
/// more than once without reading the file again.
fn read_bytes(path: &Path, maximum: usize) -> Result<Vec<u8>, Fault> {
    let before = checked_file(path, maximum)?;
    let file = File::open(path).map_err(|error| storage("open stored file", error))?;
    let opened = file
        .metadata()
        .map_err(|error| storage("inspect opened file", error))?;
    #[cfg(unix)]
    if before.dev() != opened.dev() || before.ino() != opened.ino() || opened.nlink() != 1 {
        return Err(Fault::new(
            "Storage",
            "stored file changed while being opened",
        ));
    }
    if !opened.is_file() || opened.len() != before.len() {
        return Err(Fault::new(
            "Storage",
            "stored file changed while being opened",
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| storage("read stored file", error))?;
    if bytes.len() > maximum {
        return Err(limit("stored file exceeds its byte bound"));
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Fault> {
    serde_json::from_slice(bytes).map_err(|_| malformed())
}

fn exists(path: &Path) -> Result<bool, Fault> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage("inspect storage destination", error)),
    }
}

fn encode<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>, Fault> {
    struct Bounded {
        bytes: Vec<u8>,
        maximum: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("stored JSON exceeds its byte bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| limit("stored JSON cannot be encoded within its byte bound"))?;
    Ok(output.bytes)
}

fn write_atomic(
    destination: &Path,
    bytes: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), Fault> {
    let directory = destination
        .parent()
        .ok_or_else(|| Fault::new("Storage", "storage destination has no parent"))?;
    check_directory(directory)?;
    if exists(destination)? {
        checked_file(destination, MAX_PROFILE_BYTES)?;
    }
    let temporary = destination.with_extension("pending");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|error| storage("create atomic write", error))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| storage("write atomic data", error))?;
        file.flush()
            .map_err(|error| storage("flush atomic data", error))?;
        file.sync_all()
            .map_err(|error| storage("sync atomic data", error))?;
        drop(file);
        replace(&temporary, destination).map_err(|error| storage("replace stored file", error))
    })();
    match result {
        Ok(()) => Ok(()),
        Err(mut fault) => {
            // Only remove the temporary file created by this call, never the original.
            if fs::remove_file(&temporary).is_err() {
                fault.context["temporary_cleanup"] = json!("failed");
            }
            Err(fault)
        }
    }
}

fn limit(message: &str) -> Fault {
    Fault::new("StorageLimit", message)
}

fn malformed() -> Fault {
    Fault::new(
        "StorageFormat",
        "stored JSON is malformed or incompatible; original data was preserved",
    )
}

fn storage(operation: &str, error: io::Error) -> Fault {
    Fault::new("Storage", format!("could not {operation}"))
        .with_context(json!({"operation": operation, "kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mado_runtime_comparison::inventory::{Entries, Entry};
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("mado-storage-{}", new_id().unwrap()));
            private_directory(&path).unwrap();
            Self(path)
        }

        fn store(&self) -> Store {
            Store::new(self.0.clone()).unwrap()
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn inventory() -> Inventory {
        Inventory {
            identity: "fixture".into(),
            package_id: "portable-options".into(),
            sources: BTreeMap::new(),
            assets: BTreeMap::new(),
            schema: json!({
                "version": 1, "type": "object", "additionalProperties": false,
                "properties": {
                    "priorities": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": ["left", "right"]}},
                    "window": {"type": "object", "additionalProperties": false,
                        "properties": {"width": {"type": "integer"}, "height": {"type": "integer"}},
                        "required": ["width", "height"], "default": {"width": 10, "height": 20}},
                    "label": {"type": "string", "default": "secret token"}
                }, "required": ["priorities", "window"]
            }),
            profiles: BTreeMap::new(),
            entries: Entries {
                readiness: Entry {
                    module: "main.js".into(),
                    function: "ready".into(),
                },
                workflow: Entry {
                    module: "main.js".into(),
                    function: "run".into(),
                },
            },
            metadata: json!({}),
            source_maps: BTreeMap::new(),
        }
    }

    fn options() -> Value {
        json!({"priorities": ["right", "left"], "label": "assets/token.png"})
    }

    #[test]
    fn restart_and_rename_preserve_ids_values_and_order() {
        let directory = Directory::new();
        let inventory = inventory();
        let store = directory.store();
        let first = store.save(&inventory, None, "First", options()).unwrap();
        let second_values = json!({"priorities": ["left", "right"]});
        let second = store
            .save(&inventory, None, "Second", second_values.clone())
            .unwrap();
        drop(store);
        let reopened = directory.store();
        let renamed = reopened.rename(&inventory, &first.id, "Renamed").unwrap();
        assert_eq!(renamed.id, first.id);
        assert_eq!(renamed.values, options());
        let profiles = reopened
            .list(&inventory.package_id, &identity(&inventory.schema).unwrap())
            .unwrap()
            .profiles;
        assert_eq!(
            profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            [first.id.as_str(), second.id.as_str()]
        );
        assert_eq!(profiles[1].values, second_values);
        reopened.delete(&first.id).unwrap();
        assert_eq!(
            reopened
                .list(&inventory.package_id, &identity(&inventory.schema).unwrap())
                .unwrap()
                .profiles[0]
                .id,
            second.id
        );
    }

    #[test]
    fn foreign_entries_do_not_block_profile_operations() {
        let directory = Directory::new();
        let store = directory.store();
        let inventory = inventory();
        let profiles = directory.0.join("profiles");
        fs::write(profiles.join(".DS_Store"), b"Finder metadata").unwrap();
        fs::write(profiles.join("notes.txt"), b"Operator notes").unwrap();
        fs::create_dir(profiles.join("archive")).unwrap();
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt;
            fs::write(
                profiles.join(std::ffi::OsStr::from_bytes(b"\xff-metadata")),
                b"Foreign metadata",
            )
            .unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing-target", profiles.join("foreign-link")).unwrap();
        let saved = store.save(&inventory, None, "Original", options()).unwrap();
        let renamed = store.rename(&inventory, &saved.id, "Renamed").unwrap();
        assert_eq!(renamed.id, saved.id);
        let listed = store
            .list(&inventory.package_id, &saved.schema_identity)
            .unwrap();
        assert_eq!(listed.profiles.len(), 1);
        assert_eq!(listed.profiles[0].id, saved.id);
        assert_eq!(listed.profiles[0].name, "Renamed");
        assert_eq!(listed.profiles[0].values, options());
        store.delete(&saved.id).unwrap();
        assert!(
            store
                .list(&inventory.package_id, &saved.schema_identity)
                .unwrap()
                .profiles
                .is_empty()
        );
        assert_eq!(
            fs::read(profiles.join(".DS_Store")).unwrap(),
            b"Finder metadata"
        );
        assert_eq!(
            fs::read(profiles.join("notes.txt")).unwrap(),
            b"Operator notes"
        );
    }

    #[test]
    fn foreign_entries_still_consume_directory_capacity() {
        let directory = Directory::new();
        let store = directory.store();
        let inventory = inventory();
        let profiles = directory.0.join("profiles");
        for index in 0..MAX_DIRECTORY_ENTRIES - 2 {
            fs::write(profiles.join(format!("note-{index}.txt")), b"").unwrap();
        }
        let saved = store
            .save(&inventory, None, "Last slot", options())
            .unwrap();
        store.rename(&inventory, &saved.id, "At capacity").unwrap();
        let path = store.profile_path(&saved.id);
        let before = fs::read(&path).unwrap();
        fs::write(profiles.join("one-too-many.txt"), b"").unwrap();
        assert_eq!(
            store
                .list(&inventory.package_id, &saved.schema_identity)
                .unwrap_err()
                .category,
            "StorageLimit"
        );
        assert_eq!(
            store
                .save(&inventory, None, "Overflow", options())
                .unwrap_err()
                .category,
            "StorageLimit"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn owned_entry_names_and_pending_files_remain_checked() {
        let directory = Directory::new();
        let store = directory.store();
        let inventory = inventory();
        let schema_identity = identity(&inventory.schema).unwrap();
        let profiles = directory.0.join("profiles");
        fs::write(profiles.join(".DS_Store"), b"Finder metadata").unwrap();
        for name in [".json", ".pending"] {
            let path = profiles.join(name);
            fs::write(&path, b"Evidence").unwrap();
            let fault = store
                .list(&inventory.package_id, &schema_identity)
                .unwrap_err();
            assert_eq!(fault.category, "ProfileIdentity");
            assert_eq!(fault.context["file"], name);
            assert_eq!(fs::read(&path).unwrap(), b"Evidence");
            fs::remove_file(path).unwrap();
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = profiles.join(std::ffi::OsStr::from_bytes(b"\xff.json"));
            fs::write(&path, b"Evidence").unwrap();
            let fault = store
                .list(&inventory.package_id, &schema_identity)
                .unwrap_err();
            assert_eq!(fault.category, "Storage");
            assert_eq!(fault.context["file"], r"\xff.json");
            assert_eq!(fs::read(&path).unwrap(), b"Evidence");
            fs::remove_file(path).unwrap();
        }
        let pending_name = format!("{}.pending", new_id().unwrap());
        let pending = profiles.join(&pending_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        options
            .open(&pending)
            .unwrap()
            .set_len(MAX_PROFILE_BYTES as u64 + 1)
            .unwrap();
        let fault = store
            .list(&inventory.package_id, &schema_identity)
            .unwrap_err();
        assert_eq!(fault.category, "StorageLimit");
        assert_eq!(fault.context["file"], pending_name);
        assert_eq!(
            fs::metadata(pending).unwrap().len(),
            MAX_PROFILE_BYTES as u64 + 1
        );
    }

    #[test]
    fn profile_read_failure_preserves_io_context_and_safe_identity() {
        let directory = Directory::new();
        let store = directory.store();
        let id = new_id().unwrap();
        let fault = store.delete(&id).unwrap_err();
        assert_eq!(fault.category, "Storage");
        assert_eq!(fault.context["profile_id"], id);
        assert_eq!(fault.context["operation"], "inspect stored file");
        assert_eq!(fault.context["kind"], "NotFound");
        assert!(fault.context.get("os_code").is_some());
    }

    #[test]
    fn schema_mismatch_and_invalid_replacement_preserve_original_bytes() {
        let directory = Directory::new();
        let store = directory.store();
        let mut inventory = inventory();
        let saved = store.save(&inventory, None, "Original", options()).unwrap();
        let path = store.profile_path(&saved.id);
        let before = fs::read(&path).unwrap();
        let partial = json!({"priorities": ["left"], "window": {"width": 3}});
        assert_eq!(
            store
                .save(&inventory, Some(&saved.id), "Invalid", partial)
                .unwrap_err()
                .category,
            "Profile"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        inventory.schema["properties"]["label"]["maxLength"] = json!(80);
        let changed_identity = identity(&inventory.schema).unwrap();
        let rejected = store
            .list(&inventory.package_id, &changed_identity)
            .unwrap();
        assert!(rejected.profiles.is_empty());
        assert_eq!(rejected.rejected[0].category, "ProfileIdentity");
        assert_eq!(
            store
                .rename(&inventory, &saved.id, "Changed")
                .unwrap_err()
                .category,
            "ProfileIdentity"
        );
        assert_eq!(
            store
                .save(&inventory, Some(&saved.id), "Changed", options())
                .unwrap_err()
                .category,
            "ProfileIdentity"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let compatible = store
            .save(&inventory, None, "Compatible", options())
            .unwrap();
        let listed = store
            .list(&inventory.package_id, &changed_identity)
            .unwrap();
        assert_eq!(listed.profiles[0].id, compatible.id);
        assert_eq!(listed.rejected[0].context["profile_id"], saved.id);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn failed_rename_syscall_keeps_the_previous_profile_readable() {
        let directory = Directory::new();
        let store = directory.store();
        let inventory = inventory();
        let mut profile = store.save(&inventory, None, "Original", options()).unwrap();
        let path = store.profile_path(&profile.id);
        let before = fs::read(&path).unwrap();
        profile.name = "Unsaved".into();
        let bytes = encode(&profile, MAX_PROFILE_BYTES).unwrap();
        let fault = write_atomic(&path, &bytes, |temporary, destination| {
            assert_eq!(fs::read(temporary)?, bytes);
            // A real replacement failure, after writing and syncing the new data.
            fs::rename(temporary, destination.join("not-a-directory"))
        })
        .unwrap_err();
        assert_eq!(fault.context["operation"], "replace stored file");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("pending").exists());
        assert_eq!(
            directory
                .store()
                .list(&inventory.package_id, &identity(&inventory.schema).unwrap())
                .unwrap()
                .profiles[0]
                .name,
            "Original"
        );
    }

    #[test]
    fn invalid_settings_and_incompatible_files_are_never_reset() {
        let directory = Directory::new();
        let store = directory.store();
        store
            .save_preferences(EditableSettings {
                gui_log_limit: 12,
                ..editable_settings()
            })
            .unwrap();
        store
            .save_package_hint("/private/local/package".into())
            .unwrap();
        let path = directory.0.join("settings.json");
        let before = fs::read(&path).unwrap();
        let mut invalid = editable_settings();
        invalid.gui_log_limit = 10001;
        assert!(store.save_preferences(invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        for hint in [
            String::new(),
            "x".repeat(MAX_PATH_BYTES + 1),
            "line\nbreak".to_owned(),
        ] {
            assert_eq!(
                store.save_package_hint(hint).unwrap_err().category,
                "Settings"
            );
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        assert_eq!(directory.store().settings().unwrap().gui_log_limit, 12);
        // Duplicate keys are refused as written: a generic JSON parse would keep the last value,
        // turning an unknown `version: 2` marker into a valid document and rewriting the file.
        for malformed in [
            br#"{"version":1,"gui_log_limit":-1,"package_path":null}"#.as_slice(),
            br#"{"version":1,"gui_log_limit":1.5,"package_path":null}"#.as_slice(),
            br#"{"version":2,"gui_log_limit":12,"package_path":null}"#.as_slice(),
            br#"{"version":2,"version":1,"gui_log_limit":12,"package_path":null}"#.as_slice(),
            br#"{"version":1,"gui_log_limit":12,"gui_log_limit":34,"package_path":null}"#
                .as_slice(),
            br#"{"version":1,"gui_log_limit":12,"locale":"invalid","locale":"ja"}"#
                .as_slice(),
            br#"{"version":1,"gui_log_limit":12,"package_path":null,"notifications":{"visible_count":1,"visible_count":2,"timeout_seconds":8,"show_success":true}}"#
                .as_slice(),
            b"not JSON".as_slice(),
        ] {
            fs::write(&path, malformed).unwrap();
            assert!(store.settings().is_err());
            assert!(store.save_preferences(editable_settings()).is_err());
            assert!(
                store
                    .save_package_hint("/private/local/other".into())
                    .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), malformed);
        }
        // A JSON sequence is refused by the hint write instead of being indexed as an object.
        let sequence = br#"[1,12,"/private/local/package"]"#;
        fs::write(&path, sequence).unwrap();
        assert_eq!(
            store
                .save_package_hint("/private/local/other".into())
                .unwrap_err()
                .category,
            "StorageFormat"
        );
        assert_eq!(fs::read(&path).unwrap(), sequence);
    }

    fn editable_settings() -> EditableSettings {
        EditableSettings {
            locale: Locale::English,
            gui_log_limit: 1000,
            ocr_environment: None,
            notifications: NotificationPreferences::default(),
        }
    }

    #[test]
    fn editable_settings_requires_an_explicit_locale() {
        let mut preferences = serde_json::to_value(editable_settings()).unwrap();
        preferences.as_object_mut().unwrap().remove("locale");
        assert!(serde_json::from_value::<EditableSettings>(preferences.clone()).is_err());
        for (value, expected) in [("en", Locale::English), ("ja", Locale::Japanese)] {
            preferences["locale"] = json!(value);
            let decoded: EditableSettings = serde_json::from_value(preferences.clone()).unwrap();
            assert_eq!(decoded.locale, expected);
        }
    }

    #[test]
    fn invalid_locale_refuses_reads_and_writes_without_changing_bytes() {
        let directory = Directory::new();
        let store = directory.store();
        store.save_preferences(editable_settings()).unwrap();
        let path = directory.0.join("settings.json");
        for locale in [
            Value::Null,
            json!("fr"),
            json!(1),
            json!(true),
            json!(["ja"]),
            json!({"ja": null}),
        ] {
            let mut preferences = serde_json::to_value(editable_settings()).unwrap();
            preferences["locale"] = locale.clone();
            assert!(serde_json::from_value::<EditableSettings>(preferences).is_err());
            let document = json!({
                "version": 1,
                "gui_log_limit": 12,
                "package_path": "old-root",
                "locale": locale
            });
            let bytes = serde_json::to_vec(&document).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert_eq!(store.settings().unwrap_err().category, "StorageFormat");
            assert_eq!(fs::read(&path).unwrap(), bytes);
            for hint in ["old-root", "other-root"] {
                assert_eq!(
                    store.save_package_hint(hint.into()).unwrap_err().category,
                    "StorageFormat"
                );
                assert_eq!(fs::read(&path).unwrap(), bytes);
            }
            assert_eq!(
                store
                    .save_preferences(editable_settings())
                    .unwrap_err()
                    .category,
                "StorageFormat"
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn old_settings_load_without_rewrite_and_explicit_save_persists_preferences() {
        let directory = Directory::new();
        let store = directory.store();
        assert_eq!(store.settings().unwrap().locale, Locale::English);
        assert!(!directory.0.join("settings.json").exists());
        store.save_preferences(editable_settings()).unwrap();
        let path = directory.0.join("settings.json");
        let original = br#"{ "version":1, "gui_log_limit":12, "package_path":"old-root" }"#;
        fs::write(&path, original).unwrap();
        let loaded = store.settings().unwrap();
        assert_eq!(loaded.locale, Locale::English);
        assert_eq!(loaded.notifications, NotificationPreferences::default());
        assert_eq!(fs::read(&path).unwrap(), original);

        let saved = store
            .save_preferences(EditableSettings {
                locale: Locale::Japanese,
                gui_log_limit: 24,
                ocr_environment: None,
                notifications: NotificationPreferences {
                    visible_count: 1,
                    timeout_seconds: 5,
                    show_success: false,
                },
            })
            .unwrap();
        assert_eq!(saved.package_path.as_deref(), Some("old-root"));
        assert_eq!(saved.version, VERSION);
        assert_eq!(saved.locale, Locale::Japanese);
        let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["locale"], "ja");
        let reopened = directory.store().settings().unwrap();
        assert_eq!(reopened.locale, Locale::Japanese);
        assert_eq!(reopened.gui_log_limit, 24);
        assert_eq!(reopened.notifications, saved.notifications);
        assert_eq!(reopened.package_path.as_deref(), Some("old-root"));

        let updated = store
            .save_preferences(EditableSettings {
                locale: saved.locale,
                notifications: NotificationPreferences {
                    visible_count: 2,
                    timeout_seconds: 12,
                    show_success: true,
                },
                ..editable_settings()
            })
            .unwrap();
        assert_eq!(
            directory.store().settings().unwrap().notifications,
            updated.notifications
        );
        assert_eq!(
            directory.store().settings().unwrap().locale,
            Locale::Japanese
        );
        store.save_preferences(editable_settings()).unwrap();
        let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["locale"], "en");
        assert_eq!(
            directory.store().settings().unwrap().locale,
            Locale::English
        );
    }

    #[test]
    fn package_hint_changes_only_the_stored_path_until_an_explicit_save() {
        let directory = Directory::new();
        let store = directory.store();
        let path = directory.0.join("settings.json");
        let stored = || -> Value { serde_json::from_slice(&fs::read(&path).unwrap()).unwrap() };

        // A fresh store gets only the required fields plus the hint; preferences stay implicit.
        store.save_package_hint("first-root".into()).unwrap();
        assert_eq!(
            stored(),
            json!({"version": 1, "gui_log_limit": 1000, "package_path": "first-root"})
        );
        let fresh = directory.store().settings().unwrap();
        assert_eq!(fresh.locale, Locale::English);
        assert_eq!(fresh.package_path.as_deref(), Some("first-root"));
        assert_eq!(fresh.notifications, NotificationPreferences::default());
        assert!(fresh.ocr_environment.is_none());

        // A pre-change file keeps every absent optional field absent after the hint changes.
        let legacy = br#"{ "version":1, "gui_log_limit":12, "package_path":"old-root" }"#;
        fs::write(&path, legacy).unwrap();
        store.save_package_hint("new-root".into()).unwrap();
        assert_eq!(
            stored(),
            json!({"version": 1, "gui_log_limit": 12, "package_path": "new-root"})
        );
        let loaded = directory.store().settings().unwrap();
        assert_eq!(loaded.locale, Locale::English);
        assert_eq!(loaded.package_path.as_deref(), Some("new-root"));
        assert_eq!(loaded.notifications, NotificationPreferences::default());

        // Repeating the current hint does not rewrite the file.
        let unchanged = fs::read(&path).unwrap();
        store.save_package_hint("new-root".into()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), unchanged);

        // Only an explicit Save introduces preferences; later hints preserve them.
        let saved = store
            .save_preferences(EditableSettings {
                locale: Locale::Japanese,
                gui_log_limit: 24,
                notifications: NotificationPreferences {
                    visible_count: 1,
                    timeout_seconds: 5,
                    show_success: false,
                },
                ..editable_settings()
            })
            .unwrap();
        assert_eq!(
            stored()["notifications"],
            json!({"visible_count": 1, "timeout_seconds": 5, "show_success": false})
        );
        store.save_package_hint("third-root".into()).unwrap();
        let mut expected = serde_json::to_value(&saved).unwrap();
        expected["package_path"] = json!("third-root");
        assert_eq!(stored(), expected);
        let reopened = directory.store().settings().unwrap();
        assert_eq!(reopened.locale, Locale::Japanese);
        assert_eq!(reopened.package_path.as_deref(), Some("third-root"));
        assert_eq!(reopened.notifications, saved.notifications);
        assert_eq!(reopened.gui_log_limit, 24);
    }

    #[test]
    fn invalid_notification_values_and_incompatible_settings_preserve_bytes() {
        let directory = Directory::new();
        let store = directory.store();
        let path = directory.0.join("settings.json");
        store.save_preferences(editable_settings()).unwrap();
        let before = fs::read(&path).unwrap();
        for (visible_count, timeout_seconds) in [(0, 8), (3, 8), (2, 0), (2, 13)] {
            let mut invalid = editable_settings();
            invalid.notifications.visible_count = visible_count;
            invalid.notifications.timeout_seconds = timeout_seconds;
            assert_eq!(
                store.save_preferences(invalid).unwrap_err().category,
                "Settings"
            );
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        let mut invalid = editable_settings();
        invalid.gui_log_limit = 0;
        assert_eq!(
            store.save_preferences(invalid).unwrap_err().category,
            "Settings"
        );
        assert_eq!(fs::read(&path).unwrap(), before);

        let base = json!({
            "version": 1,
            "gui_log_limit": 12,
            "package_path": null,
            "notifications": {"visible_count": 2, "timeout_seconds": 8, "show_success": true}
        });
        let mut malformed = Vec::new();
        for (field, value) in [
            ("visible_count", json!(3)),
            ("timeout_seconds", json!(7)),
            ("show_success", json!("true")),
            ("extra", json!(true)),
        ] {
            let mut candidate = base.clone();
            candidate["notifications"][field] = value;
            malformed.push(candidate);
        }
        let mut missing = base.clone();
        missing["notifications"]
            .as_object_mut()
            .unwrap()
            .remove("show_success");
        malformed.push(missing);
        let mut null = base.clone();
        null["notifications"] = Value::Null;
        malformed.push(null);
        let mut unknown_version = base.clone();
        unknown_version["version"] = json!(2);
        malformed.push(unknown_version);
        let mut unknown_setting = base.clone();
        unknown_setting["new_setting"] = json!(true);
        malformed.push(unknown_setting);
        for candidate in malformed {
            let bytes = serde_json::to_vec(&candidate).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert!(store.settings().is_err());
            assert!(store.save_preferences(editable_settings()).is_err());
            assert!(store.save_package_hint("other-root".into()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn stale_dialog_save_preserves_newer_package_hint() {
        let directory = Directory::new();
        let store = directory.store();
        store.save_package_hint("first-root".into()).unwrap();
        let mut dialog = editable_settings();
        dialog.locale = Locale::Japanese;
        dialog.gui_log_limit = 31;
        dialog.notifications.show_success = false;

        store
            .save_package_hint("newly-inspected-root".into())
            .unwrap();
        let saved = store.save_preferences(dialog).unwrap();
        assert_eq!(saved.locale, Locale::Japanese);
        assert_eq!(saved.package_path.as_deref(), Some("newly-inspected-root"));
        let reopened = directory.store().settings().unwrap();
        assert_eq!(reopened.locale, Locale::Japanese);
        assert_eq!(
            reopened.package_path.as_deref(),
            Some("newly-inspected-root")
        );
        assert_eq!(reopened.gui_log_limit, 31);
        assert!(!reopened.notifications.show_success);
    }

    #[test]
    fn failed_settings_atomic_writes_preserve_previous_bytes() {
        let directory = Directory::new();
        let store = directory.store();
        let path = directory.0.join("settings.json");
        store
            .save_preferences(EditableSettings {
                locale: Locale::Japanese,
                ..editable_settings()
            })
            .unwrap();
        let before = fs::read(&path).unwrap();
        let pending = path.with_extension("pending");
        fs::write(&pending, b"unfinished previous write").unwrap();
        let fault = store.save_preferences(editable_settings()).unwrap_err();
        assert_eq!(fault.context["operation"], "create atomic write");
        let fault = store.save_package_hint("blocked-root".into()).unwrap_err();
        assert_eq!(fault.context["operation"], "create atomic write");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&pending).unwrap(), b"unfinished previous write");
        fs::remove_file(&pending).unwrap();
        assert!(directory.store().settings().unwrap().package_path.is_none());
        assert_eq!(
            directory.store().settings().unwrap().locale,
            Locale::Japanese
        );

        let mut updated = store.settings().unwrap();
        updated.locale = Locale::English;
        updated.gui_log_limit = 31;
        let bytes = encode(&updated, MAX_SETTINGS_BYTES).unwrap();
        let fault = write_atomic(&path, &bytes, |temporary, destination| {
            assert_eq!(fs::read(temporary)?, bytes);
            fs::rename(temporary, destination.join("not-a-directory"))
        })
        .unwrap_err();
        assert_eq!(fault.context["operation"], "replace stored file");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!pending.exists());
        assert_eq!(directory.store().settings().unwrap().gui_log_limit, 1000);
        assert_eq!(
            directory.store().settings().unwrap().locale,
            Locale::Japanese
        );
    }

    #[test]
    fn environment_settings_are_structural_and_preserve_existing_data() {
        use mado_runtime_comparison::environment::{
            G004_PROFILE, LANGUAGE, PROVIDER, RUNTIME_PROFILE,
        };

        let directory = Directory::new();
        let store = directory.store();
        store.save_preferences(editable_settings()).unwrap();
        let profile = store
            .save(&inventory(), None, "Portable", options())
            .unwrap();
        let profile_path = store.profile_path(&profile.id);
        let profile_before = fs::read(&profile_path).unwrap();
        let path = directory.0.join("settings.json");
        let original =
            br#"{ "version":1, "gui_log_limit":12, "package_path":"remembered-package" }"#;
        fs::write(&path, original).unwrap();
        let settings = store.settings().unwrap();
        assert!(settings.ocr_environment.is_none());
        assert_eq!(fs::read(&path).unwrap(), original);
        let environment = OcrEnvironment {
            model: G004_PROFILE.into(),
            profile: G004_PROFILE.into(),
            language: LANGUAGE.into(),
            provider: PROVIDER.into(),
            runtime_profile: RUNTIME_PROFILE.into(),
            model_root: directory
                .0
                .join("missing-models")
                .to_string_lossy()
                .into_owned(),
            runtime_path: directory
                .0
                .join("missing-runtime")
                .to_string_lossy()
                .into_owned(),
            native_library_paths: vec![
                directory
                    .0
                    .join("missing-library")
                    .to_string_lossy()
                    .into_owned(),
            ],
        };
        store
            .save_preferences(EditableSettings {
                locale: settings.locale,
                gui_log_limit: settings.gui_log_limit,
                ocr_environment: Some(environment.clone()),
                notifications: settings.notifications,
            })
            .unwrap();
        let reopened = directory.store().settings().unwrap();
        assert_eq!(reopened.ocr_environment, Some(environment));
        assert_eq!(reopened.gui_log_limit, 12);
        assert_eq!(reopened.package_path.as_deref(), Some("remembered-package"));
        assert_eq!(fs::read(profile_path).unwrap(), profile_before);
        let saved = fs::read(&path).unwrap();
        let mut invalid = reopened.clone();
        invalid.ocr_environment.as_mut().unwrap().provider = "cuda".into();
        assert!(
            store
                .save_preferences(EditableSettings {
                    locale: invalid.locale,
                    gui_log_limit: invalid.gui_log_limit,
                    ocr_environment: invalid.ocr_environment,
                    notifications: invalid.notifications,
                })
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), saved);
        for field in ["provider", "target"] {
            let mut corrupted = serde_json::to_value(&reopened).unwrap();
            corrupted["ocr_environment"][field] = json!("unauthorized");
            let bytes = serde_json::to_vec(&corrupted).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert!(store.settings().is_err());
            assert!(store.save_package_hint("other-package".into()).is_err());
            assert!(store.save_preferences(editable_settings()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn portability_checks_fields_and_paths_not_arbitrary_string_secrets() {
        for portable in [
            json!({"label": "secret token password"}),
            json!({"asset": "assets/right.png"}),
            json!({"key": "Enter", "item": "game token", "label": "A: strategy"}),
        ] {
            portable_values(&portable).unwrap();
        }
        for forbidden in [
            json!({"nested": {"api_key": "value"}}),
            json!({"token": "value"}),
            json!({"inputAuthority": false}),
            json!({"target_executable_path": "relative-game"}),
            json!({"asset": "/private/model"}),
            json!({"asset": "C:\\models\\ocr"}),
            json!({"asset": "../outside"}),
        ] {
            assert_eq!(
                portable_values(&forbidden).unwrap_err().category,
                "ProfileAuthority"
            );
        }
        let directory = Directory::new();
        let mut inventory = inventory();
        inventory.schema["properties"]["label"]["default"] = json!("/private/default-model");
        assert_eq!(
            directory
                .store()
                .save(
                    &inventory,
                    None,
                    "No authority",
                    json!({"priorities": ["left"]})
                )
                .unwrap_err()
                .category,
            "ProfileAuthority"
        );
    }

    #[test]
    fn malformed_profile_and_unsafe_filename_preserve_stored_data() {
        let directory = Directory::new();
        let store = directory.store();
        let inventory = inventory();
        let profile = store.save(&inventory, None, "Original", options()).unwrap();
        let path = store.profile_path(&profile.id);
        let profiles = directory.0.join("profiles");
        fs::write(profiles.join(".DS_Store"), b"Finder metadata").unwrap();
        fs::write(profiles.join("notes.txt"), b"Operator notes").unwrap();
        let mut incompatible = serde_json::to_value(&profile).unwrap();
        incompatible["version"] = json!(2);
        let mut mismatched = serde_json::to_value(&profile).unwrap();
        mismatched["id"] = json!(new_id().unwrap());
        let mut authority = serde_json::to_value(&profile).unwrap();
        authority["values"] = json!({"nested": {"api_key": "credential"}});
        for (bytes, category, field) in [
            (b"not JSON".to_vec(), "StorageFormat", None),
            (
                serde_json::to_vec(&incompatible).unwrap(),
                "ProfileVersion",
                None,
            ),
            (
                serde_json::to_vec(&mismatched).unwrap(),
                "ProfileIdentity",
                None,
            ),
            (
                serde_json::to_vec(&authority).unwrap(),
                "ProfileAuthority",
                Some("$.nested.api_key"),
            ),
        ] {
            fs::write(&path, &bytes).unwrap();
            let fault = store
                .list(&inventory.package_id, &profile.schema_identity)
                .unwrap_err();
            assert_eq!(fault.category, category);
            assert_eq!(fault.context["profile_id"], profile.id);
            if let Some(field) = field {
                assert_eq!(fault.context["field"], field);
            }
            for id in [None, Some(profile.id.as_str())] {
                let fault = store
                    .save(&inventory, id, "Replacement", options())
                    .unwrap_err();
                assert_eq!(fault.category, category);
                assert_eq!(fault.context["profile_id"], profile.id);
            }
            let fault = store
                .rename(&inventory, &profile.id, "Renamed")
                .unwrap_err();
            assert_eq!(fault.category, category);
            assert_eq!(fault.context["profile_id"], profile.id);
            let fault = store.delete(&profile.id).unwrap_err();
            assert_eq!(fault.category, category);
            assert_eq!(fault.context["profile_id"], profile.id);
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        assert!(store.delete("../settings").is_err());
    }

    #[test]
    fn profile_size_and_count_bounds_preserve_saved_profiles() {
        let directory = Directory::new();
        let store = directory.store();
        let inventory = inventory();
        let profile = store.save(&inventory, None, "Original", options()).unwrap();
        let path = store.profile_path(&profile.id);
        let before = fs::read(&path).unwrap();
        let oversized = json!({"priorities": ["left"], "label": "a".repeat(MAX_PROFILE_BYTES)});
        assert_eq!(
            store
                .save(&inventory, Some(&profile.id), "Too large", oversized)
                .unwrap_err()
                .category,
            "StorageLimit"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        for index in 1..MAX_PROFILES {
            store
                .save(&inventory, None, &format!("Profile {index}"), options())
                .unwrap();
        }
        assert_eq!(
            store
                .save(&inventory, None, "Overflow", options())
                .unwrap_err()
                .category,
            "StorageLimit"
        );
        let renamed = store
            .rename(&inventory, &profile.id, "Renamed at capacity")
            .unwrap();
        assert_eq!(renamed.id, profile.id);
        assert_eq!(
            store
                .list(&inventory.package_id, &profile.schema_identity)
                .unwrap()
                .profiles
                .len(),
            MAX_PROFILES
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_insecure_directories_are_refused_without_chmod() {
        for relative in ["", "profiles"] {
            let directory = Directory::new();
            let path = directory.0.join(relative);
            if !relative.is_empty() {
                fs::create_dir(&path).unwrap();
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            let fault = Store::new(directory.0.clone())
                .err()
                .expect("existing shared storage must be refused");
            assert_eq!(fault.category, "Storage");
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_storage_refuses_symlink_profile_authority() {
        let directory = Directory::new();
        let store = directory.store();
        let inventory = inventory();
        let profile = store.save(&inventory, None, "Original", options()).unwrap();
        let path = store.profile_path(&profile.id);
        assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(directory.0.join("profiles")).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let outside = directory.0.join("original.json");
        fs::rename(&path, &outside).unwrap();
        let before = fs::read(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(
            store
                .list(&inventory.package_id, &profile.schema_identity)
                .is_err()
        );
        assert!(
            store
                .save(&inventory, Some(&profile.id), "Replacement", options())
                .is_err()
        );
        assert!(store.delete(&profile.id).is_err());
        assert_eq!(fs::read(&outside).unwrap(), before);
    }
}
