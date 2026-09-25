mod fs;
mod profiles;
mod settings;
mod tabs;
mod targets;

pub use profiles::{LegacyImport, Profile, ProfileListing};
pub use settings::{EditableSettings, Locale, NotificationPreferences, Settings};
pub use tabs::{PackageReference, PackageSource, TabListing, TabRecord};

pub(crate) use fs::{
    check_directory, checked_file, decode, encode, exists, filesystem_key, private_directory,
    read_bytes, write_atomic,
};
pub(crate) use profiles::{RecoveryRecord, portable_values, validate_profile};
pub(crate) use settings::validate_settings;
pub(crate) use tabs::{validate_internal_name, validate_tab};

use self::fs::{limit, storage};
use crate::configuration::{MAX_BYTES, MAX_ENUMERATED, MAX_FILES};
use mado_runtime_comparison::model::Fault;
use serde_json::json;
use std::fs as std_fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod fixtures;

const VERSION: u32 = 1;
pub(crate) const MAX_PROFILES: usize = 64;
pub(crate) const MAX_PROFILE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TOTAL_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 128;
pub(crate) const MAX_SETTINGS_BYTES: usize = 32 * 1024;
pub(crate) const MAX_TAB_BYTES: usize = 128 * 1024;
pub(crate) const MAX_TABS: usize = 64;
pub(crate) const MAX_OPEN_TABS: usize = 8;
const MAX_PACKAGES: usize = 16;
const MAX_NAME_BYTES: usize = 128;
pub(crate) const MAX_PATH_BYTES: usize = 4096;
const MAX_VALUE_NODES: usize = 8192;
const MAX_VALUE_DEPTH: usize = 32;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

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

    pub fn profile_store(&self, tab_name: &str, package_id: &str) -> Result<ProfileStore, Fault> {
        let store = ProfileStore {
            root: self.root.clone(),
            tab_name: tab_name.to_owned(),
            package_id: package_id.to_owned(),
        };
        store.check_owner()?;
        Ok(store)
    }
}

impl ProfileStore {
    fn directory(&self) -> PathBuf {
        self.root
            .join("tabs")
            .join(&self.tab_name)
            .join(&self.package_id)
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

/// Admits one write against the 4096-file/16 MiB managed bound from metadata alone: no
/// content is read or hashed, no link is followed and no owner's mode is judged, so one
/// owner's unresolved or unsafe entry never blocks another owner's write. Only a tree that
/// cannot be measured or the aggregate itself refuses, each naming the managed path it
/// concerns relative to the root. Snapshot capture stays strict and separate.
fn check_budget(root: &Path, relative: &str, size: usize) -> Result<(), Fault> {
    let mut budget = Budget {
        replacing: relative,
        files: 0,
        bytes: 0,
        enumerated: 0,
    };
    budget.measure(root)?;
    let files = budget.files + 1;
    let bytes = budget.bytes.saturating_add(size as u64);
    if files > MAX_FILES || bytes > MAX_BYTES as u64 {
        return Err(limit("managed configuration exceeds 4096 files or 16 MiB")
            .with_context(json!({"path": relative, "files": files, "bytes": bytes})));
    }
    Ok(())
}

struct Budget<'a> {
    replacing: &'a str,
    files: usize,
    bytes: u64,
    enumerated: usize,
}

impl Budget<'_> {
    fn measure(&mut self, root: &Path) -> Result<(), Fault> {
        if !exists(root)? {
            return Ok(());
        }
        for (path, name, metadata) in self.entries(root, ".")? {
            let key = filesystem_key(&name);
            match key.as_str() {
                "settings.json" | "settings.pending" => {
                    self.account(&name, &key, &metadata, MAX_SETTINGS_BYTES);
                }
                "profiles" => {
                    if self.container(&name, &metadata)? {
                        for (_, child, metadata) in self.entries(&path, &name)? {
                            let key = filesystem_key(&child);
                            if key.ends_with(".json") || key.ends_with(".pending") {
                                let relative = format!("{name}/{child}");
                                self.account(&relative, &key, &metadata, MAX_PROFILE_BYTES);
                            }
                        }
                    }
                }
                "tabs" => {
                    if self.container(&name, &metadata)? {
                        self.measure_tabs(&path, &name)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn measure_tabs(&mut self, tabs: &Path, relative: &str) -> Result<(), Fault> {
        for (tab_path, tab, tab_metadata) in self.entries(tabs, relative)? {
            let tab_relative = format!("{relative}/{tab}");
            if !self.container(&tab_relative, &tab_metadata)? {
                continue;
            }
            for (child_path, child, child_metadata) in self.entries(&tab_path, &tab_relative)? {
                let key = filesystem_key(&child);
                let child_relative = format!("{tab_relative}/{child}");
                if key == "tab.config" || key == "tab.pending" {
                    self.account(&child_relative, &key, &child_metadata, MAX_TAB_BYTES);
                } else if self.container(&child_relative, &child_metadata)? {
                    for (_, file, metadata) in self.entries(&child_path, &child_relative)? {
                        let key = filesystem_key(&file);
                        if key.ends_with(".config") || key.ends_with(".pending") {
                            let relative = format!("{child_relative}/{file}");
                            self.account(&relative, &key, &metadata, MAX_PROFILE_BYTES);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Lists one real directory without following a link or judging its mode; a directory
    /// that cannot be listed is unmeasurable and refuses with its managed path.
    fn entries(
        &mut self,
        directory: &Path,
        relative: &str,
    ) -> Result<Vec<(PathBuf, String, std_fs::Metadata)>, Fault> {
        let listing = std_fs::read_dir(directory).map_err(|error| {
            measure_fault(storage("measure managed configuration", error), relative)
        })?;
        let mut entries = Vec::new();
        for entry in listing {
            self.enumerated += 1;
            if self.enumerated > MAX_ENUMERATED {
                return Err(measure_fault(
                    limit("managed configuration enumeration exceeds its bound"),
                    relative,
                ));
            }
            let entry = entry
                .map_err(|error| measure_fault(storage("read managed entry", error), relative))?;
            let path = entry.path();
            let metadata = std_fs::symlink_metadata(&path).map_err(|error| {
                measure_fault(storage("inspect managed entry", error), relative)
            })?;
            entries.push((
                path,
                entry.file_name().to_string_lossy().into_owned(),
                metadata,
            ));
        }
        Ok(entries)
    }

    /// A container is descended only as a real directory. A plain file there holds nothing;
    /// a link or other entry hides unknown capacity and refuses rather than being skipped.
    fn container(&self, relative: &str, metadata: &std_fs::Metadata) -> Result<bool, Fault> {
        let kind = metadata.file_type();
        if kind.is_dir() {
            return Ok(true);
        }
        if kind.is_file() {
            return Ok(false);
        }
        Err(measure_fault(
            Fault::new(
                "Storage",
                "managed configuration container is not a real directory and cannot be measured",
            ),
            relative,
        ))
    }

    /// A committed regular file counts at its length. A pending, linked or otherwise
    /// non-regular entry counts at its kind maximum without being read or followed. The
    /// file this write replaces is excluded.
    fn account(&mut self, relative: &str, key: &str, metadata: &std_fs::Metadata, maximum: usize) {
        if relative == self.replacing {
            return;
        }
        self.files += 1;
        let bytes = if key.ends_with(".pending") || !metadata.file_type().is_file() {
            maximum as u64
        } else {
            metadata.len()
        };
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

fn measure_fault(mut fault: Fault, relative: &str) -> Fault {
    fault.context["path"] = json!(relative);
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

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::target::{MAX_TARGET_BYTES, TargetExpectation};
    use mado_runtime_comparison::environment::OcrEnvironment;
    #[cfg(unix)]
    use mado_runtime_comparison::inventory::TargetDeclaration;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

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
    fn managed_budget_counts_preserved_orphan_configuration() {
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        let profiles = directory.profiles("Recovery");
        let saved = profiles
            .save(&inventory(), None, "Original", options())
            .unwrap();
        let expected = profiles.recovery_records().unwrap().pop().unwrap();
        let profile_path = profiles.profile_path(&saved.id);
        let profile_before = fs::read(&profile_path).unwrap();
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
        let fault = profiles
            .replace_recovery(&inventory(), &expected, options())
            .unwrap_err();
        assert_eq!(fault.category, "StorageLimit");
        assert_eq!(fault.context["profile_id"], saved.id);
        assert_eq!(fs::read(&profile_path).unwrap(), profile_before);
        assert!(!profile_path.with_extension("pending").exists());
    }

    #[test]
    fn foreign_owner_pending_and_unsafe_entries_do_not_block_other_owners() {
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        let blocked = directory.profiles("Blocked");
        let healthy = directory.profiles("Healthy");
        let inv = inventory();
        let saved = blocked.save(&inv, None, "Original", options()).unwrap();
        let profile_before = fs::read(blocked.profile_path(&saved.id)).unwrap();
        let pending = blocked.profile_path(&saved.id).with_extension("pending");
        put(&pending, b"unfinished");
        let tab_pending = tab_path(&directory, "Blocked").with_extension("pending");
        put(&tab_pending, b"interrupted");
        // A Tab copied back without private modes is unsafe for capture, yet measurable.
        let copied = tab_path(&directory, "Copied");
        put(&copied, b"{}");
        #[cfg(unix)]
        {
            fs::set_permissions(copied.parent().unwrap(), fs::Permissions::from_mode(0o755))
                .unwrap();
            fs::set_permissions(&copied, fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(
            blocked
                .save(&inv, Some(&saved.id), "Unsaved", options())
                .is_err()
        );
        assert!(blocked.delete(&saved.id).is_err());
        assert!(store.set_tab_open("Blocked", false).is_err());
        let independent = healthy.save(&inv, None, "Independent", options()).unwrap();
        assert_eq!(
            healthy.read_profile(&independent.id).unwrap().0.name,
            "Independent"
        );
        store.save_preferences(preferences()).unwrap();
        store.create_tab("Fresh", "Fresh").unwrap();
        assert!(!store.set_tab_open("Healthy", false).unwrap().open);
        assert!(store.set_tab_open("Healthy", true).unwrap().open);
        assert_eq!(
            fs::read(blocked.profile_path(&saved.id)).unwrap(),
            profile_before
        );
        assert_eq!(fs::read(&pending).unwrap(), b"unfinished");
        assert_eq!(fs::read(&tab_pending).unwrap(), b"interrupted");
        assert_eq!(fs::read(&copied).unwrap(), b"{}");
    }

    #[cfg(unix)]
    #[test]
    fn unmeasurable_foreign_container_refuses_writes_with_its_managed_path() {
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        let healthy = directory.profiles("Healthy");
        store.create_tab("Linked", "Linked").unwrap();
        store.set_tab_open("Linked", false).unwrap();
        let outside = directory.0.join("outside");
        private_directory(&outside).unwrap();
        let link = directory.0.join("tabs/Linked/package");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let settings = directory.0.join("settings.json");
        let before = fs::read(&settings).unwrap();
        let refusal = store.save_preferences(preferences()).unwrap_err();
        assert_eq!(refusal.context["path"], "tabs/Linked/package");
        assert_eq!(
            healthy
                .save(&inventory(), None, "Blocked", options())
                .unwrap_err()
                .context["path"],
            "tabs/Linked/package"
        );
        assert_eq!(fs::read(&settings).unwrap(), before);
        assert!(!healthy.directory().exists());
        fs::remove_file(&link).unwrap();
        store.save_preferences(preferences()).unwrap();
        healthy
            .save(&inventory(), None, "Unblocked", options())
            .unwrap();
    }

    #[test]
    fn target_pending_and_malformed_data_do_not_poison_profiles_but_snapshot_refuses_pending() {
        let directory = Directory::new();
        let store = directory.store();
        store.initialize(preferences()).unwrap();
        let profiles = directory.profiles("Owner");
        let inv = inventory();
        let profile = profiles.save(&inv, None, "Original", options()).unwrap();
        let path = target_path(&directory, "Owner");
        put(&path, b"malformed target");
        let pending = path.with_extension("pending");
        put(&pending, b"retain interrupted target");
        assert_eq!(
            profiles
                .list(&inv.package_id, &profile.schema_identity)
                .unwrap()
                .profiles[0]
                .id,
            profile.id
        );
        profiles.rename(&inv, &profile.id, "Still usable").unwrap();
        assert_eq!(
            store
                .read_target("Owner", &inv.package_id)
                .unwrap_err()
                .category,
            "StoragePending"
        );
        assert!(
            store
                .remove_target(
                    "Owner",
                    &inv.package_id,
                    &TargetExpectation {
                        revision: 0,
                        binding_id: None
                    }
                )
                .is_err()
        );
        assert!(crate::configuration::capture(&directory.0).is_err());
        assert_eq!(fs::read(&pending).unwrap(), b"retain interrupted target");
        assert_eq!(fs::read(&path).unwrap(), b"malformed target");
        fs::remove_file(&pending).unwrap();
        let snapshot = crate::configuration::capture(&directory.0).unwrap();
        assert_eq!(
            snapshot.files[&format!("tabs/Owner/{}/target.config", inv.package_id)],
            b"malformed target"
        );
        assert!(crate::restore::validate(&snapshot).is_err());
        let repaired = stored_target("Owner");
        fs::write(&path, serde_json::to_vec(&repaired).unwrap()).unwrap();
        assert_eq!(
            store.read_target("Owner", &inv.package_id).unwrap(),
            repaired
        );
        fs::write(&path, vec![b'x'; MAX_TARGET_BYTES + 1]).unwrap();
        assert_eq!(
            profiles
                .list(&inv.package_id, &profile.schema_identity)
                .unwrap()
                .profiles[0]
                .name,
            "Still usable"
        );
        profiles
            .rename(&inv, &profile.id, "Still independent")
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn target_links_and_public_modes_are_refused_without_poisoning_profile_admission() {
        let directory = Directory::new();
        let profiles = directory.profiles("Owner");
        let inv = inventory();
        let profile = profiles.save(&inv, None, "Profile", options()).unwrap();
        let store = directory.store();
        let path = target_path(&directory, "Owner");
        let record = stored_target("Owner");
        put(&path, &serde_json::to_vec(&record).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let outside = directory.0.join("retained-target.json");
        fs::rename(&path, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(store.read_target("Owner", &inv.package_id).is_err());
        assert_eq!(
            profiles
                .list(&inv.package_id, &profile.schema_identity)
                .unwrap()
                .profiles[0]
                .id,
            profile.id
        );
        fs::remove_file(&path).unwrap();
        fs::hard_link(&outside, &path).unwrap();
        assert!(
            store
                .remove_target("Owner", &inv.package_id, &record.expectation())
                .is_err()
        );
        fs::remove_file(&path).unwrap();
        fs::rename(&outside, &path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.read_target("Owner", &inv.package_id).is_err());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
        assert_eq!(
            profiles
                .list(&inv.package_id, &profile.schema_identity)
                .unwrap()
                .profiles[0]
                .id,
            profile.id
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.read_target("Owner", &inv.package_id).unwrap(), record);
    }

    #[cfg(unix)]
    #[test]
    fn target_save_restart_owner_isolation_relocation_and_declaration_replacement() {
        use crate::target::tests::{MetadataFixture, configuration, declaration};
        let metadata = MetadataFixture::new();
        let game = metadata.executable("game");
        let launcher = metadata.executable("launcher");
        let mut configuration = configuration(game.to_str().unwrap());
        configuration.launcher = Some(crate::target::TargetLocation {
            kind: "executable".into(),
            path: launcher.to_str().unwrap().into(),
        });
        let directory = Directory::new();
        let profiles = directory.profiles("First");
        directory.profiles("Second");
        let store = directory.store();
        let package = inventory().package_id;
        let empty = store.read_target("First", &package).unwrap();
        let checked = store
            .check_target(
                "First",
                &package,
                &declaration(),
                &empty.expectation(),
                &configuration,
            )
            .unwrap();
        assert!(!profiles.directory().exists());
        let (saved, saved_check) = store
            .save_target(
                "First",
                &package,
                &declaration(),
                &empty.expectation(),
                configuration.clone(),
                None,
            )
            .unwrap();
        assert_eq!(checked, saved_check);
        assert_eq!(
            directory.store().read_target("First", &package).unwrap(),
            saved
        );
        assert_eq!(
            saved.binding.as_ref().unwrap().configuration.arguments,
            ["", "two words", "$(literal)", "\"quoted\""]
        );
        assert_eq!(store.read_target("Second", &package).unwrap().revision, 0);
        let (other, _) = store
            .save_target(
                "Second",
                &package,
                &declaration(),
                &empty.expectation(),
                configuration.clone(),
                None,
            )
            .unwrap();
        assert_ne!(
            saved.binding.as_ref().unwrap().id,
            other.binding.as_ref().unwrap().id
        );
        store
            .bind_package("First", &package, &directory.0.join("relocated-package"))
            .unwrap();
        assert_eq!(store.read_target("First", &package).unwrap(), saved);
        let mut edited = configuration.clone();
        edited.arguments.push("literal next".into());
        let (updated, _) = store
            .save_target(
                "First",
                &package,
                &declaration(),
                &saved.expectation(),
                edited.clone(),
                None,
            )
            .unwrap();
        assert_eq!(
            updated.binding.as_ref().unwrap().id,
            saved.binding.as_ref().unwrap().id
        );
        assert_eq!(updated.revision, saved.revision + 1);
        assert_eq!(
            store
                .save_target(
                    "First",
                    &package,
                    &declaration(),
                    &saved.expectation(),
                    configuration,
                    None
                )
                .unwrap_err()
                .category,
            "TargetConflict"
        );
        assert_eq!(
            store
                .remove_target("First", &package, &saved.expectation())
                .unwrap_err()
                .category,
            "TargetConflict"
        );
        let mut wrong_id = updated.expectation();
        wrong_id.binding_id = other.expectation().binding_id;
        assert_eq!(
            store
                .check_target("First", &package, &declaration(), &wrong_id, &edited)
                .unwrap_err()
                .category,
            "TargetConflict"
        );
        let changed = TargetDeclaration {
            id: "different-game".into(),
            window_title: None,
            macos: None,
        };
        assert!(!updated.binding.as_ref().unwrap().compatible(
            &package,
            &changed.id,
            &changed.identity().unwrap()
        ));
        assert_eq!(store.read_target("First", &package).unwrap(), updated);
        let changed_title = TargetDeclaration {
            id: declaration().id,
            window_title: Some("Another exact title".into()),
            macos: None,
        };
        assert!(!updated.binding.as_ref().unwrap().compatible(
            &package,
            &changed_title.id,
            &changed_title.identity().unwrap(),
        ));
        let (replaced, _) = store
            .save_target(
                "First",
                &package,
                &changed,
                &updated.expectation(),
                edited,
                None,
            )
            .unwrap();
        assert_ne!(
            replaced.binding.as_ref().unwrap().id,
            updated.binding.as_ref().unwrap().id
        );
        assert_eq!(store.read_target("Second", &package).unwrap(), other);
        let removed = store
            .remove_target("First", &package, &replaced.expectation())
            .unwrap();
        assert_eq!(removed.binding, None);
        assert_eq!(removed.revision, replaced.revision + 1);
        let stale_configuration = replaced.binding.as_ref().unwrap().configuration.clone();
        assert_eq!(
            store
                .save_target(
                    "First",
                    &package,
                    &changed,
                    &empty.expectation(),
                    stale_configuration.clone(),
                    None
                )
                .unwrap_err()
                .category,
            "TargetConflict"
        );
        assert_eq!(
            store
                .save_target(
                    "First",
                    &package,
                    &changed,
                    &replaced.expectation(),
                    stale_configuration,
                    None
                )
                .unwrap_err()
                .category,
            "TargetConflict"
        );
        assert!(game.exists());
        assert!(launcher.exists());
    }

    #[test]
    fn target_mutations_share_the_global_budget_with_retained_target_files() {
        let directory = Directory::new();
        directory.profiles("Owner");
        let store = directory.store();
        let package = inventory().package_id;
        let empty = store.read_target("Owner", &package).unwrap();
        let padding = vec![b' '; MAX_TARGET_BYTES];
        for tab in 0..16 {
            for package in 0..16 {
                put(
                    &directory
                        .0
                        .join(format!("tabs/Retained{tab}/pkg{package}/target.config")),
                    &padding,
                );
            }
        }
        assert_eq!(
            store
                .remove_target("Owner", &package, &empty.expectation())
                .unwrap_err()
                .category,
            "StorageLimit"
        );
        assert!(!target_path(&directory, "Owner").exists());
        fs::remove_file(directory.0.join("tabs/Retained0/pkg0/target.config")).unwrap();
        let removed = store
            .remove_target("Owner", &package, &empty.expectation())
            .unwrap();
        assert_eq!(removed.revision, 1);
        assert_eq!(removed.binding, None);
    }
}
