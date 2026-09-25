use super::fs::{check_alias, check_directory};
use super::{
    MAX_PATH_BYTES, MAX_SETTINGS_BYTES, Store, VERSION, check_budget, decode, encode, exists,
    private_directory, read_bytes, write_atomic,
};
use crate::configuration::publish_no_replace;
use mado_runtime_comparison::environment::OcrEnvironment;
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

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
    #[serde(default)]
    pub packages_root: Option<String>,
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
    #[serde(default)]
    pub packages_root: Option<String>,
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
            packages_root: None,
        }
    }
}

impl Store {
    pub fn packages_root(&self) -> Result<PathBuf, Fault> {
        self.resolve_packages_root(&self.settings()?)
    }

    fn resolve_packages_root(&self, settings: &Settings) -> Result<PathBuf, Fault> {
        let root = settings
            .packages_root
            .as_ref()
            .map_or_else(|| self.root.join("pkgs"), PathBuf::from);
        std::path::absolute(root).map_err(|error| {
            Fault::new("Settings", format!("cannot resolve packages root: {error}"))
        })
    }

    fn check_packages_root(&self, settings: &Settings) -> Result<(), Fault> {
        crate::authoring::Publisher::new(self.root.clone())
            .packages_root(&self.resolve_packages_root(settings)?)
            .map(|_| ())
    }

    pub fn initialize(&self, preferences: EditableSettings) -> Result<Settings, Fault> {
        let settings = Settings {
            locale: preferences.locale,
            gui_log_limit: preferences.gui_log_limit,
            ocr_environment: preferences.ocr_environment,
            notifications: preferences.notifications,
            backup_directory: preferences.backup_directory,
            packages_root: preferences.packages_root,
            ..Settings::default()
        };
        validate_settings(&settings)?;
        self.check_packages_root(&settings)?;
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
        settings.packages_root = preferences.packages_root;
        validate_settings(&settings)?;
        self.check_packages_root(&settings)?;
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
}

fn settings_missing() -> Fault {
    Fault::new(
        "SettingsMissing",
        "application settings are missing; explicit initialization is required",
    )
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
    if settings.packages_root.as_ref().is_some_and(|path| {
        path.trim().is_empty()
            || path.len() > MAX_PATH_BYTES
            || !Path::new(path).is_absolute()
            || Path::new(path)
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            || path.chars().any(char::is_control)
    }) {
        return Err(Fault::new(
            "Settings",
            "packages root must be an absolute path of at most 4096 bytes without parent traversal",
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

#[cfg(test)]
mod tests;
