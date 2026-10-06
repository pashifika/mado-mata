//! The saved recipe is inert until the owned Native preparation admits it once.
use super::{TargetConfiguration, TargetDeclaration, TargetResolution};
use mado_runtime_comparison::desktop::LaunchDisposition;
use mado_runtime_comparison::model::{Control, Fault};
use serde_json::json;

#[cfg(target_os = "macos")]
#[path = "launch_macos.rs"]
mod native;

#[derive(Debug)]
pub(crate) struct LaunchRecipe {
    #[cfg(target_os = "macos")]
    native: native::Recipe,
}

pub(crate) struct PreparedLaunch {
    #[cfg(target_os = "macos")]
    native: mado_application_launch::PreparedLaunch,
}

#[derive(Debug)]
pub(crate) struct LaunchFailure {
    pub disposition: LaunchDisposition,
    pub fault: Fault,
}

impl LaunchFailure {
    fn new(disposition: LaunchDisposition, mut fault: Fault) -> Self {
        if !fault.context.is_object() {
            fault.context = json!({});
        }
        fault.context["launch_disposition"] = json!(disposition);
        Self { disposition, fault }
    }

    #[cfg(not(target_os = "macos"))]
    fn not_requested(fault: Fault) -> Self {
        Self::new(LaunchDisposition::NotRequested, fault)
    }
}

fn disposition(value: mado_application_launch::LaunchDisposition) -> LaunchDisposition {
    match value {
        mado_application_launch::LaunchDisposition::NotRequested => LaunchDisposition::NotRequested,
        mado_application_launch::LaunchDisposition::Accepted => LaunchDisposition::Accepted,
        mado_application_launch::LaunchDisposition::Rejected => LaunchDisposition::Rejected,
        mado_application_launch::LaunchDisposition::Uncertain => LaunchDisposition::Uncertain,
    }
}

impl From<mado_application_launch::LaunchError> for LaunchFailure {
    fn from(error: mado_application_launch::LaunchError) -> Self {
        use mado_application_launch::LaunchErrorKind;
        let category = match error.kind {
            LaunchErrorKind::InvalidRequest => "NativeLaunchRecipe",
            LaunchErrorKind::UnsupportedPlatform => "TargetPlatform",
            LaunchErrorKind::Preparation => "NativeLaunchPreparation",
            LaunchErrorKind::Rejected => "NativeLaunchRejected",
            LaunchErrorKind::Uncertain => "NativeLaunchUncertain",
        };
        Self::new(
            disposition(error.disposition),
            Fault::new(category, error.to_string())
                .with_context(json!({"os_status": error.os_code})),
        )
    }
}

impl LaunchRecipe {
    pub(crate) fn capture(
        configuration: &TargetConfiguration,
        declaration: &TargetDeclaration,
        resolution: &TargetResolution,
    ) -> Result<Self, Fault> {
        configuration.validate_declaration(declaration)?;
        resolution.validate(configuration)?;
        if configuration.platform != "macos" || configuration.game.kind != "bundle" {
            return Err(Fault::new(
                "NativeLaunchRecipe",
                "Native launch requires a saved macOS game bundle",
            ));
        }
        let recipient = configuration
            .launcher
            .as_ref()
            .unwrap_or(&configuration.game);
        if recipient.kind == "bundle" && configuration.working_directory.is_some() {
            return Err(super::configuration_fault(
                "working_directory",
                "Bundle launch uses an OS-defined working directory; remove the explicit directory",
            ));
        }
        #[cfg(target_os = "macos")]
        return native::Recipe::capture(configuration, declaration, resolution)
            .map(|native| Self { native });
        #[cfg(not(target_os = "macos"))]
        Err(Fault::new("TargetPlatform", "Native launch requires macOS"))
    }

    /// Completes all recipe verification before the caller's final target discovery.
    pub(crate) fn prepare(self, control: &Control) -> Result<PreparedLaunch, Fault> {
        control.check()?;
        #[cfg(target_os = "macos")]
        return self
            .native
            .prepare(control)
            .map(|native| PreparedLaunch { native });
        #[cfg(not(target_os = "macos"))]
        Err(Fault::new("TargetPlatform", "Native launch requires macOS"))
    }
}

impl PreparedLaunch {
    /// Consumes an admitted request and settles it even after Stop. No revalidation
    /// may separate the caller's final discovery/admission from this submission.
    pub(crate) fn submit(self) -> Result<LaunchDisposition, LaunchFailure> {
        #[cfg(target_os = "macos")]
        return self
            .native
            .submit()
            .map(disposition)
            .map_err(LaunchFailure::from);
        #[cfg(not(target_os = "macos"))]
        Err(LaunchFailure::not_requested(Fault::new(
            "TargetPlatform",
            "Native launch requires macOS",
        )))
    }
}
