use crate::configuration::{capture, publish_no_replace};
use mado_runtime_comparison::environment::OcrEnvironment;
use mado_runtime_comparison::host::resolve_options;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, identity};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

const VERSION: u32 = 1;
const MAX_PROFILES: usize = 64;
const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 128;
const MAX_SETTINGS_BYTES: usize = 32 * 1024;
const MAX_TAB_BYTES: usize = 128 * 1024;
const MAX_TABS: usize = 64;
const MAX_OPEN_TABS: usize = 8;
const MAX_PACKAGES: usize = 16;
const MAX_MANAGED_FILES: usize = 4096;
const MAX_MANAGED_BYTES: usize = 16 * 1024 * 1024;
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
    #[serde(default)]
    pub backup_directory: Option<String>,
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
    #[serde(default)]
    pub backup_directory: Option<String>,
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
            backup_directory: None,
        }
    }
}

#[derive(Debug)]
pub struct ProfileListing {
    pub profiles: Vec<Profile>,
    pub rejected: Vec<Fault>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TabRecord {
    pub version: u32,
    pub internal_name: String,
    pub display_name: String,
    pub open: bool,
    pub packages: Vec<PackageReference>,
    pub selected_package_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageReference {
    pub package_id: String,
    pub source: PackageSource,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageSource {
    Directory { path: String },
    CustomArchive { path: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct TabListing {
    pub tabs: Vec<TabRecord>,
    pub faults: Vec<Fault>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LegacyImport {
    pub imported: Vec<String>,
    pub unchanged: Vec<String>,
    pub fault: Option<Fault>,
}

/// A package owner, not a global catalog. Callers serialize it with the Store mutex.
pub struct ProfileStore {
    root: PathBuf,
    tab_name: String,
    package_id: String,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Result<Self, Fault> {
        if exists(&root)? {
            check_directory(&root)?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn initialize(&self, preferences: EditableSettings) -> Result<Settings, Fault> {
        let settings = Settings {
            locale: preferences.locale,
            gui_log_limit: preferences.gui_log_limit,
            ocr_environment: preferences.ocr_environment,
            notifications: preferences.notifications,
            backup_directory: preferences.backup_directory,
            ..Settings::default()
        };
        validate_settings(&settings)?;
        let bytes = encode(&settings, MAX_SETTINGS_BYTES)?;
        private_directory(&self.root)?;
        check_alias(&self.root, "settings.json")?;
        if exists(&self.root.join("settings.json"))? {
            return Err(Fault::new(
                "SettingsPresent",
                "settings already exist; initialization never replaces them",
            ));
        }
        check_budget(&self.root, "settings.json", bytes.len())?;
        write_atomic(&self.root.join("settings.json"), &bytes, publish_no_replace)?;
        Ok(settings)
    }

    pub fn settings(&self) -> Result<Settings, Fault> {
        let bytes = self.read_settings()?.ok_or_else(settings_missing)?;
        let settings: Settings = decode(&bytes)?;
        validate_settings(&settings)?;
        Ok(settings)
    }

    pub fn save_preferences(&self, preferences: EditableSettings) -> Result<Settings, Fault> {
        let mut settings = self.settings()?;
        settings.locale = preferences.locale;
        settings.gui_log_limit = preferences.gui_log_limit;
        settings.ocr_environment = preferences.ocr_environment;
        settings.notifications = preferences.notifications;
        settings.backup_directory = preferences.backup_directory;
        validate_settings(&settings)?;
        self.write_settings(&settings)?;
        Ok(settings)
    }

    fn read_settings(&self) -> Result<Option<Vec<u8>>, Fault> {
        if !exists(&self.root)? {
            return Ok(None);
        }
        check_directory(&self.root)?;
        check_alias(&self.root, "settings.json")?;
        let path = self.root.join("settings.json");
        if !exists(&path)? {
            return Ok(None);
        }
        read_bytes(&path, MAX_SETTINGS_BYTES).map(Some)
    }

    fn write_settings(&self, document: &impl Serialize) -> Result<(), Fault> {
        let bytes = encode(document, MAX_SETTINGS_BYTES)?;
        check_budget(&self.root, "settings.json", bytes.len())?;
        write_atomic(&self.root.join("settings.json"), &bytes, |from, to| {
            fs::rename(from, to)
        })
    }

    pub fn tabs(&self) -> Result<TabListing, Fault> {
        check_directory(&self.root)?;
        check_alias(&self.root, "tabs")?;
        let directory = self.root.join("tabs");
        let mut listing = TabListing {
            tabs: Vec::new(),
            faults: Vec::new(),
        };
        if !exists(&directory)? {
            return Ok(listing);
        }
        check_directory(&directory)?;
        let entries = bounded_entries(&directory)?;
        for entry in entries {
            let filename = entry.file_name();
            let name = filename.to_string_lossy();
            let result = (|| {
                let metadata = fs::symlink_metadata(entry.path())
                    .map_err(|error| storage("inspect Tab container", error))?;
                if metadata.is_file() {
                    return Ok(None);
                }
                validate_internal_name(&name)?;
                check_directory(&entry.path())?;
                if !exists(&entry.path().join("tab.config"))? {
                    if bounded_entries(&entry.path())?.is_empty() {
                        return Ok(None);
                    }
                    return Err(Fault::new(
                        "TabOrphan",
                        "nonempty Tab container has no tab.config; original data was preserved",
                    ));
                }
                self.tab(&name).map(Some)
            })();
            match result {
                Ok(Some(tab)) => listing.tabs.push(tab),
                Ok(None) => {}
                Err(fault) => listing.faults.push(tab_fault(fault, &name)),
            }
            if listing.tabs.len() + listing.faults.len() > MAX_TABS {
                return Err(limit("saved Tab count exceeds 64"));
            }
        }
        if listing.tabs.iter().filter(|tab| tab.open).count() > MAX_OPEN_TABS {
            return Err(limit("saved open Tab count exceeds eight"));
        }
        listing
            .tabs
            .sort_by(|a, b| a.internal_name.cmp(&b.internal_name));
        Ok(listing)
    }

    pub fn tab(&self, name: &str) -> Result<TabRecord, Fault> {
        let result = (|| {
            let directory = self.tab_directory(name)?;
            check_directory(&directory)?;
            check_alias(&directory, "tab.config")?;
            let tab: TabRecord =
                decode(&read_bytes(&directory.join("tab.config"), MAX_TAB_BYTES)?)?;
            validate_tab(&tab)?;
            if tab.internal_name != name {
                return Err(Fault::new(
                    "TabIdentity",
                    "Tab name does not match its containing directory",
                ));
            }
            for package in &tab.packages {
                check_alias(&directory, &package.package_id)?;
                let path = directory.join(&package.package_id);
                if exists(&path)? {
                    check_directory(&path)?;
                }
            }
            Ok(tab)
        })();
        result.map_err(|fault| tab_fault(fault, name))
    }

    pub fn create_tab(&self, internal_name: &str, display_name: &str) -> Result<TabRecord, Fault> {
        let tab = TabRecord {
            version: VERSION,
            internal_name: internal_name.to_owned(),
            display_name: if display_name.is_empty() {
                internal_name
            } else {
                display_name
            }
            .to_owned(),
            open: true,
            packages: Vec::new(),
            selected_package_id: None,
        };
        validate_tab(&tab)?;
        let listing = self.tabs()?;
        if listing.tabs.len() + listing.faults.len() >= MAX_TABS {
            return Err(limit("saved Tab count exceeds 64"));
        }
        ensure_open_slot(&listing)?;
        let tabs = self.root.join("tabs");
        private_directory(&tabs)?;
        check_alias(&tabs, internal_name)?;
        let directory = tabs.join(internal_name);
        if exists(&directory)? {
            check_directory(&directory)?;
            if !bounded_entries(&directory)?.is_empty() {
                return Err(tab_fault(
                    Fault::new(
                        "TabExists",
                        "this internal name is already saved or contains retained data",
                    ),
                    internal_name,
                ));
            }
        }
        let bytes = encode(&tab, MAX_TAB_BYTES)?;
        check_budget(
            &self.root,
            &format!("tabs/{internal_name}/tab.config"),
            bytes.len(),
        )?;
        private_directory(&directory)?;
        write_atomic(&directory.join("tab.config"), &bytes, publish_no_replace)?;
        Ok(tab)
    }

    pub fn set_tab_open(&self, name: &str, open: bool) -> Result<TabRecord, Fault> {
        let mut tab = self.tab(name)?;
        if tab.open == open {
            return Ok(tab);
        }
        if open {
            ensure_open_slot(&self.tabs()?)?;
        }
        tab.open = open;
        self.write_tab(&tab)?;
        Ok(tab)
    }

    pub fn bind_package(
        &self,
        name: &str,
        package_id: &str,
        path: &Path,
    ) -> Result<TabRecord, Fault> {
        validate_package_id(package_id)?;
        let mut tab = self.tab(name)?;
        if !tab.open {
            return Err(tab_fault(
                Fault::new("TabClosed", "reopen the Tab before binding a package"),
                name,
            ));
        }
        let path = path
            .to_str()
            .ok_or_else(|| Fault::new("PackageSource", "package source must be UTF-8"))?;
        validate_source_path(path)?;
        let directory = self.tab_directory(name)?;
        check_alias(&directory, package_id)?;
        if exists(&directory.join(package_id))? {
            check_directory(&directory.join(package_id))?;
        }
        let source = PackageSource::Directory {
            path: path.to_owned(),
        };
        if let Some(reference) = tab
            .packages
            .iter_mut()
            .find(|reference| reference.package_id == package_id)
        {
            reference.source = source;
        } else {
            tab.packages.push(PackageReference {
                package_id: package_id.to_owned(),
                source,
            });
        }
        tab.selected_package_id = Some(package_id.to_owned());
        self.write_tab(&tab)?;
        Ok(tab)
    }

    fn tab_directory(&self, name: &str) -> Result<PathBuf, Fault> {
        validate_internal_name(name)?;
        check_directory(&self.root)?;
        check_alias(&self.root, "tabs")?;
        let directory = self.root.join("tabs");
        check_directory(&directory)?;
        check_alias(&directory, name)?;
        Ok(directory.join(name))
    }

    fn write_tab(&self, tab: &TabRecord) -> Result<(), Fault> {
        validate_tab(tab)?;
        let directory = self.tab_directory(&tab.internal_name)?;
        let bytes = encode(tab, MAX_TAB_BYTES)?;
        check_budget(
            &self.root,
            &format!("tabs/{}/tab.config", tab.internal_name),
            bytes.len(),
        )?;
        write_atomic(&directory.join("tab.config"), &bytes, |from, to| {
            fs::rename(from, to)
        })
        .map_err(|fault| tab_fault(fault, &tab.internal_name))
    }

    pub fn profile_store(&self, tab_name: &str, package_id: &str) -> Result<ProfileStore, Fault> {
        let store = ProfileStore {
            root: self.root.clone(),
            tab_name: tab_name.to_owned(),
            package_id: package_id.to_owned(),
        };
        store.check_owner()?;
        Ok(store)
    }

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
        // Refuse an unresolved write, including an unrelated owner pending file.
        capture(&self.root)?;
        fs::remove_file(self.profile_path(id))
            .map_err(|error| self.owner_fault(storage("delete profile", error)))
    }

    fn directory(&self) -> PathBuf {
        self.root
            .join("tabs")
            .join(&self.tab_name)
            .join(&self.package_id)
    }

    fn profile_path(&self, id: &str) -> PathBuf {
        self.directory().join(format!("{id}.config"))
    }

    fn check_package(&self, package_id: &str) -> Result<(), Fault> {
        if self.package_id != package_id {
            return Err(self.owner_fault(Fault::new(
                "ProfileIdentity",
                "package does not belong to this profile store",
            )));
        }
        Ok(())
    }

    fn check_owner(&self) -> Result<(), Fault> {
        let result = (|| {
            validate_package_id(&self.package_id)?;
            let tab = Store {
                root: self.root.clone(),
            }
            .tab(&self.tab_name)?;
            if !tab.open {
                return Err(Fault::new(
                    "TabClosed",
                    "reopen the Tab before using its profiles",
                ));
            }
            if !tab
                .packages
                .iter()
                .any(|reference| reference.package_id == self.package_id)
            {
                return Err(Fault::new(
                    "ProfileIdentity",
                    "package is not bound to this Tab",
                ));
            }
            if exists(&self.directory())? {
                check_directory(&self.directory())?;
            }
            Ok(())
        })();
        result.map_err(|fault| self.owner_fault(fault))
    }

    fn owner_fault(&self, mut fault: Fault) -> Fault {
        fault.context["internal_name"] = json!(self.tab_name);
        fault.context["package_id"] = json!(self.package_id);
        fault
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
            if name.ends_with(".pending") {
                checked_file(&entry.path(), MAX_PROFILE_BYTES).map_err(with_filename)?;
                return Err(with_filename(Fault::new(
                    "StoragePending",
                    "package configuration has an unresolved pending file",
                )));
            }
            if name == "target.config" {
                checked_file(&entry.path(), MAX_PROFILE_BYTES).map_err(with_filename)?;
                continue;
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

fn settings_missing() -> Fault {
    Fault::new(
        "SettingsMissing",
        "application settings are missing; explicit initialization is required",
    )
}

fn tab_fault(mut fault: Fault, name: &str) -> Fault {
    fault.context["internal_name"] = json!(name);
    fault
}

fn ensure_open_slot(listing: &TabListing) -> Result<(), Fault> {
    // An unreadable record may be open; reserve its slot rather than exceed the bound.
    if listing.tabs.iter().filter(|tab| tab.open).count() + listing.faults.len() >= MAX_OPEN_TABS {
        return Err(limit(
            "open Tab count exceeds eight, including unresolved saved records",
        ));
    }
    Ok(())
}

fn bounded_entries(directory: &Path) -> Result<Vec<fs::DirEntry>, Fault> {
    check_directory(directory)?;
    let mut entries = Vec::new();
    for entry in
        fs::read_dir(directory).map_err(|error| storage("list storage directory", error))?
    {
        if entries.len() >= MAX_DIRECTORY_ENTRIES - 1 {
            return Err(limit(
                "storage directory has too many entries; retain space for an atomic write",
            ));
        }
        entries.push(entry.map_err(|error| storage("read storage entry", error))?);
    }
    Ok(entries)
}

pub(crate) fn filesystem_key(name: &str) -> String {
    name.nfd()
        .flat_map(char::to_lowercase)
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .nfd()
        .collect()
}

fn check_alias(directory: &Path, name: &str) -> Result<(), Fault> {
    let key = filesystem_key(name);
    #[cfg(unix)]
    let target = match fs::symlink_metadata(directory.join(name)) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(storage("inspect component identity", error)),
    };
    for entry in bounded_entries(directory)? {
        let filename = entry.file_name();
        if let Some(existing) = filename.to_str() {
            if existing != name && filesystem_key(existing) == key {
                return Err(Fault::new(
                    "StorageAlias",
                    "component aliases another retained filesystem identity",
                ));
            }
            #[cfg(unix)]
            if existing != name {
                if let Some(target) = &target {
                    let metadata = fs::symlink_metadata(entry.path())
                        .map_err(|error| storage("inspect component identity", error))?;
                    if target.dev() == metadata.dev() && target.ino() == metadata.ino() {
                        return Err(Fault::new(
                            "StorageAlias",
                            "component resolves to another retained filesystem identity",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_internal_name(name: &str) -> Result<(), Fault> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(Fault::new(
            "TabName",
            "internal name must contain 1 to 64 ASCII letters, digits, underscores or hyphens",
        ));
    }
    Ok(())
}

pub(crate) fn validate_package_id(id: &str) -> Result<(), Fault> {
    let stem = id
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if id.is_empty()
        || id.len() > 240
        || id.trim() != id
        || id.ends_with('.')
        || id == "."
        || id == ".."
        || reserved
        || id.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
        || matches!(filesystem_key(id).as_str(), "tab.config" | "tab.pending")
    {
        return Err(Fault::new(
            "ProfileIdentity",
            "invalid contained package identity",
        ));
    }
    Ok(())
}

fn validate_source_path(path: &str) -> Result<(), Fault> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || !Path::new(path).is_absolute()
        || path.chars().any(char::is_control)
    {
        return Err(Fault::new(
            "PackageSource",
            "package source must be an absolute UTF-8 path of at most 4096 bytes",
        ));
    }
    Ok(())
}

pub(crate) fn validate_tab(tab: &TabRecord) -> Result<(), Fault> {
    if tab.version != VERSION {
        return Err(Fault::new(
            "TabVersion",
            "unsupported Tab version; original data was preserved",
        ));
    }
    validate_internal_name(&tab.internal_name)?;
    if tab.display_name.trim().is_empty()
        || tab.display_name.chars().count() > 80
        || tab.display_name.chars().any(char::is_control)
    {
        return Err(Fault::new(
            "TabName",
            "display name must contain 1 to 80 Unicode scalars, be nonblank and contain no controls",
        ));
    }
    if tab.packages.len() > MAX_PACKAGES {
        return Err(limit("package references per Tab exceed 16"));
    }
    let mut identities = BTreeSet::new();
    for reference in &tab.packages {
        validate_package_id(&reference.package_id)?;
        if !identities.insert(filesystem_key(&reference.package_id)) {
            return Err(Fault::new(
                "StorageAlias",
                "Tab package references contain duplicate or aliased identities",
            ));
        }
        match &reference.source {
            PackageSource::Directory { path } | PackageSource::CustomArchive { path } => {
                validate_source_path(path)?
            }
        }
    }
    if tab.selected_package_id.as_ref().is_some_and(|id| {
        !tab.packages
            .iter()
            .any(|reference| &reference.package_id == id)
    }) {
        return Err(Fault::new(
            "TabIdentity",
            "selected package is not recorded in this Tab",
        ));
    }
    Ok(())
}

fn check_budget(root: &Path, relative: &str, size: usize) -> Result<(), Fault> {
    let current = capture(root)?;
    let previous = current.files.get(relative);
    let count = current.files.len() + usize::from(previous.is_none());
    let bytes =
        current.files.values().map(Vec::len).sum::<usize>() - previous.map_or(0, Vec::len) + size;
    if count > MAX_MANAGED_FILES || bytes > MAX_MANAGED_BYTES {
        return Err(limit("managed configuration exceeds 4096 files or 16 MiB"));
    }
    Ok(())
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

pub(crate) fn validate_id(id: &str) -> Result<(), Fault> {
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

pub(crate) fn validate_settings(settings: &Settings) -> Result<(), Fault> {
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
    if settings.backup_directory.as_ref().is_some_and(|path| {
        path.trim().is_empty()
            || path.len() > MAX_PATH_BYTES
            || !Path::new(path).is_absolute()
            || path.chars().any(char::is_control)
    }) {
        return Err(Fault::new(
            "Settings",
            "backup directory must be an absolute path of at most 4096 bytes",
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

pub(crate) fn private_directory(path: &Path) -> Result<(), Fault> {
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

pub(crate) fn check_directory(path: &Path) -> Result<(), Fault> {
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

pub(crate) fn checked_file(path: &Path, maximum: usize) -> Result<fs::Metadata, Fault> {
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
pub(crate) fn read_bytes(path: &Path, maximum: usize) -> Result<Vec<u8>, Fault> {
    let before = checked_file(path, maximum)?;
    let mut file = File::open(path).map_err(|error| storage("open stored file", error))?;
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
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| storage("read stored file", error))?;
    if bytes.len() > maximum {
        return Err(limit("stored file exceeds its byte bound"));
    }
    let after = checked_file(path, maximum)?;
    if bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
    {
        return Err(Fault::new(
            "StorageChanged",
            "stored file changed while being read",
        ));
    }
    #[cfg(unix)]
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(Fault::new(
            "StorageChanged",
            "stored file changed while being read",
        ));
    }
    Ok(bytes)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Fault> {
    if bytes
        .iter()
        .copied()
        .find(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        != Some(b'{')
    {
        return Err(malformed());
    }
    serde_json::from_slice(bytes).map_err(|_| malformed())
}

pub(crate) fn exists(path: &Path) -> Result<bool, Fault> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage("inspect storage destination", error)),
    }
}

pub(crate) fn encode<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>, Fault> {
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

pub(crate) fn write_atomic(
    destination: &Path,
    bytes: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), Fault> {
    let directory = destination
        .parent()
        .ok_or_else(|| Fault::new("Storage", "storage destination has no parent"))?;
    check_directory(directory)?;
    if exists(destination)? {
        checked_file(destination, MAX_TAB_BYTES)?;
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

        fn profiles(&self, name: &str) -> ProfileStore {
            let store = self.store();
            store.create_tab(name, "Workspace").unwrap();
            store
                .bind_package(name, &inventory().package_id, &self.0.join("source"))
                .unwrap();
            store.profile_store(name, &inventory().package_id).unwrap()
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

    fn preferences() -> EditableSettings {
        EditableSettings {
            locale: Locale::English,
            gui_log_limit: 1000,
            ocr_environment: None,
            notifications: NotificationPreferences::default(),
            backup_directory: None,
        }
    }

    fn put(path: &Path, bytes: &[u8]) {
        private_directory(path.parent().unwrap()).unwrap();
        write_atomic(path, bytes, |from, to| fs::rename(from, to)).unwrap();
    }

    fn tab_path(directory: &Directory, name: &str) -> PathBuf {
        directory.0.join("tabs").join(name).join("tab.config")
    }

    #[test]
    fn invalid_initialization_leaves_absent_destination_and_legacy_source_untouched() {
        let directory = Directory::new();
        let root = directory.0.join("new-root");
        let legacy = directory.0.join("legacy/settings.json");
        put(&legacy, br#"{"version":1,"gui_log_limit":42}"#);
        let original = fs::read(&legacy).unwrap();
        let store = Store::new(root.clone()).unwrap();
        assert!(!root.exists());
        assert_eq!(store.settings().unwrap_err().category, "SettingsMissing");
        let mut invalid = preferences();
        invalid.gui_log_limit = 0;
        assert!(store.initialize(invalid).is_err());
        assert!(!root.exists());
        assert_eq!(fs::read(&legacy).unwrap(), original);
        store.initialize(preferences()).unwrap();
        assert!(root.join("settings.json").exists());
        assert_eq!(fs::read(&legacy).unwrap(), original);
    }

    #[test]
    fn missing_settings_require_explicit_missing_only_initialization() {
        let directory = Directory::new();
        let store = directory.store();
        assert_eq!(store.settings().unwrap_err().category, "SettingsMissing");
        assert_eq!(
            store.save_preferences(preferences()).unwrap_err().category,
            "SettingsMissing"
        );
        assert!(!directory.0.join("settings.json").exists());
        assert!(!directory.0.join("profiles").exists());
        let tab = store.create_tab("BeforeSetup", "Retained").unwrap();
        let mut invalid = preferences();
        invalid.gui_log_limit = 0;
        assert!(store.initialize(invalid).is_err());
        assert!(!directory.0.join("settings.json").exists());
        store.initialize(preferences()).unwrap();
        assert_eq!(store.tab("BeforeSetup").unwrap(), tab);
        let path = directory.0.join("settings.json");
        for bytes in [fs::read(&path).unwrap(), b"malformed evidence".to_vec()] {
            fs::write(&path, &bytes).unwrap();
            assert_eq!(
                store.initialize(preferences()).unwrap_err().category,
                "SettingsPresent"
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn missing_only_publication_refuses_destination_appearance() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let bytes = encode(&Settings::default(), MAX_SETTINGS_BYTES).unwrap();
        let fault = write_atomic(&path, &bytes, |from, to| {
            fs::write(to, b"other writer")?;
            publish_no_replace(from, to)
        })
        .unwrap_err();
        assert_eq!(fault.context["kind"], "AlreadyExists");
        assert_eq!(fs::read(&path).unwrap(), b"other writer");
        assert!(!path.with_extension("pending").exists());
    }

    #[test]
    fn legacy_settings_load_without_rewrite_and_preferences_preserve_current_hint() {
        let directory = Directory::new();
        let store = directory.store();
        let path = directory.0.join("settings.json");
        let original = br#"{ "version":1, "gui_log_limit":12, "package_path":"old-root" }"#;
        put(&path, original);
        let loaded = store.settings().unwrap();
        assert_eq!(loaded.locale, Locale::English);
        assert!(loaded.backup_directory.is_none());
        assert_eq!(fs::read(&path).unwrap(), original);
        let draft = EditableSettings {
            locale: Locale::Japanese,
            backup_directory: Some(directory.0.join("archives").to_str().unwrap().into()),
            ..preferences()
        };
        put(
            &path,
            br#"{"version":1,"gui_log_limit":12,"package_path":"newer-root"}"#,
        );
        let saved = store.save_preferences(draft).unwrap();
        assert_eq!(saved.package_path.as_deref(), Some("newer-root"));
        assert_eq!(
            directory.store().settings().unwrap().locale,
            Locale::Japanese
        );
        assert_eq!(
            directory.store().settings().unwrap().backup_directory,
            saved.backup_directory
        );
        assert!(!directory.0.join("archives").exists());
    }

    #[test]
    fn malformed_settings_never_become_defaults_or_accept_replacements() {
        let directory = Directory::new();
        let store = directory.store();
        let path = directory.0.join("settings.json");
        for malformed in [
            br#"{"version":1,"gui_log_limit":-1}"#.as_slice(),
            br#"{"version":1,"gui_log_limit":1.5}"#.as_slice(),
            br#"{"version":2,"gui_log_limit":12}"#.as_slice(),
            br#"{"version":2,"version":1,"gui_log_limit":12}"#.as_slice(),
            br#"{"version":1,"gui_log_limit":12,"gui_log_limit":34}"#.as_slice(),
            br#"{"version":1,"gui_log_limit":12,"locale":"invalid","locale":"ja"}"#.as_slice(),
            br#"{"version":1,"gui_log_limit":12,"notifications":{"visible_count":1,"visible_count":2,"timeout_seconds":8,"show_success":true}}"#.as_slice(),
            br#"{"version":1,"gui_log_limit":12,"future":true}"#.as_slice(),
            b"not JSON".as_slice(),
            br#"[1,12,"path"]"#.as_slice(),
        ] {
            put(&path, malformed);
            assert!(store.settings().is_err());
            assert!(store.initialize(preferences()).is_err());
            assert!(store.save_preferences(preferences()).is_err());
            assert_eq!(fs::read(&path).unwrap(), malformed);
        }
    }

    #[test]
    fn invalid_locale_notifications_and_backup_paths_preserve_settings() {
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        let path = directory.0.join("settings.json");
        let before = fs::read(&path).unwrap();
        for value in [
            Value::Null,
            json!("fr"),
            json!(1),
            json!(true),
            json!(["ja"]),
            json!({"ja":null}),
        ] {
            let mut editable = serde_json::to_value(preferences()).unwrap();
            editable["locale"] = value.clone();
            assert!(serde_json::from_value::<EditableSettings>(editable).is_err());
            let mut document = serde_json::to_value(Settings::default()).unwrap();
            document["locale"] = value;
            let bytes = serde_json::to_vec(&document).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert!(store.settings().is_err());
            assert!(store.save_preferences(preferences()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::write(&path, &before).unwrap();
        let mut missing = serde_json::to_value(preferences()).unwrap();
        missing.as_object_mut().unwrap().remove("locale");
        assert!(serde_json::from_value::<EditableSettings>(missing).is_err());
        for (count, timeout) in [(0, 8), (3, 8), (2, 0), (2, 13)] {
            let mut invalid = preferences();
            invalid.notifications.visible_count = count;
            invalid.notifications.timeout_seconds = timeout;
            assert!(store.save_preferences(invalid).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        for backup in [
            "relative".to_owned(),
            "".to_owned(),
            "/line\nbreak".to_owned(),
            format!("/{}", "x".repeat(MAX_PATH_BYTES)),
        ] {
            let mut invalid = preferences();
            invalid.backup_directory = Some(backup);
            assert!(store.save_preferences(invalid).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn settings_and_profile_mutations_preserve_other_owners() {
        use mado_runtime_comparison::environment::{
            G004_PROFILE, LANGUAGE, PROVIDER, RUNTIME_PROFILE,
        };
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        let profiles = directory.profiles("Owned");
        let profile = profiles
            .save(&inventory(), None, "Portable", options())
            .unwrap();
        let path = profiles.profile_path(&profile.id);
        let before = fs::read(&path).unwrap();
        let tab_before = fs::read(tab_path(&directory, "Owned")).unwrap();
        let environment = OcrEnvironment {
            model: G004_PROFILE.into(),
            profile: G004_PROFILE.into(),
            language: LANGUAGE.into(),
            provider: PROVIDER.into(),
            runtime_profile: RUNTIME_PROFILE.into(),
            model_root: directory.0.join("missing-models").to_str().unwrap().into(),
            runtime_path: directory.0.join("missing-runtime").to_str().unwrap().into(),
            native_library_paths: vec![
                directory.0.join("missing-library").to_str().unwrap().into(),
            ],
        };
        store
            .save_preferences(EditableSettings {
                ocr_environment: Some(environment.clone()),
                ..preferences()
            })
            .unwrap();
        assert_eq!(
            directory.store().settings().unwrap().ocr_environment,
            Some(environment.clone())
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(tab_path(&directory, "Owned")).unwrap(), tab_before);
        let settings_before = fs::read(directory.0.join("settings.json")).unwrap();
        profiles
            .rename(&inventory(), &profile.id, "Renamed")
            .unwrap();
        assert_eq!(
            fs::read(directory.0.join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(fs::read(tab_path(&directory, "Owned")).unwrap(), tab_before);
        let mut invalid = environment;
        invalid.provider = "cuda".into();
        assert!(
            store
                .save_preferences(EditableSettings {
                    ocr_environment: Some(invalid),
                    ..preferences()
                })
                .is_err()
        );
        assert_eq!(
            fs::read(directory.0.join("settings.json")).unwrap(),
            settings_before
        );
    }

    #[test]
    fn name_only_tabs_restart_and_closed_state_preserve_names() {
        let directory = Directory::new();
        let store = directory.store();
        let name = format!("0{}_-9", "Ab1".repeat(20));
        let display = "界".repeat(80);
        let created = store.create_tab(&name, &display).unwrap();
        assert!(created.open);
        assert!(created.packages.is_empty());
        assert!(created.selected_package_id.is_none());
        assert_eq!(directory.store().tab(&name).unwrap(), created);
        assert!(!directory.0.join("settings.json").exists());
        store.set_tab_open(&name, false).unwrap();
        assert!(!directory.store().tab(&name).unwrap().open);
        assert_eq!(store.set_tab_open(&name, true).unwrap(), created);
        store.create_tab("DuplicateDisplay", &display).unwrap();
        assert_eq!(store.tabs().unwrap().tabs.len(), 2);
    }

    #[test]
    fn invalid_names_and_case_aliases_preserve_existing_tabs() {
        let directory = Directory::new();
        let store = directory.store();
        let original = store.create_tab("Alpha", "  Preserved name  ").unwrap();
        let before = fs::read(tab_path(&directory, "Alpha")).unwrap();
        for name in [
            "",
            "Alpha１",
            " Alpha",
            "Alpha ",
            ".",
            "..",
            "../Alpha",
            "日",
            &"A".repeat(65),
        ] {
            assert!(store.create_tab(name, "Display").is_err());
        }
        for display in [" \u{3000} ", "line\nbreak", "\u{007f}", &"界".repeat(81)] {
            assert!(store.create_tab("Valid", display).is_err());
        }
        assert!(store.create_tab("alpha", "Alias").is_err());
        assert!(store.tab("ALPHA").is_err());
        assert!(store.create_tab("Alpha", "Replacement").is_err());
        assert_eq!(fs::read(tab_path(&directory, "Alpha")).unwrap(), before);
        assert_eq!(store.tab("Alpha").unwrap(), original);
    }

    #[test]
    fn saved_and_open_bounds_refuse_without_eviction() {
        let directory = Directory::new();
        let store = directory.store();
        let names: Vec<_> = (1..=MAX_TABS).map(|index| "A".repeat(index)).collect();
        for name in &names[..MAX_OPEN_TABS] {
            store.create_tab(name, "Same display").unwrap();
        }
        assert!(store.create_tab(&names[MAX_OPEN_TABS], "Overflow").is_err());
        store.set_tab_open(&names[0], false).unwrap();
        store
            .create_tab(&names[MAX_OPEN_TABS], "Slot reused")
            .unwrap();
        assert!(store.set_tab_open(&names[0], true).is_err());
        for name in &names[..=MAX_OPEN_TABS] {
            store.set_tab_open(name, false).unwrap();
        }
        for name in &names[MAX_OPEN_TABS + 1..] {
            store.create_tab(name, "Saved").unwrap();
            store.set_tab_open(name, false).unwrap();
        }
        assert_eq!(store.tabs().unwrap().tabs.len(), MAX_TABS);
        assert!(store.create_tab("Extra", "Overflow").is_err());
        assert!(store.set_tab_open(&names[0], true).unwrap().open);
    }

    #[test]
    fn package_bounds_aliases_and_unsupported_source_forms_are_preserved() {
        let directory = Directory::new();
        let store = directory.store();
        store.create_tab("Owner", "Owner").unwrap();
        store.bind_package("Owner", "Café", &directory.0).unwrap();
        let path = tab_path(&directory, "Owner");
        let before = fs::read(&path).unwrap();
        for id in [
            "CAFÉ",
            "Cafe\u{301}",
            "../outside",
            "tab.config",
            "NUL",
            "ends.",
            "space ",
            "drive:part",
        ] {
            assert!(store.bind_package("Owner", id, &directory.0).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        for index in 1..MAX_PACKAGES {
            store
                .bind_package("Owner", &format!("package-{index}"), &directory.0)
                .unwrap();
        }
        let complete = fs::read(&path).unwrap();
        assert!(
            store
                .bind_package("Owner", "overflow", &directory.0)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), complete);
        let mut tab = store.tab("Owner").unwrap();
        tab.packages[0].source = PackageSource::CustomArchive {
            path: directory.0.join("package.custom").to_str().unwrap().into(),
        };
        put(&path, &encode(&tab, MAX_TAB_BYTES).unwrap());
        assert_eq!(store.tab("Owner").unwrap(), tab);
        let mut invalid = serde_json::to_value(&tab).unwrap();
        invalid["packages"][0]["source"]["unknown"] = json!(true);
        let bytes = serde_json::to_vec(&invalid).unwrap();
        put(&path, &bytes);
        assert!(store.tab("Owner").is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn filesystem_aliases_cannot_select_retained_package_data() {
        let directory = Directory::new();
        let store = directory.store();
        store.create_tab("Owner", "Owner").unwrap();
        let path = tab_path(&directory, "Owner");
        let before = fs::read(&path).unwrap();
        private_directory(&path.parent().unwrap().join("Café")).unwrap();
        private_directory(&path.parent().unwrap().join("Σ")).unwrap();
        for id in ["CAFÉ", "Cafe\u{301}", "σ", "ς"] {
            assert_eq!(
                store
                    .bind_package("Owner", id, &directory.0)
                    .unwrap_err()
                    .category,
                "StorageAlias"
            );
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn malformed_and_orphaned_tabs_remain_attributable_without_rewriting() {
        let directory = Directory::new();
        let store = directory.store();
        store.create_tab("Healthy", "Healthy").unwrap();
        let path = tab_path(&directory, "Broken");
        put(&path, b"broken evidence");
        let orphan = directory.0.join("tabs/Orphan/package/target.config");
        put(&orphan, b"retained");
        private_directory(&directory.0.join("tabs/Empty")).unwrap();
        fs::write(directory.0.join("tabs/.DS_Store"), b"Finder metadata").unwrap();
        let listing = store.tabs().unwrap();
        assert_eq!(
            listing
                .tabs
                .iter()
                .map(|tab| tab.internal_name.as_str())
                .collect::<Vec<_>>(),
            ["Healthy"]
        );
        let owners: BTreeSet<_> = listing
            .faults
            .iter()
            .map(|fault| fault.context["internal_name"].as_str().unwrap())
            .collect();
        assert_eq!(owners, BTreeSet::from(["Broken", "Orphan"]));
        assert!(store.set_tab_open("Broken", false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken evidence");
        assert_eq!(fs::read(&orphan).unwrap(), b"retained");
        assert_eq!(
            fs::read(directory.0.join("tabs/.DS_Store")).unwrap(),
            b"Finder metadata"
        );
        let mut wrong = store.tab("Healthy").unwrap();
        wrong.internal_name = "Different".into();
        put(&path, &encode(&wrong, MAX_TAB_BYTES).unwrap());
        assert_eq!(store.tab("Broken").unwrap_err().category, "TabIdentity");
    }

    #[test]
    fn failed_create_and_reopen_preserve_prior_records_and_pending_evidence() {
        let directory = Directory::new();
        let store = directory.store();
        let creation = tab_path(&directory, "New").with_extension("pending");
        put(&creation, b"interrupted creation");
        assert!(store.create_tab("New", "Unsaved").is_err());
        assert!(!tab_path(&directory, "New").exists());
        assert_eq!(fs::read(&creation).unwrap(), b"interrupted creation");
        fs::remove_file(&creation).unwrap();
        store.create_tab("New", "Saved").unwrap();
        store.set_tab_open("New", false).unwrap();
        let closed = fs::read(tab_path(&directory, "New")).unwrap();
        put(&creation, b"interrupted reopen");
        assert!(store.set_tab_open("New", true).is_err());
        assert!(!store.tab("New").unwrap().open);
        assert_eq!(fs::read(tab_path(&directory, "New")).unwrap(), closed);
        assert_eq!(fs::read(&creation).unwrap(), b"interrupted reopen");
    }

    #[test]
    fn tab_document_bound_is_independent_of_smaller_profile_bound() {
        let directory = Directory::new();
        let store = directory.store();
        store.create_tab("Owner", "Owner").unwrap();
        let path = tab_path(&directory, "Owner");
        let mut padded = fs::read(&path).unwrap();
        padded.resize(100 * 1024, b' ');
        put(&path, &padded);
        assert!(store.tab("Owner").unwrap().open);
        assert!(!store.set_tab_open("Owner", false).unwrap().open);
        padded.resize(MAX_TAB_BYTES + 1, b' ');
        fs::write(&path, &padded).unwrap();
        assert!(store.tab("Owner").is_err());
        assert!(store.set_tab_open("Owner", true).is_err());
        assert_eq!(fs::read(&path).unwrap(), padded);
    }

    #[test]
    fn pending_and_failed_publication_preserve_tabs_and_settings() {
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        let tab = store.create_tab("Owner", "Owner").unwrap();
        let path = tab_path(&directory, "Owner");
        let before = fs::read(&path).unwrap();
        let pending = path.with_extension("pending");
        put(&pending, b"unfinished");
        assert!(store.set_tab_open("Owner", false).is_err());
        assert!(
            store
                .bind_package("Owner", "package", &directory.0)
                .is_err()
        );
        assert_eq!(store.tab("Owner").unwrap(), tab);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&pending).unwrap(), b"unfinished");
        fs::remove_file(&pending).unwrap();
        let mut changed = tab;
        changed.open = false;
        let bytes = encode(&changed, MAX_TAB_BYTES).unwrap();
        assert!(
            write_atomic(&path, &bytes, |from, to| fs::rename(
                from,
                to.join("not-a-directory")
            ))
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!pending.exists());
        let settings = directory.0.join("settings.json");
        let settings_before = fs::read(&settings).unwrap();
        put(&settings.with_extension("pending"), b"settings unfinished");
        assert!(store.save_preferences(preferences()).is_err());
        assert_eq!(fs::read(&settings).unwrap(), settings_before);
        assert_eq!(
            fs::read(settings.with_extension("pending")).unwrap(),
            b"settings unfinished"
        );
    }

    #[test]
    fn same_source_tabs_isolate_profiles_and_reject_cross_owner_references() {
        let directory = Directory::new();
        let first = directory.profiles("First");
        let second = directory.profiles("Second");
        assert!(!first.directory().exists());
        let inv = inventory();
        let profile = first.save(&inv, None, "First value", options()).unwrap();
        assert!(
            second
                .list(&inv.package_id, &profile.schema_identity)
                .unwrap()
                .profiles
                .is_empty()
        );
        assert!(
            second
                .save(&inv, Some(&profile.id), "Stolen", options())
                .is_err()
        );
        assert!(second.rename(&inv, &profile.id, "Stolen").is_err());
        assert!(second.delete(&profile.id).is_err());
        let values = json!({"priorities":["left"]});
        let other = second
            .save(&inv, None, "Second value", values.clone())
            .unwrap();
        first.rename(&inv, &profile.id, "Renamed").unwrap();
        assert_eq!(second.read_profile(&other.id).unwrap().0.values, values);
        let before = fs::read(first.profile_path(&profile.id)).unwrap();
        assert!(!directory.0.join("profiles").exists());
        let store = directory.store();
        store.set_tab_open("First", false).unwrap();
        assert!(first.save(&inv, None, "Closed", options()).is_err());
        store.set_tab_open("First", true).unwrap();
        store
            .bind_package("First", &inv.package_id, &directory.0.join("relocated"))
            .unwrap();
        let reopened = store.profile_store("First", &inv.package_id).unwrap();
        assert_eq!(
            fs::read(reopened.profile_path(&profile.id)).unwrap(),
            before
        );
        reopened.delete(&profile.id).unwrap();
        assert_eq!(second.read_profile(&other.id).unwrap().0.values, values);
    }

    #[test]
    fn schema_mismatch_and_invalid_replacement_preserve_original_bytes() {
        let directory = Directory::new();
        let store = directory.profiles("Owner");
        let mut inv = inventory();
        let saved = store.save(&inv, None, "Original", options()).unwrap();
        let path = store.profile_path(&saved.id);
        let before = fs::read(&path).unwrap();
        assert!(
            store
                .save(
                    &inv,
                    Some(&saved.id),
                    "Invalid",
                    json!({"priorities":["left"],"window":{"width":3}})
                )
                .is_err()
        );
        inv.schema["properties"]["label"]["maxLength"] = json!(80);
        let schema = identity(&inv.schema).unwrap();
        let rejected = store.list(&inv.package_id, &schema).unwrap();
        assert!(rejected.profiles.is_empty());
        assert_eq!(rejected.rejected[0].context["profile_id"], saved.id);
        assert!(store.rename(&inv, &saved.id, "Changed").is_err());
        assert!(
            store
                .save(&inv, Some(&saved.id), "Changed", options())
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let compatible = store.save(&inv, None, "Compatible", options()).unwrap();
        assert_eq!(
            store.list(&inv.package_id, &schema).unwrap().profiles[0].id,
            compatible.id
        );
    }

    #[test]
    fn malformed_profiles_and_wrong_containing_identities_preserve_data() {
        let directory = Directory::new();
        let store = directory.profiles("Owner");
        let inv = inventory();
        let saved = store.save(&inv, None, "Original", options()).unwrap();
        let path = store.profile_path(&saved.id);
        let mut malformed = vec![b"not JSON".to_vec()];
        for (key, value) in [
            ("version", json!(2)),
            ("id", json!(new_id().unwrap())),
            ("package_id", json!("other-owner")),
            ("values", json!({"nested":{"api_key":"credential"}})),
        ] {
            let mut document = serde_json::to_value(&saved).unwrap();
            document[key] = value;
            malformed.push(serde_json::to_vec(&document).unwrap());
        }
        for bytes in malformed {
            fs::write(&path, &bytes).unwrap();
            let fault = store
                .list(&inv.package_id, &saved.schema_identity)
                .unwrap_err();
            assert_eq!(fault.context["profile_id"], saved.id);
            assert_eq!(fault.context["internal_name"], "Owner");
            assert!(store.save(&inv, None, "Replacement", options()).is_err());
            assert!(
                store
                    .save(&inv, Some(&saved.id), "Replacement", options())
                    .is_err()
            );
            assert!(store.rename(&inv, &saved.id, "Renamed").is_err());
            assert!(store.delete(&saved.id).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        assert!(store.delete("../settings").is_err());
    }

    #[test]
    fn profile_size_count_and_aggregate_bounds_preserve_existing_profiles() {
        let directory = Directory::new();
        let store = directory.profiles("Owner");
        let inv = inventory();
        let saved = store.save(&inv, None, "Original", options()).unwrap();
        let path = store.profile_path(&saved.id);
        let before = fs::read(&path).unwrap();
        assert!(
            store
                .save(
                    &inv,
                    Some(&saved.id),
                    "Too large",
                    json!({"priorities":["left"],"label":"a".repeat(MAX_PROFILE_BYTES)})
                )
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        for index in 1..MAX_PROFILES {
            store
                .save(&inv, None, &format!("Profile {index}"), options())
                .unwrap();
        }
        assert!(store.save(&inv, None, "Overflow", options()).is_err());
        assert_eq!(
            store.rename(&inv, &saved.id, "At capacity").unwrap().id,
            saved.id
        );
        assert_eq!(
            store
                .list(&inv.package_id, &saved.schema_identity)
                .unwrap()
                .profiles
                .len(),
            MAX_PROFILES
        );
        let large = directory.profiles("Large");
        let values = json!({"priorities":["left"],"label":"x".repeat(59 * 1024)});
        for index in 0..17 {
            large
                .save(&inv, None, &format!("Large {index}"), values.clone())
                .unwrap();
        }
        assert!(large.save(&inv, None, "Over one MiB", values).is_err());
        assert_eq!(
            large
                .list(&inv.package_id, &saved.schema_identity)
                .unwrap()
                .profiles
                .len(),
            17
        );
    }

    #[test]
    fn managed_budget_counts_preserved_orphan_configuration() {
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        for package in 0..5 {
            for profile in 0..64 {
                put(
                    &directory
                        .0
                        .join(format!("tabs/Orphan/package-{package}/{profile}.config")),
                    &vec![b'x'; 52 * 1024],
                );
            }
        }
        let path = directory.0.join("settings.json");
        let before = fs::read(&path).unwrap();
        assert!(store.save_preferences(preferences()).is_err());
        assert!(store.create_tab("Blocked", "Budget").is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!tab_path(&directory, "Blocked").exists());
    }

    #[test]
    fn pending_profiles_block_mutation_but_foreign_notes_remain_untouched() {
        let directory = Directory::new();
        let store = directory.profiles("Owner");
        let inv = inventory();
        let saved = store.save(&inv, None, "Original", options()).unwrap();
        let notes = store.directory().join("notes.txt");
        fs::write(&notes, b"operator notes").unwrap();
        store.rename(&inv, &saved.id, "Renamed").unwrap();
        let pending = store.profile_path(&saved.id).with_extension("pending");
        put(&pending, b"unfinished");
        let before = fs::read(store.profile_path(&saved.id)).unwrap();
        assert!(store.list(&inv.package_id, &saved.schema_identity).is_err());
        assert!(
            store
                .save(&inv, Some(&saved.id), "Unsaved", options())
                .is_err()
        );
        assert!(store.delete(&saved.id).is_err());
        assert_eq!(fs::read(store.profile_path(&saved.id)).unwrap(), before);
        assert_eq!(fs::read(&pending).unwrap(), b"unfinished");
        assert_eq!(fs::read(&notes).unwrap(), b"operator notes");
    }

    #[test]
    fn foreign_entries_consume_bounded_directory_capacity() {
        let directory = Directory::new();
        let store = directory.profiles("Owner");
        private_directory(&store.directory()).unwrap();
        for index in 0..MAX_DIRECTORY_ENTRIES - 2 {
            fs::write(store.directory().join(format!("note-{index}.txt")), b"").unwrap();
        }
        let inv = inventory();
        let saved = store.save(&inv, None, "Last slot", options()).unwrap();
        store.rename(&inv, &saved.id, "At capacity").unwrap();
        let before = fs::read(store.profile_path(&saved.id)).unwrap();
        fs::write(store.directory().join("one-too-many.txt"), b"").unwrap();
        assert!(store.list(&inv.package_id, &saved.schema_identity).is_err());
        assert!(store.save(&inv, None, "Overflow", options()).is_err());
        assert_eq!(fs::read(store.profile_path(&saved.id)).unwrap(), before);
    }

    #[test]
    fn failed_profile_publication_retains_previous_bytes() {
        let directory = Directory::new();
        let store = directory.profiles("Owner");
        let inv = inventory();
        let mut profile = store.save(&inv, None, "Original", options()).unwrap();
        let path = store.profile_path(&profile.id);
        let before = fs::read(&path).unwrap();
        profile.name = "Unsaved".into();
        let bytes = encode(&profile, MAX_PROFILE_BYTES).unwrap();
        assert!(
            write_atomic(&path, &bytes, |temporary, destination| {
                assert_eq!(fs::read(temporary)?, bytes);
                fs::rename(temporary, destination.join("not-a-directory"))
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("pending").exists());
        let reopened = directory
            .store()
            .profile_store("Owner", &inv.package_id)
            .unwrap();
        assert_eq!(
            reopened.read_profile(&profile.id).unwrap().0.name,
            "Original"
        );
    }

    #[test]
    fn portability_checks_fields_paths_and_defaults_not_arbitrary_words() {
        for portable in [
            json!({"label":"secret token password"}),
            json!({"asset":"assets/right.png"}),
            json!({"key":"Enter","item":"game token","label":"A: strategy"}),
        ] {
            portable_values(&portable).unwrap();
        }
        for forbidden in [
            json!({"nested":{"api_key":"value"}}),
            json!({"token":"value"}),
            json!({"inputAuthority":false}),
            json!({"target_executable_path":"relative-game"}),
            json!({"asset":"/private/model"}),
            json!({"asset":"C:\\models\\ocr"}),
            json!({"asset":"../outside"}),
        ] {
            assert_eq!(
                portable_values(&forbidden).unwrap_err().category,
                "ProfileAuthority"
            );
        }
        let directory = Directory::new();
        let store = directory.profiles("Owner");
        let mut inv = inventory();
        inv.schema["properties"]["label"]["default"] = json!("/private/default-model");
        assert_eq!(
            store
                .save(&inv, None, "No authority", json!({"priorities":["left"]}))
                .unwrap_err()
                .category,
            "ProfileAuthority"
        );
    }

    fn legacy_profile(sequence: u64, name: &str) -> Profile {
        let inv = inventory();
        Profile {
            version: VERSION,
            id: format!("p-{sequence:032x}-{:08x}-{:016x}", 1, 1),
            name: name.into(),
            package_id: inv.package_id,
            schema_identity: identity(&inv.schema).unwrap(),
            values: options(),
        }
    }

    fn legacy(directory: &Directory, profile: &Profile) -> Vec<u8> {
        let bytes = serde_json::to_vec_pretty(profile).unwrap();
        put(
            &directory
                .0
                .join("profiles")
                .join(format!("{}.json", profile.id)),
            &bytes,
        );
        bytes
    }

    #[test]
    fn explicit_legacy_import_is_exact_idempotent_and_not_shared() {
        let directory = Directory::new();
        let first = directory.profiles("First");
        let second = directory.profiles("Second");
        let profile = legacy_profile(1, "Legacy");
        let bytes = legacy(&directory, &profile);
        let inv = inventory();
        assert!(
            first
                .list(&inv.package_id, &profile.schema_identity)
                .unwrap()
                .profiles
                .is_empty()
        );
        let store = directory.store();
        let imported = store.import_legacy_profiles("First", &inv).unwrap();
        assert_eq!(imported.imported, [profile.id.clone()]);
        assert!(imported.fault.is_none());
        assert_eq!(fs::read(first.profile_path(&profile.id)).unwrap(), bytes);
        let retry = store.import_legacy_profiles("First", &inv).unwrap();
        assert!(retry.imported.is_empty());
        assert_eq!(retry.unchanged, [profile.id.clone()]);
        assert!(retry.fault.is_none());
        assert!(
            second
                .list(&inv.package_id, &profile.schema_identity)
                .unwrap()
                .profiles
                .is_empty()
        );
        assert_eq!(
            fs::read(
                directory
                    .0
                    .join("profiles")
                    .join(format!("{}.json", profile.id))
            )
            .unwrap(),
            bytes
        );
    }

    #[test]
    fn legacy_conflict_reports_committed_subset_and_retry_preserves_sources() {
        let directory = Directory::new();
        let scoped = directory.profiles("Owner");
        let first = legacy_profile(1, "First");
        let second = legacy_profile(2, "Second");
        let first_bytes = legacy(&directory, &first);
        let second_bytes = legacy(&directory, &second);
        let mut conflict = second.clone();
        conflict.name = "Different retained value".into();
        let conflicting_bytes = encode(&conflict, MAX_PROFILE_BYTES).unwrap();
        put(&scoped.profile_path(&second.id), &conflicting_bytes);
        let store = directory.store();
        let result = store.import_legacy_profiles("Owner", &inventory()).unwrap();
        assert_eq!(result.imported, [first.id.clone()]);
        assert_eq!(result.fault.unwrap().category, "LegacyConflict");
        assert_eq!(
            fs::read(scoped.profile_path(&first.id)).unwrap(),
            first_bytes
        );
        assert_eq!(
            fs::read(scoped.profile_path(&second.id)).unwrap(),
            conflicting_bytes
        );
        let retry = store.import_legacy_profiles("Owner", &inventory()).unwrap();
        assert!(retry.imported.is_empty());
        assert_eq!(retry.unchanged, [first.id.clone()]);
        assert_eq!(retry.fault.unwrap().category, "LegacyConflict");
        assert_eq!(
            fs::read(
                directory
                    .0
                    .join("profiles")
                    .join(format!("{}.json", second.id))
            )
            .unwrap(),
            second_bytes
        );
    }

    #[test]
    fn interrupted_legacy_batch_resumes_after_external_source_repair() {
        let directory = Directory::new();
        let scoped = directory.profiles("Owner");
        let first = legacy_profile(1, "First");
        let second = legacy_profile(2, "Second");
        let first_bytes = legacy(&directory, &first);
        let broken = directory
            .0
            .join("profiles")
            .join(format!("{}.json", second.id));
        put(&broken, b"interrupted source");
        let store = directory.store();
        let partial = store.import_legacy_profiles("Owner", &inventory()).unwrap();
        assert_eq!(partial.imported, [first.id.clone()]);
        assert_eq!(partial.fault.unwrap().category, "StorageFormat");
        assert_eq!(
            fs::read(scoped.profile_path(&first.id)).unwrap(),
            first_bytes
        );
        assert!(!scoped.profile_path(&second.id).exists());
        assert_eq!(fs::read(&broken).unwrap(), b"interrupted source");
        let second_bytes = legacy(&directory, &second);
        let resumed = store.import_legacy_profiles("Owner", &inventory()).unwrap();
        assert_eq!(resumed.unchanged, [first.id.clone()]);
        assert_eq!(resumed.imported, [second.id.clone()]);
        assert!(resumed.fault.is_none());
        assert_eq!(
            fs::read(scoped.profile_path(&second.id)).unwrap(),
            second_bytes
        );
        assert_eq!(
            fs::read(scoped.profile_path(&first.id)).unwrap(),
            first_bytes
        );
    }

    #[test]
    fn legacy_import_skips_incompatible_data_and_preserves_malformed_sources() {
        let directory = Directory::new();
        let scoped = directory.profiles("Owner");
        let mut profile = legacy_profile(1, "Other package");
        profile.package_id = "different-package".into();
        legacy(&directory, &profile);
        let store = directory.store();
        let result = store.import_legacy_profiles("Owner", &inventory()).unwrap();
        assert!(result.imported.is_empty());
        assert!(result.fault.is_none());
        assert!(!scoped.directory().exists());
        let path = directory
            .0
            .join("profiles")
            .join(format!("{}.json", profile.id));
        put(&path, b"malformed source");
        let result = store.import_legacy_profiles("Owner", &inventory()).unwrap();
        assert_eq!(result.fault.unwrap().category, "StorageFormat");
        assert_eq!(fs::read(&path).unwrap(), b"malformed source");
        assert!(!scoped.directory().exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_owner_directories_and_linked_profiles_are_refused_without_repair() {
        let directory = Directory::new();
        let scoped = directory.profiles("Owner");
        let saved = scoped
            .save(&inventory(), None, "Original", options())
            .unwrap();
        let path = scoped.profile_path(&saved.id);
        assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(scoped.directory()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let outside = directory.0.join("original.json");
        fs::rename(&path, &outside).unwrap();
        let before = fs::read(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(
            scoped
                .list(&inventory().package_id, &saved.schema_identity)
                .is_err()
        );
        assert!(
            scoped
                .save(&inventory(), Some(&saved.id), "Replacement", options())
                .is_err()
        );
        assert!(scoped.delete(&saved.id).is_err());
        assert_eq!(fs::read(&outside).unwrap(), before);
        fs::remove_file(&path).unwrap();
        fs::hard_link(&outside, &path).unwrap();
        assert!(scoped.delete(&saved.id).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&outside, &path).unwrap();
        for owner in [
            directory.0.clone(),
            directory.0.join("tabs"),
            directory.0.join("tabs/Owner"),
            scoped.directory(),
        ] {
            fs::set_permissions(&owner, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                scoped
                    .list(&inventory().package_id, &saved.schema_identity)
                    .is_err()
            );
            assert_eq!(fs::metadata(&owner).unwrap().mode() & 0o777, 0o755);
            fs::set_permissions(&owner, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}
