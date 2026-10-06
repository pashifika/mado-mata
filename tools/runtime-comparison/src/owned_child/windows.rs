use super::{ChildStdio, Environment, OwnedChild, startup_fault};
use crate::model::Fault;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, PipeReader, PipeWriter};
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitStatus};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, HANDLE_FLAG_INHERIT,
    SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Globalization::{
    CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

const COMMAND_UNITS: usize = 32_767;
const ENVIRONMENT_UNITS: usize = 1_048_576;
const ENVIRONMENT_ENTRIES: usize = 4096;

pub(super) struct Process {
    handle: OwnedHandle,
    job: Job,
    id: u32,
    status: Option<ExitStatus>,
}

impl Process {
    pub(super) fn id(&self) -> u32 {
        self.id
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.observe(0)
    }

    pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
        self.observe(INFINITE)?
            .ok_or_else(|| io::Error::other("Process wait returned without settlement"))
    }

    pub(super) fn kill(&mut self) -> io::Result<()> {
        self.job.terminate()
    }

    #[expect(
        unsafe_code,
        reason = "waiting on the retained application-owned process handle"
    )]
    fn observe(&mut self, timeout: u32) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        // SAFETY: The owned process handle stays live; no PID lookup or target handle is used.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), timeout) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: The handle has signaled, and code is writable. Exit code 259 is not liveness.
                if unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                let status = ExitStatus::from_raw(code);
                self.status = Some(status);
                Ok(Some(status))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

struct Job(OwnedHandle);

impl Job {
    #[expect(
        unsafe_code,
        reason = "owning a non-inheritable anonymous kill-on-close Job"
    )]
    fn new() -> io::Result<Self> {
        // SAFETY: Null name/security creates a private, non-inheritable handle.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW transferred sole handle ownership.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: Correctly sized initialized limits and the live Job outlive the call.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    #[expect(
        unsafe_code,
        reason = "terminating only atomically assigned application-owned descendants"
    )]
    fn terminate(&self) -> io::Result<()> {
        // SAFETY: The Job receives only our created process, never a selected native target.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// The attribute values remain borrowed and stable until DeleteProcThreadAttributeList.
struct Attributes<'a> {
    storage: Vec<u128>,
    values: PhantomData<&'a [HANDLE]>,
}

impl<'a> Attributes<'a> {
    #[expect(
        unsafe_code,
        reason = "audited aligned STARTUPINFOEX attribute list with borrowed values"
    )]
    fn new(jobs: &'a [HANDLE], handles: &'a [HANDLE]) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: The documented sizing call accepts a null list and writable size.
        let sized =
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut bytes) };
        let error = io::Error::last_os_error();
        if sized != 0
            || error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || bytes == 0
            || bytes > 65_536
        {
            return Err(io::Error::other(format!(
                "Process attribute sizing failed: {error}"
            )));
        }
        // u128 supplies at least Windows' 16-byte allocation alignment.
        let mut storage = vec![0_u128; bytes.div_ceil(std::mem::size_of::<u128>())];
        // SAFETY: The aligned allocation covers the OS-reported byte count and remains stable.
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut list = Self {
            storage,
            values: PhantomData,
        };
        for (attribute, value) in [
            (PROC_THREAD_ATTRIBUTE_JOB_LIST, jobs),
            (PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles),
        ] {
            // SAFETY: Both slices are borrowed for the list lifetime, and contain live real handles.
            if unsafe {
                UpdateProcThreadAttribute(
                    list.pointer(),
                    0,
                    attribute as usize,
                    value.as_ptr().cast(),
                    std::mem::size_of_val(value),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(list)
    }

    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for Attributes<'_> {
    #[expect(
        unsafe_code,
        reason = "deleting an initialized attribute list before its allocation and values"
    )]
    fn drop(&mut self) {
        // SAFETY: Constructed only after initialization; storage and borrowed values remain live.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

#[expect(
    unsafe_code,
    reason = "inheriting only duplicated child stdio handles, not parent pipes or Job"
)]
fn inheritable(handle: &OwnedHandle) -> io::Result<OwnedHandle> {
    let mut duplicate = std::ptr::null_mut();
    // SAFETY: The source is live; GetCurrentProcess pseudo handles are used only by DuplicateHandle.
    let process = unsafe { GetCurrentProcess() };
    if unsafe {
        DuplicateHandle(
            process,
            handle.as_raw_handle(),
            process,
            &mut duplicate,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle succeeded and transferred one real handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}

#[expect(
    unsafe_code,
    reason = "ensuring application-side pipe handles cannot escape into children"
)]
fn non_inheritable(handle: &impl AsRawHandle) -> io::Result<()> {
    // SAFETY: The caller owns this live handle; only its inheritance bit is changed.
    if unsafe { SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn input(piped: bool) -> io::Result<(OwnedHandle, Option<PipeWriter>)> {
    if piped {
        let (reader, writer) = io::pipe()?;
        non_inheritable(&writer)?;
        Ok((inheritable(&reader.into())?, Some(writer)))
    } else {
        let null = std::fs::File::options().read(true).open(r"\\.\NUL")?;
        Ok((inheritable(&null.into())?, None))
    }
}

fn output(piped: bool) -> io::Result<(OwnedHandle, Option<PipeReader>)> {
    if piped {
        let (reader, writer) = io::pipe()?;
        non_inheritable(&reader)?;
        Ok((inheritable(&writer.into())?, Some(reader)))
    } else {
        let null = std::fs::File::options().write(true).open(r"\\.\NUL")?;
        Ok((inheritable(&null.into())?, None))
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let value: Vec<_> = value.encode_wide().take(COMMAND_UNITS + 1).collect();
    if value.len() > COMMAND_UNITS || value.contains(&0) {
        return Err(invalid(
            "Process parameter contains NUL or exceeds the Windows string bound",
        ));
    }
    Ok(value)
}

fn wide_z(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value = wide(value)?;
    if value.len() == COMMAND_UNITS {
        return Err(invalid(
            "Process parameter has no room for its NUL terminator",
        ));
    }
    value.push(0);
    Ok(value)
}

fn quoted_length(value: &OsStr) -> io::Result<usize> {
    let mut size = 2_usize;
    let mut slashes = 0;
    for unit in value.encode_wide() {
        if unit == 0 {
            return Err(invalid("Process argument contains NUL"));
        }
        size += 1;
        if unit == b'\\' as u16 {
            slashes += 1;
        } else {
            if unit == b'"' as u16 {
                size += slashes + 1;
            }
            slashes = 0;
        }
        if size >= COMMAND_UNITS {
            return Err(invalid("Process command line exceeds the Windows bound"));
        }
    }
    size += slashes;
    Ok(size)
}

fn quote(value: &OsStr, output: &mut Vec<u16>) {
    output.push(b'"' as u16);
    let mut slashes = 0;
    for unit in value.encode_wide() {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        let count = if unit == b'"' as u16 {
            slashes * 2 + 1
        } else {
            slashes
        };
        output.extend(std::iter::repeat_n(b'\\' as u16, count));
        output.push(unit);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
}

fn command_line(command: &Command) -> io::Result<Vec<u16>> {
    let mut length = quoted_length(command.get_program())? + 1;
    for argument in command.get_args() {
        length = length
            .checked_add(1 + quoted_length(argument)?)
            .filter(|length| *length <= COMMAND_UNITS)
            .ok_or_else(|| invalid("Process command line exceeds the Windows bound"))?;
    }
    if length > COMMAND_UNITS {
        return Err(invalid("Process command line exceeds the Windows bound"));
    }
    let mut output = Vec::with_capacity(length);
    quote(command.get_program(), &mut output);
    for argument in command.get_args() {
        output.push(b' ' as u16);
        quote(argument, &mut output);
    }
    output.push(0);
    Ok(output)
}

struct EnvKey(Vec<u16>);

impl PartialEq for EnvKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for EnvKey {}

impl Ord for EnvKey {
    #[expect(
        unsafe_code,
        reason = "Windows ordinal case-insensitive environment key ordering"
    )]
    fn cmp(&self, other: &Self) -> Ordering {
        // SAFETY: Nonempty, NUL-free UTF-16 buffers are live and bounded below i32::MAX.
        match unsafe {
            CompareStringOrdinal(
                self.0.as_ptr(),
                self.0.len() as i32,
                other.0.as_ptr(),
                other.0.len() as i32,
                1,
            )
        } {
            CSTR_LESS_THAN => Ordering::Less,
            CSTR_EQUAL => Ordering::Equal,
            CSTR_GREATER_THAN => Ordering::Greater,
            _ => unreachable!("valid CompareStringOrdinal arguments"),
        }
    }
}

impl PartialOrd for EnvKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn environment(command: &Command, mode: Environment) -> io::Result<Option<Vec<u16>>> {
    if matches!(mode, Environment::Inherited) && command.get_envs().len() == 0 {
        return Ok(None);
    }
    let mut values = BTreeMap::<EnvKey, Vec<u16>>::new();
    let mut units = 1;
    let mut update = |name: &OsStr, value: Option<&OsStr>, inherited: bool| -> io::Result<()> {
        let name = wide(name)?;
        if name.is_empty()
            || name
                .iter()
                .enumerate()
                .any(|(index, unit)| *unit == b'=' as u16 && !(inherited && index == 0))
        {
            return Err(invalid("Invalid process environment name"));
        }
        let key = EnvKey(name);
        if let Some((old_key, old_value)) = values.remove_entry(&key) {
            units -= old_key.0.len() + old_value.len() + 2;
        }
        if let Some(value) = value {
            let value = wide(value)?;
            units += key.0.len() + value.len() + 2;
            if units > ENVIRONMENT_UNITS || values.len() >= ENVIRONMENT_ENTRIES {
                return Err(invalid(
                    "Process environment exceeds its bounded startup allowance",
                ));
            }
            values.insert(key, value);
        }
        Ok(())
    };
    if matches!(mode, Environment::Inherited) {
        for (name, value) in std::env::vars_os() {
            update(&name, Some(&value), true)?;
        }
    }
    for (name, value) in command.get_envs() {
        update(name, value, false)?;
    }
    let mut block = Vec::with_capacity(units.max(2));
    for (name, value) in values {
        block.extend(name.0);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(Some(block))
}

#[expect(
    unsafe_code,
    reason = "atomic CreateProcessW Job assignment and explicit stdio handle inheritance"
)]
pub(super) fn spawn(
    command: &Command,
    stdio: ChildStdio,
    mode: Environment,
) -> Result<OwnedChild, Fault> {
    let parameters = |error| startup_fault(error, "parameters");
    let program = Path::new(command.get_program());
    let application = wide_z(command.get_program()).map_err(parameters)?;
    if !program.is_absolute()
        || application.contains(&(b'"' as u16))
        || !program
            .extension()
            .and_then(|part| part.to_str())
            .is_some_and(|part| part.eq_ignore_ascii_case("exe"))
    {
        return Err(parameters(invalid(
            "Owned Windows child requires an absolute .exe path",
        )));
    }
    let mut arguments = command_line(command).map_err(parameters)?;
    let environment = environment(command, mode).map_err(parameters)?;
    let directory = command
        .get_current_dir()
        .map(|directory| {
            wide(directory.as_os_str())?;
            wide_z(std::path::absolute(directory)?.as_os_str())
        })
        .transpose()
        .map_err(parameters)?;
    let piped = !matches!(stdio, ChildStdio::Null);
    let (child_input, stdin) = input(piped).map_err(|error| startup_fault(error, "stdio"))?;
    let (child_output, stdout) = output(piped).map_err(|error| startup_fault(error, "stdio"))?;
    let (child_error, stderr) = output(matches!(stdio, ChildStdio::Piped))
        .map_err(|error| startup_fault(error, "stdio"))?;
    let job = Job::new().map_err(|error| startup_fault(error, "job_setup"))?;
    let jobs = [job.0.as_raw_handle()];
    let handles = [
        child_input.as_raw_handle(),
        child_output.as_raw_handle(),
        child_error.as_raw_handle(),
    ];
    let mut attributes =
        Attributes::new(&jobs, &handles).map_err(|error| startup_fault(error, "attributes"))?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.pointer();
    let mut information = PROCESS_INFORMATION::default();
    // SAFETY: All strings/buffers/attribute values outlive this call. Only the three stdio handles
    // are inheritable. JOB_LIST attaches before execution: parent death cannot strand a child
    // between creation and assignment. Existing-parent-job refusal is returned, never bypassed.
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            arguments.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            environment
                .as_ref()
                .map_or(std::ptr::null(), |block| block.as_ptr().cast()),
            directory
                .as_ref()
                .map_or(std::ptr::null(), |path| path.as_ptr()),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(startup_fault(io::Error::last_os_error(), "spawn"));
    }
    // SAFETY: CreateProcessW succeeded and transferred two real, non-inheritable handles.
    let handle = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    drop(thread);
    drop(attributes);
    Ok(OwnedChild {
        process: Process {
            handle,
            job,
            id: information.dwProcessId,
            status: None,
        },
        stdin,
        stdout,
        stderr,
    })
}
