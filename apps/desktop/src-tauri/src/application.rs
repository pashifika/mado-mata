use crate::logging::{LogBatch, LogStatus, Logger};
use crate::storage::{Profile, Settings, Store};
use mado_runtime_comparison::desktop::{DesktopController, PackageInfo, StartRequest};
use mado_runtime_comparison::host::resolve_options;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, Plan};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Serialize)]
pub struct Selection {
    pub package: PackageInfo,
    pub profiles: Vec<Profile>,
    pub profiles_error: Option<Fault>,
}

#[derive(Serialize)]
pub struct Poll {
    pub controller: Value,
    pub logs: LogBatch,
}

struct Selected {
    path: PathBuf,
    inventory: Inventory,
}

pub struct Application {
    runner: DesktopController,
    store: Arc<Mutex<Store>>,
    selected: Mutex<Option<Selected>>,
    logger: Logger,
    view: Mutex<Value>,
    closing: AtomicBool,
    bridge: Mutex<Option<JoinHandle<()>>>,
    shutdown_outcome: OnceLock<Result<(), Fault>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Application {
    pub fn new(root: PathBuf, controlled: PathBuf, engine: PathBuf) -> Result<Arc<Self>, Fault> {
        let application = Arc::new(Self {
            runner: DesktopController::new(controlled, engine),
            store: Arc::new(Mutex::new(Store::new(root.clone())?)),
            selected: Mutex::new(None),
            logger: Logger::new(root.join("logs"))?,
            view: Mutex::new(json!({"state":"idle","run":null,"operation":"run"})),
            closing: AtomicBool::new(false),
            bridge: Mutex::new(None),
            shutdown_outcome: OnceLock::new(),
        });
        let weak = Arc::downgrade(&application);
        let bridge = std::thread::Builder::new()
            .name("desktop-log-bridge".into())
            .spawn(move || {
                let mut previous = (Value::Null, Value::Null);
                loop {
                    let Some(application) = weak.upgrade() else {
                        break;
                    };
                    if application.closing.load(Ordering::Acquire) {
                        break;
                    }
                    application.collect(&mut previous);
                    drop(application);
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
            .map_err(|error| Fault::new("Application", error.to_string()))?;
        *lock(&application.bridge) = Some(bridge);
        application.logger.with_dispatch(|| {
            tracing::info!(
                code = "application.ready",
                lane = "controlled",
                "Application ready"
            )
        });
        Ok(application)
    }

    fn collect(&self, previous: &mut (Value, Value)) {
        let mut view = self.runner.poll();
        for event in view.logs.drain(..) {
            self.logger.emit(
                "Script",
                event["severity"].as_str().unwrap_or("info"),
                event["run"].as_str(),
                "script.log",
                event["message"].as_str().unwrap_or("Script log"),
                event.clone(),
            );
        }
        let value = serde_json::to_value(view).expect("serializable controller view");
        let identity = (value["run"].clone(), value["state"].clone());
        if *previous != identity {
            self.logger.with_dispatch(|| {
                tracing::info!(
                    code = "run.state",
                    run = value["run"].as_str().unwrap_or("application"),
                    state = value["state"].as_str().unwrap_or("unknown"),
                    status = value["result"]["status"].as_str().unwrap_or("unsettled"),
                    entry_outcome = value["result"]["entry_outcome"]
                        .as_str()
                        .unwrap_or("Unobserved"),
                    cleanup = value["result"]["cleanup"]["clean"]
                        .as_bool()
                        .unwrap_or(false),
                    "Run state changed"
                )
            });
            *previous = identity;
        }
        *lock(&self.view) = value;
    }

    pub fn settings(&self) -> Result<Settings, Fault> {
        lock(&self.store).settings()
    }

    pub fn save_settings(&self, settings: Settings) -> Result<Settings, Fault> {
        lock(&self.store).save_settings(settings)
    }

    pub fn select(&self, path: &Path) -> Result<Selection, Fault> {
        let package = self.runner.inspect(path)?;
        check_webview_value(&package.schema, "$.schema")?;
        for (name, preset) in &package.profiles {
            check_webview_value(preset, &format!("$.presets[{name:?}]"))?;
        }
        if let Some(defaults) = &package.effective_defaults {
            check_webview_value(defaults, "$.effective_defaults")?;
        }
        let plan: Plan = serde_json::from_str(include_str!(
            "../../../../tools/runtime-comparison/fixtures/manual-plan.json"
        ))
        .map_err(|error| Fault::new("Application", error.to_string()))?;
        let inventory = Inventory::capture(path, &plan.limits)?;
        if inventory.identity != package.inventory_identity {
            return Err(Fault::new(
                "InventoryChanged",
                "Package changed during inspection; select it again",
            ));
        }
        let store = lock(&self.store);
        let (profiles, profiles_error) = match validated_profiles(&store, &inventory) {
            Ok(listing) => listing,
            Err(error) => (Vec::new(), Some(error)),
        };
        drop(store);
        *lock(&self.selected) = Some(Selected {
            path: path.to_path_buf(),
            inventory,
        });
        let store = lock(&self.store);
        let saved_hint = store.settings().and_then(|mut settings| {
            settings.package_path = Some(path.to_string_lossy().into_owned());
            store.save_settings(settings)
        });
        drop(store);
        if let Err(error) = saved_hint {
            self.logger.with_dispatch(|| {
                tracing::warn!(
                    code = "settings.package_hint_not_saved",
                    category = %error.category,
                    "Package selected, but its location hint was not saved. Settings data was preserved; close the app and inspect settings.json and settings.pending before retrying"
                )
            });
        }
        Ok(Selection {
            package,
            profiles,
            profiles_error,
        })
    }

    pub fn validate(&self, mut values: Value) -> Result<Value, Fault> {
        let selected = lock(&self.selected);
        let selected = selected
            .as_ref()
            .ok_or_else(|| Fault::new("Package", "Select a package first"))?;
        normalize_editor_numbers(&selected.inventory.schema, &mut values);
        desktop_options(&selected.inventory, values)
    }

    pub fn profiles(&self) -> Result<Vec<Profile>, Fault> {
        let selected = lock(&self.selected);
        let selected = selected
            .as_ref()
            .ok_or_else(|| Fault::new("Package", "Select a package first"))?;
        validated_profiles(&lock(&self.store), &selected.inventory).map(|listing| listing.0)
    }

    pub fn save_profile(
        &self,
        id: Option<&str>,
        name: &str,
        mut values: Value,
    ) -> Result<Profile, Fault> {
        let selected = lock(&self.selected);
        let selected = selected
            .as_ref()
            .ok_or_else(|| Fault::new("Package", "Select a package first"))?;
        let current = self.runner.inspect(&selected.path)?;
        if current.inventory_identity != selected.inventory.identity {
            return Err(Fault::new(
                "InventoryChanged",
                "Package changed; select it again before saving",
            ));
        }
        normalize_editor_numbers(&selected.inventory.schema, &mut values);
        desktop_options(&selected.inventory, values.clone())?;
        let store = lock(&self.store);
        if let Some(id) = id {
            checked_profile(&store, &current.package_id, &current.schema_identity, id)?;
        }
        store.save(&selected.inventory, id, name, values)
    }

    pub fn rename_profile(&self, id: &str, name: &str) -> Result<Profile, Fault> {
        let selected = lock(&self.selected);
        let selected = selected
            .as_ref()
            .ok_or_else(|| Fault::new("Package", "Select a package first"))?;
        let store = lock(&self.store);
        let profile = checked_profile(
            &store,
            &selected.inventory.package_id,
            &mado_runtime_comparison::model::identity(&selected.inventory.schema)?,
            id,
        )?;
        desktop_options(&selected.inventory, profile.values.clone())?;
        store.rename(&selected.inventory, id, name)
    }

    pub fn delete_profile(&self, id: &str) -> Result<(), Fault> {
        lock(&self.store).delete(id)
    }

    pub fn start(&self, mut request: StartRequest) -> Result<String, Fault> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        {
            let selected = lock(&self.selected);
            let selected = selected
                .as_ref()
                .ok_or_else(|| Fault::new("Package", "Select a package first"))?;
            if request.inventory_identity != selected.inventory.identity
                || request.package_id != selected.inventory.package_id
                || request.schema_identity
                    != mado_runtime_comparison::model::identity(&selected.inventory.schema)?
            {
                return Err(Fault::new(
                    "StaleIdentity",
                    "Selected package changed; inspect it again",
                ));
            }
            normalize_editor_numbers(&selected.inventory.schema, &mut request.values);
            desktop_options(&selected.inventory, request.values.clone())?;
        }
        let store = self.store.clone();
        let (acquired, ready) = mpsc::sync_channel(1);
        let run = self
            .runner
            .start_with_preparation(request, move |request, control| {
                let store = lock(&store);
                let _ = acquired.send(());
                control.check()?;
                let environment = if request.lane == "replay" {
                    store.settings()?.ocr_environment
                } else {
                    None
                };
                if request.profile_id != "draft" {
                    let profile = checked_profile(
                        &store,
                        &request.package_id,
                        &request.schema_identity,
                        &request.profile_id,
                    )?;
                    if !same_json_values(&profile.values, &request.values) {
                        return Err(Fault::new(
                            "ProfileIdentity",
                            "Saved profile values changed; select it again",
                        )
                        .with_context(json!({"profile_id":profile.id})));
                    }
                }
                control.check()?;
                Ok(environment)
            })?;
        // The worker owns the store lock before admission returns. Later saves
        // cannot overtake its immutable input capture, and all I/O stays reserved.
        let _ = ready.recv();
        Ok(run)
    }

    pub fn check_environment(
        &self,
        replay_descriptor_path: Option<String>,
        package_inventory_identity: Option<String>,
    ) -> Result<String, Fault> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        let package = match package_inventory_identity {
            Some(identity) => {
                let selected = lock(&self.selected);
                let selected = selected
                    .as_ref()
                    .filter(|selected| selected.inventory.identity == identity)
                    .ok_or_else(|| {
                        Fault::new(
                            "StaleIdentity",
                            "Selected package changed; inspect it again",
                        )
                    })?;
                Some((selected.path.clone(), identity))
            }
            None => None,
        };
        let store = self.store.clone();
        let (acquired, ready) = mpsc::sync_channel(1);
        let run = self.runner.check_environment_with_preparation(
            package,
            replay_descriptor_path,
            move |control| {
                let store = lock(&store);
                let _ = acquired.send(());
                control.check()?;
                Ok(store.settings()?.ocr_environment)
            },
        )?;
        let _ = ready.recv();
        Ok(run)
    }

    pub fn stop(&self, run: &str) -> Result<(), Fault> {
        self.runner.stop(run)
    }

    pub fn poll(&self) -> Poll {
        Poll {
            controller: lock(&self.view).clone(),
            logs: self.logger.drain(),
        }
    }

    pub fn shutdown(&self) -> Result<(), Fault> {
        // Native Quit may arrive while the window-close worker is still shutting down.
        // Share its bounded result rather than racing final collection or log flushing.
        self.shutdown_outcome
            .get_or_init(|| {
                self.closing.store(true, Ordering::Release);
                let outcome = self.runner.shutdown();
                if let Some(bridge) = lock(&self.bridge).take() {
                    let _ = bridge.join();
                }
                self.collect(&mut (Value::Null, Value::Null));
                report_log_shutdown(self.logger.shutdown());
                outcome
            })
            .clone()
    }
}

fn report_log_shutdown(status: LogStatus) {
    if !status.shutdown_timed_out && status.file_pending == 0 && status.file_errors == 0 {
        return;
    }
    // Do not use the failed/closed logger, expose private error text, or wait on a
    // blocked stderr pipe. The diagnostic is best-effort and never changes cleanup.
    let (done, wait) = mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("log-shutdown-diagnostic".into())
        .spawn(move || {
            let _ = writeln!(
                std::io::stderr(),
                "Log shutdown: shutdown_timed_out={} file_pending={} file_errors={}",
                status.shutdown_timed_out,
                status.file_pending,
                status.file_errors,
            );
            let _ = done.send(());
        })
        .is_ok()
    {
        let _ = wait.recv_timeout(Duration::from_millis(50));
    }
}

// i128 avoids the saturating i64/u64 cast that would accept their rounded maxima.
fn exact_integer(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

fn check_webview_value(value: &Value, path: &str) -> Result<(), Fault> {
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
                check_webview_value(value, &format!("{path}.{name}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

// A number editor intentionally uses f64 semantics. JSON.stringify can emit an
// integer spelling for a float (1.0, 1e18, etc.); restore that schema-known type
// only on incoming editor values, never on unchecked package or stored data.
fn normalize_editor_numbers(schema: &Value, value: &mut Value) {
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

fn desktop_options(inventory: &Inventory, values: Value) -> Result<Value, Fault> {
    check_webview_value(&values, "$")?;
    let effective = resolve_options(
        &inventory.schema,
        &json!({"package_id":inventory.package_id,"schema_version":inventory.schema["version"],"options":values}),
        &inventory.package_id,
    )?;
    check_webview_value(&effective, "$")?;
    Ok(effective)
}

fn checked_profile(
    store: &Store,
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
fn same_json_values(left: &Value, right: &Value) -> bool {
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

fn validated_profiles(
    store: &Store,
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
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        application: Arc<Application>,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "mado-application-{}-{nonce}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
            ));
            let application = Application::new(
                root.clone(),
                root.join("runner-must-not-be-launched"),
                root.join("engine-must-not-be-launched"),
            )
            .unwrap();
            Self { root, application }
        }

        fn numeric_package(&self) -> PathBuf {
            let path = self.root.join("package");
            for relative in [
                "package.json",
                "main.js",
                "decisions.js",
                "schema.json",
                "profiles/template-first.json",
                "profiles/ocr-first.json",
                "assets/marker.rgba",
            ] {
                let destination = path.join(relative);
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                fs::copy(package_path().join(relative), destination).unwrap();
            }
            let schema_path = path.join("schema.json");
            let mut schema: Value =
                serde_json::from_slice(&fs::read(&schema_path).unwrap()).unwrap();
            schema["properties"]["amount"] = json!({"type":"number","default":1});
            schema["properties"]["numbers"] = json!({"type":"array","items":{"type":"number"}});
            fs::write(schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.application.shutdown();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn package_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tools/runtime-comparison/fixtures/javascript")
            .canonicalize()
            .unwrap()
    }

    fn settled(application: &Application) -> mado_runtime_comparison::desktop::ControllerView {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let view = application.runner.poll();
            if view.state == "terminal" {
                return view;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "operation did not settle"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn environment_check_refuses_missing_or_stale_package_selection() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = package_path();
        let identity = application.runner.inspect(&path).unwrap().inventory_identity;
        let error = application
            .check_environment(None, Some(identity.clone()))
            .unwrap_err();
        assert_eq!(error.category, "StaleIdentity");
        assert!(application.runner.poll().run.is_none());

        application.select(&path).unwrap();
        let replacement = application.select(&fixture.numeric_package()).unwrap();
        assert_ne!(replacement.package.inventory_identity, identity);
        let error = application
            .check_environment(None, Some(identity))
            .unwrap_err();
        assert_eq!(error.category, "StaleIdentity");
        assert!(application.runner.poll().run.is_none());
    }

    #[test]
    fn environment_check_without_package_does_not_inherit_cached_selection() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let selection = application.select(&package_path()).unwrap();
        let run = application.check_environment(None, None).unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "EnvironmentUnset");
        assert!(fault.context["package_inventory_identity"].is_null());

        // Omitting the package must not discard the cached inspection either.
        let identity = selection.package.inventory_identity;
        let run = application
            .check_environment(None, Some(identity.clone()))
            .unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "EnvironmentUnset");
        assert_eq!(fault.context["package_inventory_identity"], identity);
    }

    #[test]
    fn stale_saved_values_are_refused_before_runner_startup() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = package_path();
        application.select(&path).unwrap();
        let plan: Plan = serde_json::from_str(include_str!(
            "../../../../tools/runtime-comparison/fixtures/manual-plan.json"
        ))
        .unwrap();
        let inventory = Inventory::capture(&path, &plan.limits).unwrap();
        let values = inventory.profiles["template-first"]["options"].clone();
        let saved = lock(&application.store)
            .save(&inventory, None, "Saved choice", values.clone())
            .unwrap();
        let mut replacement = values.clone();
        replacement["priorities"] = json!(["ocr", "template"]);
        lock(&application.store)
            .save(
                &inventory,
                Some(&saved.id),
                "Saved choice",
                replacement.clone(),
            )
            .unwrap();
        let stored_path = fixture
            .root
            .join("profiles")
            .join(format!("{}.json", saved.id));
        let before = fs::read(&stored_path).unwrap();
        let run = application
            .start(StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: inventory.identity,
                package_id: saved.package_id.clone(),
                schema_identity: saved.schema_identity.clone(),
                profile_id: saved.id.clone(),
                values,
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            })
            .unwrap();
        let controller = settled(application);
        let fault = controller.error.unwrap();
        assert_eq!(fault.category, "ProfileIdentity");
        assert_eq!(fault.context["profile_id"], saved.id);
        assert_eq!(controller.run.as_deref(), Some(run.as_str()));
        assert_eq!(
            fault.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
        assert_eq!(fs::read(&stored_path).unwrap(), before);
        let profiles = lock(&application.store)
            .list(&saved.package_id, &saved.schema_identity)
            .unwrap();
        assert_eq!(profiles.profiles[0].values, replacement);
    }

    #[test]
    fn admission_reserves_before_store_io_and_owns_the_profile_snapshot() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = package_path();
        let selection = application.select(&path).unwrap();
        let values = selection.package.profiles["template-first"]["options"].clone();
        let saved = application
            .save_profile(None, "Before", values.clone())
            .unwrap();
        let request = StartRequest {
            package_path: path.to_string_lossy().into_owned(),
            inventory_identity: selection.package.inventory_identity,
            package_id: saved.package_id.clone(),
            schema_identity: saved.schema_identity.clone(),
            profile_id: saved.id.clone(),
            values: values.clone(),
            lane: "controlled".into(),
            scenario: "workflow".into(),
            replay_descriptor_path: None,
        };
        let store = lock(&application.store);
        let (admitted, admission) = mpsc::sync_channel(1);
        let starting = application.clone();
        let starter = std::thread::spawn(move || {
            let _ = admitted.send(starting.start(request));
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while application.runner.poll().state != "preparing" {
            assert!(
                std::time::Instant::now() < deadline,
                "Start did not reserve"
            );
            std::thread::yield_now();
        }
        let (checked, check) = mpsc::sync_channel(1);
        let checking = application.clone();
        let competitor = std::thread::spawn(move || {
            let _ = checked.send(checking.check_environment(None, None));
        });
        let refusal = check.recv_timeout(Duration::from_secs(2));
        let premature = admission.recv_timeout(Duration::from_millis(100));
        drop(store);
        competitor.join().unwrap();
        starter.join().unwrap();
        assert_eq!(refusal.unwrap().unwrap_err().category, "RunActive");
        assert!(
            matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)),
            "Start must acquire snapshot ownership before returning admission"
        );
        let run = admission
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let mut next_values = values;
        next_values["priorities"] = json!(["ocr", "template"]);
        application
            .save_profile(Some(&saved.id), "After", next_values)
            .unwrap();
        let terminal = settled(application);
        assert_eq!(terminal.run.as_deref(), Some(run.as_str()));
        // The absent fixture executable is the expected boundary, not a changed-profile refusal.
        assert_eq!(terminal.error.unwrap().category, "ChildStartup");
    }

    #[test]
    fn pending_settings_do_not_block_selection_or_hide_the_warning() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        application
            .save_settings(Settings {
                package_path: Some("previous-package".into()),
                ..Settings::default()
            })
            .unwrap();
        let settings_path = fixture.root.join("settings.json");
        let before = fs::read(&settings_path).unwrap();
        let pending_path = fixture.root.join("settings.pending");
        let pending = b"interrupted settings write";
        fs::write(&pending_path, pending).unwrap();

        let selection = application.select(&package_path()).unwrap();
        assert_eq!(selection.package.package_id, "m0-workload");
        let validated = application
            .validate(json!({"priorities":["ocr", "template"]}))
            .unwrap();
        assert_eq!(validated["priorities"], json!(["ocr", "template"]));
        assert_eq!(fs::read(&settings_path).unwrap(), before);
        assert_eq!(fs::read(&pending_path).unwrap(), pending.as_slice());
        assert_eq!(
            application.settings().unwrap().package_path.as_deref(),
            Some("previous-package")
        );
        let logs = application.poll().logs;
        let warning = logs
            .entries
            .iter()
            .find(|entry| entry.code == "settings.package_hint_not_saved")
            .expect("the GUI must receive the hint-persistence warning");
        assert!(warning.level.eq_ignore_ascii_case("warn"));
        assert_eq!(warning.fields["category"], "Storage");
    }

    #[test]
    fn unsafe_package_numbers_are_refused_before_ipc_export() {
        for (source, pointer, expected_path) in [
            (
                "schema.json",
                "/properties/amount/default",
                "$.schema.properties.amount.default",
            ),
            (
                "schema.json",
                "/properties/amount/maximum",
                "$.schema.properties.amount.maximum",
            ),
            (
                "profiles/template-first.json",
                "/options/amount",
                "$.presets[\"template-first\"].options.amount",
            ),
        ] {
            let fixture = Fixture::new();
            let package = fixture.numeric_package();
            let path = package.join(source);
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            if source == "schema.json" {
                value["properties"]["amount"]["maximum"] = json!(u64::MAX);
            } else {
                value["options"]["amount"] = Value::Null;
            }
            *value.pointer_mut(pointer).unwrap() = json!(9_007_199_254_740_993_u64);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let before = fs::read(&path).unwrap();
            let error = fixture
                .application
                .select(&package)
                .err()
                .expect("unsafe source must be refused");
            assert_eq!(error.category, "NumericPrecision", "{source}");
            assert_eq!(error.context["path"], expected_path);
            assert_eq!(error.context["value"], "9007199254740993");
            assert_eq!(fs::read(&path).unwrap(), before);
            assert!(lock(&fixture.application.selected).is_none());
        }
    }

    #[test]
    fn unsafe_stored_numbers_remain_preserved_and_unavailable_to_commands() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = fixture.numeric_package();
        application.select(&path).unwrap();
        let inventory = lock(&application.selected)
            .as_ref()
            .unwrap()
            .inventory
            .clone();
        let saved = lock(&application.store)
            .save(
                &inventory,
                None,
                "Exact imported number",
                json!({"amount":9_007_199_254_740_993_u64}),
            )
            .unwrap();
        let stored_path = fixture
            .root
            .join("profiles")
            .join(format!("{}.json", saved.id));
        let before = fs::read(&stored_path).unwrap();
        let reopened = application.select(&path).unwrap();
        assert!(reopened.profiles.is_empty());
        let error = reopened.profiles_error.unwrap();
        assert_eq!(
            error.context["rejected"][0]["context"]["profile_id"],
            saved.id
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["path"],
            "$.amount"
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["value"],
            "9007199254740993"
        );
        assert!(application.profiles().unwrap().is_empty());
        assert_eq!(
            application
                .save_profile(Some(&saved.id), "Rounded replacement", json!({"amount":1}))
                .unwrap_err()
                .category,
            "ProfileRejected",
        );
        assert_eq!(
            application
                .rename_profile(&saved.id, "Renamed")
                .unwrap_err()
                .category,
            "ProfileRejected"
        );
        application
            .start(StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: inventory.identity,
                package_id: saved.package_id,
                schema_identity: saved.schema_identity,
                profile_id: saved.id,
                values: json!({"amount":9_007_199_254_740_992_u64}),
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            })
            .unwrap();
        let controller = settled(application);
        assert_eq!(controller.error.unwrap().category, "ProfileRejected");
        assert_eq!(fs::read(stored_path).unwrap(), before);
    }

    #[test]
    fn omitted_defaults_are_checked_without_replacing_explicit_values() {
        let fixture = Fixture::new();
        let path = fixture.numeric_package();
        fixture.application.select(&path).unwrap();
        let mut selected = lock(&fixture.application.selected);
        let inventory = &mut selected.as_mut().unwrap().inventory;
        inventory.schema["properties"]["amount"]["default"] = json!(9_007_199_254_740_993_u64);
        let error = desktop_options(inventory, json!({})).unwrap_err();
        assert_eq!(error.category, "NumericPrecision");
        assert_eq!(error.context["path"], "$.amount");
        let explicit = desktop_options(inventory, json!({"amount":0.1})).unwrap();
        assert_eq!(explicit["amount"], json!(0.1));
    }

    #[test]
    fn integer_precision_checks_do_not_saturate_or_conflate_values() {
        for text in [
            "9007199254740992",
            "9007199254740993",
            "-9007199254740993",
            "9223372036854775807",
            "18446744073709551615",
            "-9223372036854775808",
        ] {
            let value: Value = serde_json::from_str(text).unwrap();
            let error = check_webview_value(&json!({"items":[value]}), "$").unwrap_err();
            assert_eq!(error.context["path"], "$.items[0]");
            assert_eq!(error.context["value"], text);
        }
        let safe: Value =
            serde_json::from_str("[9007199254740991,-9007199254740991,0.1,1e18,1e100]").unwrap();
        check_webview_value(&safe, "$").unwrap();
        assert!(same_json_values(
            &json!({"items":[1.0]}),
            &json!({"items":[1]})
        ));
        assert!(!same_json_values(
            &json!(9_007_199_254_740_993_u64),
            &json!(9_007_199_254_740_992_u64)
        ));
        assert!(!same_json_values(
            &json!(9_007_199_254_740_993_u64),
            &json!(9_007_199_254_740_992_f64)
        ));
        assert!(!same_json_values(&json!(u64::MAX), &json!(u64::MAX as f64)));
    }

    #[test]
    fn floating_profiles_survive_webview_normalization_save_reopen_and_start() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = fixture.numeric_package();
        application.select(&path).unwrap();
        let inventory = lock(&application.selected)
            .as_ref()
            .unwrap()
            .inventory
            .clone();
        let source: Value = serde_json::from_str(
            r#"{"amount":1000000000000000100.0,"numbers":[1.0,1e18,1e100,0.1,9007199254740991.0]}"#,
        )
        .unwrap();
        let stored = lock(&application.store)
            .save(&inventory, None, "Floating values", source.clone())
            .unwrap();
        let selection = application.select(&path).unwrap();
        assert_eq!(selection.profiles[0].values, source);
        // These are the integer spellings emitted by WebView JSON.stringify.
        let submitted: Value = serde_json::from_str(
            r#"{"amount":1000000000000000100,"numbers":[1,1000000000000000000,1e100,0.1,9007199254740991]}"#
        ).unwrap();
        let effective = application.validate(submitted.clone()).unwrap();
        assert_eq!(effective["amount"], source["amount"]);
        let saved = application
            .save_profile(Some(&stored.id), "Floating values", submitted.clone())
            .unwrap();
        assert_eq!(saved.values, source);
        let reopened = application.select(&path).unwrap();
        assert!(reopened.profiles_error.is_none());
        assert_eq!(reopened.profiles[0].values, source);
        let before = fs::read(
            fixture
                .root
                .join("profiles")
                .join(format!("{}.json", saved.id)),
        )
        .unwrap();
        let run = application
            .start(StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: inventory.identity,
                package_id: saved.package_id,
                schema_identity: saved.schema_identity,
                profile_id: saved.id.clone(),
                values: submitted,
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            })
            .unwrap();
        assert_eq!(application.runner.poll().run.as_deref(), Some(run.as_str()));
        let terminal = settled(application);
        assert_eq!(terminal.error.as_ref().unwrap().category, "ChildStartup");
        assert_eq!(
            terminal.error.unwrap().context["operation_stage"],
            "execution"
        );
        assert_eq!(
            fs::read(
                fixture
                    .root
                    .join("profiles")
                    .join(format!("{}.json", saved.id))
            )
            .unwrap(),
            before,
        );
    }

    #[test]
    fn controlled_start_ignores_unavailable_environment_settings() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = package_path();
        let selection = application.select(&path).unwrap();
        let settings_path = fixture.root.join("settings.json");
        let preserved = b"unreadable OCR settings must not disable controlled runs";
        fs::write(&settings_path, preserved).unwrap();
        let package = selection.package;
        application
            .start(StartRequest {
                package_path: path.to_string_lossy().into_owned(),
                inventory_identity: package.inventory_identity,
                package_id: package.package_id,
                schema_identity: package.schema_identity,
                profile_id: "draft".into(),
                values: package.profiles["template-first"]["options"].clone(),
                lane: "controlled".into(),
                scenario: "workflow".into(),
                replay_descriptor_path: None,
            })
            .unwrap();
        let terminal = settled(application);
        let fault = terminal.error.unwrap();
        assert_eq!(fault.category, "ChildStartup");
        assert_eq!(fault.context["operation_stage"], "execution");
        assert!(fault.context["environment_identity"].is_null());
        assert_eq!(fs::read(settings_path).unwrap(), preserved);
    }

    #[test]
    fn signed_zero_sources_are_refused_without_rewriting_profiles() {
        let fixture = Fixture::new();
        let application = &fixture.application;
        let path = fixture.numeric_package();
        application.select(&path).unwrap();
        let inventory = lock(&application.selected)
            .as_ref()
            .unwrap()
            .inventory
            .clone();
        let saved = lock(&application.store)
            .save(&inventory, None, "Signed zero", json!({"amount":-0.0}))
            .unwrap();
        let stored_path = fixture
            .root
            .join("profiles")
            .join(format!("{}.json", saved.id));
        let before = fs::read(&stored_path).unwrap();
        let reopened = application.select(&path).unwrap();
        assert!(reopened.profiles.is_empty());
        let error = reopened.profiles_error.unwrap();
        assert_eq!(
            error.context["rejected"][0]["context"]["profile_id"],
            saved.id
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["path"],
            "$.amount"
        );
        assert_eq!(
            error.context["rejected"][0]["context"]["cause"]["context"]["value"],
            "-0.0"
        );
        assert_eq!(
            application
                .rename_profile(&saved.id, "Unsigned")
                .unwrap_err()
                .category,
            "ProfileRejected"
        );
        assert_eq!(
            application
                .validate(json!({"amount":-0.0}))
                .unwrap_err()
                .category,
            "NumericPrecision",
        );
        let schema_path = path.join("schema.json");
        let mut schema = inventory.schema;
        schema["properties"]["amount"]["default"] = json!(-0.0);
        fs::write(&schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
        let source_before = fs::read(&schema_path).unwrap();
        let error = application
            .select(&path)
            .err()
            .expect("signed-zero default must be refused");
        assert_eq!(error.category, "NumericPrecision");
        assert_eq!(error.context["path"], "$.schema.properties.amount.default");
        assert_eq!(error.context["value"], "-0.0");
        assert_eq!(fs::read(schema_path).unwrap(), source_before);
        assert_eq!(fs::read(stored_path).unwrap(), before);
    }
}
