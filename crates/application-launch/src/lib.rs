//! Single-submission application launch without automation or target authority.
//!
//! Preparing a request is inert. The caller must perform its own approval and
//! cancellation admission immediately before consuming `PreparedLaunch::submit`.
//! Submission waits for physical request settlement, not for the application to
//! exit. Acceptance never identifies the game process or establishes readiness.
use std::path::PathBuf;

mod executable;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug)]
pub enum Recipient {
    /// A direct process, without a shell. Omitted cwd uses the executable parent.
    /// Windows batch recipients are unsupported because they require an implicit shell.
    Executable {
        path: PathBuf,
        working_directory: Option<PathBuf>,
    },
    /// The outer application bundle; cwd is OS-defined and cannot be overridden.
    MacOSBundle { path: PathBuf },
}

#[derive(Debug)]
pub struct LaunchRequest {
    pub recipient: Recipient,
    /// Exact ordered elements, including empty arguments; never shell-expanded.
    pub arguments: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchDisposition {
    NotRequested,
    Accepted,
    Rejected,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchErrorKind {
    InvalidRequest,
    UnsupportedPlatform,
    Preparation,
    Rejected,
    Uncertain,
}

/// Bounded diagnostics never contain a path, argument, or platform error string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchError {
    pub kind: LaunchErrorKind,
    pub disposition: LaunchDisposition,
    pub os_code: Option<i32>,
}

impl LaunchError {
    fn before_submission(kind: LaunchErrorKind) -> Self {
        Self {
            kind,
            disposition: LaunchDisposition::NotRequested,
            os_code: None,
        }
    }

    #[cfg(target_os = "macos")]
    fn settlement(disposition: LaunchDisposition) -> Self {
        Self {
            kind: if disposition == LaunchDisposition::Rejected {
                LaunchErrorKind::Rejected
            } else {
                LaunchErrorKind::Uncertain
            },
            disposition,
            os_code: None,
        }
    }
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self.kind {
            LaunchErrorKind::InvalidRequest => "The launch request is invalid",
            LaunchErrorKind::UnsupportedPlatform => {
                "The launch recipient is unsupported on this platform"
            }
            LaunchErrorKind::Preparation => "The launch request could not be prepared",
            LaunchErrorKind::Rejected => "The launch request was rejected",
            LaunchErrorKind::Uncertain => "The launch request did not establish acceptance",
        })
    }
}

impl std::error::Error for LaunchError {}

pub struct PreparedLaunch(PreparedRecipient);

enum PreparedRecipient {
    Executable(executable::PreparedExecutable),
    #[cfg(target_os = "macos")]
    Bundle(macos::PreparedBundle),
}

impl PreparedLaunch {
    /// Validates request shape and acquires request/reaping resources without launching.
    ///
    /// # Errors
    /// Refuses invalid paths/arguments, unsupported recipients, or unavailable
    /// platform/request-ownership resources. No launch has been submitted.
    pub fn prepare(request: LaunchRequest) -> Result<Self, LaunchError> {
        if request
            .arguments
            .iter()
            .any(|argument| argument.contains('\0'))
        {
            return Err(LaunchError::before_submission(
                LaunchErrorKind::InvalidRequest,
            ));
        }
        let recipient = match request.recipient {
            Recipient::Executable {
                path,
                working_directory,
            } => PreparedRecipient::Executable(executable::PreparedExecutable::prepare(
                path,
                working_directory,
                &request.arguments,
            )?),
            Recipient::MacOSBundle { path } => {
                #[cfg(target_os = "macos")]
                {
                    PreparedRecipient::Bundle(macos::PreparedBundle::prepare(
                        &path,
                        &request.arguments,
                    )?)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = path;
                    return Err(LaunchError::before_submission(
                        LaunchErrorKind::UnsupportedPlatform,
                    ));
                }
            }
        };
        Ok(Self(recipient))
    }

    /// Consumes the one request and retains callback ownership until settlement.
    ///
    /// There is deliberately no cancellation/deadline parameter: after caller
    /// admission, cancelling automation cannot undo an OS submission. The caller
    /// records this result before deciding whether any later work remains allowed.
    /// Direct children transfer to one non-terminating application-lifetime reaper;
    /// a long-lived launcher does not delay this return or hold an automation slot.
    ///
    /// # Errors
    /// Returns a rejected or uncertain submission with a separate disposition.
    /// Never retries, activates a bundle, or terminates an external application.
    pub fn submit(self) -> Result<LaunchDisposition, LaunchError> {
        match self.0 {
            PreparedRecipient::Executable(prepared) => prepared.submit(),
            #[cfg(target_os = "macos")]
            PreparedRecipient::Bundle(prepared) => prepared.submit(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_argument_refuses_before_any_submission() {
        let result = PreparedLaunch::prepare(LaunchRequest {
            recipient: Recipient::Executable {
                path: std::env::current_exe().unwrap(),
                working_directory: None,
            },
            arguments: vec!["not\0an argument".into()],
        });
        let error = result.err().expect("NUL cannot be a process argument");
        assert_eq!(error.kind, LaunchErrorKind::InvalidRequest);
        assert_eq!(error.disposition, LaunchDisposition::NotRequested);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn bundle_request_is_explicitly_unsupported_elsewhere() {
        let result = PreparedLaunch::prepare(LaunchRequest {
            recipient: Recipient::MacOSBundle {
                path: PathBuf::from("/Applications/Example.app"),
            },
            arguments: Vec::new(),
        });
        let error = result.err().expect("no bundle backend on this platform");
        assert_eq!(error.kind, LaunchErrorKind::UnsupportedPlatform);
        assert_eq!(error.disposition, LaunchDisposition::NotRequested);
    }
}
