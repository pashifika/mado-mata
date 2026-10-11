//! Fixed OMP client operations with finite duration and output. Each run owns a
//! new process group and ends it before returning.

use super::payload::NAME;
use mado_runtime_comparison::model::Fault;
use serde_json::json;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::io::{self, Read};
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
#[cfg(unix)]
use std::process::{Child, Command, ExitStatus, Stdio};
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
/// After the leader exits, its pipes must close within this bound.
#[cfg(unix)]
const PIPE_GRACE: Duration = Duration::from_secs(1);
/// After the owned process group is killed, it must be empty within this bound.
#[cfg(unix)]
const TERMINATE: Duration = Duration::from_secs(1);

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
    /// The command started and was ended early; its whole process group is gone
    /// and any effect is unknown.
    Started(Fault),
    /// The command started, but its process group could not be confirmed empty.
    /// Its effects may continue, so the installation stays owned.
    Unsettled(Fault),
}

impl RunError {
    pub(super) fn fault(self) -> Fault {
        match self {
            Self::NotStarted(fault) | Self::Started(fault) | Self::Unsettled(fault) => fault,
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

/// The command's own process group: the spawned leader and every descendant
/// that stays in it, such as the Bun process OMP starts. Only this group is
/// signaled, never an OMP session or the application's own group. The leader
/// stays unreaped until the group has been killed, so the ID naming the group
/// cannot be reused by an unrelated process before the signal.
#[cfg(unix)]
struct Group {
    leader: Child,
    id: libc::pid_t,
}

#[cfg(unix)]
impl Group {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let leader = command.process_group(0).spawn()?;
        // std derives the child ID from its pid_t, so the conversion is exact.
        let id = leader.id().cast_signed();
        Ok(Self { leader, id })
    }

    /// Whether the leader has exited; it stays unreaped either way.
    #[expect(
        unsafe_code,
        reason = "audited non-reaping exit query for the owned leader"
    )]
    fn exited(&self) -> io::Result<bool> {
        let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
        loop {
            // SAFETY: `info` is writable storage for one siginfo_t that waitid fills
            // synchronously and does not retain. WNOWAIT leaves the leader waitable,
            // so `Child` still owns reaping it.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.leader.id(),
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        // SAFETY: an all-zero siginfo_t is a valid value, and waitid only stores a
        // complete one. Under WNOHANG si_pid stays zero while the leader runs.
        Ok(unsafe { info.assume_init_ref().si_pid() } != 0)
    }

    #[expect(
        unsafe_code,
        reason = "audited signal limited to the owned process group"
    )]
    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        // SAFETY: killpg takes plain integers and touches no Rust memory; `id` is
        // the spawned leader's PID, which names the group it leads.
        if unsafe { libc::killpg(self.id, signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// No member remains; EPERM still reports a member this process cannot signal.
    fn empty(&self) -> bool {
        matches!(self.signal(0), Err(error) if error.raw_os_error() == Some(libc::ESRCH))
    }

    /// Ends every owned process and returns the leader's status. The group is
    /// killed only while the leader is confirmed unreaped; once it is reaped, only
    /// the null signal observes the group until no member remains.
    fn settle(mut self) -> Result<ExitStatus, Fault> {
        let unsettled = || {
            Fault::new(
                "PluginUnsettled",
                "Processes started by the OMP command could not be confirmed to have ended",
            )
        };
        if self.exited().is_err() {
            return Err(unsettled());
        }
        // A group left with only its exited leader may report ESRCH.
        let _ = self.signal(libc::SIGKILL);
        let deadline = Instant::now() + TERMINATE;
        let status = loop {
            match self.leader.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
                Ok(None) | Err(_) => return Err(unsettled()),
            }
        };
        while !self.empty() {
            if Instant::now() >= deadline {
                return Err(unsettled());
            }
            thread::sleep(POLL);
        }
        Ok(status)
    }
}

/// Owns both nonblocking output readers and the command's process group; none
/// outlives the finite command.
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
    let group = Group::spawn(&mut command).map_err(|error| {
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
    let mut exited = false;
    let finished = loop {
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
        if !exited {
            match group.exited() {
                Ok(true) => {
                    exited = true;
                    pipes_deadline = deadline.min(Instant::now() + PIPE_GRACE);
                }
                Ok(false) => {}
                Err(error) => {
                    break Err(io_fault(
                        "PluginProcess",
                        "The OMP command could not be observed",
                        &error,
                    ));
                }
            }
        }
        if exited && out_closed && err_closed {
            break Ok(());
        }
        let now = Instant::now();
        if exited && now >= pipes_deadline {
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
    // Every result, success included, ends what the command left in its group.
    let status = group.settle().map_err(|mut fault| {
        fault.context["cause"] = json!(finished.as_ref().err().map(|cause| &cause.category));
        RunError::Unsettled(fault)
    })?;
    match finished {
        Ok(()) => Ok(Output {
            success: status.success(),
            code: status.code(),
            stdout,
            stderr,
        }),
        Err(fault) => Err(RunError::Started(fault)),
    }
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
