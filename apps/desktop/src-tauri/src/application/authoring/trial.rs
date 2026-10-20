//! Script trials owned by the current Edit session. Whole package and a selected
//! function run on the shared supervised runner from the exact saved revision,
//! without releasing the lease or opening ordinary Start.
use super::{AuthoringRef, Lease};
use crate::application::native_run::NativeBinding;
use crate::application::profiles::{
    check_selected_profile, desktop_options, normalize_editor_numbers, validated_profiles,
};
use crate::application::recovery::RecoveryContext;
use crate::application::targets::{TargetScope, target_view};
use crate::application::workspaces::check_package_values;
use crate::application::{
    Application, MAX_SESSION_COUNTER, OperationOwner, RecoveryView, Selected, TargetView, lock,
    serialize_shared,
};
use crate::storage::Profile;
use mado_runtime_comparison::desktop::{NativeReview, PackageInfo, StartPreparation, StartRequest};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::{Control, Fault, identity};
use mado_runtime_comparison::typescript::{FunctionCandidate, function_candidates};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::{Arc, mpsc};

/// Non-executing setup for one saved revision. Profile and target failures stay
/// beside the package so the owner can correct them without leaving Edit.
#[derive(Debug, Serialize)]
pub struct TrialContext {
    pub owner: AuthoringRef,
    pub revision: String,
    #[serde(serialize_with = "serialize_shared")]
    pub package: Arc<PackageInfo>,
    pub profiles: Vec<Profile>,
    pub profiles_error: Option<Fault>,
    pub recovery: Option<RecoveryView>,
    pub target: Option<TargetView>,
    pub target_error: Option<Fault>,
    pub functions: Vec<FunctionCandidate>,
}

impl Lease {
    /// Settles a trial in the `collect` critical section that ends its ownership.
    pub(in crate::application) fn settle_trial(
        &mut self,
        controller: &Value,
        native_reconciliation: bool,
    ) {
        if self.containment.is_none() {
            self.containment = containment(controller, native_reconciliation);
        }
    }
}

/// Native reconciliation contains the lease even after its child has been reaped.
fn containment(controller: &Value, native_reconciliation: bool) -> Option<Fault> {
    let result = &controller["result"];
    let (cleanup, primary, released) = if result.is_null() {
        let error = &controller["error"];
        let cleanup = &error["context"]["cleanup"];
        (
            cleanup,
            &error["category"],
            cleanup["child_started"] == false,
        )
    } else {
        (
            &result["cleanup"],
            &result["primary"]["category"],
            result["observations"]["child_reaped"] == true,
        )
    };
    let contained = native_reconciliation
        || (cleanup["clean"] != true && !released)
        || *primary == "Containment"
        || *primary == "Controller";
    contained.then(|| {
        Fault::new(
            "TrialCleanup",
            "Script trial cleanup or child ownership is unconfirmed; close the application before further work",
        )
        .with_context(json!({"primary":primary,"cleanup":cleanup,"child_released":released}))
    })
}

/// Releases the lease's discovery reservation however discovery returns.
struct Discovery<'a> {
    application: &'a Application,
    owner: &'a AuthoringRef,
    control: &'a Arc<Control>,
}

impl Drop for Discovery<'_> {
    fn drop(&mut self) {
        if let Some(lease) = lock(&self.application.workspaces)
            .authoring
            .as_mut()
            .filter(|lease| lease.owner == *self.owner)
        {
            lease.discovery = false;
        }
        if let Some(slot) = lock(&self.application.authoring_stop).as_mut()
            && slot
                .context
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, self.control))
        {
            slot.context = None;
        }
    }
}

impl Application {
    pub fn authoring_trial_context(
        &self,
        owner: &AuthoringRef,
        revision: &str,
    ) -> Result<TrialContext, Fault> {
        let result = (|| {
            let (command, mut state) = self.command_state()?;
            let candidate = state.authoring_revision(owner, revision)?;
            self.collect(&mut state);
            state.work_idle()?;
            let workspace = state.resolve_workspace(&owner.workspace)?;
            let internal_name = workspace.internal_name.clone();
            let display_name = workspace.display_name.clone();
            let control = Arc::new(Control::new(&crate::authoring::limits()?));
            let lease = state.authoring.as_mut().expect("owner checked");
            // Publication retires this token; repeated setup reads preserve repair drafts.
            let retained_recovery = lease.recovery.clone();
            let recovery_token = if retained_recovery.is_none() {
                if lease.next_recovery > MAX_SESSION_COUNTER {
                    return Err(Fault::new(
                        "RecoveryLimit",
                        "Recovery context counter exhausted",
                    ));
                }
                let token = format!("recovery-{}-{}", owner.token, lease.next_recovery);
                lease.next_recovery += 1;
                Some(token)
            } else {
                None
            };
            lease.discovery = true;
            lock(&self.authoring_stop)
                .as_mut()
                .expect("lease owns cancellation slot")
                .context = Some(control.clone());
            let _discovery = Discovery {
                application: self,
                owner,
                control: &control,
            };
            // The reservation excludes publication, runs and exit for this lease; Stop,
            // polling and other commands stay available during the bounded compiler work.
            drop(state);
            drop(command);
            control.check()?;
            let inventory = candidate.validate()?;
            let functions = function_candidates(&inventory, &control)?;
            control.check()?;
            let declaration = inventory.target()?;
            let scope = TargetScope {
                workspace: owner.workspace.clone(),
                internal_name: internal_name.clone(),
                package_id: inventory.package_id.clone(),
                declaration_identity: declaration
                    .as_ref()
                    .map(TargetDeclaration::identity)
                    .transpose()?,
                declaration,
            };
            let ((profiles, mut profiles_error), (target, target_error)) = {
                let store = lock(&self.store);
                let profiles = (|| {
                    let tab = store.tab(&internal_name)?;
                    if !tab
                        .packages
                        .iter()
                        .any(|saved| saved.package_id == inventory.package_id)
                    {
                        return Ok((Vec::new(), None));
                    }
                    let profiles = store.profile_store(&internal_name, &inventory.package_id)?;
                    validated_profiles(&profiles, &inventory)
                })()
                .unwrap_or_else(|error| (Vec::new(), Some(error)));
                let target = match store.read_target(&internal_name, &inventory.package_id) {
                    Ok(record) => (Some(target_view(&scope, record)), None),
                    Err(error) => (None, Some(error)),
                };
                (profiles, target)
            };
            control.check()?;
            // Retain a captured inventory only when profiles need recovery.
            let recovery_inventory = (retained_recovery.is_none() && profiles_error.is_some())
                .then(|| Arc::new(inventory.clone()));
            let package = Arc::new(self.runner.inspect_inventory(inventory, Some(&control))?);
            check_package_values(&package)?;
            let recovery = if let Some(context) = retained_recovery {
                Some(context)
            } else if let Some(inventory) = recovery_inventory {
                let selected = Selected {
                    workspace: owner.workspace.clone(),
                    internal_name,
                    display_name,
                    path: candidate.root().to_path_buf(),
                    installed: None,
                    inventory,
                    package: package.clone(),
                };
                // Catalog faults remain visible when safe repair records cannot be read.
                RecoveryContext::for_authoring(
                    &lock(&self.store),
                    selected,
                    owner,
                    revision,
                    recovery_token.expect("new recovery token reserved"),
                )
                .unwrap_or_else(|error| {
                    profiles_error = Some(error);
                    None
                })
                .map(Arc::new)
            } else {
                None
            };
            control.check()?;
            let recovery_view = recovery.as_ref().map(|context| context.view());
            lock(&self.workspaces)
                .authoring
                .as_mut()
                .expect("discovery retains the authoring lease")
                .recovery = recovery;
            Ok(TrialContext {
                owner: owner.clone(),
                revision: revision.to_owned(),
                package,
                profiles,
                profiles_error,
                recovery: recovery_view,
                target,
                target_error,
                functions,
            })
        })();
        self.outcome(
            Some(&owner.workspace),
            "authoring_trial_context",
            None,
            result,
        )
    }

    /// Reserve the shared runner for the current owner's saved revision. The worker
    /// rereads that revision and refuses any other bytes; the lease is never released.
    pub fn authoring_trial_start(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        mut request: StartRequest,
    ) -> Result<String, Fault> {
        let result = (|| {
            let (_command, mut state) = self.command_state()?;
            let candidate = state.authoring_revision(owner, revision)?;
            self.collect(&mut state);
            state.work_idle()?;
            let native = request.lane == "native";
            if native {
                state.native_ready()?;
                state.native_capture_released(owner)?;
                if !matches!(
                    request.native_intent.as_ref().map(|intent| &intent.review),
                    Some(NativeReview::Script {})
                ) {
                    return Err(Fault::new(
                        "NativeRefused",
                        "Edit Native requires explicit approval of the selected saved Script",
                    ));
                }
            }
            if request.package_id != candidate.package_id() {
                return Err(Fault::new(
                    "StaleIdentity",
                    "Trial setup names another package; refresh it",
                ));
            }
            let internal_name = state
                .resolve_workspace(&owner.workspace)?
                .internal_name
                .clone();
            // The caller's location is never an execution authority.
            request.package_path = candidate.root().to_string_lossy().into_owned();
            let publisher = self.publisher.clone();
            let store = self.store.clone();
            let expected = revision.to_owned();
            let (acquired, ready) = mpsc::sync_channel(1);
            // Stop cannot observe the reservation before its run is recorded here.
            let mut stop = lock(&self.authoring_stop);
            let run = self.runner.start_with_preparation(
                request,
                move |request, control| {
                    control.check()?;
                    let saved = publisher.open(candidate.root())?;
                    if saved.revision() != expected || saved.package_id() != candidate.package_id()
                    {
                        return Err(Fault::new(
                            "AuthoringConflict",
                            "Saved source changed before the trial; refresh and run again",
                        ));
                    }
                    let inventory = saved.validate()?;
                    control.check()?;
                    if inventory.identity != request.inventory_identity
                        || identity(&inventory.schema)? != request.schema_identity
                    {
                        return Err(Fault::new(
                            "StaleIdentity",
                            "Trial setup does not name the saved revision; refresh it",
                        ));
                    }
                    normalize_editor_numbers(&inventory.schema, &mut request.values);
                    desktop_options(&inventory, request.values.clone())?;
                    let declaration = if native {
                        let declaration = inventory.target()?;
                        let declaration_identity = declaration
                            .as_ref()
                            .map(TargetDeclaration::identity)
                            .transpose()?;
                        declaration.zip(declaration_identity)
                    } else {
                        None
                    };
                    let (environment, binding) = {
                        let store = lock(&store);
                        let _ = acquired.send(());
                        control.check()?;
                        let environment = if matches!(request.lane.as_str(), "replay" | "native") {
                            store.settings()?.ocr_environment
                        } else {
                            None
                        };
                        if request.profile_id != "draft" {
                            let profiles =
                                store.profile_store(&internal_name, &request.package_id)?;
                            check_selected_profile(&profiles, request)?;
                        }
                        let binding = native
                            .then(|| {
                                let record =
                                    store.read_target(&internal_name, &request.package_id)?;
                                NativeBinding::capture(
                                    request,
                                    &request.package_id,
                                    declaration.as_ref().map(|(declaration, identity)| {
                                        (declaration, identity.as_str())
                                    }),
                                    record,
                                )
                            })
                            .transpose()?;
                        (environment, binding)
                    };
                    control.check()?;
                    Ok(StartPreparation {
                        environment,
                        native: binding,
                        inventory: Some(inventory),
                    })
                },
                NativeBinding::resolver,
            )?;
            stop.as_mut().expect("lease owns cancellation slot").run = Some(run.clone());
            drop(stop);
            state.owner = Some(OperationOwner {
                run: run.clone(),
                workspace: Some(owner.workspace.clone()),
                check: None,
                native,
                trial: Some(owner.clone()),
                terminal: false,
            });
            self.collect(&mut state);
            drop(_command);
            drop(state);
            // Store capture precedes admission return, as for ordinary Start.
            let _ = ready.recv();
            Ok(run)
        })();
        self.outcome(
            Some(&owner.workspace),
            "authoring_trial_start",
            None,
            result,
        )
    }

    pub(in crate::application) fn release_trial_stop(&self, owner: &AuthoringRef, run: &str) {
        if let Some(slot) = lock(&self.authoring_stop)
            .as_mut()
            .filter(|slot| slot.owner == *owner && slot.run.as_deref() == Some(run))
        {
            slot.run = None;
        }
    }

    pub(in crate::application) fn cancel_trial_context(&self) {
        if let Some(control) = lock(&self.authoring_stop)
            .as_ref()
            .and_then(|slot| slot.context.as_ref())
        {
            control.cancel();
        }
    }
}

#[cfg(test)]
mod tests;
