//! Windows commands enter a non-breakaway Job during CreateProcessW itself.
//! A handle allowlist carries only stdin/stdout/stderr; no lease or other session
//! handle is inherited. Closing the Job also kills its tree after an unwind.

use super::{Limits, Output, RunError, interrupted};
use mado_runtime_comparison::model::Fault;
use serde_json::json;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::marker::PhantomData;
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

const POLL: Duration = Duration::from_millis(10);
const PIPE_GRACE: Duration = Duration::from_secs(1);
const TERMINATE: Duration = Duration::from_secs(1);

fn io_fault(category: &str, message: &str, error: &io::Error) -> Fault {
    Fault::new(category, message).with_context(
        json!({"kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}),
    )
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid("a Windows process argument contains NUL"));
    }
    value.push(0);
    Ok(value)
}

/// CRT/Bun argv quoting, not cmd.exe syntax. Metacharacters have no shell to
/// interpret them; trailing backslashes and embedded quotes remain literal.
fn command_line(program: &Path, arguments: &[&OsStr]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    for argument in std::iter::once(program.as_os_str()).chain(arguments.iter().copied()) {
        if !line.is_empty() {
            line.push(u16::from(b' '));
        }
        line.push(u16::from(b'"'));
        let mut slashes = 0;
        for unit in argument.encode_wide() {
            if unit == 0 {
                return Err(invalid("a Windows process argument contains NUL"));
            }
            if unit == u16::from(b'\\') {
                slashes += 1;
                continue;
            }
            let quote = unit == u16::from(b'"');
            line.extend(std::iter::repeat_n(
                u16::from(b'\\'),
                slashes * if quote { 2 } else { 1 },
            ));
            if quote {
                line.push(u16::from(b'\\'));
            }
            line.push(unit);
            slashes = 0;
        }
        line.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2));
        line.push(u16::from(b'"'));
    }
    line.push(0);
    if line.len() > 32_767 {
        return Err(invalid("the Windows command line exceeds its bound"));
    }
    Ok(line)
}

fn environment_block(environment: &[(&str, OsString)]) -> io::Result<Vec<u16>> {
    let mut entries: Vec<_> = environment.iter().collect();
    entries.sort_unstable_by(|(left, _), (right, _)| {
        left.bytes()
            .map(|byte| byte.to_ascii_uppercase())
            .cmp(right.bytes().map(|byte| byte.to_ascii_uppercase()))
    });
    let mut block = Vec::new();
    let mut previous: Option<&str> = None;
    for (name, value) in entries {
        if name.is_empty()
            || name.contains(['=', '\0'])
            || previous.is_some_and(|previous| previous.eq_ignore_ascii_case(name))
        {
            return Err(invalid("invalid or duplicate Windows environment variable"));
        }
        previous = Some(name);
        block.extend(name.encode_utf16());
        block.push(u16::from(b'='));
        for unit in value.encode_wide() {
            if unit == 0 {
                return Err(invalid("a Windows environment value contains NUL"));
            }
            block.push(unit);
        }
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[expect(unsafe_code, reason = "audited ownership of anonymous pipe handles")]
fn pipe() -> io::Result<(File, OwnedHandle)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let (mut reader, mut writer) = (null_mut(), null_mut());
    // SAFETY: both output handles and the attributes are live for this call.
    if unsafe { CreatePipe(&raw mut reader, &raw mut writer, &attributes, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreatePipe transfers two distinct owned handles.
    let (reader, writer) = unsafe {
        (
            File::from_raw_handle(reader),
            OwnedHandle::from_raw_handle(writer),
        )
    };
    // SAFETY: reader owns a valid handle; it must not be inherited by the child.
    if unsafe { SetHandleInformation(reader.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((reader, writer))
}

struct Attributes<'a> {
    storage: Vec<usize>,
    values: PhantomData<&'a [HANDLE]>,
}

impl<'a> Attributes<'a> {
    #[expect(
        unsafe_code,
        reason = "audited process attribute storage and borrowed handle lists"
    )]
    fn new(handles: &'a [HANDLE], jobs: &'a [HANDLE]) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: a null buffer requests the required size of two attributes.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &raw mut bytes) };
        if bytes == 0 || bytes > 64 * 1024 {
            return Err(invalid("invalid process attribute size"));
        }
        let mut storage = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
        let pointer = storage.as_mut_ptr().cast();
        // SAFETY: pointer is aligned storage of at least bytes, which remains live
        // until DeleteProcThreadAttributeList; no Rust references alias it.
        if unsafe { InitializeProcThreadAttributeList(pointer, 2, 0, &raw mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = Self {
            storage,
            values: PhantomData,
        };
        for (kind, values) in [
            (PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles),
            (PROC_THREAD_ATTRIBUTE_JOB_LIST, jobs),
        ] {
            // SAFETY: the initialized list is writable and each borrowed value
            // slice outlives it. The API retains their addresses until deletion.
            if unsafe {
                UpdateProcThreadAttribute(
                    attributes.pointer(),
                    0,
                    kind as usize,
                    values.as_ptr().cast(),
                    size_of_val(values),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(attributes)
    }

    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for Attributes<'_> {
    #[expect(
        unsafe_code,
        reason = "audited release of initialized process attributes"
    )]
    fn drop(&mut self) {
        // SAFETY: this list was successfully initialized and its backing storage
        // and borrowed values are still alive; it is deleted exactly once.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

struct Job(OwnedHandle);

impl Job {
    #[expect(
        unsafe_code,
        reason = "audited creation and configuration of an unnamed Job"
    )]
    fn new() -> io::Result<Self> {
        // SAFETY: null attributes/name create a non-inherited, unnamed Job.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW transferred ownership of this valid handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // No BREAKAWAY_OK or SILENT_BREAKAWAY_OK: every descendant stays contained.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the owned Job and the fully initialized fixed-size structure
        // stay live for this synchronous call; the structure is not retained.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    #[expect(
        unsafe_code,
        reason = "audited query of the owned Job's active process count"
    )]
    fn empty(&self) -> io::Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: only this owned Job is queried into one initialized structure.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                size_of_val(&accounting) as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses == 0)
    }

    #[expect(
        unsafe_code,
        reason = "audited termination limited to this command's Job"
    )]
    fn terminate(&self) {
        // SAFETY: this handle owns only the Job created for this invocation.
        // Settlement below, not the return value, proves that all members ended.
        unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
    }
}

struct Tree {
    job: Job,
    leader: OwnedHandle,
}

impl Tree {
    #[expect(
        unsafe_code,
        reason = "audited atomic Job assignment in CreateProcessW"
    )]
    fn spawn(
        program: &Path,
        arguments: &[&OsStr],
        directory: &Path,
        environment: &[(&str, OsString)],
        stdout: &OwnedHandle,
        stderr: &OwnedHandle,
    ) -> io::Result<Self> {
        // Never let CreateProcessW infer a launcher or search a working directory.
        if !program.is_absolute()
            || !program
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Err(invalid("select a native OMP .exe by its absolute path"));
        }
        let application = wide(program.as_os_str())?;
        let mut command = command_line(program, arguments)?;
        let directory = wide(directory.as_os_str())?;
        let environment = environment_block(environment)?;
        let job = Job::new()?;
        // An already-closed stdin pipe gives EOF without a device or console.
        let (stdin, stdin_writer) = pipe()?;
        drop(stdin_writer);
        // SAFETY: stdin owns a valid pipe handle; the allowlist below is the only
        // set inherited by this command. Leases and Job handles stay private.
        if unsafe {
            SetHandleInformation(
                stdin.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let inherited = [
            stdin.as_raw_handle(),
            stdout.as_raw_handle(),
            stderr.as_raw_handle(),
        ];
        let jobs = [job.0.as_raw_handle()];
        let mut attributes = Attributes::new(&inherited, &jobs)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited[0];
        startup.StartupInfo.hStdOutput = inherited[1];
        startup.StartupInfo.hStdError = inherited[2];
        startup.lpAttributeList = attributes.pointer();
        let mut process = PROCESS_INFORMATION::default();
        // SAFETY: all strings are NUL terminated, the mutable command line and
        // output structure are exclusively borrowed, and attributes/handles stay
        // alive. JOB_LIST assigns containment before any user code can execute;
        // failure never falls back to uncontained creation. Windows 10+ required.
        if unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_ptr().cast(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &raw mut process,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful creation transfers the two unique handles. The
        // primary thread is already running in the Job; only process observation
        // needs a retained handle.
        let leader = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
        drop(unsafe { OwnedHandle::from_raw_handle(process.hThread) });
        Ok(Self { job, leader })
    }

    #[expect(
        unsafe_code,
        reason = "audited observation of the retained process handle"
    )]
    fn status(&self) -> io::Result<Option<i32>> {
        // SAFETY: the retained handle cannot identify a reused or unrelated PID.
        match unsafe { WaitForSingleObject(self.leader.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: code is writable storage, and the process has exited.
                if unsafe { GetExitCodeProcess(self.leader.as_raw_handle(), &raw mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(code.cast_signed()))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }

    fn settle(self) -> Result<i32, Fault> {
        self.job.terminate();
        let deadline = Instant::now() + TERMINATE;
        loop {
            match (self.status(), self.job.empty()) {
                (Ok(Some(code)), Ok(true)) => return Ok(code),
                (Err(_), _) | (_, Err(_)) => break,
                _ if Instant::now() >= deadline => break,
                _ => thread::sleep(POLL),
            }
        }
        Err(Fault::new(
            "PluginUnsettled",
            "Processes started by the OMP command could not be confirmed to have ended",
        ))
    }
}

#[expect(
    unsafe_code,
    reason = "audited nonblocking observation of an owned anonymous pipe"
)]
fn drain(
    stream: &mut File,
    bytes: &mut Vec<u8>,
    limit: usize,
    truncate: bool,
) -> Result<bool, Fault> {
    let mut buffer = [0_u8; 8192];
    // Each pass is finite even when a descendant continuously fills either pipe.
    for _ in 0..8 {
        let mut available = 0;
        // SAFETY: stream is the sole reader of this synchronous pipe. Peek never
        // consumes data; the subsequent read requests at most the available bytes.
        if unsafe {
            PeekNamedPipe(
                stream.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                &raw mut available,
                null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE.cast_signed()) {
                return Ok(true);
            }
            return Err(io_fault(
                "PluginProcess",
                "OMP output could not be read",
                &error,
            ));
        }
        if available == 0 {
            return Ok(false);
        }
        let count = stream
            .read(&mut buffer[..(available as usize).min(8192)])
            .map_err(|error| io_fault("PluginProcess", "OMP output could not be read", &error))?;
        if count == 0 {
            return Ok(true);
        }
        let remaining = limit.saturating_sub(bytes.len());
        if !truncate && count > remaining {
            return Err(Fault::new(
                "PluginOutputLimit",
                "OMP output exceeded its byte bound",
            ));
        }
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(false)
}

pub(in crate::plugin_management) fn run(
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
    let pipes = || -> io::Result<_> { Ok((pipe()?, pipe()?)) };
    let ((mut stdout_pipe, stdout_child), (mut stderr_pipe, stderr_child)) =
        pipes().map_err(|error| {
            RunError::NotStarted(io_fault(
                "PluginProcess",
                "OMP output could not be prepared",
                &error,
            ))
        })?;
    // Recheck after preparation, before any child can execute.
    if cancel.load(Ordering::Acquire) {
        return Err(RunError::NotStarted(interrupted()));
    }
    let tree = Tree::spawn(
        program,
        arguments,
        directory,
        environment,
        &stdout_child,
        &stderr_child,
    )
    .map_err(|error| {
        RunError::NotStarted(io_fault(
            "PluginSpawn",
            "The selected OMP executable could not be started",
            &error,
        ))
    })?;
    drop((stdout_child, stderr_child));
    let deadline = Instant::now() + limits.duration;
    let mut pipes_deadline = deadline;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
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
            match tree.status() {
                Ok(Some(_)) => {
                    exited = true;
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
        if exited && out_closed && err_closed {
            break Ok(());
        }
        if exited && Instant::now() >= pipes_deadline {
            break Err(Fault::new(
                "PluginProcess",
                "OMP output did not close after the command exited",
            ));
        }
        if Instant::now() >= deadline {
            break Err(Fault::new(
                "PluginTimeout",
                "The OMP command did not finish within its time bound",
            ));
        }
        thread::sleep(POLL);
    };
    let code = tree.settle().map_err(|mut fault| {
        fault.context["cause"] = json!(finished.as_ref().err().map(|cause| &cause.category));
        RunError::Unsettled(fault)
    })?;
    match finished {
        Ok(()) => Ok(Output {
            success: code == 0,
            code: Some(code),
            stdout,
            stderr,
        }),
        Err(fault) => Err(RunError::Started(fault)),
    }
}
