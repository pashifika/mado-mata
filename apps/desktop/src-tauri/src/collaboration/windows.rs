//! Windows-only private named pipes and handle-pinned endpoint publication.
//!
//! Every operation uses a distinct OVERLAPPED/event. Cancellation is sticky and
//! checked by every operation, including ones started after shutdown. Buffers and
//! OVERLAPPED storage remain alive until the kernel completes cancellation.
#![expect(
    unsafe_code,
    reason = "audited Windows handle, security and overlapped I/O boundary"
)]

use super::{
    FRAME_BYTES, MAX_CLIENTS, MAX_PENDING, OUTBOX_FRAMES, PAGE_UNITS, PENDING_BYTES, PROTOCOL, lock,
};
use mado_runtime_comparison::model::Fault;
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;

pub(super) const DIRECTORY: &str = "collaboration";
pub(super) const PIPE_PREFIX: &str = r"\\.\pipe\mado-mata-collaboration-";
const DESCRIPTOR_BYTES: usize = 4096;

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "The collaboration endpoint is not private, local and stable",
    )
}
fn fault(error: io::Error) -> Fault {
    Fault::new(
        "CollaborationUnavailable",
        format!("Could not create the private collaboration endpoint: {error}"),
    )
}
fn wide(value: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(refused());
    }
    value.push(0);
    Ok(value)
}
fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: All callers pass a newly created, exclusively owned kernel handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
fn event() -> io::Result<OwnedHandle> {
    // SAFETY: No name, no inherited handle, no borrowed pointer retained.
    owned(unsafe { CreateEventW(null(), 1, 0, null()) })
}
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: Security APIs return LocalAlloc-owned storage; freed exactly once.
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    // SAFETY: The caller keeps the validated token/security descriptor SID alive.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocation = Local(text.cast());
    let mut length = 0;
    // SAFETY: Successful conversion returns a NUL-terminated UTF-16 SID string.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        drop(allocation);
        Ok(result)
    }
}
fn process_user(process: HANDLE) -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: process is borrowed live; token is an initialized output location.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut size = 0;
    // SAFETY: Sizing query intentionally supplies a null zero-length buffer.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            null_mut(),
            0,
            &raw mut size,
        );
    }
    if size == 0 || size > 65536 {
        return Err(refused());
    }
    // usize elements provide alignment for TOKEN_USER and its embedded SID.
    let mut bytes = vec![0_usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: Aligned, writable buffer has at least size bytes and remains live.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            bytes.as_mut_ptr().cast(),
            size,
            &raw mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: TokenUser succeeded and initializes a TOKEN_USER at the buffer start.
    sid_text(unsafe { (*bytes.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}
fn current_user() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns the infallible borrowed pseudo-handle.
    process_user(unsafe { GetCurrentProcess() })
}
struct Security(Local);
impl Security {
    fn new(user: &str) -> io::Result<Self> {
        let text = wide(std::ffi::OsStr::new(&format!(
            "O:{user}D:P(A;;FA;;;{user})"
        )))?;
        let mut descriptor = null_mut();
        // SAFETY: The terminated SDDL is live; the returned allocation is owned.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &raw mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(Local(descriptor)))
    }
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: 0,
        }
    }
}

/// Private entries accept only the exact current-user allow ACE and a protected
/// DACL. The configured root may retain normal SYSTEM/Administrators access and
/// read-only users; it is inspected, never chmod'ed or ACL-rewritten.
fn check_acl(handle: HANDLE, user: &str, private: bool) -> io::Result<()> {
    let mut owner = null_mut();
    let mut acl = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: Borrowed live handle and writable outputs; the descriptor owns SID/ACL.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            &raw mut acl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _allocation = Local(descriptor);
    if owner.is_null() || acl.is_null() || sid_text(owner)? != user {
        return Err(refused());
    }
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: Returned descriptor is live for this query.
    if unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) } == 0
        || (private && control & SE_DACL_PROTECTED == 0)
    {
        return Err(refused());
    }
    // SAFETY: GetSecurityInfo returned a valid ACL in its owned descriptor.
    for index in 0..unsafe { (*acl).AceCount } {
        let mut ace = null_mut();
        // SAFETY: Index is bounded by AceCount; output points into the live ACL.
        if unsafe { GetAce(acl, u32::from(index), &raw mut ace) } == 0 {
            return Err(refused());
        }
        // SAFETY: A valid ACE begins with ACE_HEADER. Only ordinary allow ACEs are
        // interpreted as ACCESS_ALLOWED_ACE; deny ACEs cannot grant authority.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if !private && u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        if header.AceType == 1 && !private {
            continue;
        }
        if header.AceType != 0 {
            return Err(refused());
        }
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let trustee = sid_text((&raw const allowed.SidStart).cast_mut().cast())?;
        if trustee == user {
            continue;
        }
        if private {
            return Err(refused());
        }
        if trustee == "S-1-5-18" || trustee == "S-1-5-32-544" {
            continue;
        }
        let mutation = FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_EA
            | FILE_WRITE_ATTRIBUTES
            | FILE_DELETE_CHILD
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER
            | GENERIC_WRITE
            | GENERIC_ALL;
        if allowed.Mask & mutation != 0 {
            return Err(refused());
        }
    }
    Ok(())
}

fn open_file(
    path: &Path,
    access: u32,
    share: u32,
    creation: u32,
    security: Option<&Security>,
) -> io::Result<File> {
    let path = wide(path.as_os_str())?;
    let attributes = security.map(Security::attributes);
    // SAFETY: Path/optional descriptor are live; no handle inheritance; returned handle is owned.
    let handle = owned(unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            share,
            attributes.as_ref().map_or(null(), std::ptr::from_ref),
            creation,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    })?;
    Ok(File::from(handle))
}
fn information(file: &File, directory: bool) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: Live file handle and correctly sized writable structure.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(refused());
    }
    Ok(info)
}
fn pin_root(root: &Path, user: &str) -> io::Result<Vec<File>> {
    if !root.is_absolute() {
        return Err(refused());
    }
    let mut path = PathBuf::new();
    let mut pins = Vec::new();
    for component in root.components() {
        match component {
            Component::Prefix(prefix)
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
            {
                path.push(component)
            }
            Component::RootDir => {
                path.push(component);
                let drive = wide(path.as_os_str())?;
                // SAFETY: The terminated drive root is live; reject mapped remote drives too.
                if unsafe { GetDriveTypeW(drive.as_ptr()) } == DRIVE_REMOTE {
                    return Err(refused());
                }
            }
            Component::Normal(name)
                if !name
                    .encode_wide()
                    .any(|unit| unit == 0 || unit == u16::from(b':')) =>
            {
                path.push(name);
                let file = open_file(
                    &path,
                    READ_CONTROL | FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    OPEN_EXISTING,
                    None,
                )?;
                information(&file, true)?;
                pins.push(file);
            }
            _ => return Err(refused()),
        }
    }
    let root = pins.last().ok_or_else(refused)?;
    check_acl(root.as_raw_handle(), user, false)?;
    Ok(pins)
}

pub(super) struct Endpoint {
    descriptor: Option<File>,
    // Deny delete sharing on every directory component throughout publication.
    _pins: Vec<File>,
}
impl Endpoint {
    pub(super) fn remove(&mut self) {
        if let Some(file) = self.descriptor.take() {
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            // SAFETY: Delete by our retained creation handle, never by a re-resolved path.
            unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle(),
                    FileDispositionInfo,
                    (&raw const info).cast(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                );
            }
        }
        self._pins.clear();
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.remove();
    }
}

pub(super) fn create(
    root: &Path,
    instance: &str,
    token: &str,
) -> Result<(Endpoint, Listener), Fault> {
    (|| {
        let user = current_user()?;
        let security = Security::new(&user)?;
        let mut pins = pin_root(root, &user)?;
        let directory = root.join(DIRECTORY);
        let name = wide(directory.as_os_str())?;
        let attributes = security.attributes();
        // SAFETY: Path/security live for this atomic directory creation; no inheritance.
        if unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
                return Err(error);
            }
        }
        let directory_handle = open_file(
            &directory,
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            OPEN_EXISTING,
            None,
        )?;
        information(&directory_handle, true)?;
        check_acl(directory_handle.as_raw_handle(), &user, true)?;
        pins.push(directory_handle);
        let pipe = format!("{PIPE_PREFIX}{instance}");
        let first = Stream::server(&pipe, &user, true)?;
        let listener = Listener {
            name: pipe.clone(),
            user,
            next: Mutex::new(Some(first)),
        };
        let mut endpoint = Endpoint {
            descriptor: None,
            _pins: pins,
        };
        let bytes = serde_json::to_vec(&serde_json::json!({
            "protocol": PROTOCOL, "instance": instance, "socket": pipe, "token": token,
            "pid": std::process::id(), "limits": { "frameBytes": FRAME_BYTES,
                "clients": MAX_CLIENTS, "pending": MAX_PENDING, "pageUnits": PAGE_UNITS,
                "pendingBytes": PENDING_BYTES, "outboxFrames": OUTBOX_FRAMES }
        }))
        .map_err(io::Error::other)?;
        if bytes.len() > DESCRIPTOR_BYTES {
            return Err(refused());
        }
        let file = open_file(
            &directory.join(format!("{instance}.json")),
            GENERIC_READ | GENERIC_WRITE | DELETE | READ_CONTROL,
            FILE_SHARE_READ,
            CREATE_NEW,
            Some(&security),
        )?;
        // Retain before any fallible validation/write; partial publication is
        // removed by the exact creation handle, never by its pathname.
        endpoint.descriptor = Some(file);
        let file = endpoint.descriptor.as_mut().ok_or_else(refused)?;
        information(file, false)?;
        check_acl(file.as_raw_handle(), &listener.user, true)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok((endpoint, listener))
    })()
    .map_err(fault)
}

pub(super) fn random_hex<const N: usize>() -> Result<String, Fault> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes)
        .map_err(|_| fault(io::Error::other("System random source unavailable")))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(N * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(result)
}

#[derive(Clone)]
pub(super) struct Wake(Arc<OwnedHandle>);
impl Wake {
    pub(super) fn shutdown(&self, _how: Shutdown) -> io::Result<()> {
        // SAFETY: The owned manual-reset event is live and may be signalled concurrently.
        if unsafe { SetEvent(self.0.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
pub(super) fn wake_pair() -> io::Result<(Wake, Wake)> {
    let wake = Wake(Arc::new(event()?));
    Ok((wake.clone(), wake))
}

pub(super) struct Stream {
    handle: OwnedHandle,
    stop: OwnedHandle,
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
}
impl Stream {
    fn server(name: &str, user: &str, first: bool) -> io::Result<Self> {
        let security = Security::new(user)?;
        let attributes = security.attributes();
        let name = wide(std::ffi::OsStr::new(name))?;
        // SAFETY: Live terminated name and private security; kernel copies both.
        let handle = owned(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX
                    | FILE_FLAG_OVERLAPPED
                    | if first {
                        FILE_FLAG_FIRST_PIPE_INSTANCE
                    } else {
                        0
                    },
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                (MAX_CLIENTS + 2) as u32,
                65536,
                65536,
                0,
                &raw const attributes,
            )
        })?;
        Ok(Self {
            handle,
            stop: event()?,
            read_timeout: Mutex::new(None),
            write_timeout: Mutex::new(None),
        })
    }
    pub(super) fn set_nonblocking(&self, value: bool) -> io::Result<()> {
        if value {
            Err(io::ErrorKind::Unsupported.into())
        } else {
            Ok(())
        }
    }
    pub(super) fn set_read_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        *lock(&self.read_timeout) = value;
        Ok(())
    }
    pub(super) fn set_write_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        *lock(&self.write_timeout) = value;
        Ok(())
    }
    pub(super) fn shutdown(&self, _how: Shutdown) -> io::Result<()> {
        // SAFETY: Sticky event closes the start/cancel race. Pending operations own
        // their buffers and individually reap cancellation before returning.
        unsafe {
            SetEvent(self.stop.as_raw_handle());
            CancelIoEx(self.handle.as_raw_handle(), null());
        }
        Ok(())
    }
    fn operation(
        &self,
        cancel: HANDLE,
        timeout: Option<Duration>,
        start: impl FnOnce(*mut OVERLAPPED, *mut u32) -> io::Result<bool>,
    ) -> io::Result<usize> {
        let completed = event()?;
        let mut overlapped = OVERLAPPED {
            hEvent: completed.as_raw_handle(),
            ..OVERLAPPED::default()
        };
        // SAFETY: Borrowed live event; zero wait does not retain any pointer.
        if unsafe { WaitForSingleObject(cancel, 0) } == WAIT_OBJECT_0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let mut count = 0;
        // Immediate completion need not signal the event and has no pending
        // operation for GetOverlappedResult to observe.
        if start(&raw mut overlapped, &raw mut count)? {
            return Ok(count as usize);
        }
        let handles = [cancel, completed.as_raw_handle()];
        let millis = timeout.map_or(INFINITE, |time| {
            u32::try_from(time.as_millis().max(1)).unwrap_or(INFINITE - 1)
        });
        // SAFETY: Both events remain live through the wait and I/O completion.
        let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, millis) };
        if result != WAIT_OBJECT_0 + 1 {
            // SAFETY: This exact operation is still alive; cancel then reap before
            // the stack OVERLAPPED/event or any caller buffer can be released.
            unsafe {
                CancelIoEx(self.handle.as_raw_handle(), &raw const overlapped);
                GetOverlappedResult(
                    self.handle.as_raw_handle(),
                    &raw const overlapped,
                    &raw mut count,
                    1,
                );
            }
            return Err(if result == WAIT_TIMEOUT {
                io::ErrorKind::TimedOut
            } else {
                io::ErrorKind::BrokenPipe
            }
            .into());
        }
        // SAFETY: Completion was signalled; the live output stores transferred bytes.
        if unsafe {
            GetOverlappedResult(
                self.handle.as_raw_handle(),
                &raw const overlapped,
                &raw mut count,
                0,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(count as usize)
    }
}
fn completion(success: i32) -> io::Result<bool> {
    if success != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_IO_PENDING as i32) {
        Ok(false)
    } else {
        Err(error)
    }
}
impl Read for &Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let length = u32::try_from(bytes.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
        self.operation(
            self.stop.as_raw_handle(),
            *lock(&self.read_timeout),
            |overlapped, count| {
                // SAFETY: Both the buffer and count stay live until operation reaps I/O.
                completion(unsafe {
                    ReadFile(
                        self.handle.as_raw_handle(),
                        bytes.as_mut_ptr(),
                        length,
                        count,
                        overlapped,
                    )
                })
            },
        )
        .or_else(|error| {
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                Ok(0)
            } else {
                Err(error)
            }
        })
    }
}
impl Write for &Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u32::try_from(bytes.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
        self.operation(
            self.stop.as_raw_handle(),
            *lock(&self.write_timeout),
            |overlapped, count| {
                // SAFETY: Both the buffer and count stay live until operation reaps I/O.
                completion(unsafe {
                    WriteFile(
                        self.handle.as_raw_handle(),
                        bytes.as_ptr(),
                        length,
                        count,
                        overlapped,
                    )
                })
            },
        )
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    } // No FlushFileBuffers: it waits for an untrusted reader.
}

pub(super) struct Listener {
    name: String,
    user: String,
    next: Mutex<Option<Stream>>,
}
pub(super) fn accept(listener: &Listener, wake: &Wake) -> io::Result<Option<Stream>> {
    let mut next = lock(&listener.next);
    let stream = next
        .as_ref()
        .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
    let connected = stream.operation(wake.0.as_raw_handle(), None, |overlapped, _| {
        // SAFETY: The stream and operation remain alive through completion.
        let result = unsafe { ConnectNamedPipe(stream.handle.as_raw_handle(), overlapped) };
        if result == 0
            && io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32)
        {
            // A client connected before ConnectNamedPipe. Nothing was queued.
            Ok(true)
        } else {
            completion(result)
        }
    });
    if let Err(error) = connected {
        // SAFETY: Wake is live; shutdown takes precedence over pipe errors.
        if unsafe { WaitForSingleObject(wake.0.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
            return Ok(None);
        }
        // A disconnected pending client must not permanently poison admission.
        *next = Some(Stream::server(&listener.name, &listener.user, false)?);
        return Err(error);
    }
    // Keep a server handle alive while replacing the listener: no namespace gap.
    let successor = Stream::server(&listener.name, &listener.user, false)?;
    Ok(next.replace(successor))
}
pub(super) fn same_user(stream: &Stream) -> bool {
    let mut pid = 0;
    // SAFETY: Query the actual connected local pipe, not a claimed PID in JSON.
    if unsafe { GetNamedPipeClientProcessId(stream.handle.as_raw_handle(), &raw mut pid) } == 0 {
        return false;
    }
    // SAFETY: Query-only process open; owned handle pins the queried process lifetime.
    let Ok(process) = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
    else {
        return false;
    };
    matches!((process_user(process.as_raw_handle()), current_user()), (Ok(peer), Ok(user)) if peer == user)
}

#[cfg(test)]
pub(super) fn test_directory(path: &Path) -> io::Result<()> {
    let security = Security::new(&current_user()?)?;
    let attributes = security.attributes();
    let path = wide(path.as_os_str())?;
    // SAFETY: Atomic creation with this test user's protected descriptor.
    if unsafe { CreateDirectoryW(path.as_ptr(), &raw const attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
impl Stream {
    pub(super) fn connect(name: impl AsRef<Path>) -> io::Result<Self> {
        let name = wide(name.as_ref().as_os_str())?;
        // Bounded native connection admission, before any frame can be sent.
        // SAFETY: The terminated local pipe name is live for the finite wait.
        if unsafe { WaitNamedPipeW(name.as_ptr(), 5000) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Test-only real local pipe client; owned overlapped handle.
        let handle = owned(unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        })?;
        Ok(Self {
            handle,
            stop: event()?,
            read_timeout: Mutex::new(None),
            write_timeout: Mutex::new(None),
        })
    }
}

#[cfg(test)]
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        (&*self).read(bytes)
    }
}

#[cfg(test)]
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        (&*self).write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[test]
fn buffered_pipe_io_completes_without_a_pending_event() {
    let user = current_user().unwrap();
    let name = format!("{PIPE_PREFIX}{}", random_hex::<16>().unwrap());
    let server = Stream::server(&name, &user, true).unwrap();
    let pipe_name = wide(std::ffi::OsStr::new(&name)).unwrap();
    // SAFETY: Connect before the server starts accept, exercising the real
    // ERROR_PIPE_CONNECTED race. The name and returned owned handle stay live.
    let handle = owned(unsafe {
        CreateFileW(
            pipe_name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            null_mut(),
        )
    })
    .unwrap();
    let mut client = Stream {
        handle,
        stop: event().unwrap(),
        read_timeout: Mutex::new(Some(Duration::from_secs(2))),
        write_timeout: Mutex::new(Some(Duration::from_secs(2))),
    };
    const REQUEST: &[u8] = b"buffered request";
    client.write_all(REQUEST).unwrap();
    let listener = Listener {
        name,
        user,
        next: Mutex::new(Some(server)),
    };
    let (wake, _) = wake_pair().unwrap();
    let mut connected = accept(&listener, &wake).unwrap().unwrap();
    connected
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    connected
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut request = [0; REQUEST.len()];
    connected.read_exact(&mut request).unwrap();
    assert_eq!(request, REQUEST);
    connected.write_all(b"reply").unwrap();
    let mut reply = [0; 5];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"reply");
}
