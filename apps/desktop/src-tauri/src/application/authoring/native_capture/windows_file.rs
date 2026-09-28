//! Disk-file correspondence, not proof of an already-running process's mapped image.
//! New write/delete opens are denied while any clone lives. This does not revoke
//! pre-existing writable mappings or establish which bytes a process mapped before
//! selection. Cancellation/deadline checks bracket each synchronous filesystem call;
//! they cannot interrupt an OS/filesystem driver that has not returned.
use mado_runtime_comparison::model::Fault;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

#[derive(Clone)]
pub(super) struct WindowsExecutableGuard {
    retained: Arc<RetainedExecutable>,
}

struct RetainedExecutable {
    canonical_path: PathBuf,
    identity: String,
    #[cfg(windows)]
    proof: windows::Proof,
    // There is no constructible non-Windows evidence or fallback identity.
    #[cfg(not(windows))]
    _unsupported: std::convert::Infallible,
}

impl WindowsExecutableGuard {
    pub(super) fn open(
        selected_path: &Path,
        cancellation: &AtomicBool,
        deadline: Instant,
    ) -> Result<Self, Fault> {
        check_control(cancellation, deadline)?;
        #[cfg(windows)]
        {
            let retained = windows::open(selected_path, cancellation, deadline)?;
            Ok(Self {
                retained: Arc::new(retained),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = selected_path;
            Err(unsupported())
        }
    }

    pub(super) fn canonical_path(&self) -> &Path {
        &self.retained.canonical_path
    }

    pub(super) fn identity(&self) -> &str {
        &self.retained.identity
    }

    pub(super) fn validate(
        &self,
        selected_path: &Path,
        cancellation: &AtomicBool,
        deadline: Instant,
    ) -> Result<(), Fault> {
        check_control(cancellation, deadline)?;
        #[cfg(windows)]
        {
            self.retained.proof.validate(
                selected_path,
                self.canonical_path(),
                cancellation,
                deadline,
            )
        }
        #[cfg(not(windows))]
        {
            let _ = selected_path;
            Err(unsupported())
        }
    }
}

fn check_control(cancellation: &AtomicBool, deadline: Instant) -> Result<(), Fault> {
    if cancellation.load(Ordering::Acquire) {
        Err(Fault::new("Cancelled", "Native authoring was cancelled"))
    } else if Instant::now() >= deadline {
        Err(Fault::new(
            "NativeCaptureTimeout",
            "Native authoring deadline expired",
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn unsupported() -> Fault {
    Fault::new(
        "NativeCapturePlatform",
        "Windows executable file identity requires Windows",
    )
}

#[cfg(windows)]
mod windows {
    use super::{RetainedExecutable, check_control};
    use mado_runtime_comparison::model::Fault;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Component, Path, PathBuf, Prefix};
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_ID_INFO, FILE_SHARE_READ, FILE_TYPE_DISK, FILE_TYPE_UNKNOWN, FileIdInfo,
        GetFileInformationByHandleEx, GetFileType,
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct FileIdentity {
        volume: u64,
        file: [u8; 16],
    }

    struct PinnedFile {
        _file: File,
        identity: FileIdentity,
    }

    pub(super) struct Proof {
        // Root first, then descendants: never traverse an unexamined reparse point.
        // Directory handles prevent parent replacement from rebinding the worker path.
        files: Vec<PinnedFile>,
    }

    fn refused(changed: bool) -> Fault {
        if changed {
            Fault::new(
                "StaleNativeSelection",
                "The selected Windows executable or its path changed; discover again",
            )
        } else {
            Fault::new(
                "NativeExecutable",
                "Select a stable absolute regular .exe without symbolic links or reparse points",
            )
        }
    }

    fn io_fault(error: io::Error, changed: bool) -> Fault {
        let status = error.raw_os_error();
        let denied = error.kind() == io::ErrorKind::PermissionDenied
            || status.is_some_and(|status| {
                status == ERROR_SHARING_VIOLATION as i32 || status == ERROR_LOCK_VIOLATION as i32
            });
        let fault = if denied {
            Fault::new(
                "NativeExecutableAccessDenied",
                "Windows executable identity was refused by file access or sharing permissions",
            )
        } else if error.kind() == io::ErrorKind::NotFound {
            refused(changed)
        } else {
            Fault::new(
                "NativeExecutable",
                "Windows executable file identity could not be verified",
            )
        };
        fault.with_context(serde_json::json!({ "status": status }))
    }

    fn checked_io<T>(
        cancellation: &AtomicBool,
        deadline: Instant,
        changed: bool,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> Result<T, Fault> {
        check_control(cancellation, deadline)?;
        let result = operation();
        check_control(cancellation, deadline)?;
        result.map_err(|error| io_fault(error, changed))
    }

    fn check_path(path: &Path, changed: bool) -> Result<(), Fault> {
        if !path.is_absolute()
            || path.as_os_str().len() > crate::storage::MAX_PATH_BYTES
            || !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Err(refused(changed));
        }
        for component in path.components() {
            match component {
                Component::Prefix(prefix)
                    if matches!(
                        prefix.kind(),
                        Prefix::Disk(_)
                            | Prefix::VerbatimDisk(_)
                            | Prefix::UNC(_, _)
                            | Prefix::VerbatimUNC(_, _)
                    ) => {}
                Component::RootDir => {}
                Component::Normal(name)
                    if !name
                        .encode_wide()
                        .any(|unit| unit == 0 || unit == u16::from(b':')) => {}
                // In particular, do not accept device namespaces or alternate streams.
                _ => return Err(refused(changed)),
            }
        }
        Ok(())
    }

    impl PinnedFile {
        fn open(
            path: &Path,
            directory: bool,
            changed: bool,
            cancellation: &AtomicBool,
            deadline: Instant,
        ) -> Result<Self, Fault> {
            let file = checked_io(cancellation, deadline, changed, || {
                OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
                    .open(path)
            })?;
            let disk = checked_io(cancellation, deadline, changed, || {
                // SAFETY: File owns a live handle for the duration of this call.
                #[expect(unsafe_code, reason = "audited borrowed Windows file handle")]
                let kind = unsafe { GetFileType(file.as_raw_handle()) };
                if kind == FILE_TYPE_UNKNOWN {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(kind == FILE_TYPE_DISK)
                }
            })?;
            let metadata = checked_io(cancellation, deadline, changed, || file.metadata())?;
            if !disk
                || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || (directory && !metadata.is_dir())
                || (!directory && !metadata.is_file())
            {
                return Err(refused(changed));
            }
            let identity = checked_io(cancellation, deadline, changed, || {
                let mut information = FILE_ID_INFO::default();
                // SAFETY: The borrowed handle stays live, and the writable buffer has
                // exactly the layout and size required by FileIdInfo. No pointer escapes.
                #[expect(unsafe_code, reason = "audited FILE_ID_INFO output buffer")]
                let result = unsafe {
                    GetFileInformationByHandleEx(
                        file.as_raw_handle(),
                        FileIdInfo,
                        (&raw mut information).cast(),
                        std::mem::size_of::<FILE_ID_INFO>() as u32,
                    )
                };
                if result == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(FileIdentity {
                    volume: information.VolumeSerialNumber,
                    file: information.FileId.Identifier,
                })
            })?;
            Ok(Self {
                _file: file,
                identity,
            })
        }
    }

    pub(super) fn open(
        selected_path: &Path,
        cancellation: &AtomicBool,
        deadline: Instant,
    ) -> Result<RetainedExecutable, Fault> {
        check_path(selected_path, false)?;
        let mut files = Vec::with_capacity(selected_path.components().count() - 1);
        let mut path = PathBuf::with_capacity(selected_path.as_os_str().len());
        let mut components = selected_path.components().peekable();
        while let Some(component) = components.next() {
            path.push(component.as_os_str());
            if matches!(component, Component::Prefix(_)) {
                continue;
            }
            files.push(PinnedFile::open(
                &path,
                components.peek().is_some(),
                false,
                cancellation,
                deadline,
            )?);
        }
        let canonical_path = checked_io(cancellation, deadline, false, || {
            selected_path.canonicalize()
        })?;
        check_path(&canonical_path, false)?;
        let proof = Proof { files };
        // Recheck both path spellings against the pinned chain before it can
        // authorize the worker. Canonicalization alone is not file identity.
        proof.validate(selected_path, &canonical_path, cancellation, deadline)?;
        let identity = proof.files.last().ok_or_else(|| refused(false))?.identity;
        Ok(RetainedExecutable {
            canonical_path,
            identity: format!(
                "windows-file:{:016x}:{:032x}",
                identity.volume,
                u128::from_be_bytes(identity.file),
            ),
            proof,
        })
    }

    impl Proof {
        pub(super) fn validate(
            &self,
            selected_path: &Path,
            canonical_path: &Path,
            cancellation: &AtomicBool,
            deadline: Instant,
        ) -> Result<(), Fault> {
            check_control(cancellation, deadline)?;
            check_path(selected_path, true)?;
            let mut retained = self.files.iter();
            let mut path = PathBuf::with_capacity(selected_path.as_os_str().len());
            let mut components = selected_path.components().peekable();
            while let Some(component) = components.next() {
                path.push(component.as_os_str());
                if matches!(component, Component::Prefix(_)) {
                    continue;
                }
                let expected = retained.next().ok_or_else(|| refused(true))?;
                let current = PinnedFile::open(
                    &path,
                    components.peek().is_some(),
                    true,
                    cancellation,
                    deadline,
                )?;
                if current.identity != expected.identity {
                    return Err(refused(true));
                }
            }
            if retained.next().is_some() {
                return Err(refused(true));
            }
            let current_path = checked_io(cancellation, deadline, true, || {
                selected_path.canonicalize()
            })?;
            if current_path != canonical_path {
                return Err(refused(true));
            }
            let canonical = PinnedFile::open(canonical_path, false, true, cancellation, deadline)?;
            if canonical.identity != self.files.last().ok_or_else(|| refused(true))?.identity {
                return Err(refused(true));
            }
            check_control(cancellation, deadline)
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::WindowsExecutableGuard;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "mado-mata-windows-executable-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&root).unwrap();
            fs::write(root.join("selected.exe"), b"original fixture bytes").unwrap();
            Self(root)
        }

        fn executable(&self) -> PathBuf {
            self.0.join("selected.exe")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn open(path: &Path) -> WindowsExecutableGuard {
        WindowsExecutableGuard::open(path, &AtomicBool::new(false), deadline()).unwrap()
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn last_clone_keeps_same_path_replacement_and_writes_denied() {
        let fixture = Fixture::new();
        let path = fixture.executable();
        let guard = open(&path);
        let identity = guard.identity().to_owned();
        let retained = guard.clone();
        drop(guard);
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::remove_file(&path).is_err());
        let previous = fixture.0.join("previous.exe");
        assert!(fs::rename(&path, &previous).is_err());
        retained
            .validate(&path, &AtomicBool::new(false), deadline())
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"original fixture bytes");
        drop(retained);
        fs::rename(&path, &previous).unwrap();
        fs::write(&path, b"replacement fixture bytes").unwrap();
        let replacement = open(&path);
        assert_ne!(replacement.identity(), identity);
    }

    #[test]
    fn retained_parent_cannot_be_renamed_to_rebind_the_selected_path() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("installation");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("game.exe");
        fs::write(&path, b"fixture executable").unwrap();
        let guard = open(&path);
        let moved = fixture.0.join("moved");
        assert!(fs::rename(&directory, &moved).is_err());
        guard
            .validate(&path, &AtomicBool::new(false), deadline())
            .unwrap();
        drop(guard);
        fs::rename(&directory, &moved).unwrap();
    }

    #[test]
    fn existing_writer_is_access_denied_even_when_it_shares_all_access() {
        let fixture = Fixture::new();
        let path = fixture.executable();
        let writer = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .unwrap();
        let error = WindowsExecutableGuard::open(&path, &AtomicBool::new(false), deadline())
            .err()
            .unwrap();
        assert_eq!(error.category, "NativeExecutableAccessDenied");
        drop(writer);
        let guard = open(&path);
        guard
            .validate(&path, &AtomicBool::new(false), deadline())
            .unwrap();
    }

    #[test]
    fn validation_refuses_another_file_instead_of_rebinding() {
        let fixture = Fixture::new();
        let other = fixture.0.join("other.exe");
        fs::write(&other, b"different fixture bytes").unwrap();
        let guard = open(&fixture.executable());
        let error = guard
            .validate(&other, &AtomicBool::new(false), deadline())
            .unwrap_err();
        assert_eq!(error.category, "StaleNativeSelection");
    }

    #[test]
    fn cancellation_and_deadline_are_preserved_at_open_and_use() {
        let fixture = Fixture::new();
        let path = fixture.executable();
        let cancelled = AtomicBool::new(true);
        let ready = AtomicBool::new(false);
        let expired = Instant::now();
        assert_eq!(
            WindowsExecutableGuard::open(&path, &cancelled, deadline())
                .err()
                .unwrap()
                .category,
            "Cancelled",
        );
        assert_eq!(
            WindowsExecutableGuard::open(&path, &ready, expired)
                .err()
                .unwrap()
                .category,
            "NativeCaptureTimeout",
        );
        let guard = open(&path);
        assert_eq!(
            guard
                .validate(&path, &cancelled, deadline())
                .unwrap_err()
                .category,
            "Cancelled",
        );
        assert_eq!(
            guard.validate(&path, &ready, expired).unwrap_err().category,
            "NativeCaptureTimeout",
        );
        // Cancellation takes precedence even if the deadline also expired.
        assert_eq!(
            guard
                .validate(&path, &cancelled, expired)
                .unwrap_err()
                .category,
            "Cancelled",
        );
    }

    #[test]
    fn directory_with_exe_suffix_is_not_executable_evidence() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("directory.exe");
        fs::create_dir(&directory).unwrap();
        let error = WindowsExecutableGuard::open(&directory, &AtomicBool::new(false), deadline())
            .err()
            .unwrap();
        assert_eq!(error.category, "NativeExecutable");
    }

    #[test]
    #[ignore = "requires Windows Developer Mode or pre-authorized symbolic-link privilege"]
    fn symbolic_link_leaf_and_ancestor_are_refused_without_following() {
        let fixture = Fixture::new();
        let link = fixture.0.join("link.exe");
        std::os::windows::fs::symlink_file(fixture.executable(), &link).unwrap();
        let error = WindowsExecutableGuard::open(&link, &AtomicBool::new(false), deadline())
            .err()
            .unwrap();
        assert_eq!(error.category, "NativeExecutable");
        let directory = fixture.0.join("actual");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("game.exe"), b"fixture executable").unwrap();
        let alias = fixture.0.join("alias");
        std::os::windows::fs::symlink_dir(&directory, &alias).unwrap();
        let error = WindowsExecutableGuard::open(
            &alias.join("game.exe"),
            &AtomicBool::new(false),
            deadline(),
        )
        .err()
        .unwrap();
        assert_eq!(error.category, "NativeExecutable");
    }
}
