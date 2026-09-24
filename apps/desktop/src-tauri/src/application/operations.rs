use super::profiles::{
    checked_profile, desktop_options, normalize_editor_numbers, same_json_values,
};
use super::{
    Application, CheckInputs, OperationOwner, Poll, RetainedCheck, WorkspaceRef, WorkspaceResult,
    Workspaces, lock,
};
use crate::logging::LogStatus;
use mado_runtime_comparison::desktop::StartRequest;
use mado_runtime_comparison::model::Fault;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{Arc, OnceLock, atomic::Ordering, mpsc};
use std::time::Duration;

impl Workspaces {
    pub(super) fn idle(&self) -> Result<(), Fault> {
        if self.owner.as_ref().is_some_and(|owner| !owner.terminal) {
            Err(Fault::new(
                "RunActive",
                "Previous operation has not settled and been reaped",
            ))
        } else {
            Ok(())
        }
    }
}

impl Application {
    pub(super) fn collect(&self, state: &mut Workspaces) {
        let Some(owner) = &mut state.owner else {
            return;
        };
        if owner.terminal {
            return;
        }
        let mut view = self.runner.poll();
        let workspace = owner.workspace.as_ref();
        let workspace_id = workspace.map(|value| value.workspace_id.as_str());
        for event in view.logs.drain(..) {
            self.logger.emit(
                "Script",
                event["severity"].as_str().unwrap_or("info"),
                Some(&owner.run),
                workspace_id,
                "script.log",
                event["message"].as_str().unwrap_or("Script log"),
                event.clone(),
            );
        }
        let mut value = serde_json::to_value(view).expect("serializable controller view");
        value["workspace_id"] = json!(workspace_id);
        value["workspace_revision"] = json!(workspace.map(|value| value.revision));
        if state.controller["run"] != value["run"] || state.controller["state"] != value["state"] {
            self.logger.emit(
                "Rust",
                "info",
                Some(&owner.run),
                workspace_id,
                "run.state",
                "Run state changed",
                json!({"state":value["state"],"operation":value["operation"]}),
            );
        }
        let terminal = value["state"] == "terminal";
        let controller = Arc::new(value);
        if terminal {
            let outcome = TerminalOutcome::from_view(&controller);
            let (level, message) = outcome.notice();
            self.logger.emit(
                "Rust",
                level,
                Some(&owner.run),
                workspace_id,
                "run.terminal",
                message,
                outcome.fields(&controller["operation"]),
            );
            if let Some(workspace) = workspace {
                if let Some(selected) = state
                    .open
                    .iter_mut()
                    .find(|value| value.workspace == *workspace)
                {
                    selected.terminal = Some(WorkspaceResult {
                        workspace: workspace.clone(),
                        controller: controller.clone(),
                    });
                }
            }
            if let Some(check) = &owner.check {
                state.last_check = Some(Arc::new(RetainedCheck {
                    workspace: owner.workspace.clone(),
                    environment: check.environment.get().cloned().flatten(),
                    descriptor_path: check.descriptor_path.clone(),
                    package_inventory_identity: check.package_inventory_identity.clone(),
                    controller: controller.clone(),
                }));
            }
            owner.terminal = true;
        }
        state.controller = controller;
    }

    pub fn prepare_reconstruction(&self) -> Result<(), Fault> {
        {
            let (_command, mut state) = self.command_state().map_err(|error| {
                if self.closing.load(Ordering::Acquire) {
                    retired_fault(error)
                } else {
                    error
                }
            })?;
            self.collect(&mut state);
            state.idle()?;
            self.closing.store(true, Ordering::Release);
        }
        self.shutdown().map_err(retired_fault)?;
        let status = self.logger.status();
        if status.shutdown_timed_out || status.file_pending != 0 {
            return Err(Fault::new(
                "LoggingShutdown",
                "Configuration was not replaced because log-writer shutdown was incomplete; exit and relaunch before retrying",
            ).with_context(json!({"logging":status,"application_retired":true})));
        }
        Ok(())
    }

    pub fn start(
        &self,
        workspace: &WorkspaceRef,
        mut request: StartRequest,
    ) -> Result<String, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let selected = state.resolve(workspace)?;
            if request.inventory_identity != selected.inventory.identity
                || request.package_id != selected.inventory.package_id
                || request.schema_identity != selected.package.schema_identity
            {
                return Err(Fault::new(
                    "StaleIdentity",
                    "Workspace package changed; inspect it again",
                ));
            }
            // The caller's location is never an execution authority.
            request.package_path = selected.path.to_string_lossy().into_owned();
            normalize_editor_numbers(&selected.inventory.schema, &mut request.values);
            desktop_options(&selected.inventory, request.values.clone())?;
            let internal_name = selected.internal_name.clone();
            self.collect(&mut state);
            // Never let reserve refresh away an uncollected terminal outcome.
            state.idle()?;
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
                    let profiles = store.profile_store(&internal_name, &request.package_id)?;
                    if request.profile_id != "draft" {
                        let profile = checked_profile(
                            &profiles,
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
            state.owner = Some(OperationOwner {
                run: run.clone(),
                workspace: Some(workspace.clone()),
                check: None,
                terminal: false,
            });
            self.collect(&mut state);
            drop(_command);
            drop(state);
            // Publication precedes collection; store capture precedes admission return.
            let _ = ready.recv();
            Ok(run)
        })();
        self.outcome(Some(workspace), "start", None, result)
    }

    pub fn check_environment(
        &self,
        workspace: Option<&WorkspaceRef>,
        replay_descriptor_path: Option<String>,
    ) -> Result<String, Fault> {
        let result = (|| {
            if replay_descriptor_path
                .as_ref()
                .is_some_and(|path| path.len() > 4096)
            {
                return Err(Fault::new(
                    "ReplayPrerequisite",
                    "Recorded corpus location exceeds the path limit",
                ));
            }
            let (_command, mut state) = self.command_state()?;
            let selected = workspace
                .map(|workspace| state.resolve(workspace))
                .transpose()?;
            let package = selected
                .map(|selected| (selected.path.clone(), selected.inventory.identity.clone()));
            let scope = selected.map(|selected| {
                (
                    selected.internal_name.clone(),
                    selected.inventory.package_id.clone(),
                )
            });
            self.collect(&mut state);
            state.idle()?;
            let environment = Arc::new(OnceLock::new());
            let captured = environment.clone();
            let check = CheckInputs {
                environment,
                descriptor_path: replay_descriptor_path.clone(),
                package_inventory_identity: package.as_ref().map(|(_, identity)| identity.clone()),
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
                    if let Some((internal_name, package_id)) = &scope {
                        store.profile_store(internal_name, package_id)?;
                    }
                    let environment = store.settings()?.ocr_environment;
                    let _ = captured.set(environment.clone());
                    Ok(environment)
                },
            )?;
            state.owner = Some(OperationOwner {
                run: run.clone(),
                workspace: workspace.cloned(),
                check: Some(check),
                terminal: false,
            });
            self.collect(&mut state);
            drop(_command);
            drop(state);
            let _ = ready.recv();
            Ok(run)
        })();
        self.outcome(workspace, "check_environment", None, result)
    }

    pub fn stop(&self, run: &str) -> Result<(), Fault> {
        // No workspace/store locks on the cancellation path.
        self.outcome(None, "stop", None, self.runner.stop(run))
    }

    pub fn poll(&self) -> Poll {
        let mut state = lock(&self.workspaces);
        self.collect(&mut state);
        Poll {
            controller: state.controller.clone(),
            logs: self.logger.drain(),
            workspace_results: state
                .open
                .iter()
                .filter_map(|selected| selected.terminal.clone())
                .collect(),
            last_check: state.last_check.clone(),
        }
    }

    pub fn shutdown(&self) -> Result<(), Fault> {
        self.shutdown_outcome
            .get_or_init(|| {
                self.closing.store(true, Ordering::Release);
                let outcome = self.runner.shutdown();
                if let Some(bridge) = lock(&self.bridge).take() {
                    let _ = bridge.join();
                }
                self.collect(&mut lock(&self.workspaces));
                report_log_shutdown(self.logger.shutdown());
                outcome
            })
            .clone()
    }
}

fn retired_fault(mut error: Fault) -> Fault {
    let cause = std::mem::take(&mut error.context);
    error.with_context(json!({"application_retired":true,"cause":cause}))
}

/// Display-independent facts of a settled operation for the durable record.
/// Absent evidence stays absent; no field is a path, message, or recognized text.
struct TerminalOutcome<'a> {
    status: Option<&'a str>,
    category: Option<&'a str>,
    entry_outcome: Option<&'a str>,
    cleanup_clean: Option<bool>,
    child_started: Option<bool>,
    forced: Option<bool>,
}

impl<'a> TerminalOutcome<'a> {
    fn from_view(controller: &'a Value) -> Self {
        let result = &controller["result"];
        let error = &controller["error"];
        // A result record owns its cleanup and containment facts; without one, only
        // the returning worker's fault context can attest them (never inferred).
        let (cleanup, boundary) = if result.is_null() {
            (&error["context"]["cleanup"], &error["context"])
        } else {
            (&result["cleanup"], result)
        };
        Self {
            status: result["status"].as_str(),
            category: result["primary"]["category"]
                .as_str()
                .or_else(|| error["category"].as_str()),
            entry_outcome: result["entry_outcome"].as_str(),
            cleanup_clean: cleanup["clean"].as_bool(),
            child_started: cleanup["child_started"]
                .as_bool()
                .or_else(|| boundary["child_started"].as_bool()),
            forced: boundary["forced"].as_bool(),
        }
    }

    fn notice(&self) -> (&'static str, &'static str) {
        if self.category.is_none() && self.cleanup_clean == Some(true) {
            return if self.status == Some("PASS") {
                ("info", "Operation completed")
            } else {
                ("warn", "Operation settled without success")
            };
        }
        let message = match (self.cleanup_clean, self.forced, self.child_started) {
            (Some(false), _, _) | (_, Some(true), _) => {
                "Operation failed; cleanup is incomplete or forced"
            }
            (Some(true), Some(false), _) => "Operation failed; cleanup is clean",
            (Some(true), None, Some(false)) => {
                "Operation failed before a child started; cleanup is clean"
            }
            _ => "Operation failed; cleanup is unverified",
        };
        ("error", message)
    }

    fn fields(&self, action: &Value) -> Value {
        json!({
            "action":action,
            "state":"terminal",
            "status":self.status,
            "category":self.category,
            "entry_outcome":self.entry_outcome,
            "cleanup_clean":self.cleanup_clean,
            "child_started":self.child_started,
            "forced":self.forced,
        })
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

#[cfg(test)]
mod tests;
