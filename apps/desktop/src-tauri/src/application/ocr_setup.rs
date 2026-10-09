use super::{Application, lock};
use crate::ocr_setup::{self, SetupProgress, SetupView};
use mado_runtime_comparison::environment::OcrEnvironment;
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Serialize)]
pub struct SetupOperation {
    pub id: String,
    pub active: bool,
    pub progress: SetupProgress,
    pub result: Option<SetupView>,
    pub error: Option<Fault>,
    pub cleanup_error: Option<Fault>,
}

#[derive(Default)]
pub(super) struct SetupSlot {
    next: u64,
    job: Option<SetupJob>,
}

struct SetupJob {
    view: Arc<Mutex<SetupOperation>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SetupSlot {
    fn reap(&mut self) {
        let Some(job) = &mut self.job else { return };
        if job.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            if job.worker.take().expect("finished worker").join().is_err() {
                let mut view = lock(&job.view);
                view.error = Some(Fault::new("OcrSetupWorker", "Resource setup worker failed"));
                view.cleanup_error = Some(Fault::new(
                    "OcrSetupCleanup",
                    "Resource cleanup was not confirmed; restart before retrying",
                ));
            }
            lock(&job.view).active = false;
        }
    }

    fn idle(&mut self) -> Result<(), Fault> {
        self.reap();
        if let Some(job) = &self.job {
            let view = lock(&job.view);
            if view.active {
                return Err(Fault::new(
                    "OcrSetupBusy",
                    "Wait for resource setup or cancel it before continuing",
                ));
            }
            if let Some(error) = &view.cleanup_error {
                return Err(Fault::new("OcrSetupCleanup", error.message.clone())
                    .with_context(serde_json::json!({"cause": error})));
            }
        }
        Ok(())
    }
}

impl Application {
    pub(super) fn setup_idle(&self) -> Result<(), Fault> {
        lock(&self.ocr_setup).idle()
    }

    pub fn ocr_setup_start(
        &self,
        resource_id: String,
        environment: Option<OcrEnvironment>,
        native_selection: Option<ocr_setup::NativeSelection>,
    ) -> Result<String, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.idle()?;
        let catalog = ocr_setup::catalog_view()?;
        if resource_id != "inspect"
            && !catalog
                .items
                .iter()
                .any(|item| item.id == resource_id && item.downloadable)
        {
            return Err(Fault::new(
                "OcrSetupResource",
                "Select a downloadable catalog resource",
            ));
        }
        if resource_id != "inspect" && (environment.is_some() || native_selection.is_some()) {
            return Err(Fault::new(
                "OcrSetupResource",
                "Downloads do not accept environment overrides",
            ));
        }
        let mut slot = lock(&self.ocr_setup);
        slot.idle()?;
        slot.next = slot
            .next
            .checked_add(1)
            .ok_or_else(|| Fault::new("OcrSetupLimit", "Restart before another setup operation"))?;
        let id = format!("{}-setup-{}", state.session, slot.next);
        let view = Arc::new(Mutex::new(SetupOperation {
            id: id.clone(),
            active: true,
            progress: SetupProgress {
                stage: "resolving".into(),
                resource_id: resource_id.clone(),
                bytes: 0,
                total: 0,
            },
            result: None,
            error: None,
            cleanup_error: None,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_view = view.clone();
        let worker_cancel = cancel.clone();
        let root = self.root.clone();
        let worker = std::thread::Builder::new()
            .name("ocr-resource-setup".into())
            .spawn(move || {
                let outcome = if resource_id == "inspect" {
                    ocr_setup::inspect(
                        &root,
                        environment.as_ref(),
                        native_selection.as_ref(),
                        &worker_cancel,
                    )
                } else {
                    ocr_setup::download(&root, &resource_id, &worker_cancel, |progress| {
                        lock(&worker_view).progress = progress;
                    })
                };
                let mut view = lock(&worker_view);
                match outcome {
                    Ok(result) => {
                        view.result = Some(result);
                        view.progress.stage = "complete".into();
                    }
                    Err(error) => {
                        view.cleanup_error = error
                            .context
                            .get("cleanup_error")
                            .filter(|value| !value.is_null())
                            .map(|value| {
                                serde_json::from_value(value.clone()).unwrap_or_else(|_| {
                                    Fault::new(
                                        "OcrSetupCleanup",
                                        "Resource cleanup failed; restart before retrying",
                                    )
                                })
                            });
                        view.progress.stage = if error.category == "OcrSetupCancelled" {
                            "cancelled"
                        } else {
                            "failed"
                        }
                        .into();
                        view.error = Some(error);
                    }
                }
                // Admission remains held until the thread is joined by reap.
            })
            .map_err(|error| Fault::new("OcrSetupWorker", error.to_string()))?;
        slot.job = Some(SetupJob {
            view,
            cancel,
            worker: Some(worker),
        });
        Ok(id)
    }

    pub fn ocr_setup_poll(&self) -> Option<SetupOperation> {
        let mut slot = lock(&self.ocr_setup);
        slot.reap();
        slot.job.as_ref().map(|job| lock(&job.view).clone())
    }

    pub fn ocr_setup_cancel(&self, operation_id: &str) -> Result<(), Fault> {
        let slot = lock(&self.ocr_setup);
        let job = slot
            .job
            .as_ref()
            .filter(|job| lock(&job.view).id == operation_id)
            .ok_or_else(|| {
                Fault::new(
                    "OcrSetupStale",
                    "The resource setup operation is no longer current",
                )
            })?;
        job.cancel.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn cancel_setup(&self) {
        if let Some(job) = &lock(&self.ocr_setup).job {
            job.cancel.store(true, Ordering::Release);
        }
    }

    pub(super) fn settle_setup(&self, deadline: Instant) -> Result<(), Fault> {
        self.cancel_setup();
        loop {
            match self.setup_idle() {
                Err(error) if error.category == "OcrSetupBusy" => {
                    if Instant::now() >= deadline {
                        return Err(Fault::new(
                            "OcrSetupCleanup",
                            "Resource setup has not settled; cleanup remains unconfirmed",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                outcome => return outcome,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::test_support::{Fixture, preferences};
    use std::sync::mpsc;

    fn hold_setup(application: &Application) -> mpsc::Sender<()> {
        let (release, wait) = mpsc::channel();
        let view = Arc::new(Mutex::new(SetupOperation {
            id: "held".into(),
            active: true,
            progress: SetupProgress {
                stage: "downloading".into(),
                resource_id: "models".into(),
                bytes: 0,
                total: 1,
            },
            result: None,
            error: None,
            cleanup_error: None,
        }));
        lock(&application.ocr_setup).job = Some(SetupJob {
            view,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: Some(std::thread::spawn(move || {
                let _ = wait.recv();
            })),
        });
        release
    }

    #[test]
    fn setup_excludes_settings_and_reconstruction_until_worker_settles() {
        let fixture = Fixture::new();
        let app = &fixture.application;
        let before = std::fs::read(fixture.root.join("settings.json")).unwrap();
        let release = hold_setup(app);
        assert_eq!(
            app.save_settings(preferences()).unwrap_err().category,
            "OcrSetupBusy"
        );
        assert_eq!(
            app.prepare_reconstruction().unwrap_err().category,
            "OcrSetupBusy"
        );
        assert_eq!(
            app.capture_configuration().unwrap_err().category,
            "OcrSetupBusy"
        );
        assert_eq!(
            app.create_workspace("blocked", "Blocked")
                .unwrap_err()
                .category,
            "OcrSetupBusy"
        );
        assert_eq!(
            std::fs::read(fixture.root.join("settings.json")).unwrap(),
            before
        );
        app.ocr_setup_cancel("held").unwrap();
        assert_eq!(
            app.save_settings(preferences()).unwrap_err().category,
            "OcrSetupBusy"
        );
        release.send(()).unwrap();
        app.settle_setup(Instant::now() + Duration::from_secs(2))
            .unwrap();
        app.save_settings(preferences()).unwrap();
    }

    #[test]
    fn bootstrap_status_during_setup_keeps_the_ready_session() {
        use crate::bootstrap::{Bootstrap, Phase};
        let fixture = Fixture::new();
        let bootstrap = Bootstrap::new(
            Ok(fixture.root.join("bootstrap")),
            None,
            fixture.root.join("unused-runner"),
            fixture.root.join("unused-engine"),
        );
        assert!(bootstrap.initialize(preferences(), false).unwrap().state == Phase::Ready);
        let app = bootstrap.application().unwrap();
        let release = hold_setup(&app);
        assert!(bootstrap.ensure_started().unwrap().state == Phase::Ready);
        app.ocr_setup_cancel("held").unwrap();
        assert!(bootstrap.status().state == Phase::Ready);
        assert!(Arc::ptr_eq(&app, &bootstrap.application().unwrap()));
        release.send(()).unwrap();
        app.settle_setup(Instant::now() + Duration::from_secs(2))
            .unwrap();
        app.create_workspace("after-setup", "After setup").unwrap();
        assert!(bootstrap.status().state == Phase::Ready);
        let release = hold_setup(&app);
        lock(&lock(&app.ocr_setup).job.as_ref().unwrap().view).cleanup_error =
            Some(Fault::new("OcrSetup", "Staging could not be removed"));
        release.send(()).unwrap();
        assert_eq!(
            app.settle_setup(Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .category,
            "OcrSetupCleanup"
        );
        let status = bootstrap.status();
        assert!(status.state == Phase::Ready);
        assert_eq!(status.fault.unwrap().category, "OcrSetupCleanup");
        assert!(Arc::ptr_eq(&app, &bootstrap.application().unwrap()));
        assert_eq!(
            bootstrap.shutdown().unwrap_err().category,
            "OcrSetupCleanup"
        );
    }

    #[test]
    fn managed_models_are_not_configuration_snapshot_payloads() {
        let fixture = Fixture::new();
        let before = fixture.application.capture_configuration().unwrap();
        let directory = fixture.root.join("ocr-resources");
        std::fs::create_dir(&directory).unwrap();
        let model = std::fs::File::create(directory.join("verified-model.onnx")).unwrap();
        model.set_len(26_000_000).unwrap();
        assert_eq!(fixture.application.capture_configuration().unwrap(), before);
        assert_eq!(model.metadata().unwrap().len(), 26_000_000);
    }

    #[test]
    fn cleanup_failure_remains_a_blocker_after_the_worker_exits() {
        let fixture = Fixture::new();
        let app = &fixture.application;
        let release = hold_setup(app);
        lock(&lock(&app.ocr_setup).job.as_ref().unwrap().view).cleanup_error = Some(Fault::new(
            "OcrSetupCleanup",
            "Staging could not be removed",
        ));
        release.send(()).unwrap();
        assert_eq!(
            app.settle_setup(Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .category,
            "OcrSetupCleanup"
        );
        assert!(!app.ocr_setup_poll().unwrap().active);
        assert_eq!(
            app.save_settings(preferences()).unwrap_err().category,
            "OcrSetupCleanup"
        );
        assert_eq!(
            app.ocr_setup_start("inspect".into(), None, None)
                .unwrap_err()
                .category,
            "OcrSetupCleanup"
        );
    }

    #[test]
    fn closing_cancels_setup_without_waiting_on_worker() {
        let fixture = Fixture::new();
        let app = &fixture.application;
        let release = hold_setup(app);
        assert_eq!(
            app.ocr_setup_cancel("stale").unwrap_err().category,
            "OcrSetupStale"
        );
        assert!(
            !lock(&app.ocr_setup)
                .job
                .as_ref()
                .unwrap()
                .cancel
                .load(Ordering::Acquire)
        );
        app.prepare_close(None).unwrap();
        assert!(
            lock(&app.ocr_setup)
                .job
                .as_ref()
                .unwrap()
                .cancel
                .load(Ordering::Acquire)
        );
        assert_eq!(
            app.save_settings(preferences()).unwrap_err().category,
            "Closing"
        );
        release.send(()).unwrap();
        app.shutdown().unwrap();
        assert!(!app.ocr_setup_poll().unwrap().active);
    }
}
