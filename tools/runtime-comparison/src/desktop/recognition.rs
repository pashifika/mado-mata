use super::DesktopController;
use crate::environment::{OcrEnvironment, capture_environment};
use crate::model::{Control, ENGINE_REVISION, Fault};
use crate::recognition_trial::{
    Capabilities, TrialFrame, TrialIdentity, TrialRequest, TrialSelection,
};
use crate::runner::{run_capabilities, run_trial};
use serde_json::{Value, json};

impl DesktopController {
    /// Read the actual fixed child's facade capability, without OCR initialization.
    pub fn recognition_capabilities(&self) -> Result<String, Fault> {
        self.reserve(
            "recognition_capabilities",
            true,
            |run, control, observer, _, engine| {
                run_capabilities(engine, run, control, observer)
                    .map_err(|error| preparation_fault(error, Value::Null))
            },
        )
    }

    /// Observation only. The application supplies lease-checked pixels and captured revisions.
    pub fn recognition_trial(
        &self,
        identity: TrialIdentity,
        prepare: impl FnOnce(&Control) -> Result<(TrialFrame, TrialSelection), Fault> + Send + 'static,
        environment: Option<OcrEnvironment>,
    ) -> Result<String, Fault> {
        self.reserve(
            "recognition_trial",
            true,
            move |run, control, observer, _, engine| {
                let captured = json!(identity);
                let request = (|| {
                    control.check()?;
                    let (frame, selection) = prepare(control)?;
                    control.check()?;
                    Ok(TrialRequest {
                        identity,
                        frame,
                        selection,
                    })
                })()
                .map_err(|error| preparation_fault(error, captured.clone()))?;
                let identity = captured;
                let snapshot = (|| {
                    control.check()?;
                    let environment = environment.as_ref().ok_or_else(|| {
                        Fault::new(
                            "EnvironmentUnset",
                            "save an OCR environment before trialing recognition",
                        )
                    })?;
                    capture_environment(environment, control)
                })()
                .map_err(|error| preparation_fault(error, identity.clone()))?;
                // Never duplicate MAX_OCR_ZONES in a non-engine desktop build. Re-probe the
                // fixed artifact before each operation, so replacing it cannot retain a stale cap.
                let mut probe = run_capabilities(engine, run, control, observer)
                    .map_err(|error| preparation_fault(error, identity.clone()))?;
                if !probe["primary"].is_null() || probe["cleanup"]["clean"] != true {
                    probe["operation"] = json!("recognition_trial");
                    probe["identity"] = identity;
                    probe["environment_identity"] = json!(snapshot.identity);
                    return Ok(probe);
                }
                let capability: Capabilities =
                    serde_json::from_value(probe["capabilities"].clone()).map_err(|_| {
                        preparation_fault(
                            Fault::new(
                                "EngineUnavailable",
                                "fixed runner returned no recognition capability",
                            ),
                            identity.clone(),
                        )
                    })?;
                if capability.version != 1
                    || capability.engine_revision != ENGINE_REVISION
                    || capability.max_ocr_zones == 0
                {
                    return Err(preparation_fault(
                        Fault::new(
                            "EngineUnavailable",
                            "fixed runner recognition capability has an incompatible identity",
                        ),
                        identity,
                    ));
                }
                control
                    .check()
                    .map_err(|error| preparation_fault(error, identity.clone()))?;
                let mut result = run_trial(
                    engine,
                    run,
                    request,
                    snapshot.configuration,
                    &capability,
                    control,
                    observer,
                )
                .map_err(|error| preparation_fault(error, identity))?;
                result["environment_identity"] = json!(snapshot.identity);
                Ok(result)
            },
        )
    }
}

fn preparation_fault(mut fault: Fault, identity: Value) -> Fault {
    fault.bound_diagnostics();
    let cause = fault.context;
    let cleanup = cause
        .get("cleanup")
        .cloned()
        .unwrap_or_else(|| json!({"clean":true,"child_started":false}));
    fault.context = json!({"operation":if identity.is_null() {"recognition_capabilities"} else {"recognition_trial"},
        "identity":identity,"stage":"preparation","cause":cause,"cleanup":cleanup});
    fault
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::test_support::settled;
    use crate::images::DecodedImage;
    use crate::recognition::PixelRect;
    use crate::recognition_trial::{OcrZone, TrialFrame, TrialIdentity, TrialSelection};
    use std::sync::Arc;

    fn request() -> TrialRequest {
        TrialRequest {
            identity: TrialIdentity {
                owner: "owner".into(),
                revision: "revision".into(),
                frame: "frame".into(),
                frame_revision: 1,
                content_revision: 1,
                zones_revision: 1,
                configuration_revision: "config".into(),
            },
            frame: TrialFrame {
                image: Arc::new(DecodedImage::from_rgba(1, 1, vec![0, 0, 0, 255]).unwrap()),
            },
            selection: TrialSelection::Ocr {
                zones: vec![OcrZone {
                    id: "zone".into(),
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                }],
            },
        }
    }

    #[test]
    fn missing_environment_is_a_clean_prerequisite_refusal_without_package_execution() {
        let controller =
            DesktopController::new("no-controlled-runner".into(), "no-engine-runner".into());
        let first = request();
        controller
            .recognition_trial(
                first.identity,
                move |_| Ok((first.frame, first.selection)),
                None,
            )
            .unwrap();
        let terminal = settled(&controller);
        let error = terminal.error.unwrap();
        assert_eq!(error.category, "EnvironmentUnset");
        assert_eq!(error.context["identity"]["frame"], "frame");
        assert_eq!(
            error.context["cleanup"],
            json!({"clean":true,"child_started":false})
        );
        assert!(terminal.logs.is_empty());
        let next = request();
        controller
            .recognition_trial(
                next.identity,
                move |_| Ok((next.frame, next.selection)),
                None,
            )
            .unwrap();
        assert_eq!(
            settled(&controller).error.unwrap().category,
            "EnvironmentUnset"
        );
    }

    #[test]
    fn missing_engine_never_supplies_a_guessed_grouped_limit() {
        let controller =
            DesktopController::new("no-controlled-runner".into(), "no-engine-runner".into());
        controller.recognition_capabilities().unwrap();
        let terminal = settled(&controller);
        assert_eq!(terminal.error.unwrap().category, "EngineUnavailable");
        assert!(terminal.result.is_none());
    }

    #[test]
    fn stop_retains_the_shared_slot_until_the_cancelled_worker_has_settled() {
        let controller =
            DesktopController::new("no-controlled-runner".into(), "no-engine-runner".into());
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let run = controller
            .reserve("recognition_trial", true, move |_, control, _, _, _| {
                started.send(()).unwrap();
                wait.recv().unwrap();
                control.check()?;
                Ok(Value::Null)
            })
            .unwrap();
        ready
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        controller.stop(&run).unwrap();
        assert_eq!(
            controller.recognition_capabilities().unwrap_err().category,
            "RunActive"
        );
        assert_eq!(
            controller.stop("stale-owner").unwrap_err().category,
            "StaleIdentity"
        );
        release.send(()).unwrap();
        assert_eq!(settled(&controller).error.unwrap().category, "Cancelled");
        controller.recognition_capabilities().unwrap();
        assert_eq!(
            settled(&controller).error.unwrap().category,
            "EngineUnavailable"
        );
    }
}
