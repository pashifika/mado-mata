use super::{Application, AuthoringRef, WorkspaceRef, lock};
use crate::author_signing::{self, SigningInfo};
use crate::distribution::{self, PreparedPackage, Publication, ReleaseFacts};
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

#[derive(Clone, Serialize)]
pub struct DistributionReview {
    #[serde(flatten)]
    pub facts: ReleaseFacts,
    pub token: String,
    pub workspace: WorkspaceRef,
    pub owner: Option<AuthoringRef>,
    pub destination: String,
    pub runtime: String,
    pub signature_valid: bool,
    pub account_verified: bool,
}

#[derive(Default)]
pub(super) struct DistributionSlot {
    next: u64,
    active: Option<Active>,
    review: Option<Review>,
    containment: Option<Fault>,
}

struct Active {
    workspace: WorkspaceRef,
    cancel: Arc<AtomicBool>,
}

struct Review {
    view: DistributionReview,
    prepared: PreparedPackage,
}

struct Attempt<'a> {
    slot: &'a Mutex<DistributionSlot>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        let mut slot = lock(self.slot);
        if slot
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.cancel, &self.cancel))
        {
            slot.active = None;
        }
    }
}

pub struct DistributionPickerGuard {
    application: Arc<Application>,
    workspace: WorkspaceRef,
    cancel: Arc<AtomicBool>,
}

impl DistributionPickerGuard {
    pub fn validate(&self) -> Result<(), Fault> {
        cancelled(&self.cancel)?;
        if self.application.closing.load(Ordering::Acquire) {
            return Err(Fault::new("Closing", "Application is closing"));
        }
        lock(&self.application.workspaces).resolve_workspace(&self.workspace)?;
        Ok(())
    }
}

impl Drop for DistributionPickerGuard {
    fn drop(&mut self) {
        let mut slot = lock(&self.application.distribution);
        if slot
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.cancel, &self.cancel))
        {
            slot.active = None;
        }
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), Fault> {
    if cancel.load(Ordering::Acquire) {
        Err(Fault::new(
            "PackageCancelled",
            "Package operation was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn stale() -> Fault {
    Fault::new(
        "PackageReviewStale",
        "The package review no longer belongs to this revision; prepare it again",
    )
}

impl Application {
    pub(super) fn distribution_idle(&self) -> Result<(), Fault> {
        let slot = lock(&self.distribution);
        if slot.active.is_some() {
            return Err(Fault::new(
                "PackageBusy",
                "Wait for package transfer or signing enrollment, or cancel it",
            ));
        }
        if let Some(error) = &slot.containment {
            return Err(error.clone());
        }
        Ok(())
    }

    fn distribution_attempt(
        &self,
        workspace: &WorkspaceRef,
    ) -> Result<(Attempt<'_>, String), Fault> {
        let mut slot = lock(&self.distribution);
        if slot.active.is_some() {
            return Err(Fault::new(
                "PackageBusy",
                "Another package operation is active",
            ));
        }
        if let Some(error) = &slot.containment {
            return Err(error.clone());
        }
        slot.next = slot
            .next
            .checked_add(1)
            .filter(|next| *next <= super::MAX_SESSION_COUNTER)
            .ok_or_else(|| {
                Fault::new("PackageLimit", "Restart before another package operation")
            })?;
        let token = format!("{}-package-{}", workspace.workspace_id, slot.next);
        let cancel = Arc::new(AtomicBool::new(false));
        slot.active = Some(Active {
            workspace: workspace.clone(),
            cancel: cancel.clone(),
        });
        Ok((
            Attempt {
                slot: &self.distribution,
                cancel,
            },
            token,
        ))
    }

    pub fn package_distribution_cancel(&self, workspace: &WorkspaceRef) -> Result<bool, Fault> {
        // Cancellation never waits for command, workspace, filesystem or compiler locks.
        let mut slot = lock(&self.distribution);
        let mut changed = false;
        if let Some(active) = &slot.active {
            if active.workspace != *workspace {
                return Err(stale());
            }
            active.cancel.store(true, Ordering::Release);
            changed = true;
        }
        if slot
            .review
            .as_ref()
            .is_some_and(|review| review.view.workspace == *workspace)
        {
            slot.review = None;
            changed = true;
        }
        Ok(changed)
    }

    pub(super) fn cancel_distribution(&self) {
        let mut slot = lock(&self.distribution);
        if let Some(active) = &slot.active {
            active.cancel.store(true, Ordering::Release);
        }
        slot.review = None;
    }

    fn wait_distribution(&self, deadline: Instant) -> Result<(), Fault> {
        self.cancel_distribution();
        while lock(&self.distribution).active.is_some() || self.command_guard().is_err() {
            if Instant::now() >= deadline {
                return Err(Fault::new(
                    "PackageCleanup",
                    "Package operation has not settled; cleanup remains unconfirmed",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    pub(super) fn settle_distribution(&self, deadline: Instant) -> Result<(), Fault> {
        self.wait_distribution(deadline)?;
        self.distribution_idle()
    }

    pub fn prepare_distribution_close(
        &self,
        resolved_owner: Option<&AuthoringRef>,
    ) -> Result<(), Fault> {
        if let Some(owner) = self.authoring_owner() {
            if resolved_owner != Some(&owner) {
                return Err(
                    Fault::new("AuthoringActive", "Resolve editor drafts before closing")
                        .with_context(serde_json::json!({"owner": owner})),
                );
            }
        }
        // A settled cleanup fault is reported by shutdown; it must not trap Close.
        self.wait_distribution(Instant::now() + Duration::from_secs(14))
    }

    pub fn reserve_distribution_picker(
        self: &Arc<Self>,
        workspace: &WorkspaceRef,
        exporting: bool,
    ) -> Result<DistributionPickerGuard, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.resolve_workspace(workspace)?;
        if exporting {
            let lease = state
                .authoring
                .as_ref()
                .filter(|lease| lease.owner.workspace == *workspace)
                .ok_or_else(stale)?;
            if lease.containment.is_some() {
                return Err(stale());
            }
            state.work_idle()?;
        } else {
            state.idle()?;
        }
        let mut slot = lock(&self.distribution);
        let cancel = Arc::new(AtomicBool::new(false));
        slot.active = Some(Active {
            workspace: workspace.clone(),
            cancel: cancel.clone(),
        });
        Ok(DistributionPickerGuard {
            application: self.clone(),
            workspace: workspace.clone(),
            cancel,
        })
    }

    pub fn package_export_prepare(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        destination: &Path,
    ) -> Result<DistributionReview, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.work_idle()?;
        let candidate = state.authoring_revision(owner, revision)?;
        if candidate.managed().is_some() {
            return Err(Fault::new(
                "ManagedPackage",
                "Duplicate the installed release to a source project before exporting",
            ));
        }
        let (attempt, token) = self.distribution_attempt(&owner.workspace)?;
        lock(&self.distribution).review = None;
        drop(state);
        let candidate = self.publisher.check_current(&candidate, revision)?;
        let destination = export_destination(destination, candidate.root())?;
        cancelled(&attempt.cancel)?;
        let signer = author_signing::Signer::open(&self.root)?;
        let prepared =
            distribution::prepare_export(candidate.draft().clone(), &signer, &attempt.cancel)?;
        let package = self
            .runner
            .inspect_inventory(prepared.draft().validate()?)?;
        cancelled(&attempt.cancel)?;
        let view = DistributionReview {
            facts: prepared.facts().clone(),
            token,
            workspace: owner.workspace.clone(),
            owner: Some(owner.clone()),
            destination: destination.to_string_lossy().into_owned(),
            runtime: package.runtime,
            signature_valid: true,
            account_verified: false,
        };
        lock(&self.workspaces).authoring_revision(owner, revision)?;
        let mut slot = lock(&self.distribution);
        cancelled(&attempt.cancel)?;
        slot.review = Some(Review {
            view: view.clone(),
            prepared,
        });
        Ok(view)
    }

    pub fn package_import_prepare(
        &self,
        workspace: &WorkspaceRef,
        path: &Path,
    ) -> Result<DistributionReview, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.resolve_workspace(workspace)?;
        state.idle()?;
        let (attempt, token) = self.distribution_attempt(workspace)?;
        lock(&self.distribution).review = None;
        drop(state);
        let prepared = distribution::prepare_import(path, &attempt.cancel)?;
        let package = self
            .runner
            .inspect_inventory(prepared.draft().validate()?)?;
        cancelled(&attempt.cancel)?;
        let facts = prepared.facts();
        let destination = self
            .root
            .join("pkgs")
            .join(&facts.package_id)
            .join(&facts.artifact_id);
        let view = DistributionReview {
            facts: facts.clone(),
            token,
            workspace: workspace.clone(),
            owner: None,
            destination: destination.to_string_lossy().into_owned(),
            runtime: package.runtime,
            signature_valid: true,
            account_verified: false,
        };
        lock(&self.workspaces).resolve_workspace(workspace)?;
        let mut slot = lock(&self.distribution);
        cancelled(&attempt.cancel)?;
        slot.review = Some(Review {
            view: view.clone(),
            prepared,
        });
        Ok(view)
    }

    pub fn package_export_publish(
        &self,
        owner: &AuthoringRef,
        revision: &str,
        token: &str,
    ) -> Result<Publication, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.work_idle()?;
        state.authoring_revision(owner, revision)?;
        let review = self.take_distribution_review(&owner.workspace, Some(owner), token)?;
        if review.view.facts.revision != revision {
            return Err(stale());
        }
        let (attempt, _) = self.distribution_attempt(&owner.workspace)?;
        drop(state);
        let result = distribution::publish_export(
            &review.prepared,
            Path::new(&review.view.destination),
            &attempt.cancel,
        );
        self.distribution_outcome(result)
    }

    pub fn package_import_publish(
        &self,
        workspace: &WorkspaceRef,
        token: &str,
    ) -> Result<Publication, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.resolve_workspace(workspace)?;
        state.idle()?;
        let review = self.take_distribution_review(workspace, None, token)?;
        let (attempt, _) = self.distribution_attempt(workspace)?;
        drop(state);
        let result = distribution::publish_import(&self.root, &review.prepared, &attempt.cancel);
        self.distribution_outcome(result)
    }

    fn take_distribution_review(
        &self,
        workspace: &WorkspaceRef,
        owner: Option<&AuthoringRef>,
        token: &str,
    ) -> Result<Review, Fault> {
        let mut slot = lock(&self.distribution);
        let review = slot.review.as_ref().ok_or_else(stale)?;
        if review.view.token != token
            || review.view.workspace != *workspace
            || review.view.owner.as_ref() != owner
        {
            return Err(stale());
        }
        Ok(slot.review.take().expect("matched review"))
    }

    fn distribution_outcome(
        &self,
        result: Result<Publication, Fault>,
    ) -> Result<Publication, Fault> {
        let cleanup = match &result {
            Ok(publication) => publication.cleanup_error.clone(),
            Err(error) => error
                .context
                .get("cleanup_error")
                .filter(|value| !value.is_null())
                .and_then(|value| serde_json::from_value(value.clone()).ok()),
        };
        if let Some(error) = cleanup {
            lock(&self.distribution).containment = Some(
                Fault::new(
                    "PackageCleanup",
                    "Resolve retained package publication cleanup before continuing",
                )
                .with_context(serde_json::json!({"cause": error})),
            );
        }
        result
    }

    pub fn author_signing_status(&self) -> Result<Option<SigningInfo>, Fault> {
        let (_command, _state) = self.command_state()?;
        author_signing::status(&self.root)
    }

    pub fn author_signing_enroll(
        &self,
        workspace: &WorkspaceRef,
        open_browser: impl FnOnce(&str) -> Result<(), Fault>,
    ) -> Result<SigningInfo, Fault> {
        let (_command, mut state) = self.command_state()?;
        self.collect(&mut state);
        state.resolve_workspace(workspace)?;
        if let Some(lease) = &state.authoring {
            if lease.owner.workspace != *workspace {
                return Err(stale());
            }
        }
        state.work_idle()?;
        let (attempt, _) = self.distribution_attempt(workspace)?;
        drop(state);
        let result = author_signing::enroll(&self.root, &attempt.cancel, open_browser);
        if let Err(error) = &result {
            if let Some(cleanup) = error
                .context
                .get("cleanup_error")
                .filter(|value| !value.is_null())
            {
                lock(&self.distribution).containment = Some(
                    Fault::new(
                        "PackageCleanup",
                        "Resolve retained signing-key cleanup before continuing",
                    )
                    .with_context(serde_json::json!({"cause": cleanup})),
                );
            }
        }
        result
    }
}

fn export_destination(path: &Path, source: &Path) -> Result<PathBuf, Fault> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("mmpkg"))
    {
        return Err(Fault::new(
            "PackageDestination",
            "Choose an absolute .mmpkg filename outside the source package",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(stale)?
        .canonicalize()
        .map_err(|_| {
            Fault::new(
                "PackageDestination",
                "The destination folder is unavailable",
            )
        })?;
    if parent.starts_with(source) {
        return Err(Fault::new(
            "PackageDestination",
            "Export destination must be outside the source package",
        ));
    }
    Ok(parent.join(path.file_name().ok_or_else(stale)?))
}
