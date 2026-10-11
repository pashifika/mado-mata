//! Fixed OMP client operations with finite duration and output.

use super::payload::NAME;
use mado_runtime_comparison::model::Fault;
use serde_json::json;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
#[cfg(unix)]
use std::thread;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
const POLL: Duration = Duration::from_millis(10);
/// After the child exits, its pipes must close within this bound.
#[cfg(unix)]
const PIPE_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub(super) duration: Duration,
    pub(super) stdout: usize,
    pub(super) stderr: usize,
}

const PROBE: Limits = Limits {
    duration: Duration::from_secs(15),
    stdout: 64 * 1024,
    stderr: 16 * 1024,
};
const INVENTORY: Limits = Limits {
    duration: Duration::from_secs(30),
    stdout: 1024 * 1024,
    stderr: 64 * 1024,
};
const MUTATION: Limits = Limits {
    duration: Duration::from_secs(120),
    stdout: 64 * 1024,
    stderr: 64 * 1024,
};

/// One run of the selected executable in the established user target.
pub(super) struct Invocation<'a> {
    pub(super) executable: &'a Path,
    pub(super) home: &'a Path,
    pub(super) environment: &'a [(&'static str, OsString)],
    pub(super) cancel: &'a AtomicBool,
}

/// The only client operations management performs. npm/link operations always
/// target the normal user registry; no scope or project argument is meaningful.
#[derive(Clone, Copy, Debug)]
pub(super) enum Operation<'a> {
    Version,
    Help,
    List,
    Link(&'a Path),
    Uninstall,
}

pub(super) struct Output {
    pub(super) success: bool,
    pub(super) code: Option<i32>,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

pub(super) enum RunError {
    /// Nothing ran.
    NotStarted(Fault),
    /// The child started and was settled early; any effect is unknown.
    Started(Fault),
}

impl RunError {
    pub(super) fn fault(self) -> Fault {
        match self {
            Self::NotStarted(fault) | Self::Started(fault) => fault,
        }
    }
}

pub(super) trait Client: Send + Sync {
    fn run(
        &self,
        invocation: &Invocation<'_>,
        operation: Operation<'_>,
    ) -> Result<Output, RunError>;
}

/// The real client: an explicit executable with an argument vector, never a shell.
pub(super) struct Cli;

impl Client for Cli {
    fn run(
        &self,
        invocation: &Invocation<'_>,
        operation: Operation<'_>,
    ) -> Result<Output, RunError> {
        let (arguments, limits): (&[&OsStr], Limits) = match operation {
            Operation::Version => (&["--version".as_ref()], PROBE),
            Operation::Help => (&["plugin".as_ref(), "--help".as_ref()], PROBE),
            Operation::List => (
                &["plugin".as_ref(), "list".as_ref(), "--json".as_ref()],
                INVENTORY,
            ),
            Operation::Link(source) => (
                &[
                    "plugin".as_ref(),
                    "link".as_ref(),
                    source.as_os_str(),
                    "--json".as_ref(),
                ],
                MUTATION,
            ),
            Operation::Uninstall => (
                &[
                    "plugin".as_ref(),
                    "uninstall".as_ref(),
                    NAME.as_ref(),
                    "--json".as_ref(),
                ],
                MUTATION,
            ),
        };
        run(
            invocation.executable,
            arguments,
            invocation.home,
            invocation.environment,
            limits,
            invocation.cancel,
        )
    }
}

#[cfg(unix)]
fn io_fault(category: &str, message: &str, error: &io::Error) -> Fault {
    Fault::new(category, message).with_context(
        json!({"kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}),
    )
}

pub(super) fn interrupted() -> Fault {
    Fault::new(
        "PluginInterrupted",
        "Application shutdown interrupted the OMP command",
    )
}

#[cfg(unix)]
fn output_pipe() -> io::Result<(UnixStream, Stdio)> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    Ok((reader, Stdio::from(OwnedFd::from(writer))))
}

// Each pass is finite even when a descendant writes continuously.
#[cfg(unix)]
fn drain(
    stream: &mut UnixStream,
    bytes: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<bool, Fault> {
    let mut buffer = [0_u8; 8192];
    for _ in 0..8 {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                let remaining = limit.saturating_sub(bytes.len());
                if !truncate && count > remaining {
                    return Err(Fault::new(
                        "PluginOutputLimit",
                        "OMP output exceeded its byte bound",
                    ));
                }
                bytes.extend_from_slice(&buffer[..count.min(remaining)]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(io_fault(
                    "PluginProcess",
                    "OMP output could not be read",
                    &error,
                ));
            }
        }
    }
    Ok(false)
}

/// Owns both nonblocking output readers; no reader outlives the finite command.
#[cfg(unix)]
pub(super) fn run(
    program: &Path,
    arguments: &[&OsStr],
    directory: &Path,
    environment: &[(&'static str, OsString)],
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Output, RunError> {
    if cancel.load(Ordering::Acquire) {
        return Err(RunError::NotStarted(interrupted()));
    }
    let pipes = || -> io::Result<_> { Ok((output_pipe()?, output_pipe()?)) };
    let ((mut stdout_pipe, stdout_child), (mut stderr_pipe, stderr_child)) =
        pipes().map_err(|error| {
            RunError::NotStarted(io_fault(
                "PluginProcess",
                "OMP output could not be prepared",
                &error,
            ))
        })?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(directory)
        .env_clear()
        .envs(environment.iter().map(|(name, value)| (*name, value)))
        .stdin(Stdio::null())
        .stdout(stdout_child)
        .stderr(stderr_child);
    let mut child = command.spawn().map_err(|error| {
        RunError::NotStarted(io_fault(
            "PluginSpawn",
            "The selected OMP executable could not be started",
            &error,
        ))
    })?;
    drop(command);
    let deadline = Instant::now() + limits.duration;
    let mut pipes_deadline = deadline;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = None;
    let result = loop {
        if cancel.load(Ordering::Acquire) {
            break Err(interrupted());
        }
        let output = drain(&mut stdout_pipe, &mut stdout, limits.stdout, false).and_then(|out| {
            drain(&mut stderr_pipe, &mut stderr, limits.stderr, true).map(|err| (out, err))
        });
        let (out_closed, err_closed) = match output {
            Ok(closed) => closed,
            Err(fault) => break Err(fault),
        };
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit);
                    pipes_deadline = deadline.min(Instant::now() + PIPE_GRACE);
                }
                Ok(None) => {}
                Err(error) => {
                    break Err(io_fault(
                        "PluginProcess",
                        "The OMP command could not be observed",
                        &error,
                    ));
                }
            }
        }
        if let (Some(exit), true, true) = (status, out_closed, err_closed) {
            break Ok(Output {
                success: exit.success(),
                code: exit.code(),
                stdout,
                stderr,
            });
        }
        let now = Instant::now();
        if status.is_some() && now >= pipes_deadline {
            break Err(Fault::new(
                "PluginProcess",
                "OMP output did not close after the command exited",
            ));
        }
        if now >= deadline {
            break Err(Fault::new(
                "PluginTimeout",
                "The OMP command did not finish within its time bound",
            ));
        }
        thread::sleep(POLL);
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result.map_err(RunError::Started)
}

#[cfg(not(unix))]
pub(super) fn run(
    _program: &Path,
    _arguments: &[&OsStr],
    _directory: &Path,
    _environment: &[(&'static str, OsString)],
    _limits: Limits,
    _cancel: &AtomicBool,
) -> Result<Output, RunError> {
    Err(RunError::NotStarted(Fault::new(
        "PluginPlatform",
        "Managed OMP installation requires the supported Unix transport",
    )))
}
