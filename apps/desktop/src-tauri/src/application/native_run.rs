use crate::target::{
    self, AuthoringApplication, LaunchFailure, LaunchRecipe, NativeDiscovery, PreparedLaunch,
    TargetBinding, TargetExpectation, TargetRecord,
};
use mado_runtime_comparison::desktop::{
    LaunchDisposition, NativeInputPolicy, NativePhase, NativeProgress, NativeTarget, PackageInfo,
    StartRequest,
};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::{Control, Fault};
use std::time::Duration;

#[derive(Debug)]
pub(super) struct NativeBinding {
    binding: TargetBinding,
    declaration: TargetDeclaration,
    launch_approved: bool,
}

impl NativeBinding {
    pub(super) fn capture(
        request: &StartRequest,
        package: &PackageInfo,
        record: TargetRecord,
    ) -> Result<Self, Fault> {
        let intent = request.native_intent.as_ref().ok_or_else(|| {
            Fault::new(
                "NativeRefused",
                "Review and approve this native operation before Start",
            )
        })?;
        let declaration = package
            .target
            .as_ref()
            .ok_or_else(|| Fault::new("TargetUndeclared", "Package has no target declaration"))?;
        if package.target_identity.as_deref() != Some(&intent.target_declaration_identity) {
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
            &package.package_id,
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
        })
    }

    pub(super) fn resolve(
        self,
        control: &Control,
        report: &dyn Fn(NativeProgress),
        verify_resources: &dyn Fn() -> Result<(), Fault>,
    ) -> Result<NativeTarget, Fault> {
        self.resolve_with(control, report, verify_resources, &mut MacosPreparation)
    }

    fn resolve_with(
        self,
        control: &Control,
        report: &dyn Fn(NativeProgress),
        verify_resources: &dyn Fn() -> Result<(), Fault>,
        platform: &mut impl PreparationPlatform,
    ) -> Result<NativeTarget, Fault> {
        let mut progress = NativeProgress {
            phase: NativePhase::TargetDiscovery,
            launch: LaunchDisposition::NotRequested,
        };
        let result = (|| {
            control.check()?;
            report(progress);
            let first = platform.discover(&self, control);
            control.check()?;
            let proof = match verified(first?)? {
                Some(proof) => proof,
                None => {
                    if !self.launch_approved {
                        return Err(Fault::new(
                            "NativeTargetMissing",
                            "No verified running application; launch was not approved",
                        ));
                    }
                    let recipe = platform.recipe(&self)?;
                    control.check()?;
                    let prepared = platform.prepare(recipe, control);
                    control.check()?;
                    let prepared = prepared?;
                    verify_resources()?;
                    control.check()?;
                    let final_check = platform.discover(&self, control);
                    control.check()?;
                    match verified(final_check?)? {
                        Some(proof) => proof,
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
                            // Keep the receipt even when Stop wins before the callback returns.
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
                            loop {
                                control.check()?;
                                let discovered = platform.discover(&self, control);
                                control.check()?;
                                if let Some(proof) = verified(discovered?)? {
                                    break proof;
                                }
                                platform.wait(control)?;
                            }
                        }
                    }
                }
            };
            control.check()?;
            let target = self.project(proof)?;
            progress.phase = NativePhase::WaitingForWindow;
            report(progress);
            control.check()?;
            Ok(target)
        })();
        result.map_err(|mut fault: Fault| {
            if !fault.context.is_object() {
                fault.context = serde_json::json!({"cause": fault.context});
            }
            fault.context["native_preparation"] = serde_json::json!(progress);
            fault
        })
    }

    fn project(self, mut proof: AuthoringApplication) -> Result<NativeTarget, Fault> {
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
        let input = self
            .binding
            .configuration
            .input
            .ok_or_else(|| Fault::new("NativeTargetUnset", "Native input policy is missing"))?;
        Ok(NativeTarget {
            executable: process.executable,
            process_id: process.pid,
            process_lifetime: format!("{:016x}", process.lifetime),
            window_title: self.binding.configuration.window_title,
            input: NativeInputPolicy {
                route: input.route,
                focus: input.focus,
                pointer_mode: input.pointer_mode,
                click_hold_ms: input.click_hold_ms,
            },
        })
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

    fn discover(
        &mut self,
        binding: &NativeBinding,
        control: &Control,
    ) -> Result<NativeDiscovery, Fault>;
    fn recipe(&mut self, binding: &NativeBinding) -> Result<Self::Recipe, Fault>;
    fn prepare(&mut self, recipe: Self::Recipe, control: &Control)
    -> Result<Self::Prepared, Fault>;
    fn submit(&mut self, prepared: Self::Prepared) -> Result<LaunchDisposition, LaunchFailure>;
    fn wait(&mut self, control: &Control) -> Result<(), Fault>;
}

struct MacosPreparation;

impl PreparationPlatform for MacosPreparation {
    type Recipe = LaunchRecipe;
    type Prepared = PreparedLaunch;

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

    fn wait(&mut self, control: &Control) -> Result<(), Fault> {
        control.check()?;
        std::thread::sleep(
            control
                .deadline()
                .saturating_duration_since(std::time::Instant::now())
                .min(Duration::from_millis(50)),
        );
        control.check()
    }
}

#[cfg(test)]
mod tests;
