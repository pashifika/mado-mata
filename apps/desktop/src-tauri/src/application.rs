use crate::logging::{LogBatch, Logger};
use crate::storage::{Profile, Settings, Store};
use mado_runtime_comparison::desktop::{DesktopController, PackageInfo, StartRequest};
use mado_runtime_comparison::host::resolve_options;
use mado_runtime_comparison::inventory::Inventory;
use mado_runtime_comparison::model::{Fault, Plan};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
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
    store: Mutex<Store>,
    selected: Mutex<Option<Selected>>,
    logger: Logger,
    view: Mutex<Value>,
    closing: AtomicBool,
    bridge: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Application {
    pub fn new(root: PathBuf, runner: PathBuf) -> Result<Arc<Self>, Fault> {
        let application = Arc::new(Self {
            runner: DesktopController::new(runner),
            store: Mutex::new(Store::new(root.clone())?),
            selected: Mutex::new(None),
            logger: Logger::new(root.join("logs"))?,
            view: Mutex::new(json!({"state":"idle","run":null})),
            closing: AtomicBool::new(false),
            bridge: Mutex::new(None),
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
        let mut settings = store.settings()?;
        settings.package_path = Some(path.to_string_lossy().into_owned());
        store.save_settings(settings)?;
        drop(store);
        *lock(&self.selected) = Some(Selected {
            path: path.to_path_buf(),
            inventory,
        });
        Ok(Selection {
            package,
            profiles,
            profiles_error,
        })
    }

    pub fn validate(&self, values: Value) -> Result<Value, Fault> {
        let selected = lock(&self.selected);
        let selected = selected
            .as_ref()
            .ok_or_else(|| Fault::new("Package", "Select a package first"))?;
        resolve_options(
            &selected.inventory.schema,
            &json!({"package_id":selected.inventory.package_id,"schema_version":1,"options":values}),
            &selected.inventory.package_id,
        )
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
        values: Value,
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
        lock(&self.store).save(&selected.inventory, id, name, values)
    }

    pub fn rename_profile(&self, id: &str, name: &str) -> Result<Profile, Fault> {
        let selected = lock(&self.selected);
        let selected = selected
            .as_ref()
            .ok_or_else(|| Fault::new("Package", "Select a package first"))?;
        lock(&self.store).rename(&selected.inventory, id, name)
    }

    pub fn delete_profile(&self, id: &str) -> Result<(), Fault> {
        lock(&self.store).delete(id)
    }

    pub fn start(&self, request: StartRequest) -> Result<String, Fault> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        if request.profile_id != "draft" {
            let profiles = lock(&self.store).list(&request.package_id, &request.schema_identity)?;
            if !profiles
                .profiles
                .iter()
                .any(|profile| profile.id == request.profile_id)
            {
                return Err(Fault::new(
                    "ProfileNotFound",
                    "Saved profile is unavailable; select or save it again",
                ));
            }
        }
        self.runner.start(request)
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
        self.closing.store(true, Ordering::Release);
        let outcome = self.runner.shutdown();
        if let Some(bridge) = lock(&self.bridge).take() {
            let _ = bridge.join();
        }
        self.collect(&mut (Value::Null, Value::Null));
        self.logger.shutdown();
        outcome
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
        match resolve_options(
            &inventory.schema,
            &json!({
                "package_id":profile.package_id, "schema_version":inventory.schema["version"], "options":profile.values
            }),
            &inventory.package_id,
        ) {
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
