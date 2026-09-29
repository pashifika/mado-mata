use crate::model::Fault;
use serde_json::json;
use std::io;
use std::process::{Command, ExitStatus};

#[cfg(all(test, windows))]
mod tests;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
type Process = windows::Process;
#[cfg(not(windows))]
type Process = std::process::Child;
#[cfg(windows)]
type ChildInput = std::io::PipeWriter;
#[cfg(not(windows))]
type ChildInput = std::process::ChildStdin;
#[cfg(windows)]
type ChildOutput = std::io::PipeReader;
#[cfg(not(windows))]
type ChildOutput = std::process::ChildStdout;
#[cfg(windows)]
type ChildError = std::io::PipeReader;
#[cfg(not(windows))]
type ChildError = std::process::ChildStderr;

#[derive(Clone, Copy)]
pub(crate) enum ChildStdio {
    Piped,
    /// The lifecycle probes use a control pipe and evidence pipe, without stderr.
    Protocol,
    Null,
}

#[derive(Clone, Copy)]
pub(crate) enum Environment {
    Inherited,
    Cleared,
}

/// Only application-owned executables use this boundary, never selected native targets.
/// Stdio and base environment are explicit: Command's private platform options are not read.
pub(crate) struct OwnedChild {
    process: Process,
    pub(crate) stdin: Option<ChildInput>,
    pub(crate) stdout: Option<ChildOutput>,
    pub(crate) stderr: Option<ChildError>,
}

impl OwnedChild {
    pub(crate) fn spawn(
        command: &mut Command,
        stdio: ChildStdio,
        environment: Environment,
    ) -> Result<Self, Fault> {
        #[cfg(windows)]
        {
            windows::spawn(command, stdio, environment)
        }
        #[cfg(not(windows))]
        {
            use std::process::Stdio;
            if matches!(environment, Environment::Cleared) {
                // Preserve explicit overrides while discarding the inherited environment.
                let changes: Vec<_> = command
                    .get_envs()
                    .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
                    .collect();
                command.env_clear();
                for (key, value) in changes {
                    if let Some(value) = value {
                        command.env(key, value);
                    }
                }
            }
            match stdio {
                ChildStdio::Piped => {
                    command
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped());
                }
                ChildStdio::Protocol => {
                    command
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null());
                }
                ChildStdio::Null => {
                    command
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null());
                }
            }
            let mut process = command
                .spawn()
                .map_err(|error| startup_fault(error, "spawn"))?;
            Ok(Self {
                stdin: process.stdin.take(),
                stdout: process.stdout.take(),
                stderr: process.stderr.take(),
                process,
            })
        }
    }

    pub(crate) fn id(&self) -> u32 {
        self.process.id()
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.process.try_wait()
    }

    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.process.wait()
    }

    pub(crate) fn kill(&mut self) -> io::Result<()> {
        self.process.kill()
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Windows Job ownership includes descendants even if the primary already exited.
        if cfg!(windows) || !matches!(self.try_wait(), Ok(Some(_))) {
            let _ = self.kill();
        }
        let _ = self.wait();
    }
}

fn startup_fault(error: io::Error, boundary: &str) -> Fault {
    Fault::new("ChildStartup", error.to_string()).with_context(json!({
        "stage":"child_startup", "boundary":boundary, "io_kind":format!("{:?}", error.kind()),
        "child_started":false, "cleanup":{"clean":true, "child_started":false}
    }))
}
