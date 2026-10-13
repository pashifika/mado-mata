//! Bounded local observations, never repair receipts or runtime admission.
//! Files are checked independently; no coherent multi-file generation is claimed.
use crate::configuration::{MAX_BYTES, MAX_ENUMERATED, MAX_FILES};
use crate::distribution::{self, InstalledIdentity};
use crate::storage::{
    self, MAX_DIRECTORY_ENTRIES, MAX_PROFILE_BYTES, MAX_PROFILES, MAX_SETTINGS_BYTES,
    MAX_TAB_BYTES, MAX_TABS, MAX_TOTAL_BYTES, PackageSource, Profile, Settings, TabRecord,
};
use crate::target::{MAX_TARGET_BYTES, TargetRecord};
use mado_runtime_comparison::host::resolve_options;
use mado_runtime_comparison::inventory::{DraftFileKind, TargetDeclaration, is_os_metadata_entry};
use mado_runtime_comparison::model::{Fault, identity};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

const MAX_CHECKS: usize = 512;
const MAX_REPORT_BYTES: usize = 256 * 1024;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorRequest {
    pub package: Option<InstalledIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Root, Settings, Transactions, Tabs, Tab, Source, Profiles, Profile, Target,
    PackageVerification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status { Passed, Failed, Unexamined }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Retry, ImportOriginal, Inspect, ProfileRecovery, Snapshot, Restore, RecoverTransaction, Manual,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Owner {
    pub internal_name: Option<String>,
    pub package_id: Option<String>,
    pub artifact_id: Option<String>,
    pub profile_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Details { pub path: Option<String> }

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub scope: Scope,
    pub owner: Owner,
    pub status: Status,
    pub code: &'static str,
    pub fault_category: Option<String>,
    pub routes: Vec<Route>,
    pub details: Details,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub root: Option<String>,
    pub checks: Vec<DoctorCheck>,
    pub truncated: bool,
}

struct Scan<'a> {
    root: &'a Path,
    requested: Option<&'a InstalledIdentity>,
    report: DoctorReport,
    report_bytes: usize,
    enumerated: usize,
    files: usize,
    bytes: usize,
    installed: BTreeMap<InstalledIdentity, Result<(), Fault>>,
    verified: Option<PackageObservation>,
}

struct PackageObservation {
    identity: InstalledIdentity,
    schema: Value,
    schema_identity: String,
    target: Option<TargetDeclaration>,
}

fn text(value: &str, maximum: usize) -> String {
    let mut result = String::new();
    for ch in value.chars() {
        let ch = if ch.is_control() { '\u{fffd}' } else { ch };
        if result.len() + ch.len_utf8() > maximum { break; }
        result.push(ch);
    }
    result
}

fn limit() -> Fault { Fault::new("StorageLimit", "diagnostic observation bound reached") }
fn io_fault(_: io::Error) -> Fault { Fault::new("Storage", "configuration is unreadable") }
fn invalid() -> Fault { Fault::new("StorageFormat", "invalid stored document") }

// Count serialized bytes without allocating a second report or disclosing Fault.context.
#[derive(Default)]
struct Size(usize);
impl Write for Size {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

pub(crate) fn observe(root: Result<&Path, &Fault>, request: &DoctorRequest) -> DoctorReport {
    let mut scan = Scan {
        root: root.unwrap_or_else(|_| Path::new("")),
        requested: request.package.as_ref(),
        report: DoctorReport {
            root: root.ok().map(|path| text(&path.to_string_lossy(), storage::MAX_PATH_BYTES)),
            checks: Vec::new(),
            truncated: false,
        },
        report_bytes: 32 * 1024, // reserve escaped root text, envelope and final incomplete marker
        enumerated: 0,
        files: 0,
        bytes: 0,
        installed: BTreeMap::new(),
        verified: None,
    };
    let owner = Owner::default();
    let ready = match root {
        Ok(root) => storage::check_directory(root),
        Err(error) => Err(error.clone()),
    };
    if let Err(error) = ready {
        scan.note(Scope::Root, &owner, Status::Failed, "root_unavailable", Some(&error), ".", &[Route::Manual, Route::Retry]);
        for scope in [Scope::Settings, Scope::Transactions, Scope::Tabs, Scope::PackageVerification] {
            scan.note(scope, &owner, Status::Unexamined, "parent_unavailable", None, "", &[]);
        }
        return scan.finish();
    }
    scan.note(Scope::Root, &owner, Status::Passed, "checked", None, ".", &[]);
    // An ordinary read is not a transaction snapshot and never grants write authority.
    scan.note(Scope::Root, &owner, Status::Unexamined, "observation_only", None, "", &[]);
    let entries = match scan.entries("") {
        Ok(entries) => entries,
        Err(error) => {
            scan.failure(Scope::Root, &owner, &error, ".");
            for scope in [Scope::Settings, Scope::Transactions, Scope::Tabs, Scope::PackageVerification] {
                scan.note(scope, &owner, Status::Unexamined, "parent_unavailable", None, "", &[]);
            }
            return scan.finish();
        }
    };
    scan.transactions(&entries);
    if scan.alias(&entries, "settings.json", Scope::Settings, &owner, "settings.json") {
        match scan.document::<Settings>("settings.json", MAX_SETTINGS_BYTES) {
            Ok(Some(settings)) => match storage::validate_settings(&settings) {
                Ok(()) => scan.note(Scope::Settings, &owner, Status::Passed, "checked", None, "settings.json", &[]),
                Err(error) => scan.failure(Scope::Settings, &owner, &error, "settings.json"),
            },
            Ok(None) => scan.note(Scope::Settings, &owner, Status::Failed, "absent", Some(&Fault::new("SettingsMissing", "")), "settings.json", &[Route::Retry, Route::Restore]),
            Err(error) => scan.failure(Scope::Settings, &owner, &error, "settings.json"),
        }
    }
    scan.verify_requested(request);
    if scan.alias(&entries, "tabs", Scope::Tabs, &owner, "tabs") {
        scan.tabs();
    }
    // Legacy profiles and historical ledgers are retained, not interpreted as current owners.
    for name in ["profiles", "identity-migrations.config"] {
        if entries.iter().any(|entry| storage::filesystem_key(entry) == name) {
            scan.note(Scope::Profiles, &owner, Status::Unexamined, "owner_unavailable", None, name, &[Route::Manual, Route::Snapshot]);
        }
    }
    scan.finish()
}

impl Scan<'_> {
    fn finish(mut self) -> DoctorReport {
        if self.report.truncated {
            self.report.checks.push(DoctorCheck {
                scope: Scope::Root, owner: Owner::default(), status: Status::Unexamined,
                code: "bound_exceeded", fault_category: Some("StorageLimit".into()),
                routes: vec![Route::Manual], details: Details { path: None },
            });
        }
        self.report
    }

    fn note(&mut self, scope: Scope, owner: &Owner, status: Status, code: &'static str,
        fault: Option<&Fault>, path: &str, routes: &[Route]) {
        if self.report.truncated { return; }
        let check = DoctorCheck {
            scope, owner: owner.clone(), status, code,
            fault_category: fault.map(|fault| text(&fault.category, 64)),
            routes: routes.to_vec(),
            details: Details { path: (!path.is_empty()).then(|| text(path, storage::MAX_PATH_BYTES)) },
        };
        let mut size = Size::default();
        if serde_json::to_writer(&mut size, &check).is_err()
            || self.report.checks.len() >= MAX_CHECKS - 1
            || self.report_bytes + size.0 + 1 > MAX_REPORT_BYTES {
            self.report.truncated = true;
            return;
        }
        self.report_bytes += size.0 + 1;
        self.report.checks.push(check);
    }

    fn failure(&mut self, scope: Scope, owner: &Owner, error: &Fault, path: &str) {
        let code = match error.category.as_str() {
            "StorageLimit" => "bound_exceeded",
            "StorageChanged" => "changed_during_read",
            "StorageFormat" | "Settings" | "SettingsVersion" | "TabVersion" | "TabIdentity"
                | "TabName" | "PackageSource" | "ProfileVersion" | "ProfileIdentity"
                | "Profile" | "ProfileAuthority" | "NumericPrecision" | "Schema"
                | "TargetOwner" | "TargetVersion" | "TargetRevision" | "TargetConfiguration" => "invalid_document",
            "StoragePending" => "pending_transaction",
            _ => "read_refused",
        };
        let routes: &[Route] = if code == "pending_transaction" {
            &[Route::Manual, Route::Retry]
        } else if matches!(scope, Scope::Source | Scope::PackageVerification) {
            &[Route::Manual, Route::Inspect, Route::Retry]
        } else if code == "invalid_document" {
            &[Route::Manual, Route::Snapshot, Route::Restore, Route::Retry]
        } else {
            &[Route::Manual, Route::Retry]
        };
        self.note(scope, owner, Status::Failed, code, Some(error), path, routes);
        if matches!(code, "bound_exceeded" | "read_refused" | "changed_during_read") {
            self.note(scope, owner, Status::Unexamined, "parent_unavailable", None, path, &[]);
        }
    }

    fn entries(&mut self, relative: &str) -> Result<Vec<String>, Fault> {
        if self.report.truncated { return Err(limit()); }
        let path = self.root.join(relative);
        storage::check_directory(&path)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(&path).map_err(io_fault)? {
            self.enumerated += 1;
            if self.enumerated > MAX_ENUMERATED { return Err(limit()); }
            let entry = entry.map_err(io_fault)?;
            if is_os_metadata_entry(&entry.file_name(), &entry).map_err(io_fault)? { continue; }
            if names.len() >= MAX_DIRECTORY_ENTRIES - 1 { return Err(limit()); }
            names.push(entry.file_name().into_string().map_err(|_| invalid())?);
        }
        names.sort();
        Ok(names)
    }

    fn alias(&mut self, entries: &[String], name: &str, scope: Scope, owner: &Owner, path: &str) -> bool {
        let key = storage::filesystem_key(name);
        if entries.iter().any(|entry| entry != name && storage::filesystem_key(entry) == key) {
            self.failure(scope, owner, &Fault::new("StorageAlias", "aliased stored identity"), path);
            return false;
        }
        true
    }

    fn document<T: DeserializeOwned>(&mut self, relative: &str, maximum: usize) -> Result<Option<T>, Fault> {
        if self.report.truncated || self.files >= MAX_FILES { return Err(limit()); }
        let path = self.root.join(relative);
        if !storage::exists(&path)? { return Ok(None); }
        self.files += 1;
        let bytes = storage::read_bytes(&path, maximum.min(MAX_BYTES.saturating_sub(self.bytes)))?;
        self.bytes += bytes.len();
        storage::decode(&bytes).map(Some)
    }

    fn transactions(&mut self, entries: &[String]) {
        let owner = Owner::default();
        let mut pending_write = false;
        for entry in entries {
            let key = storage::filesystem_key(entry);
            if key == "settings.pending" {
                pending_write = true;
                self.failure(Scope::Transactions, &owner, &Fault::new("StoragePending", "pending write"), entry);
            }
        }
        if !self.alias(entries, ".restore-journal", Scope::Transactions, &owner, ".restore-journal")
            || !self.alias(entries, ".restore-completion", Scope::Transactions, &owner, ".restore-completion")
            || !self.alias(entries, "identity-migrations.pending", Scope::Transactions, &owner, "identity-migrations.pending") { return; }
        match crate::restore::pending(self.root) {
            Ok(false) if !pending_write => self.note(Scope::Transactions, &owner, Status::Passed, "checked", None, "", &[]),
            Ok(false) => {}
            Ok(true) => match crate::restore::check_recovery(self.root) {
                Ok(()) => self.note(Scope::Transactions, &owner, Status::Failed, "pending_transaction",
                    Some(&Fault::new("RestorePending", "")), "", &[Route::RecoverTransaction]),
                Err(error) => self.note(Scope::Transactions, &owner, Status::Failed, "recovery_unavailable", Some(&error), "", &[Route::Manual, Route::Retry]),
            },
            Err(error) => self.failure(Scope::Transactions, &owner, &error, ""),
        }
    }

    fn verify_requested(&mut self, request: &DoctorRequest) {
        if self.report.truncated { return; }
        let Some(package) = &request.package else {
            self.note(Scope::PackageVerification, &Owner::default(), Status::Unexamined,
                "verification_not_requested", None, "", &[]);
            return;
        };
        let owner = Owner { package_id: Some(text(&package.package_id, 240)), artifact_id: Some(text(&package.artifact_id, 64)), ..Owner::default() };
        let result = distribution::observe_installed(self.root, &package.package_id, &package.artifact_id)
            .and_then(|()| distribution::verify_installed(self.root, &package.package_id, &package.artifact_id));
        match result {
            Ok(verified) => {
                self.note(Scope::PackageVerification, &owner, Status::Passed, "verified", None, "", &[]);
                // Only schema/target metadata is interpreted; draft.validate() would compile source.
                let result = (|| {
                    let draft = verified.draft();
                    let schema_path = draft.kinds().iter().find_map(|(path, kind)|
                        (*kind == DraftFileKind::Schema).then_some(path)).ok_or_else(invalid)?;
                    let schema: Value = serde_json::from_slice(&draft.files()[schema_path]).map_err(|_| invalid())?;
                    crate::application::check_webview_value(&schema, "$")?;
                    if let Err(error) = resolve_options(&schema, &json!({
                        "package_id": package.package_id,
                        "schema_version": schema["version"],
                        "options": {},
                    }), &package.package_id) {
                        if error.category == "Schema" { return Err(error); }
                    }
                    let schema_identity = identity(&schema)?;
                    let manifest = draft.manifest()?;
                    let target = manifest.get("target").map(TargetDeclaration::deserialize).transpose().map_err(|_| invalid())?;
                    Ok::<_, Fault>(PackageObservation { identity: package.clone(), schema, schema_identity, target })
                })();
                match result {
                    Ok(observation) => self.verified = Some(observation),
                    Err(error) => self.failure(Scope::PackageVerification, &owner, &error, ""),
                }
            }
            Err(error) => self.source_failure(Scope::PackageVerification, &owner, &error),
        }
    }

    fn source_failure(&mut self, scope: Scope, owner: &Owner, error: &Fault) {
        let (code, routes): (_, &[Route]) = match error.category.as_str() {
            "InstalledMissing" => ("archive_missing", &[Route::ImportOriginal, Route::Inspect]),
            "InstalledReference" => ("invalid_document", &[Route::Manual, Route::Retry]),
            _ => ("archive_invalid", &[Route::Manual, Route::Retry]),
        };
        self.note(scope, owner, Status::Failed, code, Some(error), "", routes);
        if error.category == "InstalledMissing" {
            self.legacy_layout(scope, owner);
        } else {
            self.note(scope, owner, Status::Unexamined, "parent_unavailable", None, "", &[]);
        }
    }

    fn legacy_layout(&mut self, scope: Scope, owner: &Owner) {
        let (Some(package), Some(artifact)) = (&owner.package_id, &owner.artifact_id) else { return; };
        let relative = format!("pkgs/{package}/{artifact}");
        for parent in ["pkgs".to_owned(), format!("pkgs/{package}"), relative.clone()] {
            let path = self.root.join(&parent);
            match storage::exists(&path).and_then(|present| {
                if present { storage::check_directory(&path)?; }
                Ok(present)
            }) {
                Ok(false) => return,
                Ok(true) => {}
                Err(error) => { self.failure(scope, owner, &error, &parent); return; }
            }
        }
        // Only fixed member names are observed; expanded source is never opened.
        match distribution::managed_identity(self.root, &self.root.join(&relative)) {
            Ok(Some(_)) => self.note(scope, owner, Status::Failed, "legacy_layout_refused", None,
                &relative, &[Route::ImportOriginal, Route::Inspect]),
            Ok(None) => self.note(scope, owner, Status::Unexamined, "source_unexamined", None,
                &relative, &[Route::Manual]),
            Err(error) => self.failure(scope, owner, &error, &relative),
        }
    }

    fn tabs(&mut self) {
        let owner = Owner::default();
        match storage::exists(&self.root.join("tabs")) {
            Ok(false) => { self.note(Scope::Tabs, &owner, Status::Passed, "absent", None, "tabs", &[]); return; }
            Err(error) => { self.failure(Scope::Tabs, &owner, &error, "tabs"); return; }
            Ok(true) => {}
        }
        let entries = match self.entries("tabs") {
            Ok(entries) => entries,
            Err(error) => { self.failure(Scope::Tabs, &owner, &error, "tabs"); return; }
        };
        let mut count = 0;
        let mut open = 0;
        for name in &entries {
            if self.report.truncated { break; }
            let relative = format!("tabs/{name}");
            let owner = Owner { internal_name: Some(text(name, 64)), ..Owner::default() };
            match fs::symlink_metadata(self.root.join(&relative)) {
                Ok(metadata) if metadata.is_file() => continue,
                Err(error) => { self.failure(Scope::Tab, &owner, &io_fault(error), &relative); continue; }
                _ => {}
            }
            count += 1;
            if count > MAX_TABS { self.failure(Scope::Tabs, &Owner::default(), &limit(), "tabs"); break; }
            if let Err(error) = storage::validate_internal_name(name) {
                self.failure(Scope::Tab, &owner, &error, &relative); continue;
            }
            if !self.alias(&entries, name, Scope::Tab, &owner, &relative) { continue; }
            if self.tab(&relative, &owner) { open += 1; }
        }
        if open > storage::MAX_OPEN_TABS { self.failure(Scope::Tabs, &Owner::default(), &limit(), "tabs"); }
    }

    fn tab(&mut self, relative: &str, owner: &Owner) -> bool {
        let entries = match self.entries(relative) {
            Ok(entries) => entries,
            Err(error) => { self.failure(Scope::Tab, owner, &error, relative); return false; }
        };
        for entry in &entries {
            if storage::filesystem_key(entry) == "tab.pending" {
                self.failure(Scope::Transactions, owner, &Fault::new("StoragePending", "pending Tab write"), &format!("{relative}/{entry}"));
            }
        }
        let path = format!("{relative}/tab.config");
        if !self.alias(&entries, "tab.config", Scope::Tab, owner, &path) { return false; }
        let result = self.document::<TabRecord>(&path, MAX_TAB_BYTES).and_then(|tab| {
            let Some(tab) = tab else {
                return if entries.is_empty() { Ok(None) } else { Err(Fault::new("TabOrphan", "retained Tab lacks a record")) };
            };
            storage::validate_tab(&tab)?;
            if Some(&tab.internal_name) != owner.internal_name.as_ref() { return Err(Fault::new("TabIdentity", "Tab owner differs")); }
            Ok(Some(tab))
        });
        let tab = match result {
            Ok(Some(tab)) => tab,
            Ok(None) => { self.note(Scope::Tab, owner, Status::Passed, "absent", None, &path, &[]); return false; }
            Err(error) => {
                self.failure(Scope::Tab, owner, &error, &path);
                for scope in [Scope::Source, Scope::Profiles, Scope::Target] {
                    self.note(scope, owner, Status::Unexamined, "owner_unavailable", None, relative, &[]);
                }
                return false;
            }
        };
        self.note(Scope::Tab, owner, Status::Passed, "checked", None, &path, &[]);
        let mut owned = BTreeSet::new();
        for reference in &tab.packages {
            if self.report.truncated { break; }
            owned.insert(reference.package_id.as_str());
            let mut owner = owner.clone();
            owner.package_id = Some(reference.package_id.clone());
            if let PackageSource::Installed { artifact_id, .. } = &reference.source { owner.artifact_id = Some(artifact_id.clone()); }
            self.source(&owner, &reference.source);
            let package_path = format!("{relative}/{}", reference.package_id);
            if self.alias(&entries, &reference.package_id, Scope::Profiles, &owner, &package_path) {
                self.package(&package_path, &owner, true);
            }
        }
        for entry in &entries {
            if entry == "tab.config" || entry.ends_with(".pending") || owned.contains(entry.as_str()) { continue; }
            let path = format!("{relative}/{entry}");
            match fs::symlink_metadata(self.root.join(&path)) {
                Ok(metadata) if metadata.is_file() => continue,
                Err(error) => { self.failure(Scope::Target, owner, &io_fault(error), &path); continue; }
                _ => {}
            }
            let package_owner = Owner { package_id: Some(text(entry, 240)), ..owner.clone() };
            if let Err(error) = storage::validate_package_id(entry) {
                self.failure(Scope::Target, &package_owner, &error, &path);
                continue;
            }
            if self.alias(&entries, entry, Scope::Target, &package_owner, &path) {
                self.package(&path, &package_owner, false);
            }
        }
        tab.open
    }

    fn source(&mut self, owner: &Owner, source: &PackageSource) {
        match source {
            PackageSource::Installed { package_id, artifact_id } => {
                let identity = InstalledIdentity { package_id: package_id.clone(), artifact_id: artifact_id.clone() };
                let result = self.installed.entry(identity).or_insert_with(|| distribution::observe_installed(self.root, package_id, artifact_id)).clone();
                match result {
                    Ok(()) => self.note(Scope::Source, owner, Status::Passed, "archive_present", None, "", &[Route::Inspect]),
                    Err(error) => self.source_failure(Scope::Source, owner, &error),
                }
                if self.verified.as_ref().is_none_or(|verified| &verified.identity.package_id != package_id || &verified.identity.artifact_id != artifact_id) {
                    let code = if self.requested.is_some_and(|requested|
                        &requested.package_id == package_id && &requested.artifact_id == artifact_id) {
                        "parent_unavailable"
                    } else {
                        "verification_not_requested"
                    };
                    self.note(Scope::Source, owner, Status::Unexamined, code, None, "", &[]);
                }
            }
            PackageSource::CustomArchive { .. } => self.note(Scope::Source, owner, Status::Failed, "unsupported_source",
                Some(&Fault::new("UnsupportedPackageSource", "")), "", &[Route::Inspect, Route::Manual]),
            PackageSource::Directory { path } => {
                match distribution::managed_identity(self.root, Path::new(path)) {
                    Ok(Some(_)) => self.note(Scope::Source, owner, Status::Failed, "legacy_layout_refused", None, path, &[Route::Manual, Route::Inspect]),
                    Err(error) => self.failure(Scope::Source, owner, &error, path),
                    Ok(None) => match fs::symlink_metadata(path) {
                        Ok(metadata) if metadata.is_dir() => self.note(Scope::Source, owner, Status::Unexamined, "source_unexamined", None, path, &[Route::Inspect]),
                        Ok(_) => self.failure(Scope::Source, owner, &Fault::new("PackageSource", "not a directory"), path),
                        Err(error) => self.failure(Scope::Source, owner, &io_fault(error), path),
                    },
                }
            }
        }
    }

    fn package(&mut self, relative: &str, owner: &Owner, bound_source: bool) {
        match storage::exists(&self.root.join(relative)) {
            Ok(false) => {
                for scope in [Scope::Profiles, Scope::Target] { self.note(scope, owner, Status::Passed, "absent", None, relative, &[]); }
                return;
            }
            Err(error) => {
                self.failure(Scope::Profiles, owner, &error, relative);
                self.note(Scope::Target, owner, Status::Unexamined, "parent_unavailable", None, relative, &[]);
                return;
            }
            Ok(true) => {}
        }
        let entries = match self.entries(relative) {
            Ok(entries) => entries,
            Err(error) => {
                self.failure(Scope::Profiles, owner, &error, relative);
                self.note(Scope::Target, owner, Status::Unexamined, "parent_unavailable", None, relative, &[]);
                return;
            }
        };
        let mut profiles = 0;
        let mut bytes = 0;
        for entry in &entries {
            if self.report.truncated { return; }
            let key = storage::filesystem_key(entry);
            let path = format!("{relative}/{entry}");
            if key.ends_with(".pending") {
                self.failure(Scope::Transactions, owner, &Fault::new("StoragePending", "pending configuration write"), &path);
                continue;
            }
            if key == "target.config" || !key.ends_with(".config") { continue; }
            profiles += 1;
            if profiles > MAX_PROFILES { self.failure(Scope::Profiles, owner, &limit(), relative); break; }
            if !bound_source { continue; }
            let id = entry.strip_suffix(".config").unwrap_or("");
            let mut profile_owner = owner.clone();
            profile_owner.profile_id = Some(text(id, 64));
            if !self.alias(&entries, entry, Scope::Profile, &profile_owner, &path) { continue; }
            if let Err(error) = storage::validate_id(id) { self.failure(Scope::Profile, &profile_owner, &error, &path); continue; }
            let before = self.bytes;
            let result = self.document::<Profile>(&path, MAX_PROFILE_BYTES).and_then(|profile| {
                let profile = profile.ok_or_else(|| Fault::new("StorageChanged", "profile disappeared"))?;
                storage::validate_profile(&profile)?;
                if profile.id != id || Some(&profile.package_id) != owner.package_id.as_ref() { return Err(Fault::new("ProfileIdentity", "profile owner differs")); }
                crate::application::check_webview_value(&profile.values, "$")?;
                Ok(profile)
            });
            bytes += self.bytes - before;
            if bytes > MAX_TOTAL_BYTES { self.failure(Scope::Profiles, owner, &limit(), relative); break; }
            match result {
                Ok(profile) => self.profile(&profile_owner, &path, profile),
                Err(error) => self.failure(Scope::Profile, &profile_owner, &error, &path),
            }
        }
        if profiles == 0 { self.note(Scope::Profiles, owner, Status::Passed, "absent", None, relative, &[]); }
        else if !bound_source {
            self.note(Scope::Profiles, owner, Status::Unexamined, "owner_unavailable", None, relative, &[Route::Manual, Route::Snapshot]);
        }
        let path = format!("{relative}/target.config");
        if self.alias(&entries, "target.config", Scope::Target, owner, &path) {
            match self.document::<TargetRecord>(&path, MAX_TARGET_BYTES) {
                Ok(Some(record)) => match record.validate_owned(owner.internal_name.as_deref().unwrap_or(""), owner.package_id.as_deref().unwrap_or("")) {
                    Ok(()) => self.target(owner, &path, &record),
                    Err(error) => self.failure(Scope::Target, owner, &error, &path),
                },
                Ok(None) => self.note(Scope::Target, owner, Status::Passed, "absent", None, &path, &[]),
                Err(error) => self.failure(Scope::Target, owner, &error, &path),
            }
        }
    }

    fn verified_for(&self, owner: &Owner) -> Option<&PackageObservation> {
        self.verified.as_ref().filter(|verified| Some(&verified.identity.package_id) == owner.package_id.as_ref()
            && Some(&verified.identity.artifact_id) == owner.artifact_id.as_ref())
    }

    fn profile(&mut self, owner: &Owner, path: &str, profile: Profile) {
        self.note(Scope::Profile, owner, Status::Passed, "checked", None, path, &[]);
        let Some(verified) = self.verified_for(owner) else {
            self.note(Scope::Profile, owner, Status::Unexamined, "schema_unexamined", None, path, &[Route::Inspect]);
            return;
        };
        let result = if profile.schema_identity != verified.schema_identity {
            Err(Fault::new("ProfileIdentity", "profile schema differs"))
        } else {
            resolve_options(&verified.schema, &json!({"package_id": profile.package_id, "schema_version": verified.schema["version"], "options": profile.values}), &verified.identity.package_id)
                .and_then(|values| crate::application::check_webview_value(&values, "$"))
        };
        match result {
            Ok(()) => self.note(Scope::Profile, owner, Status::Passed, "verified", None, path, &[]),
            Err(error) => self.note(Scope::Profile, owner, Status::Failed, "profile_incompatible", Some(&error), path, &[Route::ProfileRecovery, Route::Inspect]),
        }
    }

    fn target(&mut self, owner: &Owner, path: &str, record: &TargetRecord) {
        self.note(Scope::Target, owner, Status::Passed, "checked", None, path, &[]);
        if let (Some(verified), Some(binding)) = (self.verified_for(owner), &record.binding) {
            let compatible = verified.target.as_ref().is_some_and(|target| target.identity().is_ok_and(|identity|
                binding.compatible(&verified.identity.package_id, &target.id, &identity)));
            if !compatible {
                self.note(Scope::Target, owner, Status::Failed, "target_incompatible", None, path, &[Route::Inspect]);
            }
        }
        self.note(Scope::Target, owner, Status::Unexamined, "target_unexamined", None, path, &[Route::Inspect]);
    }
}

#[cfg(test)]
mod tests;
