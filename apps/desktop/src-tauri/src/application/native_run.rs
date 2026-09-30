use crate::target::{self, AuthoringApplication, TargetBinding, TargetExpectation, TargetRecord};
use mado_runtime_comparison::desktop::{
    NativeInputPolicy, NativeTarget, PackageInfo, StartRequest,
};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::{Control, Fault};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(super) struct NativeBinding {
    binding: TargetBinding,
    declaration: TargetDeclaration,
    duration_ms: u64,
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
            duration_ms: intent.limits.duration_ms,
        })
    }

    pub(super) fn resolve(self, control: &Control) -> Result<NativeTarget, Fault> {
        control.check()?;
        let remaining = self
            .duration_ms
            .saturating_sub(control.elapsed_us() / 1_000);
        if remaining == 0 {
            return Err(Fault::new("Timeout", "Native preparation deadline expired"));
        }
        let proof = target::authoring_application(
            &self.binding.configuration,
            &self.declaration,
            &self.binding.resolution,
            &control.cancelled,
            Instant::now() + Duration::from_millis(remaining),
        );
        // The OS call may return after Stop; it never transfers authority on its own.
        control.check()?;
        self.project(proof?)
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

#[cfg(test)]
mod tests;
