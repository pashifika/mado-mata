use crate::target::{
    self, AuthoringApplication, LaunchFailure, LaunchRecipe, NativeDiscovery, PreparedLaunch,
    TargetBinding, TargetExpectation, TargetRecord,
};
use mado_runtime_comparison::desktop::{
    LaunchDisposition, NativeInputPolicy, NativePhase, NativeProgress, NativeReview, NativeTarget,
    NativeTargetStatus, StartRequest,
};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::{Control, Fault};

#[derive(Clone, Debug)]
pub(super) struct NativeBinding {
    binding: TargetBinding,
    declaration: TargetDeclaration,
    launch_approved: bool,
    script: bool,
    progress: NativeProgress,
    finished: bool,
}

impl NativeBinding {
    /// `target` is the captured declaration with its identity. A Function trial only
    /// attaches: launch and exit recovery are refused here, whatever the caller sent.
    pub(super) fn capture(
        request: &StartRequest,
        package_id: &str,
        target: Option<(&TargetDeclaration, &str)>,
        record: TargetRecord,
    ) -> Result<Self, Fault> {
        let intent = request.native_intent.as_ref().ok_or_else(|| {
            Fault::new(
                "NativeRefused",
                "Review and approve this native operation before Start",
            )
        })?;
        if request.function.is_some() && (intent.launch_approved || intent.max_exit_recoveries != 0)
        {
            return Err(Fault::new(
                "NativeRefused",
                "A Function trial attaches to a running application; launch and recovery are unavailable",
            ));
        }
        let (declaration, declaration_identity) = target
            .ok_or_else(|| Fault::new("TargetUndeclared", "Package has no target declaration"))?;
        if declaration_identity != intent.target_declaration_identity {
            return Err(Fault::new(
                "StaleIdentity",
                "The reviewed target declaration changed",
            ));
        }
        record.compare(&TargetExpectation {
            revision: intent.target_revision,
            binding_id: Some(intent.target_binding_id.clone()),
        })?;
        let binding = record.binding.ok_or_else(|| {
            Fault::new(
                "NativeTargetUnset",
                "Save a compatible application target before Native Start",
            )
        })?;
        if !binding.compatible(
            package_id,
            &declaration.id,
            &intent.target_declaration_identity,
        ) {
            return Err(Fault::new(
                "TargetConflict",
                "Saved target does not match the reviewed package declaration",
            ));
        }
        if binding.configuration.platform != "macos" || binding.configuration.game.kind != "bundle"
        {
            return Err(Fault::new(
                "NativeTargetUnsupported",
                "Native Start requires a saved macOS application bundle",
            ));
        }
        if binding.configuration.window_title.is_empty() || binding.configuration.input.is_none() {
            return Err(Fault::new(
                "NativeTargetUnset",
                "Save an exact window title and input policy before Native Start",
            ));
        }
        Ok(Self {
            binding,
            declaration: declaration.clone(),
            launch_approved: intent.launch_approved,
            script: matches!(intent.review, NativeReview::Script {}),
            progress: NativeProgress {
                phase: NativePhase::TargetDiscovery,
                attempt: 1,
                launch: LaunchDisposition::NotRequested,
                status: NativeTargetStatus::Pending,
            },
            finished: false,
        })
    }

    pub(super) fn for_attempt(&self, attempt: u64) -> Self {
        let mut binding = self.clone();
        binding.progress = NativeProgress {
            attempt,
            phase: NativePhase::TargetDiscovery,
            launch: LaunchDisposition::NotRequested,
            status: NativeTargetStatus::Pending,
        };
        binding.finished = false;
        binding
    }

    /// A fresh one-shot resolver per admitted attempt, built from the immutable capture;
    /// non-native operations have no capture and resolve no target.
    pub(super) fn resolver(
        captured: &Option<Self>,
        attempt: u64,
    ) -> impl FnMut(
        &Control,
        &dyn Fn(NativeProgress),
        &dyn Fn() -> Result<(), Fault>,
    ) -> Result<Option<NativeTarget>, Fault>
    + Send
    + use<> {
        let mut binding = captured
            .as_ref()
            .map(|binding| binding.for_attempt(attempt));
        move |control: &Control,
              report: &dyn Fn(NativeProgress),
              verify_resources: &dyn Fn() -> Result<(), Fault>| {
            match binding.as_mut() {
                Some(binding) => binding.resolve(control, report, verify_resources),
                None => Ok(None),
            }
        }
    }

    pub(super) fn resolve(
        &mut self,
        control: &Control,
        report: &dyn Fn(NativeProgress),
        verify_resources: &dyn Fn() -> Result<(), Fault>,
    ) -> Result<Option<NativeTarget>, Fault> {
        self.resolve_with(control, report, verify_resources, &mut MacosPreparation)
    }

    fn resolve_with(
        &mut self,
        control: &Control,
        report: &dyn Fn(NativeProgress),
        verify_resources: &dyn Fn() -> Result<(), Fault>,
        platform: &mut impl PreparationPlatform,
    ) -> Result<Option<NativeTarget>, Fault> {
        let mut progress = self.progress;
        let result = (|| {
            control.check()?;
            if self.finished {
                return Err(Fault::new(
                    "NativeTargetClosed",
                    "Target preparation already settled for this operation",
                ));
            }
            report(progress);
            let first = self.discover(control, platform);
            control.check()?;
            let target = match first? {
                Some(target) => target,
                None if progress.launch == LaunchDisposition::Accepted => return Ok(None),
                None => {
                    if !self.launch_approved {
                        return Err(Fault::new(
                            "NativeTargetMissing",
                            "No verified running application; launch was not approved",
                        ));
                    }
                    let recipe = platform.recipe(self)?;
                    control.check()?;
                    let prepared = platform.prepare(recipe, control);
                    control.check()?;
                    let prepared = prepared?;
                    verify_resources()?;
                    control.check()?;
                    let final_check = self.discover(control, platform);
                    control.check()?;
                    match final_check? {
                        Some(target) => target,
                        None => {
                            progress.phase = NativePhase::LaunchSubmission;
                            report(progress);
                            control.admit_launch()?;
                            match platform.submit(prepared) {
                                Ok(disposition) => progress.launch = disposition,
                                Err(failure) => {
                                    progress.launch = failure.disposition;
                                    report(progress);
                                    return Err(control.check().err().unwrap_or(failure.fault));
                                }
                            }
                            // Preserve the receipt even if Stop precedes callback settlement.
                            report(progress);
                            control.check()?;
                            if progress.launch != LaunchDisposition::Accepted {
                                return Err(Fault::new(
                                    "NativeLaunchUncertain",
                                    "Launch did not establish accepted submission",
                                ));
                            }
                            progress.phase = NativePhase::WaitingForProcess;
                            report(progress);
                            // The next Script status probe, not a host loop, discovers arrival.
                            return Ok(None);
                        }
                    }
                }
            };
            control.check()?;
            progress.phase = NativePhase::WaitingForWindow;
            report(progress);
            control.check()?;
            Ok(Some(target))
        })();
        self.progress = progress;
        self.finished = !matches!(&result, Ok(None));
        result.map_err(|mut fault: Fault| {
            if !fault.context.is_object() {
                fault.context = serde_json::json!({"cause": fault.context});
            }
            fault.context["native_preparation"] = serde_json::json!(progress);
            fault
        })
    }

    fn discover(
        &mut self,
        control: &Control,
        platform: &mut impl PreparationPlatform,
    ) -> Result<Option<NativeTarget>, Fault> {
        let (executable, process) = if self.script {
            let Some(executable) = platform.script_executable(self, control)? else {
                return Ok(None);
            };
            (executable, None)
        } else {
            let Some(mut proof) = verified(platform.discover(self, control)?)? else {
                return Ok(None);
            };
            if proof.processes.len() > 1 {
                return Err(Fault::new(
                    "NativeTargetAmbiguous",
                    "Multiple running applications match the saved installation",
                ));
            }
            let process = proof.processes.pop().ok_or_else(|| {
                Fault::new(
                    "NativeTargetMissing",
                    "No verified running application matches the saved installation",
                )
            })?;
            (
                process.executable,
                Some((process.pid, format!("{:016x}", process.lifetime))),
            )
        };
        let input = self
            .binding
            .configuration
            .input
            .take()
            .ok_or_else(|| Fault::new("NativeTargetUnset", "Native input policy is missing"))?;
        Ok(Some(NativeTarget {
            executable,
            process,
            window_title: std::mem::take(&mut self.binding.configuration.window_title),
            input: NativeInputPolicy {
                route: input.route,
                focus: input.focus,
                pointer_mode: input.pointer_mode,
                click_hold_ms: input.click_hold_ms,
            },
        }))
    }
}

fn verified(discovery: NativeDiscovery) -> Result<Option<AuthoringApplication>, Fault> {
    match discovery {
        NativeDiscovery::Absent => Ok(None),
        NativeDiscovery::Unique(proof) => Ok(Some(proof)),
        NativeDiscovery::Ambiguous => Err(Fault::new(
            "NativeTargetAmbiguous",
            "Multiple running applications match the saved installation",
        )),
        NativeDiscovery::Unverifiable => Err(Fault::new(
            "NativeTargetUnverifiable",
            "Running application correspondence could not be verified",
        )),
    }
}

trait PreparationPlatform {
    type Recipe;
    type Prepared;

    fn script_executable(
        &mut self,
        binding: &NativeBinding,
        control: &Control,
    ) -> Result<Option<std::path::PathBuf>, Fault>;
    fn discover(
        &mut self,
        binding: &NativeBinding,
        control: &Control,
    ) -> Result<NativeDiscovery, Fault>;
    fn recipe(&mut self, binding: &NativeBinding) -> Result<Self::Recipe, Fault>;
    fn prepare(&mut self, recipe: Self::Recipe, control: &Control)
    -> Result<Self::Prepared, Fault>;
    fn submit(&mut self, prepared: Self::Prepared) -> Result<LaunchDisposition, LaunchFailure>;
}

struct MacosPreparation;

impl PreparationPlatform for MacosPreparation {
    type Recipe = LaunchRecipe;
    type Prepared = PreparedLaunch;

    fn script_executable(
        &mut self,
        binding: &NativeBinding,
        control: &Control,
    ) -> Result<Option<std::path::PathBuf>, Fault> {
        target::script_executable(
            &binding.binding.configuration,
            &binding.declaration,
            &binding.binding.resolution,
            &control.cancelled,
            control.deadline(),
        )
    }

    fn discover(
        &mut self,
        binding: &NativeBinding,
        control: &Control,
    ) -> Result<NativeDiscovery, Fault> {
        target::discover_native(
            &binding.binding.configuration,
            &binding.declaration,
            &binding.binding.resolution,
            &control.cancelled,
            control.deadline(),
        )
    }

    fn recipe(&mut self, binding: &NativeBinding) -> Result<LaunchRecipe, Fault> {
        LaunchRecipe::capture(
            &binding.binding.configuration,
            &binding.declaration,
            &binding.binding.resolution,
        )
    }

    fn prepare(
        &mut self,
        recipe: LaunchRecipe,
        control: &Control,
    ) -> Result<PreparedLaunch, Fault> {
        recipe.prepare(control)
    }

    fn submit(&mut self, prepared: PreparedLaunch) -> Result<LaunchDisposition, LaunchFailure> {
        prepared.submit()
    }
}

#[cfg(test)]
mod tests;
