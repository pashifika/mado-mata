use crate::application::{Application, ObservationSlot, WorkspaceCatalog};
use crate::backup::{self, SnapshotReceipt};
use crate::configuration;
use crate::restore;
use crate::storage::{self, EditableSettings, Profile, Settings, Store};
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

static NEXT_IMPORT: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Loading,
    Setup,
    Ready,
    Recovery,
}

#[derive(Serialize)]
pub struct BootstrapStatus {
    pub state: Phase,
    pub stage: String,
    pub root: Option<String>,
    pub legacy_root: Option<String>,
    pub default_packages_root: Option<String>,
    pub fault: Option<Fault>,
    pub settings: Option<Settings>,
    pub application_available: bool,
    pub pending_restore: bool,
    pub catalog: Option<WorkspaceCatalog>,
}

struct State {
    phase: Phase,
    stage: String,
    fault: Option<Fault>,
    settings: Option<Settings>,
    application: Option<Arc<Application>>,
    pending_restore: bool,
    receipt: Option<SnapshotReceipt>,
}

pub struct Bootstrap {
    root: Result<PathBuf, Fault>,
    legacy_root: Option<PathBuf>,
    controlled: PathBuf,
    engine: PathBuf,
    target_observation: Arc<Mutex<ObservationSlot>>,
    actions: Mutex<()>,
    state: Mutex<State>,
    closing: AtomicBool,
    #[cfg(test)]
    make_application: fn(
        PathBuf,
        PathBuf,
        PathBuf,
        Arc<Mutex<ObservationSlot>>,
    ) -> Result<Arc<Application>, Fault>,
    #[cfg(test)]
    recover_configuration: fn(&Path, bool) -> Result<(), Fault>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn selected_roots(
    home: Result<PathBuf, Fault>,
    explicit: Option<PathBuf>,
) -> (Result<PathBuf, Fault>, Option<PathBuf>) {
    if let Some(root) = explicit {
        return (Ok(root), None);
    }
    match home {
        Ok(home) => {
            let root = home.join(".config/mado-mata");
            let legacy = home.join("Library/Application Support/dev.madomata.desktop");
            let discover = matches!(fs::symlink_metadata(&root), Err(error) if error.kind() == io::ErrorKind::NotFound);
            let candidate = discover
                && !matches!(fs::symlink_metadata(&legacy), Err(error) if error.kind() == io::ErrorKind::NotFound);
            (Ok(root), candidate.then_some(legacy))
        }
        Err(error) => (Err(error), None),
    }
}

impl Bootstrap {
    pub fn new(
        root: Result<PathBuf, Fault>,
        legacy_root: Option<PathBuf>,
        controlled: PathBuf,
        engine: PathBuf,
    ) -> Self {
        Self {
            root,
            legacy_root,
            controlled,
            engine,
            target_observation: Arc::default(),
            actions: Mutex::new(()),
            closing: AtomicBool::new(false),
            #[cfg(test)]
            make_application: Application::with_observation_slot,
            #[cfg(test)]
            recover_configuration: restore::recover,
            state: Mutex::new(State {
                phase: Phase::Loading,
                stage: "loading".into(),
                fault: None,
                settings: None,
                application: None,
                pending_restore: false,
                receipt: None,
            }),
        }
    }

    pub fn ensure_started(&self) -> Result<BootstrapStatus, Fault> {
        if lock(&self.state).phase == Phase::Loading {
            match self.begin() {
                Ok(_action) => {
                    if lock(&self.state).phase == Phase::Loading {
                        self.load();
                    }
                }
                Err(error) if error.category == "RecoveryBusy" => return Ok(self.status()),
                Err(error) => return Err(error),
            }
        }
        Ok(self.status())
    }

    pub fn status(&self) -> BootstrapStatus {
        let state = lock(&self.state);
        let application = state.application.clone();
        let mut status = BootstrapStatus {
            state: state.phase,
            stage: state.stage.clone(),
            fault: state.fault.clone(),
            settings: state.settings.clone(),
            application_available: application.is_some(),
            default_packages_root: self
                .root
                .as_ref()
                .ok()
                .and_then(|path| std::path::absolute(path.join("pkgs")).ok())
                .map(|path| path.to_string_lossy().into_owned()),
            root: self
                .root
                .as_ref()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            legacy_root: self
                .legacy_root
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            pending_restore: state.pending_restore,
            catalog: None,
        };
        drop(state);
        if let Some(application) = application {
            match application.workspace_catalog() {
                Ok(catalog) => status.catalog = Some(catalog),
                Err(error) if matches!(status.state, Phase::Ready) => {
                    let transient = error.category == "WorkspaceBusy";
                    status.stage = if transient { "workspace" } else { "tabs" }.into();
                    status.fault = Some(error.clone());
                    if !transient {
                        status.state = Phase::Recovery;
                        let mut state = lock(&self.state);
                        if matches!(state.phase, Phase::Ready)
                            && state
                                .application
                                .as_ref()
                                .is_some_and(|current| Arc::ptr_eq(current, &application))
                        {
                            state.phase = Phase::Recovery;
                            state.stage = status.stage.clone();
                            state.fault = Some(error);
                        }
                    }
                }
                Err(_) => {}
            }
        }
        status
    }

    pub fn application(&self) -> Result<Arc<Application>, Fault> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        let state = lock(&self.state);
        if state.phase != Phase::Ready {
            return Err(Fault::new(
                "NotReady",
                "Complete setup or recovery before using configuration",
            ));
        }
        state
            .application
            .clone()
            .ok_or_else(|| Fault::new("NotReady", "Application is not initialized"))
    }

    pub fn running_application(&self) -> Result<Arc<Application>, Fault> {
        lock(&self.state)
            .application
            .clone()
            .ok_or_else(|| Fault::new("NotReady", "Application is not initialized"))
    }

    fn begin(&self) -> Result<MutexGuard<'_, ()>, Fault> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        match self.actions.try_lock() {
            Ok(guard) => Ok(guard),
            Err(TryLockError::Poisoned(error)) => Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) => Err(Fault::new(
                "RecoveryBusy",
                "A configuration recovery action is still in progress",
            )),
        }
    }

    fn fail(&self, stage: &str, fault: Fault) {
        let mut state = lock(&self.state);
        state.phase = Phase::Recovery;
        state.stage = stage.into();
        state.fault = Some(fault);
        state.settings = None;
    }

    fn load(&self) {
        let root = match &self.root {
            Ok(root) => root,
            Err(error) => {
                self.fail("root", error.clone());
                return;
            }
        };
        match fs::symlink_metadata(root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.setup();
                return;
            }
            Err(error) => {
                self.fail("root", Fault::new("Storage", error.to_string()));
                return;
            }
            Ok(_) => {}
        }
        if let Err(error) = storage::check_directory(root) {
            self.fail("root", error);
            return;
        }
        match restore::pending(root) {
            Ok(true) => {
                lock(&self.state).pending_restore = true;
                self.fail(
                    "restore",
                    Fault::new(
                        "RestorePending",
                        "An interrupted restore must be completed or rolled back",
                    ),
                );
                return;
            }
            Err(error) => {
                self.fail("restore", error);
                return;
            }
            Ok(false) => lock(&self.state).pending_restore = false,
        }
        let store = match Store::new(root.clone()) {
            Ok(store) => store,
            Err(error) => {
                self.fail("storage", error);
                return;
            }
        };
        let settings = match store.settings() {
            Ok(settings) => settings,
            Err(error) if error.category == "SettingsMissing" => {
                self.setup();
                return;
            }
            Err(error) => {
                self.fail("settings", error);
                return;
            }
        };
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        #[cfg(not(test))]
        let make_application = Application::with_observation_slot;
        #[cfg(test)]
        let make_application = self.make_application;
        match make_application(
            root.clone(),
            self.controlled.clone(),
            self.engine.clone(),
            self.target_observation.clone(),
        ) {
            Ok(application) => {
                if self.closing.load(Ordering::Acquire) {
                    let _ = application.shutdown();
                    return;
                }
                let mut state = lock(&self.state);
                state.application = Some(application);
                state.settings = Some(settings);
                state.phase = Phase::Ready;
                state.stage = "ready".into();
                state.fault = None;
            }
            Err(error) => self.fail("application", error),
        }
    }

    fn setup(&self) {
        let mut state = lock(&self.state);
        state.phase = Phase::Setup;
        state.stage = "settings".into();
        state.fault = None;
        state.settings = None;
        state.pending_restore = false;
    }

    pub fn initialize(
        &self,
        preferences: EditableSettings,
        confirm_fresh: bool,
    ) -> Result<BootstrapStatus, Fault> {
        let _action = self.begin()?;
        if lock(&self.state).application.is_some() {
            return Err(Fault::new(
                "NotSetup",
                "Configuration is already initialized",
            ));
        }
        let root = self.root.as_ref().map_err(Clone::clone)?;
        if restore::pending(root)? {
            return Err(Fault::new(
                "RestorePending",
                "Resolve the interrupted restore first",
            ));
        }
        if self.legacy_root.is_some() && !confirm_fresh {
            return Err(Fault::new(
                "LegacyConfirmation",
                "Confirm fresh setup instead of importing the historical configuration",
            ));
        }
        match Store::new(root.clone()).and_then(|store| store.initialize(preferences)) {
            Ok(_) => self.load(),
            Err(error) => {
                // A stale Initialize must inspect the document that appeared, never replace it.
                self.load();
                let mut state = lock(&self.state);
                if state.phase == Phase::Setup {
                    state.stage = "initialize".into();
                    state.fault = Some(error);
                }
            }
        }
        Ok(self.status())
    }

    fn retire(&self, discard: bool) -> Result<(), Fault> {
        let application = lock(&self.state).application.clone();
        if let Some(application) = application {
            if !discard {
                return Err(Fault::new(
                    "DiscardRequired",
                    "Confirm discarding session drafts before reconstruction",
                ));
            }
            if let Err(error) = application.prepare_reconstruction() {
                if error.context["application_retired"] == true {
                    self.fail("shutdown", error.clone());
                }
                return Err(error);
            }
            lock(&self.state).application = None;
        }
        Ok(())
    }

    pub fn retry(&self, discard: bool) -> Result<BootstrapStatus, Fault> {
        let _action = self.begin()?;
        self.retire(discard)?;
        self.load();
        Ok(self.status())
    }

    pub fn note_settings_result(
        &self,
        application: &Application,
        result: &Result<Settings, Fault>,
    ) {
        let mut state = lock(&self.state);
        if !state
            .application
            .as_ref()
            .is_some_and(|current| std::ptr::eq(current.as_ref(), application))
        {
            return;
        }
        match result {
            Ok(settings) => {
                state.settings = Some(settings.clone());
            }
            Err(error) => {
                state.phase = Phase::Recovery;
                state.stage = "settings".into();
                state.fault = Some(error.clone());
                state.settings = None;
            }
        }
    }

    pub fn import_legacy_root(&self) -> Result<BootstrapStatus, Fault> {
        let _action = self.begin()?;
        if lock(&self.state).application.is_some() {
            return Err(Fault::new(
                "NotSetup",
                "Configuration is already initialized",
            ));
        }
        let root = self.root.as_ref().map_err(Clone::clone)?;
        let legacy = self.legacy_root.as_ref().ok_or_else(|| {
            Fault::new("LegacyImport", "No historical root is eligible for import")
        })?;
        match import_legacy(legacy, root) {
            Ok(()) => self.load(),
            Err(error) => self.fail("legacy_import", error),
        }
        Ok(self.status())
    }

    pub fn snapshot(&self, destination: Option<&Path>) -> Result<SnapshotReceipt, Fault> {
        let _action = self.begin()?;
        let root = self.root.as_ref().map_err(Clone::clone)?;
        if restore::pending(root)? {
            return Err(Fault::new(
                "RestorePending",
                "Resolve the interrupted restore before taking another snapshot",
            ));
        }
        let application = lock(&self.state).application.clone();
        let capture = if let Some(application) = application {
            application.capture_configuration()?
        } else {
            configuration::capture(root)?
        };
        let saved_destination = capture
            .files
            .get("settings.json")
            .and_then(|bytes| storage::decode::<Settings>(bytes).ok())
            .filter(|settings| storage::validate_settings(settings).is_ok())
            .and_then(|settings| settings.backup_directory.map(PathBuf::from));
        let receipt = backup::write(root, capture, destination.or(saved_destination.as_deref()))?;
        lock(&self.state).receipt = Some(receipt.clone());
        Ok(receipt)
    }

    pub fn restore_snapshot(
        &self,
        archive: &Path,
        generation: Option<&str>,
        confirm: bool,
        discard: bool,
    ) -> Result<BootstrapStatus, Fault> {
        let _action = self.begin()?;
        if !confirm {
            return Err(Fault::new(
                "RestoreConfirmation",
                "Confirm replacement of all saved App, Tab, and package configuration",
            ));
        }
        let root = self.root.as_ref().map_err(Clone::clone)?;
        if restore::pending(root)? {
            return Err(Fault::new(
                "RestorePending",
                "Resolve the interrupted restore first",
            ));
        }
        let incoming = backup::read(archive)?;
        restore::validate(&incoming)?;
        let application = lock(&self.state).application.clone();
        let current = if let Some(application) = application {
            application.capture_configuration()?
        } else {
            configuration::capture(root)?
        };
        if !current.files.is_empty() {
            let receipt = lock(&self.state).receipt.clone().ok_or_else(|| {
                Fault::new(
                    "PreimageRequired",
                    "Click Back up now to preserve the current configuration before replacement",
                )
            })?;
            if generation != Some(receipt.generation.as_str())
                || receipt.generation != current.generation
            {
                return Err(Fault::new(
                    "StaleReceipt",
                    "Configuration changed since its snapshot; take a new snapshot",
                ));
            }
            // A deleted or changed preservation artifact is not a usable receipt.
            if backup::read(Path::new(&receipt.path))?.generation != receipt.generation {
                return Err(Fault::new(
                    "StaleReceipt",
                    "The preservation archive changed; take a new snapshot",
                ));
            }
        }
        self.retire(discard)?;
        let expected = (!current.files.is_empty()).then_some(current.generation.as_str());
        match restore::install(root, incoming, expected) {
            Ok(()) => {
                lock(&self.state).receipt = None;
                self.load();
                let mut state = lock(&self.state);
                if state.phase != Phase::Ready {
                    if let Some(fault) = &mut state.fault {
                        fault.context["configuration_installed"] = json!(true);
                    }
                }
            }
            Err(error) => {
                lock(&self.state).pending_restore = restore::pending(root).unwrap_or(true);
                self.fail("restore", error);
            }
        }
        Ok(self.status())
    }

    pub fn recover_restore(
        &self,
        rollback: bool,
        confirm: bool,
        discard: bool,
    ) -> Result<BootstrapStatus, Fault> {
        let _action = self.begin()?;
        if !confirm {
            return Err(Fault::new(
                "RestoreConfirmation",
                "Confirm recovery of the interrupted configuration transaction",
            ));
        }
        let root = self.root.as_ref().map_err(Clone::clone)?;
        if !restore::pending(root)? {
            return Err(Fault::new(
                "RestoreNotPending",
                "There is no interrupted restore to recover",
            ));
        }
        self.retire(discard)?;
        #[cfg(not(test))]
        let recover_configuration = restore::recover;
        #[cfg(test)]
        let recover_configuration = self.recover_configuration;
        match recover_configuration(root, rollback) {
            Ok(()) => {
                lock(&self.state).receipt = None;
                self.load();
                if let Some(fault) = &mut lock(&self.state).fault {
                    fault.context["configuration_installed"] = json!(true);
                    fault.context["rolled_back"] = json!(rollback);
                }
            }
            Err(error) => {
                lock(&self.state).pending_restore = restore::pending(root).unwrap_or(true);
                self.fail("restore", error);
            }
        }
        Ok(self.status())
    }

    /// Contain the published Application first: an in-flight snapshot may hold
    /// `actions` across archive I/O, and Stop containment must not wait for it.
    /// The action is still awaited afterwards so a load that passed its closing
    /// check before `closing` was set cannot publish an Application nobody retires.
    /// A hung filesystem call inside that action is still not interruptible.
    pub fn shutdown(&self) -> Result<(), Fault> {
        self.closing.store(true, Ordering::Release);
        let published = self.shutdown_published();
        let _action = lock(&self.actions);
        published.and(self.shutdown_published())
    }

    fn shutdown_published(&self) -> Result<(), Fault> {
        let application = lock(&self.state).application.clone();
        application.map_or(Ok(()), |application| application.shutdown())
    }
}

fn legacy_files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, Fault> {
    storage::check_directory(root)?;
    if storage::exists(&root.join("settings.pending"))? {
        return Err(Fault::new(
            "LegacyImport",
            "Historical settings have an interrupted pending write",
        ));
    }
    let mut files = BTreeMap::new();
    let bytes = storage::read_bytes(&root.join("settings.json"), storage::MAX_SETTINGS_BYTES)?;
    storage::validate_settings(&storage::decode::<Settings>(&bytes)?)?;
    files.insert("settings.json".into(), bytes);
    let profiles = root.join("profiles");
    if !storage::exists(&profiles)? {
        return Ok(files);
    }
    storage::check_directory(&profiles)?;
    let mut total = 0;
    let entries =
        fs::read_dir(&profiles).map_err(|error| Fault::new("LegacyImport", error.to_string()))?;
    for (index, entry) in entries.enumerate() {
        if index >= storage::MAX_DIRECTORY_ENTRIES {
            return Err(Fault::new(
                "StorageLimit",
                "Historical profile directory has too many entries",
            ));
        }
        let entry = entry.map_err(|error| Fault::new("LegacyImport", error.to_string()))?;
        let filename = entry.file_name();
        let raw = filename.as_encoded_bytes();
        if !raw.ends_with(b".json") && !raw.ends_with(b".pending") {
            continue;
        }
        let name = filename.to_str().ok_or_else(|| {
            Fault::new("LegacyImport", "Historical profile filename is not UTF-8")
        })?;
        if name.ends_with(".pending") {
            return Err(Fault::new(
                "LegacyImport",
                "Historical profiles have an interrupted pending write",
            ));
        }
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        storage::validate_id(id)?;
        if files.len() > storage::MAX_PROFILES {
            return Err(Fault::new(
                "StorageLimit",
                "Historical profile count exceeds 64",
            ));
        }
        let bytes = storage::read_bytes(&entry.path(), storage::MAX_PROFILE_BYTES)?;
        let profile: Profile = storage::decode(&bytes)?;
        storage::validate_profile(&profile)?;
        if profile.id != id {
            return Err(Fault::new(
                "ProfileIdentity",
                "Historical profile ID does not match its filename",
            ));
        }
        total += bytes.len();
        if total > storage::MAX_TOTAL_BYTES {
            return Err(Fault::new(
                "StorageLimit",
                "Historical profiles exceed 1 MiB",
            ));
        }
        files.insert(format!("profiles/{name}"), bytes);
    }
    Ok(files)
}

fn import_legacy(source: &Path, destination: &Path) -> Result<(), Fault> {
    if storage::exists(destination)? {
        return Err(Fault::new(
            "LegacyConflict",
            "The selected destination already exists; no data was merged",
        ));
    }
    let files = legacy_files(source)?;
    let parent = destination
        .parent()
        .ok_or_else(|| Fault::new("LegacyImport", "Destination has no parent"))?;
    // The dedicated destination is private; an existing home configuration parent need not be.
    if !parent.is_dir() {
        fs::create_dir_all(parent)
            .map_err(|error| Fault::new("LegacyImport", error.to_string()))?;
    }
    if fs::symlink_metadata(parent)
        .map_err(|error| Fault::new("LegacyImport", error.to_string()))?
        .file_type()
        .is_symlink()
    {
        return Err(Fault::new(
            "LegacyImport",
            "Destination parent must not be a symbolic link",
        ));
    }
    let stage = parent.join(format!(
        ".mado-import-{}-{}",
        std::process::id(),
        NEXT_IMPORT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&stage)
        .map_err(|error| Fault::new("LegacyImport", error.to_string()))?;
    let result = (|| {
        for (relative, bytes) in &files {
            let path = stage.join(relative);
            storage::private_directory(path.parent().expect("known contained path"))?;
            storage::write_atomic(&path, bytes, configuration::publish_no_replace)?;
        }
        if legacy_files(source)? != files {
            return Err(Fault::new(
                "LegacyImport",
                "Historical configuration changed during import",
            ));
        }
        if stage.join("profiles").exists() {
            configuration::sync_directory(&stage.join("profiles"))?;
        }
        configuration::sync_directory(&stage)?;
        configuration::publish_no_replace(&stage, destination)
            .map_err(|error| Fault::new("LegacyImport", error.to_string()))?;
        configuration::sync_directory(parent).map_err(|mut fault| {
            fault.context["publication_completed"] = json!(true);
            fault.context["published_path"] = json!(destination);
            fault
        })
    })();
    if let Err(mut fault) = result {
        if let Err(error) = fs::remove_dir_all(&stage) {
            if error.kind() != io::ErrorKind::NotFound {
                fault.context["retained_staging"] = json!(stage);
                fault.context["temporary_cleanup"] = json!(error.to_string());
            }
        }
        return Err(fault);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Locale, NotificationPreferences};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Home(PathBuf);

    impl Home {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mado-bootstrap-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT_IMPORT.fetch_add(1, Ordering::Relaxed),
            ));
            storage::private_directory(&path).unwrap();
            Self(path)
        }

        fn bootstrap(&self, root: PathBuf) -> Bootstrap {
            let bootstrap = Bootstrap::new(
                Ok(root),
                None,
                self.0.join("no-runner"),
                self.0.join("no-engine"),
            );
            bootstrap.ensure_started().unwrap();
            bootstrap
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn preferences(locale: Locale) -> EditableSettings {
        EditableSettings {
            locale,
            gui_log_limit: 1000,
            ocr_environment: None,
            notifications: NotificationPreferences::default(),
            backup_directory: None,
            packages_root: None,
        }
    }

    fn settings_bytes(locale: Locale) -> Vec<u8> {
        serde_json::to_vec(&Settings {
            locale,
            ..Settings::default()
        })
        .unwrap()
    }

    fn put(path: &Path, bytes: &[u8]) {
        storage::private_directory(path.parent().unwrap()).unwrap();
        storage::write_atomic(path, bytes, |from, to| fs::rename(from, to)).unwrap();
    }

    fn restore_fixture(home: &Home) -> (Bootstrap, SnapshotReceipt, SnapshotReceipt) {
        let source = home.bootstrap(home.0.join("source"));
        source
            .initialize(preferences(Locale::Japanese), false)
            .unwrap();
        source
            .application()
            .unwrap()
            .create_workspace("Restored", "Restored")
            .unwrap();
        let incoming = source.snapshot(Some(&home.0.join("incoming"))).unwrap();
        source.shutdown().unwrap();

        let bootstrap = home.bootstrap(home.0.join("selected"));
        bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        bootstrap
            .application()
            .unwrap()
            .create_workspace("Original", "Original")
            .unwrap();
        let preimage = bootstrap.snapshot(Some(&home.0.join("preimage"))).unwrap();
        (bootstrap, incoming, preimage)
    }

    fn construction_fault() -> Fault {
        Fault::new(
            "LoggingInitialization",
            "injected application constructor failure",
        )
        .with_context(json!({"operation": "start log writer"}))
    }

    #[test]
    fn missing_settings_wait_for_explicit_initialization() {
        let home = Home::new();
        let root = home.0.join("selected");
        let bootstrap = home.bootstrap(root.clone());
        assert!(matches!(bootstrap.status().state, Phase::Setup));
        assert!(!root.exists());
        assert!(bootstrap.application().is_err());
        let ready = bootstrap
            .initialize(preferences(Locale::Japanese), false)
            .unwrap();
        assert!(matches!(ready.state, Phase::Ready));
        assert_eq!(ready.settings.unwrap().locale, Locale::Japanese);
        assert!(ready.catalog.unwrap().open.is_empty());
        assert!(!root.join("profiles").exists());
        let before = fs::read(root.join("settings.json")).unwrap();
        assert!(
            bootstrap
                .initialize(preferences(Locale::English), false)
                .is_err()
        );
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), before);
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn invalid_settings_remain_recoverable_until_external_repair() {
        let home = Home::new();
        let root = home.0.join("invalid");
        put(&root.join("settings.json"), b"{invalid");
        let bootstrap = home.bootstrap(root.clone());
        let initial = bootstrap.status();
        assert!(matches!(initial.state, Phase::Recovery));
        assert!(!initial.application_available);
        let refused = bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        assert!(matches!(refused.state, Phase::Recovery));
        assert_eq!(
            refused.fault.unwrap().category,
            initial.fault.unwrap().category
        );
        assert!(matches!(
            bootstrap.retry(false).unwrap().state,
            Phase::Recovery
        ));
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), b"{invalid");
        put(
            &root.join("settings.json"),
            &settings_bytes(Locale::Japanese),
        );
        let repaired = bootstrap.retry(false).unwrap();
        assert!(matches!(repaired.state, Phase::Ready));
        assert_eq!(repaired.settings.unwrap().locale, Locale::Japanese);
        let first = bootstrap.running_application().unwrap();
        assert_eq!(
            bootstrap.retry(false).err().unwrap().category,
            "DiscardRequired"
        );
        assert!(Arc::ptr_eq(
            &first,
            &bootstrap.running_application().unwrap()
        ));
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn stale_initialize_loads_appearing_settings_without_overwrite() {
        let home = Home::new();
        let root = home.0.join("stale");
        let bootstrap = home.bootstrap(root.clone());
        let actual = settings_bytes(Locale::Japanese);
        put(&root.join("settings.json"), &actual);
        let status = bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        assert!(matches!(status.state, Phase::Ready));
        assert_eq!(status.settings.unwrap().locale, Locale::Japanese);
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), actual);
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn explicit_root_and_existing_new_root_prevent_historical_fallback() {
        let home = Home::new();
        let legacy = home
            .0
            .join("Library/Application Support/dev.madomata.desktop");
        put(
            &legacy.join("settings.json"),
            &settings_bytes(Locale::Japanese),
        );
        let (root, candidate) = selected_roots(Ok(home.0.clone()), None);
        assert_eq!(root.unwrap(), home.0.join(".config/mado-mata"));
        assert_eq!(candidate, Some(legacy));
        storage::private_directory(&home.0.join(".config/mado-mata")).unwrap();
        assert!(selected_roots(Ok(home.0.clone()), None).1.is_none());
        let override_root = home.0.join("override");
        let (root, candidate) = selected_roots(
            Err(Fault::new("HomeDirectory", "unavailable")),
            Some(override_root.clone()),
        );
        assert_eq!(root.unwrap(), override_root);
        assert!(candidate.is_none());
        let failed = Bootstrap::new(
            Err(Fault::new("HomeDirectory", "unavailable")),
            None,
            PathBuf::new(),
            PathBuf::new(),
        );
        failed.ensure_started().unwrap();
        assert!(matches!(failed.status().state, Phase::Recovery));
        failed.shutdown().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_root_has_a_usable_recovery_and_exit_without_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new();
        let root = home.0.join("shared");
        storage::private_directory(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let bootstrap = home.bootstrap(root.clone());
        assert!(matches!(bootstrap.status().state, Phase::Recovery));
        assert_eq!(bootstrap.status().stage, "root");
        assert!(!bootstrap.status().pending_restore);
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!root.join("settings.json").exists());
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn historical_import_preserves_bytes_and_source_without_logs() {
        let home = Home::new();
        let source = home.0.join("legacy");
        let root = home.0.join("new");
        let bytes = settings_bytes(Locale::Japanese);
        put(&source.join("settings.json"), &bytes);
        put(&source.join("logs/retained.jsonl"), b"private log");
        let bootstrap = Bootstrap::new(
            Ok(root.clone()),
            Some(source.clone()),
            home.0.join("no-runner"),
            home.0.join("no-engine"),
        );
        assert_eq!(
            bootstrap
                .initialize(preferences(Locale::English), false)
                .err()
                .unwrap()
                .category,
            "LegacyConfirmation"
        );
        assert!(!root.exists());
        let mut invalid = preferences(Locale::English);
        invalid.gui_log_limit = 0;
        let refused = bootstrap.initialize(invalid, true).unwrap();
        assert!(matches!(refused.state, Phase::Setup));
        assert!(refused.fault.is_some());
        assert!(!root.exists());
        let status = bootstrap.import_legacy_root().unwrap();
        assert!(matches!(status.state, Phase::Ready));
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), bytes);
        assert_eq!(fs::read(source.join("settings.json")).unwrap(), bytes);
        assert_eq!(
            fs::read(source.join("logs/retained.jsonl")).unwrap(),
            b"private log"
        );
        assert!(!root.join("logs/retained.jsonl").exists());
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn historical_pending_and_destination_conflict_never_fall_back_to_fresh() {
        let home = Home::new();
        let source = home.0.join("legacy");
        let root = home.0.join("new");
        put(
            &source.join("settings.json"),
            &settings_bytes(Locale::Japanese),
        );
        put(&source.join("settings.pending"), b"unfinished");
        let bootstrap = Bootstrap::new(
            Ok(root.clone()),
            Some(source.clone()),
            home.0.join("no-runner"),
            home.0.join("no-engine"),
        );
        assert!(matches!(
            bootstrap.import_legacy_root().unwrap().state,
            Phase::Recovery
        ));
        assert!(!root.exists());
        fs::remove_file(source.join("settings.pending")).unwrap();
        let bytes = settings_bytes(Locale::English);
        put(&root.join("settings.json"), &bytes);
        let status = bootstrap.import_legacy_root().unwrap();
        assert!(matches!(status.state, Phase::Recovery));
        assert_eq!(status.fault.unwrap().category, "LegacyConflict");
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), bytes);
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn restore_requires_a_clicked_current_preservation_receipt() {
        let home = Home::new();
        let root = home.0.join("restore");
        let bootstrap = home.bootstrap(root.clone());
        bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        let receipt = bootstrap.snapshot(Some(&home.0.join("first"))).unwrap();
        let archive = Path::new(&receipt.path);
        let missing = bootstrap
            .restore_snapshot(archive, None, true, true)
            .err()
            .unwrap();
        assert_eq!(missing.category, "StaleReceipt");
        bootstrap
            .application()
            .unwrap()
            .save_settings(preferences(Locale::Japanese))
            .unwrap();
        let stale = bootstrap
            .restore_snapshot(archive, Some(&receipt.generation), true, true)
            .err()
            .unwrap();
        assert_eq!(stale.category, "StaleReceipt");
        assert_eq!(
            bootstrap.application().unwrap().settings().unwrap().locale,
            Locale::Japanese
        );
        let current = bootstrap.snapshot(Some(&home.0.join("second"))).unwrap();
        assert_eq!(
            bootstrap
                .restore_snapshot(archive, Some(&current.generation), true, false)
                .err()
                .unwrap()
                .category,
            "DiscardRequired"
        );
        let restored = bootstrap
            .restore_snapshot(archive, Some(&current.generation), true, true)
            .unwrap();
        assert!(matches!(restored.state, Phase::Ready));
        assert_eq!(restored.settings.unwrap().locale, Locale::English);
        assert!(Path::new(&current.path).exists());
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn recovery_final_sync_failure_exposes_retry_without_a_phantom_pending_restore() {
        let home = Home::new();
        let (mut bootstrap, incoming, preimage) = restore_fixture(&home);
        let root = home.0.join("selected");
        let installed = backup::read(Path::new(&incoming.path)).unwrap();
        bootstrap.retire(true).unwrap();
        restore::tests::interrupt_install(&root, installed.clone(), &preimage.generation);
        let interrupted = bootstrap.retry(false).unwrap();
        assert!(matches!(interrupted.state, Phase::Recovery));
        assert!(interrupted.pending_restore);

        bootstrap.recover_configuration = restore::tests::fail_recovery_final_sync;
        let failed = bootstrap.recover_restore(false, true, false).unwrap();
        assert!(matches!(failed.state, Phase::Recovery));
        assert_eq!(failed.stage, "restore");
        assert!(!failed.application_available);
        assert!(failed.settings.is_none());
        assert!(!failed.pending_restore);
        assert!(!restore::pending(&root).unwrap());
        assert_eq!(configuration::capture(&root).unwrap(), installed);
        let fault = failed.fault.as_ref().unwrap();
        let original = configuration::io_fault(
            "sync configuration directory",
            io::Error::other("injected final sync failure"),
        );
        assert_eq!(fault.category, original.category);
        assert_eq!(fault.message, original.message);
        for (key, value) in original.context.as_object().unwrap() {
            assert_eq!(&fault.context[key], value);
        }
        assert_eq!(fault.context["configuration_installed"], true);
        assert_eq!(fault.context["rolled_back"], false);
        assert_eq!(fault.context["cleanup_incomplete"], true);
        assert_eq!(fault.context["pending_restore"], false);
        let observed = bootstrap.ensure_started().unwrap();
        assert!(matches!(observed.state, Phase::Recovery));
        assert!(!observed.pending_restore);
        assert!(!observed.application_available);
        assert_eq!(json!(observed.fault), json!(failed.fault));

        let ready = bootstrap.retry(false).unwrap();
        assert!(matches!(ready.state, Phase::Ready));
        assert!(ready.application_available);
        assert!(!ready.pending_restore);
        assert!(ready.fault.is_none());
        assert_eq!(ready.settings.unwrap().locale, Locale::Japanese);
        let catalog = ready.catalog.unwrap();
        assert_eq!(catalog.open.len(), 1);
        assert_eq!(catalog.open[0].internal_name, "Restored");
        assert_eq!(configuration::capture(&root).unwrap(), installed);
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn completed_install_preserves_bytes_and_consumes_receipt_when_reconstruction_fails() {
        let home = Home::new();
        let (mut bootstrap, incoming, preimage) = restore_fixture(&home);
        let root = home.0.join("selected");
        let archive = Path::new(&incoming.path);
        let installed = backup::read(archive).unwrap();
        // Log-file errors are asynchronous, so inject only the constructor result.
        // Installation, receipt admission, loading and Retry still use the real host.
        bootstrap.make_application = |_, _, _, _| Err(construction_fault());
        let failed = bootstrap
            .restore_snapshot(archive, Some(&preimage.generation), true, true)
            .unwrap();
        assert!(matches!(failed.state, Phase::Recovery));
        assert_eq!(failed.stage, "application");
        assert!(!failed.application_available);
        assert!(failed.settings.is_none());
        assert!(!failed.pending_restore);
        assert!(!restore::pending(&root).unwrap());
        assert_eq!(configuration::capture(&root).unwrap(), installed);
        let mut expected_fault = construction_fault();
        expected_fault.context["configuration_installed"] = json!(true);
        assert_eq!(json!(failed.fault), json!(Some(expected_fault)));
        assert!(
            failed
                .fault
                .as_ref()
                .unwrap()
                .context
                .get("rolled_back")
                .is_none()
        );
        let refused = bootstrap
            .restore_snapshot(archive, Some(&preimage.generation), true, true)
            .err()
            .unwrap();
        assert_eq!(refused.category, "PreimageRequired");
        assert_eq!(configuration::capture(&root).unwrap(), installed);
        let observed = bootstrap.ensure_started().unwrap();
        assert!(matches!(observed.state, Phase::Recovery));
        assert_eq!(json!(observed.fault), json!(failed.fault));

        bootstrap.make_application = Application::with_observation_slot;
        let ready = bootstrap.retry(false).unwrap();
        assert!(matches!(ready.state, Phase::Ready));
        assert!(ready.application_available);
        assert!(!ready.pending_restore);
        assert!(ready.fault.is_none());
        assert_eq!(ready.settings.unwrap().locale, Locale::Japanese);
        let catalog = ready.catalog.unwrap();
        assert_eq!(catalog.open.len(), 1);
        assert_eq!(catalog.open[0].internal_name, "Restored");
        assert_eq!(configuration::capture(&root).unwrap(), installed);
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn completed_recovery_preserves_its_direction_and_consumes_receipt_when_reconstruction_fails() {
        for rollback in [false, true] {
            let home = Home::new();
            let (mut bootstrap, incoming, preimage) = restore_fixture(&home);
            let root = home.0.join("selected");
            let archive = Path::new(&incoming.path);
            let before = backup::read(Path::new(&preimage.path)).unwrap();
            let after = backup::read(archive).unwrap();
            bootstrap.retire(true).unwrap();
            restore::tests::interrupt_install(&root, after.clone(), &preimage.generation);
            let interrupted = bootstrap.retry(false).unwrap();
            assert!(matches!(interrupted.state, Phase::Recovery));
            assert!(interrupted.pending_restore);

            bootstrap.make_application = |_, _, _, _| Err(construction_fault());
            let failed = bootstrap.recover_restore(rollback, true, false).unwrap();
            assert!(matches!(failed.state, Phase::Recovery));
            assert_eq!(failed.stage, "application");
            assert!(!failed.application_available);
            assert!(failed.settings.is_none());
            assert!(!failed.pending_restore);
            assert!(!restore::pending(&root).unwrap());
            let expected = if rollback { &before } else { &after };
            assert_eq!(configuration::capture(&root).unwrap(), *expected);
            let mut expected_fault = construction_fault();
            expected_fault.context["configuration_installed"] = json!(true);
            expected_fault.context["rolled_back"] = json!(rollback);
            assert_eq!(json!(failed.fault), json!(Some(expected_fault)));
            assert_eq!(
                bootstrap
                    .recover_restore(rollback, true, false)
                    .err()
                    .unwrap()
                    .category,
                "RestoreNotPending"
            );
            // Rollback restores exactly the receipted generation. Without receipt
            // consumption this repeat would be admitted, not merely stale.
            assert_eq!(
                bootstrap
                    .restore_snapshot(archive, Some(&preimage.generation), true, false)
                    .err()
                    .unwrap()
                    .category,
                "PreimageRequired"
            );
            assert_eq!(configuration::capture(&root).unwrap(), *expected);
            let observed = bootstrap.ensure_started().unwrap();
            assert!(matches!(observed.state, Phase::Recovery));
            assert_eq!(json!(observed.fault), json!(failed.fault));

            bootstrap.make_application = Application::with_observation_slot;
            let ready = bootstrap.retry(false).unwrap();
            assert!(matches!(ready.state, Phase::Ready));
            assert!(ready.application_available);
            assert!(!ready.pending_restore);
            assert!(ready.fault.is_none());
            assert_eq!(
                ready.settings.unwrap().locale,
                if rollback {
                    Locale::English
                } else {
                    Locale::Japanese
                }
            );
            let catalog = ready.catalog.unwrap();
            assert_eq!(catalog.open.len(), 1);
            assert_eq!(
                catalog.open[0].internal_name,
                if rollback { "Original" } else { "Restored" }
            );
            assert_eq!(configuration::capture(&root).unwrap(), *expected);
            bootstrap.shutdown().unwrap();
        }
    }

    #[test]
    fn late_settings_fault_keeps_polling_and_cannot_poison_a_new_session() {
        let home = Home::new();
        let root = home.0.join("later-failure");
        let bootstrap = home.bootstrap(root.clone());
        bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        let original = bootstrap.application().unwrap();
        put(&root.join("settings.json"), b"broken");
        let failed = original.settings();
        bootstrap.note_settings_result(&original, &failed);
        let recovery = bootstrap.status();
        assert!(matches!(recovery.state, Phase::Recovery));
        assert!(recovery.application_available);
        assert!(bootstrap.application().is_err());
        assert!(Arc::ptr_eq(
            &original,
            &bootstrap.running_application().unwrap()
        ));
        assert_eq!(
            bootstrap.running_application().unwrap().poll().controller["state"],
            "idle"
        );
        put(
            &root.join("settings.json"),
            &settings_bytes(Locale::Japanese),
        );
        assert!(matches!(bootstrap.retry(true).unwrap().state, Phase::Ready));
        assert!(!Arc::ptr_eq(&original, &bootstrap.application().unwrap()));
        bootstrap.note_settings_result(&original, &failed);
        assert!(matches!(bootstrap.status().state, Phase::Ready));
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn recovery_without_a_journal_preserves_the_ready_session() {
        let home = Home::new();
        let bootstrap = home.bootstrap(home.0.join("healthy"));
        bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        let original = bootstrap.application().unwrap();
        let workspace = original.create_workspace("Alpha", "Alpha").unwrap();
        let refusal = bootstrap.recover_restore(true, true, true).err().unwrap();
        assert_eq!(refusal.category, "RestoreNotPending");
        assert!(Arc::ptr_eq(&original, &bootstrap.application().unwrap()));
        assert_eq!(
            bootstrap.status().catalog.unwrap().open[0].workspace_id,
            workspace.workspace_id
        );
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn shutdown_contains_the_published_application_before_an_in_flight_action_settles() {
        let home = Home::new();
        let root = home.0.join("in-flight");
        let bootstrap = Arc::new(home.bootstrap(root.clone()));
        bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        let published = bootstrap.application().unwrap();
        // Stands in for snapshot archive I/O: the action stays in flight until released.
        let action = bootstrap.begin().unwrap();
        let closer = std::thread::spawn({
            let bootstrap = Arc::clone(&bootstrap);
            move || bootstrap.shutdown()
        });
        // `Application::shutdown` closes command admission as its first step. Wait on
        // that observable condition with a failure bound, never on assumed timing.
        let deadline = Instant::now() + Duration::from_secs(30);
        while published.workspace_catalog().is_ok() {
            assert!(
                Instant::now() < deadline,
                "shutdown waited for the in-flight action before containing the Application"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            published.workspace_catalog().unwrap_err().category,
            "Closing"
        );
        // Containment completes while the action is still held; only afterwards does
        // the shutdown thread wait for that action.
        published.shutdown().unwrap();
        assert!(!closer.is_finished());
        assert_eq!(bootstrap.application().err().unwrap().category, "Closing");
        // A load that passed its closing check before `closing` was set publishes
        // under the held action; that Application is retired once the action settles.
        let late =
            Application::new(root, home.0.join("no-runner"), home.0.join("no-engine")).unwrap();
        lock(&bootstrap.state).application = Some(Arc::clone(&late));
        drop(action);
        closer.join().unwrap().unwrap();
        assert_eq!(late.workspace_catalog().unwrap_err().category, "Closing");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_catalog_is_recovery_with_polling_not_a_ready_empty_list() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new();
        let root = home.0.join("catalog");
        let bootstrap = home.bootstrap(root.clone());
        bootstrap
            .initialize(preferences(Locale::English), false)
            .unwrap();
        let original = bootstrap.application().unwrap();
        original.create_workspace("Alpha", "Alpha").unwrap();
        fs::set_permissions(root.join("tabs"), fs::Permissions::from_mode(0o755)).unwrap();
        let status = bootstrap.status();
        assert!(matches!(status.state, Phase::Recovery));
        assert_eq!(status.stage, "tabs");
        assert!(status.fault.is_some());
        assert!(status.catalog.is_none());
        assert!(status.application_available);
        assert!(bootstrap.application().is_err());
        assert!(Arc::ptr_eq(
            &original,
            &bootstrap.running_application().unwrap()
        ));
        assert_eq!(original.poll().controller["state"], "idle");
        fs::set_permissions(root.join("tabs"), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(bootstrap.retry(true).unwrap().state, Phase::Ready));
        bootstrap.shutdown().unwrap();
    }

    #[test]
    fn first_load_reports_loading_without_creating_or_guessing_configuration() {
        let home = Home::new();
        let root = home.0.join("deferred");
        put(&root.join("settings.json"), b"{malformed");
        let bootstrap = Bootstrap::new(
            Ok(root.clone()),
            None,
            home.0.join("no-runner"),
            home.0.join("no-engine"),
        );
        assert!(matches!(bootstrap.status().state, Phase::Loading));
        assert!(bootstrap.application().is_err());
        let action = bootstrap.begin().unwrap();
        let concurrent = bootstrap.ensure_started().unwrap();
        assert!(matches!(concurrent.state, Phase::Loading));
        assert!(!concurrent.application_available);
        assert!(!root.join("logs").exists());
        drop(action);
        let loaded = bootstrap.ensure_started().unwrap();
        assert!(matches!(loaded.state, Phase::Recovery));
        assert_eq!(loaded.stage, "settings");
        assert_eq!(fs::read(root.join("settings.json")).unwrap(), b"{malformed");
        bootstrap.shutdown().unwrap();
    }
}
