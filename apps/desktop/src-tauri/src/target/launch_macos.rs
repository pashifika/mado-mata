use crate::target::macos::{FileIdentity, InstalledBundle, inspect_bundle};
use crate::target::{
    TargetConfiguration, TargetDeclaration, TargetResolution, canonical, executable,
    metadata_fault, path_string,
};
use mado_runtime_comparison::model::{Control, Fault};
use objc2::rc::autoreleasepool;
use serde_json::json;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct Recipe {
    configuration: TargetConfiguration,
    declaration: TargetDeclaration,
    resolution: TargetResolution,
    snapshot: Snapshot,
    recipient: Recipient,
}

#[derive(Debug, PartialEq, Eq)]
enum Recipient {
    Bundle(PathBuf),
    Executable { path: PathBuf, directory: PathBuf },
}

#[derive(Debug, PartialEq, Eq)]
enum LauncherEvidence {
    Bundle(InstalledBundle),
    Executable {
        selected: FileIdentity,
        resolved: FileIdentity,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    mode: u32,
}

impl DirectoryIdentity {
    fn read(path: &Path) -> Result<Self, Fault> {
        let metadata =
            fs::metadata(path).map_err(|_| metadata_fault("working_directory", "directory"))?;
        if !metadata.is_dir() {
            return Err(metadata_fault("working_directory", "directory"));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    game: InstalledBundle,
    launcher: Option<LauncherEvidence>,
    directory: Option<DirectoryIdentity>,
}

fn changed() -> Fault {
    Fault::new(
        "NativeLaunchRecipeChanged",
        "Saved launch recipe changed; check and review it again",
    )
}

impl Snapshot {
    fn capture(
        configuration: &TargetConfiguration,
        declaration: &TargetDeclaration,
        resolution: &TargetResolution,
        checkpoint: &impl Fn() -> Result<(), Fault>,
    ) -> Result<(Self, Recipient), Fault> {
        checkpoint()?;
        let game = inspect_bundle(&configuration.game.path, "game", checkpoint)?;
        if game.resolution != resolution.game
            || declaration.macos.as_ref().is_some_and(|constraint| {
                game.bundle_id.as_deref() != Some(constraint.bundle_id.as_str())
            })
        {
            return Err(changed());
        }
        let mut launcher = None;
        let (location, resolved) = match (&configuration.launcher, &resolution.launcher) {
            (Some(location), Some(resolved)) => {
                checkpoint()?;
                launcher = Some(if location.kind == "bundle" {
                    let bundle = inspect_bundle(&location.path, "launcher", checkpoint)?;
                    if bundle.resolution != *resolved {
                        return Err(changed());
                    }
                    LauncherEvidence::Bundle(bundle)
                } else {
                    let selected = FileIdentity::read(Path::new(&location.path), "launcher")?;
                    let path = canonical(&location.path, "launcher")?;
                    checkpoint()?;
                    if path_string(&path, "launcher")? != resolved.executable
                        || resolved.path != resolved.executable
                    {
                        return Err(changed());
                    }
                    executable(&path, "launcher")?;
                    let evidence = LauncherEvidence::Executable {
                        selected,
                        resolved: FileIdentity::read(&path, "launcher")?,
                    };
                    checkpoint()?;
                    evidence
                });
                (location, resolved)
            }
            (None, None) => (&configuration.game, &resolution.game),
            _ => return Err(changed()),
        };
        let (directory, recipient) = if location.kind == "bundle" {
            if configuration.working_directory.is_some() || resolution.working_directory.is_some() {
                return Err(crate::target::configuration_fault(
                    "working_directory",
                    "Bundle launch requires an OS-defined working directory",
                ));
            }
            (None, Recipient::Bundle(PathBuf::from(&resolved.path)))
        } else {
            checkpoint()?;
            let directory = match (
                &configuration.working_directory,
                &resolution.working_directory,
            ) {
                (Some(selected), Some(expected)) => {
                    let directory = canonical(selected, "working_directory")?;
                    if path_string(&directory, "working_directory")? != *expected {
                        return Err(changed());
                    }
                    directory
                }
                (None, None) => Path::new(&resolved.executable)
                    .parent()
                    .ok_or_else(|| metadata_fault("working_directory", "parent"))?
                    .to_owned(),
                _ => return Err(changed()),
            };
            let identity = DirectoryIdentity::read(&directory)?;
            (
                Some(identity),
                Recipient::Executable {
                    path: resolved.executable.clone().into(),
                    directory,
                },
            )
        };
        checkpoint()?;
        Ok((
            Self {
                game,
                launcher,
                directory,
            },
            recipient,
        ))
    }
}

impl Recipe {
    pub(super) fn capture(
        configuration: &TargetConfiguration,
        declaration: &TargetDeclaration,
        resolution: &TargetResolution,
    ) -> Result<Self, Fault> {
        let result = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                Snapshot::capture(configuration, declaration, resolution, &|| Ok(()))
            })
        }));
        let (snapshot, recipient) = result.map_err(|_| platform_exception())??;
        Ok(Self {
            configuration: configuration.clone(),
            declaration: declaration.clone(),
            resolution: resolution.clone(),
            snapshot,
            recipient,
        })
    }

    pub(super) fn revalidate(&self, control: &Control) -> Result<(), Fault> {
        let result = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                Snapshot::capture(
                    &self.configuration,
                    &self.declaration,
                    &self.resolution,
                    &|| control.check(),
                )
            })
        }));
        control.check()?;
        let (snapshot, recipient) = result.map_err(|_| platform_exception())??;
        if snapshot != self.snapshot || recipient != self.recipient {
            return Err(changed());
        }
        Ok(())
    }

    pub(super) fn prepare(
        &self,
        control: &Control,
    ) -> Result<mado_application_launch::PreparedLaunch, Fault> {
        let recipient = match &self.recipient {
            Recipient::Bundle(path) => {
                mado_application_launch::Recipient::MacOSBundle { path: path.clone() }
            }
            Recipient::Executable { path, directory } => {
                mado_application_launch::Recipient::Executable {
                    path: path.clone(),
                    working_directory: Some(directory.clone()),
                }
            }
        };
        let prepared = mado_application_launch::PreparedLaunch::prepare(
            mado_application_launch::LaunchRequest {
                recipient,
                arguments: self.configuration.arguments.clone(),
            },
        )
        .map_err(|error| super::LaunchFailure::from(error).fault)?;
        self.revalidate(control)?;
        Ok(prepared)
    }
}

fn platform_exception() -> Fault {
    Fault::new(
        "NativeLaunchPlatform",
        "The platform could not prepare the saved launch recipe",
    )
    .with_context(json!({"stage":"platform_exception"}))
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
