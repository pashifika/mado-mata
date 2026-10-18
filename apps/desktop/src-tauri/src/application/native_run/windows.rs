use mado_runtime_comparison::model::{Control, Fault};

const MAX_PROCESSES: usize = 4096;

// Every queryable process is checked by file identity, including hard-link aliases.
// Complete long/short names can exclude an inaccessible row, never establish a match.
enum Candidate<T> {
    Unrelated,
    Matching(T),
}

fn unavailable() -> Fault {
    Fault::new(
        "NativeTargetUnverifiable",
        "Windows process correspondence is incomplete or a plausible process is inaccessible",
    )
}

fn require_saved_resolution(
    canonical: &std::path::Path,
    saved: &crate::target::ResolvedLocation,
) -> Result<(), Fault> {
    if canonical != std::path::Path::new(&saved.path)
        || canonical != std::path::Path::new(&saved.executable)
    {
        return Err(Fault::new(
            "StaleNativeSelection",
            "Saved executable resolution changed; Check and Save the target again",
        ));
    }
    Ok(())
}

fn select_process<E, T>(
    control: &Control,
    mut next: impl FnMut() -> Result<Option<E>, Fault>,
    mut inspect: impl FnMut(E) -> Result<Candidate<T>, Fault>,
) -> Result<Option<T>, Fault> {
    let mut selected = None;
    for count in 0..=MAX_PROCESSES {
        let entry = checked(control, &mut next)?;
        let Some(entry) = entry else {
            return Ok(selected);
        };
        if count == MAX_PROCESSES {
            return Err(Fault::new(
                "NativeTargetUnverifiable",
                "Windows process discovery exceeds its finite candidate bound",
            ));
        }
        if let Candidate::Matching(process) = checked(control, || inspect(entry))?
            && selected.replace(process).is_some()
        {
            return Err(Fault::new(
                "NativeTargetAmbiguous",
                "Multiple running processes match the saved executable",
            ));
        }
    }
    unreachable!("the bounded iteration either ends or refuses overflow")
}

fn checked<T>(control: &Control, operation: impl FnOnce() -> Result<T, Fault>) -> Result<T, Fault> {
    control.check()?;
    let result = operation();
    control.check()?;
    result
}

#[cfg(windows)]
pub(super) use platform::{RetainedTarget, WindowsPreparation};

#[cfg(windows)]
mod platform {
    use super::{Candidate, checked, require_saved_resolution, select_process, unavailable};
    use crate::application::authoring::WindowsExecutableGuard;
    use crate::application::native_run::{NativeBinding, PreparationPlatform, PreparedProcess};
    use crate::target::LaunchFailure;
    use mado_runtime_comparison::desktop::LaunchDisposition;
    use mado_runtime_comparison::model::{Control, Fault};
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{
        ERROR_NO_MORE_FILES, FILETIME, GetLastError, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
    };

    pub(in crate::application::native_run) struct RetainedTarget {
        executable: WindowsExecutableGuard,
        image: WindowsExecutableGuard,
        selected_path: PathBuf,
        process: OwnedHandle,
        pid: u32,
        lifetime: u64,
    }

    impl std::fmt::Debug for RetainedTarget {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RetainedWindowsTarget")
                .finish_non_exhaustive()
        }
    }

    impl RetainedTarget {
        fn validate(&self, control: &Control) -> Result<(), Fault> {
            require_live(&self.process, control)?;
            let lifetime = process_lifetime(&self.process, control)?;
            if lifetime != self.lifetime {
                return Err(lost());
            }
            checked(control, || {
                self.executable.validate(
                    &self.selected_path,
                    &control.cancelled,
                    control.deadline(),
                )
            })?;
            if self.image.canonical_path() != self.executable.canonical_path() {
                checked(control, || {
                    self.image.validate(
                        self.image.canonical_path(),
                        &control.cancelled,
                        control.deadline(),
                    )
                })?;
            }
            checked(control, || {
                self.executable
                    .require_single_link(&control.cancelled, control.deadline())
            })?;
            require_live(&self.process, control)
        }
    }

    pub(in crate::application::native_run) struct WindowsPreparation {
        pub(in crate::application::native_run) retained: Option<Arc<RetainedTarget>>,
    }

    impl PreparationPlatform for WindowsPreparation {
        type Recipe = std::convert::Infallible;
        type Prepared = std::convert::Infallible;

        fn script_executable(
            &mut self,
            _binding: &NativeBinding,
            _control: &Control,
        ) -> Result<Option<PathBuf>, Fault> {
            Err(unavailable())
        }

        fn discover(
            &mut self,
            binding: &NativeBinding,
            control: &Control,
        ) -> Result<Option<PreparedProcess>, Fault> {
            control.check()?;
            let configuration = &binding.binding.configuration;
            configuration.validate_declaration(&binding.declaration)?;
            let selected_path = Path::new(&configuration.game.path);
            let executable = checked(control, || {
                WindowsExecutableGuard::open(selected_path, &control.cancelled, control.deadline())
            })?;
            require_saved_resolution(
                executable.canonical_path(),
                &binding.binding.resolution.game,
            )?;
            checked(control, || {
                executable.require_single_link(&control.cancelled, control.deadline())
            })?;
            let name: Vec<u16> = executable
                .canonical_path()
                .file_name()
                .ok_or_else(unavailable)?
                .encode_wide()
                .collect();
            let mut image = vec![0u16; 32_768];
            let short = short_name(executable.canonical_path(), &mut image, control)?;
            let mut snapshot = Snapshot::open(control)?;
            let process = select_process(
                control,
                || snapshot.next(),
                |entry| {
                    let process = checked(control, || {
                        // SAFETY: Query/synchronize only; a successful handle is owned immediately.
                        #[expect(unsafe_code, reason = "audited query-only Windows process open")]
                        let raw = unsafe {
                            OpenProcess(
                                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                                0,
                                entry.th32ProcessID,
                            )
                        };
                        if raw.is_null() {
                            return Ok(None);
                        }
                        // SAFETY: OpenProcess returned a new owned handle, closed exactly once.
                        #[expect(unsafe_code, reason = "audited Windows handle ownership transfer")]
                        Ok(Some(unsafe { OwnedHandle::from_raw_handle(raw) }))
                    })?;
                    let Some(process) = process else {
                        return inaccessible_candidate(&entry.szExeFile, &name, &short);
                    };
                    let path = match image_path(&process, &mut image, control) {
                        Ok(path) => path,
                        Err(_) => {
                            control.check()?;
                            return inaccessible_candidate(&entry.szExeFile, &name, &short);
                        }
                    };
                    match checked(control, || {
                        executable.same_file(&path, &control.cancelled, control.deadline())
                    }) {
                        Ok(true) => {}
                        Ok(false) => return Ok(Candidate::Unrelated),
                        Err(_) => {
                            control.check()?;
                            return inaccessible_candidate(&entry.szExeFile, &name, &short);
                        }
                    }
                    require_live(&process, control)?;
                    let lifetime = process_lifetime(&process, control)?;
                    let canonical =
                        checked(control, || path.canonicalize().map_err(|_| unavailable()))?;
                    let image = if canonical == executable.canonical_path() {
                        executable.clone()
                    } else {
                        checked(control, || {
                            WindowsExecutableGuard::open(
                                &canonical,
                                &control.cancelled,
                                control.deadline(),
                            )
                        })?
                    };
                    if image.identity() != executable.identity() {
                        return Err(unavailable());
                    }
                    require_live(&process, control)?;
                    Ok(Candidate::Matching(RetainedTarget {
                        executable: executable.clone(),
                        image,
                        selected_path: selected_path.to_owned(),
                        process,
                        pid: entry.th32ProcessID,
                        lifetime,
                    }))
                },
            )?;
            let Some(process) = process else {
                checked(control, || {
                    executable.validate(selected_path, &control.cancelled, control.deadline())
                })?;
                checked(control, || {
                    executable.require_single_link(&control.cancelled, control.deadline())
                })?;
                return Ok(None);
            };
            process.validate(control)?;
            let proof = PreparedProcess {
                pid: process.pid,
                lifetime: process.lifetime,
                executable: process.executable.canonical_path().to_owned(),
            };
            self.retained = Some(Arc::new(process));
            Ok(Some(proof))
        }

        fn recipe(&mut self, _binding: &NativeBinding) -> Result<Self::Recipe, Fault> {
            Err(Fault::new(
                "NativeRefused",
                "Windows Native never launches a game",
            ))
        }

        fn prepare(
            &mut self,
            recipe: Self::Recipe,
            _control: &Control,
        ) -> Result<Self::Prepared, Fault> {
            match recipe {}
        }

        fn submit(&mut self, prepared: Self::Prepared) -> Result<LaunchDisposition, LaunchFailure> {
            match prepared {}
        }

        fn revalidate(&mut self, control: &Control) -> Result<(), Fault> {
            self.retained
                .as_ref()
                .ok_or_else(unavailable)?
                .validate(control)
        }
    }

    struct Snapshot {
        handle: OwnedHandle,
        first: bool,
    }

    impl Snapshot {
        fn open(control: &Control) -> Result<Self, Fault> {
            checked(control, || {
                // SAFETY: Process-only snapshot, no pointers or target mutation.
                #[expect(unsafe_code, reason = "audited public process snapshot")]
                let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
                if raw == INVALID_HANDLE_VALUE || raw.is_null() {
                    return Err(unavailable());
                }
                // SAFETY: This successful snapshot handle transfers ownership exactly once.
                #[expect(unsafe_code, reason = "audited Windows snapshot ownership transfer")]
                let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
                Ok(Self {
                    handle,
                    first: true,
                })
            })
        }

        fn next(&mut self) -> Result<Option<PROCESSENTRY32W>, Fault> {
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            // SAFETY: Live snapshot, correctly sized initialized output; no pointer escapes.
            #[expect(unsafe_code, reason = "audited bounded ToolHelp process entry")]
            let result = unsafe {
                let success = if self.first {
                    Process32FirstW(self.handle.as_raw_handle(), &raw mut entry)
                } else {
                    Process32NextW(self.handle.as_raw_handle(), &raw mut entry)
                };
                (success, GetLastError())
            };
            self.first = false;
            match result {
                (0, ERROR_NO_MORE_FILES) => Ok(None),
                (0, _) => Err(unavailable()),
                _ => Ok(Some(entry)),
            }
        }
    }

    fn plausible_name(observed: &[u16; 260], saved: &[u16]) -> Result<bool, Fault> {
        let len = observed
            .iter()
            .position(|unit| *unit == 0)
            .ok_or_else(unavailable)?;
        if len == 0 || len == observed.len() - 1 || saved.is_empty() || saved.len() > 32_767 {
            return Err(unavailable());
        }
        // SAFETY: Both UTF-16 slices stay valid for their explicit bounded lengths.
        #[expect(
            unsafe_code,
            reason = "audited Windows ordinal executable-name comparison"
        )]
        let comparison = unsafe {
            CompareStringOrdinal(
                observed.as_ptr(),
                len as i32,
                saved.as_ptr(),
                saved.len() as i32,
                1,
            )
        };
        if comparison == 0 {
            return Err(unavailable());
        }
        Ok(comparison == CSTR_EQUAL)
    }

    fn inaccessible_candidate<T>(
        observed: &[u16; 260],
        saved: &[u16],
        short: &[u16],
    ) -> Result<Candidate<T>, Fault> {
        if plausible_name(observed, saved)? || plausible_name(observed, short)? {
            Err(unavailable())
        } else {
            Ok(Candidate::Unrelated)
        }
    }

    fn lost() -> Fault {
        Fault::new(
            "TargetLost",
            "The selected Windows process lifetime ended before attachment",
        )
    }

    fn require_live(process: &OwnedHandle, control: &Control) -> Result<(), Fault> {
        checked(control, || {
            // SAFETY: The retained process handle has SYNCHRONIZE access; never blocks.
            #[expect(unsafe_code, reason = "audited nonblocking retained-process status")]
            match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
                WAIT_TIMEOUT => Ok(()),
                WAIT_OBJECT_0 => Err(lost()),
                _ => Err(unavailable()),
            }
        })
    }

    fn process_lifetime(process: &OwnedHandle, control: &Control) -> Result<u64, Fault> {
        checked(control, || {
            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            // SAFETY: Queryable retained handle and four correctly sized writable outputs.
            #[expect(unsafe_code, reason = "audited process creation FILETIME query")]
            let result = unsafe {
                GetProcessTimes(
                    process.as_raw_handle(),
                    &raw mut creation,
                    &raw mut exit,
                    &raw mut kernel,
                    &raw mut user,
                )
            };
            if result == 0 {
                return Err(unavailable());
            }
            // Pinned SDK db939f0 window_authority uses this same FILETIME lifetime domain.
            let lifetime =
                (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
            if lifetime == 0 {
                return Err(unavailable());
            }
            Ok(lifetime)
        })
    }

    fn image_path(
        process: &OwnedHandle,
        buffer: &mut [u16],
        control: &Control,
    ) -> Result<PathBuf, Fault> {
        checked(control, || {
            let mut written = buffer.len() as u32;
            // SAFETY: Retained queryable process; buffer length is passed and remains writable.
            #[expect(unsafe_code, reason = "audited bounded Windows image-path query")]
            let result = unsafe {
                QueryFullProcessImageNameW(
                    process.as_raw_handle(),
                    PROCESS_NAME_WIN32,
                    buffer.as_mut_ptr(),
                    &raw mut written,
                )
            };
            let written = written as usize;
            if result == 0
                || written == 0
                || written >= buffer.len()
                || buffer[..written].contains(&0)
            {
                return Err(unavailable());
            }
            let path = PathBuf::from(OsString::from_wide(&buffer[..written]));
            if !path.is_absolute() {
                return Err(unavailable());
            }
            Ok(path)
        })
    }

    fn short_name(path: &Path, buffer: &mut [u16], control: &Control) -> Result<Vec<u16>, Fault> {
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        checked(control, || {
            // SAFETY: NUL-terminated input and bounded writable output remain live.
            #[expect(unsafe_code, reason = "audited Windows short-path metadata query")]
            let written = unsafe {
                GetShortPathNameW(path.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32)
            } as usize;
            if written == 0 || written >= buffer.len() || buffer[..written].contains(&0) {
                return Err(unavailable());
            }
            let short = PathBuf::from(OsString::from_wide(&buffer[..written]));
            Ok(short
                .file_name()
                .ok_or_else(unavailable)?
                .encode_wide()
                .collect())
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn inaccessible_candidates_require_complete_negative_long_and_short_name_evidence() {
            let saved: Vec<u16> = "LongGame.exe".encode_utf16().collect();
            let short: Vec<u16> = "LONGGA~1.EXE".encode_utf16().collect();
            let observed = |name: &str| {
                let mut result = [0u16; 260];
                for (slot, unit) in result.iter_mut().zip(name.encode_utf16()) {
                    *slot = unit;
                }
                result
            };
            for name in ["longgame.EXE", "longga~1.exe"] {
                assert!(matches!(
                    inaccessible_candidate::<()>(&observed(name), &saved, &short),
                    Err(fault) if fault.category == "NativeTargetUnverifiable",
                ));
            }
            assert!(matches!(
                inaccessible_candidate::<()>(&observed("Unrelated.exe"), &saved, &short).unwrap(),
                Candidate::Unrelated,
            ));
            for incomplete in [[1u16; 260], [0u16; 260], observed(&"a".repeat(259))] {
                assert!(inaccessible_candidate::<()>(&incomplete, &saved, &short).is_err());
            }
        }
    }
}

#[cfg(test)]
mod tests;
