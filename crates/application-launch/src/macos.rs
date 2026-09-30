use crate::{LaunchDisposition, LaunchError, LaunchErrorKind};
use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSError, NSString, NSURL};
use std::path::Path;
use std::sync::mpsc;

type Completion = RcBlock<dyn Fn(*mut NSRunningApplication, *mut NSError)>;

pub(super) struct PreparedBundle {
    workspace: Retained<NSWorkspace>,
    url: Retained<NSURL>,
    configuration: Retained<NSWorkspaceOpenConfiguration>,
    completion: Completion,
    receive: mpsc::Receiver<LaunchDisposition>,
}

impl PreparedBundle {
    pub(super) fn prepare(path: &Path, arguments: &[String]) -> Result<Self, LaunchError> {
        if !path.is_absolute()
            || !path.is_dir()
            || !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        {
            return Err(LaunchError::before_submission(
                LaunchErrorKind::InvalidRequest,
            ));
        }
        let path = path
            .to_str()
            .ok_or_else(|| LaunchError::before_submission(LaunchErrorKind::InvalidRequest))?;
        objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                let workspace = NSWorkspace::sharedWorkspace();
                let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true);
                let configuration = NSWorkspaceOpenConfiguration::configuration();
                let arguments: Vec<_> = arguments
                    .iter()
                    .map(|value| NSString::from_str(value))
                    .collect();
                configuration.setArguments(&NSArray::from_retained_slice(&arguments));
                configuration.setActivates(false);
                configuration.setPromptsUserIfNeeded(false);
                configuration.setAddsToRecentItems(false);
                configuration.setAllowsRunningApplicationSubstitution(false);
                configuration.setCreatesNewApplicationInstance(false);
                let (send, receive) = mpsc::channel();
                let completion = RcBlock::new(
                    move |application: *mut NSRunningApplication, error: *mut NSError| {
                        // A non-null receipt is not application identity or readiness.
                        let disposition =
                            callback_disposition(!application.is_null(), !error.is_null());
                        let _ = send.send(disposition);
                    },
                );
                Self {
                    workspace,
                    url,
                    configuration,
                    completion,
                    receive,
                }
            })
        }))
        .map_err(|_| LaunchError::before_submission(LaunchErrorKind::Preparation))
    }

    pub(super) fn submit(self) -> Result<LaunchDisposition, LaunchError> {
        let submitted = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                self.workspace
                    .openApplicationAtURL_configuration_completionHandler(
                        &self.url,
                        &self.configuration,
                        Some(&self.completion),
                    )
            });
        }));
        // If an exception follows submission, an OS-owned copy may still complete.
        // Dropping the local copy lets disconnection prove no callback remains owned.
        drop(self.completion);
        settle(self.receive, submitted.is_err())
    }
}

fn callback_disposition(application: bool, error: bool) -> LaunchDisposition {
    match (application, error) {
        (true, false) => LaunchDisposition::Accepted,
        (false, true) => LaunchDisposition::Rejected,
        _ => LaunchDisposition::Uncertain,
    }
}

fn settle(
    receive: mpsc::Receiver<LaunchDisposition>,
    exception: bool,
) -> Result<LaunchDisposition, LaunchError> {
    // No timeout: retain the physical request beyond the caller's visible deadline.
    // NSWorkspace invokes this completion once. Await its block release as well as
    // its receipt, so an in-flight callback cannot outlive the submitting operation.
    let disposition = receive
        .into_iter()
        .last()
        .unwrap_or(LaunchDisposition::Uncertain);
    if disposition == LaunchDisposition::Accepted {
        return Ok(disposition);
    }
    let disposition = if exception {
        LaunchDisposition::Uncertain
    } else {
        disposition
    };
    Err(LaunchError::settlement(disposition))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_uncontradicted_application_receipt_means_acceptance() {
        assert_eq!(
            callback_disposition(true, false),
            LaunchDisposition::Accepted
        );
        assert_eq!(
            callback_disposition(false, true),
            LaunchDisposition::Rejected
        );
        assert_eq!(
            callback_disposition(false, false),
            LaunchDisposition::Uncertain
        );
        assert_eq!(
            callback_disposition(true, true),
            LaunchDisposition::Uncertain
        );
    }

    #[test]
    fn exception_does_not_hide_a_late_accepted_callback() {
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || settle(receive, true));
        send.send(LaunchDisposition::Accepted).unwrap();
        drop(send);
        assert_eq!(worker.join().unwrap().unwrap(), LaunchDisposition::Accepted);
    }

    #[test]
    fn missing_callback_or_exception_cannot_be_reported_as_rejection() {
        let (send, receive) = mpsc::channel();
        drop(send);
        assert_eq!(
            settle(receive, false).unwrap_err().disposition,
            LaunchDisposition::Uncertain
        );
        let (send, receive) = mpsc::channel();
        send.send(LaunchDisposition::Rejected).unwrap();
        drop(send);
        assert_eq!(
            settle(receive, true).unwrap_err().disposition,
            LaunchDisposition::Uncertain
        );
    }
}
