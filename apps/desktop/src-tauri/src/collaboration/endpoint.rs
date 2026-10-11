//! Private filesystem endpoint: descriptor, short socket directory and socket.

use super::{
    FRAME_BYTES, MAX_CLIENTS, MAX_PENDING, OUTBOX_FRAMES, PAGE_UNITS, PENDING_BYTES, PROTOCOL,
};
use mado_runtime_comparison::model::Fault;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

pub(super) const DIRECTORY: &str = "collaboration";
const DESCRIPTOR_BYTES: usize = 4096;
/// Below macOS's 104-byte `sun_path`, which includes the terminating NUL.
const SOCKET_PATH_BYTES: usize = 100;
const SOCKET_DIRECTORY_PREFIX: &str = "mm-";
const SOCKET_NAME: &str = "s";
const NAME_ATTEMPTS: usize = 8;

fn fault(message: impl Into<String>) -> Fault {
    Fault::new("CollaborationUnavailable", message)
}

fn io_fault(operation: &str, error: &io::Error) -> Fault {
    fault(format!("Could not {operation}: {error}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Limits {
    frame_bytes: usize,
    clients: usize,
    pending: usize,
    page_units: u32,
    pending_bytes: usize,
    outbox_frames: usize,
}

#[derive(Serialize)]
struct Descriptor<'a> {
    protocol: u32,
    instance: &'a str,
    socket: &'a str,
    token: &'a str,
    pid: u32,
    limits: Limits,
}

/// Paths this process created. Removal skips any entry whose identity changed.
pub(super) struct Endpoint {
    descriptor: Option<(PathBuf, Identity)>,
    socket: Option<(PathBuf, Identity)>,
    directory: Option<(PathBuf, Identity)>,
}

impl Endpoint {
    pub(super) fn remove(&mut self) {
        if let Some((path, identity)) = self.descriptor.take() {
            remove_owned(&path, identity, false);
        }
        if let Some((path, identity)) = self.socket.take() {
            remove_owned(&path, identity, false);
        }
        if let Some((path, identity)) = self.directory.take() {
            remove_owned(&path, identity, true);
        }
    }
}

fn remove_owned(path: &Path, identity: Identity, directory: bool) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if Identity::of(&metadata) != identity {
        return;
    }
    let _ = if directory {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
}

/// Binds the socket and then publishes `<root>/collaboration/<instance>.json`.
/// The configured root must already exist; it is never created here.
pub(super) fn create(
    root: &Path,
    instance: &str,
    token: &str,
) -> Result<(Endpoint, UnixListener), Fault> {
    let mut endpoint = Endpoint {
        descriptor: None,
        socket: None,
        directory: None,
    };
    match publish(&mut endpoint, effective_uid(), root, instance, token) {
        Ok(listener) => Ok((endpoint, listener)),
        Err(error) => {
            endpoint.remove();
            Err(error)
        }
    }
}

fn publish(
    endpoint: &mut Endpoint,
    uid: libc::uid_t,
    root: &Path,
    instance: &str,
    token: &str,
) -> Result<UnixListener, Fault> {
    // Others must not be able to substitute the collaboration directory.
    check_directory(root, uid, 0o022, "configured data root")?;
    let collaboration = root.join(DIRECTORY);
    match fs::DirBuilder::new().mode(0o700).create(&collaboration) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_fault("create the collaboration directory", &error)),
    }
    check_directory(&collaboration, uid, 0o077, "collaboration directory")?;

    let directory = socket_directory(uid)?;
    let socket = directory.0.join(SOCKET_NAME);
    endpoint.directory = Some(directory);
    let listener = UnixListener::bind(&socket)
        .map_err(|error| io_fault("bind the collaboration socket", &error))?;
    let metadata = fs::symlink_metadata(&socket)
        .map_err(|error| io_fault("inspect the collaboration socket", &error))?;
    if !metadata.file_type().is_socket() || metadata.uid() != uid {
        return Err(fault("The collaboration socket was substituted"));
    }
    endpoint.socket = Some((socket.clone(), Identity::of(&metadata)));
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .map_err(|error| io_fault("restrict the collaboration socket", &error))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| io_fault("configure the collaboration socket", &error))?;

    let socket = socket
        .to_str()
        .ok_or_else(|| fault("The socket path is not UTF-8"))?;
    let bytes = serde_json::to_vec(&Descriptor {
        protocol: PROTOCOL,
        instance,
        socket,
        token,
        pid: std::process::id(),
        limits: Limits {
            frame_bytes: FRAME_BYTES,
            clients: MAX_CLIENTS,
            pending: MAX_PENDING,
            page_units: PAGE_UNITS,
            pending_bytes: PENDING_BYTES,
            outbox_frames: OUTBOX_FRAMES,
        },
    })
    .map_err(|_| fault("Could not encode the collaboration descriptor"))?;
    if bytes.len() > DESCRIPTOR_BYTES {
        return Err(fault("The collaboration descriptor exceeds its bound"));
    }
    endpoint.descriptor = Some(write_descriptor(&collaboration, instance, &bytes, uid)?);
    Ok(listener)
}

fn check_directory(
    path: &Path,
    uid: libc::uid_t,
    forbidden: u32,
    name: &str,
) -> Result<fs::Metadata, Fault> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_fault(&format!("inspect the {name}"), &error))?;
    if !metadata.file_type().is_dir() {
        return Err(fault(format!("The {name} is not a real directory")));
    }
    if metadata.uid() != uid {
        return Err(fault(format!("The {name} belongs to another user")));
    }
    if metadata.mode() & forbidden != 0 {
        return Err(fault(format!("The {name} is not private")));
    }
    Ok(metadata)
}

/// A fresh `mm-<random>` directory under a canonical temporary parent, so the
/// socket path fits `sun_path` regardless of the configured root's length.
fn socket_directory(uid: libc::uid_t) -> Result<(PathBuf, Identity), Fault> {
    let mut parents = Vec::with_capacity(2);
    for candidate in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        if let Ok(parent) = candidate.canonicalize() {
            if !parents.contains(&parent) {
                parents.push(parent);
            }
        }
    }
    let suffix = SOCKET_DIRECTORY_PREFIX.len() + 16 + 1 + SOCKET_NAME.len();
    for parent in parents {
        if parent.to_str().is_none() || parent.as_os_str().len() + 1 + suffix > SOCKET_PATH_BYTES {
            continue;
        }
        // Another user may share the parent only if its sticky bit protects our entry.
        let Ok(metadata) = fs::metadata(&parent) else {
            continue;
        };
        if !metadata.is_dir()
            || (metadata.uid() != uid && metadata.uid() != 0)
            || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
        {
            continue;
        }
        for _ in 0..NAME_ATTEMPTS {
            let directory = parent.join(format!("{SOCKET_DIRECTORY_PREFIX}{}", random_hex::<8>()?));
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {
                    let checked =
                        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                            .map_err(|error| io_fault("restrict the socket directory", &error))
                            .and_then(|()| {
                                check_directory(&directory, uid, 0o077, "socket directory")
                            });
                    return match checked {
                        Ok(metadata) => Ok((directory, Identity::of(&metadata))),
                        Err(error) => {
                            let _ = fs::remove_dir(&directory);
                            Err(error)
                        }
                    };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => break,
            }
        }
    }
    Err(fault("No private short socket directory is available"))
}

fn write_descriptor(
    collaboration: &Path,
    instance: &str,
    bytes: &[u8],
    uid: libc::uid_t,
) -> Result<(PathBuf, Identity), Fault> {
    let staged = collaboration.join(format!("{instance}.tmp"));
    let target = collaboration.join(format!("{instance}.json"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .map_err(|error| io_fault("create the collaboration descriptor", &error))?;
    let written = (|| {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let metadata = file.metadata()?;
        fs::rename(&staged, &target)?;
        Ok::<_, io::Error>(metadata)
    })();
    let written = match written {
        Ok(metadata) => metadata,
        Err(error) => {
            remove_owned_file(&staged, &file);
            return Err(io_fault("publish the collaboration descriptor", &error));
        }
    };
    let identity = Identity::of(&written);
    let published = fs::symlink_metadata(&target)
        .map_err(|error| io_fault("inspect the collaboration descriptor", &error))?;
    if Identity::of(&published) != identity
        || !published.file_type().is_file()
        || published.uid() != uid
        || published.nlink() != 1
        || published.mode() & 0o777 != 0o600
    {
        return Err(fault("The collaboration descriptor was substituted"));
    }
    Ok((target, identity))
}

fn remove_owned_file(path: &Path, file: &File) {
    if let Ok(metadata) = file.metadata() {
        remove_owned(path, Identity::of(&metadata), false);
    }
}

/// Lowercase hex of `N` bytes from the system CSPRNG.
pub(super) fn random_hex<const N: usize>() -> Result<String, Fault> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut source = File::open("/dev/urandom")
        .map_err(|error| io_fault("open the system random source", &error))?;
    let metadata = source
        .metadata()
        .map_err(|error| io_fault("inspect the system random source", &error))?;
    if !metadata.file_type().is_char_device() {
        return Err(fault("The system random source is not a character device"));
    }
    let mut bytes = [0_u8; N];
    source
        .read_exact(&mut bytes)
        .map_err(|error| io_fault("read the system random source", &error))?;
    let mut text = String::with_capacity(N * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    Ok(text)
}

#[expect(unsafe_code, reason = "audited infallible effective user query")]
pub(super) fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no Rust memory.
    unsafe { libc::geteuid() }
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[expect(unsafe_code, reason = "audited peer credential query on a live socket")]
pub(super) fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `stream` keeps the descriptor open for the call; both outputs are
    // live writable locals and the kernel retains neither pointer.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if result == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "audited SO_PEERCRED query on a live socket")]
pub(super) fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let expected = std::mem::size_of::<libc::ucred>();
    let mut length = libc::socklen_t::try_from(expected)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `stream` keeps the descriptor open; `credentials` is a writable
    // ucred of exactly `length` bytes and the kernel retains no pointer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(length).ok() != Some(expected) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(credentials.uid)
}

/// Other Unix targets have no audited peer query: every connection is refused.
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "linux"
)))]
pub(super) fn peer_uid(_stream: &UnixStream) -> io::Result<libc::uid_t> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

pub(super) enum Readable {
    Listener,
    Wake,
}

/// Blocks until a connection is pending or `wake` is readable or hung up.
#[expect(
    unsafe_code,
    reason = "audited poll over two descriptors owned by the caller"
)]
pub(super) fn wait_readable(listener: &UnixListener, wake: &UnixStream) -> io::Result<Readable> {
    let mut descriptors = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `descriptors` is a live writable array of exactly two initialized
    // entries whose descriptors the caller keeps open; poll retains no pointer.
    let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(if descriptors[1].revents == 0 {
        Readable::Listener
    } else {
        Readable::Wake
    })
}
