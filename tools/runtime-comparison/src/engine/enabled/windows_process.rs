use super::{Control, Fault, blocked, selected_process_lost};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};

// Used only before SDK attachment; the session owns subsequent target validity.
pub(super) fn check(
    pid: Option<u32>,
    expected: Option<u64>,
    control: &Control,
) -> Result<(), Fault> {
    control.check()?;
    let (Some(pid), Some(expected)) = (pid, expected) else {
        return Err(unavailable());
    };
    if pid == 0 {
        return Err(unavailable());
    }
    // SAFETY: requests query/synchronization rights only, with no borrowed pointers.
    #[expect(unsafe_code, reason = "bounded read-only Windows process query")]
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        let error = std::io::Error::last_os_error();
        control.check()?;
        return Err(
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                selected_process_lost()
            } else {
                unavailable()
            },
        );
    }
    // SAFETY: OpenProcess returned one non-null owned handle, closed by OwnedHandle.
    #[expect(unsafe_code, reason = "own the successful Windows process handle")]
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the owned handle stays live and all FILETIME buffers are writable.
    #[expect(
        unsafe_code,
        reason = "read complete process lifetime into stack buffers"
    )]
    let queried = unsafe {
        GetProcessTimes(
            handle.as_raw_handle(),
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    };
    control.check()?;
    if queried == 0 {
        return Err(unavailable());
    }
    let actual = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    if actual != expected {
        return Err(selected_process_lost());
    }
    // SAFETY: the handle includes PROCESS_SYNCHRONIZE; zero timeout never blocks.
    #[expect(
        unsafe_code,
        reason = "nonblocking process-exit check before attachment"
    )]
    let status = unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) };
    control.check()?;
    match status {
        WAIT_TIMEOUT => Ok(()),
        WAIT_OBJECT_0 => Err(selected_process_lost()),
        _ => Err(unavailable()),
    }
}

fn unavailable() -> Fault {
    blocked(
        "target_identity_unavailable",
        "selected Windows process or executable identity cannot be verified",
    )
}

pub(super) fn same_executable(left: &Path, right: &Path) -> Result<bool, Fault> {
    fn identity(path: &Path, require_single_link: bool) -> Result<Option<FILE_ID_INFO>, Fault> {
        let metadata = path.metadata().map_err(|_| unavailable())?;
        if !metadata.is_file() {
            return Ok(None);
        }
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .map_err(|_| unavailable())?;
        let mut identity = FILE_ID_INFO::default();
        // SAFETY: the file stays open and the buffer matches FileIdInfo's layout and size.
        #[expect(
            unsafe_code,
            reason = "read Windows volume and file identity from an owned handle"
        )]
        let read = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&raw mut identity).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if read == 0 {
            return Err(unavailable());
        }
        if require_single_link {
            let mut standard = FILE_STANDARD_INFO::default();
            // SAFETY: the owned handle and exact FileStandardInfo output buffer stay live.
            #[expect(
                unsafe_code,
                reason = "verify the saved executable's live single-link identity"
            )]
            let read = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileStandardInfo,
                    (&raw mut standard).cast(),
                    std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
                )
            };
            if read == 0 {
                return Err(unavailable());
            }
            if standard.NumberOfLinks != 1 || standard.DeletePending || standard.Directory {
                return Err(blocked(
                    "target_identity_unavailable",
                    "selected Windows executable requires one live file link",
                ));
            }
        }
        Ok(Some(identity))
    }
    let (Some(left), Some(right)) = (identity(left, false)?, identity(right, true)?) else {
        return Ok(false);
    };
    Ok(left.VolumeSerialNumber == right.VolumeSerialNumber
        && left.FileId.Identifier == right.FileId.Identifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_correspondence_uses_file_identity_not_filename_or_contents() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!(
            "mado-runtime-file-identity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir(&root).unwrap();
        let root = Directory(root);
        let original = root.0.join("selected");
        let alias = root.0.join("alias");
        let unrelated = root.0.join("unrelated");
        std::fs::write(&original, b"controlled metadata").unwrap();
        std::fs::write(&unrelated, b"controlled metadata").unwrap();
        assert!(same_executable(&original, &original).unwrap());
        assert!(!same_executable(&original, &unrelated).unwrap());
        assert!(!same_executable(&original, &root.0).unwrap());
        std::fs::hard_link(&original, &alias).unwrap();
        assert_eq!(
            same_executable(&alias, &original).unwrap_err().context["stage"],
            "target_identity_unavailable"
        );
        std::fs::remove_file(&alias).unwrap();
        std::fs::hard_link(&unrelated, &alias).unwrap();
        assert!(!same_executable(&alias, &original).unwrap());
    }

    #[test]
    fn interrupted_preparation_never_queries_a_process() {
        let plan: crate::model::Plan =
            serde_json::from_str(include_str!("../../../fixtures/manual-plan.json")).unwrap();
        let control = Control::new(&plan.limits);
        control.cancel();
        assert_eq!(
            check(None, None, &control).unwrap_err().category,
            "Cancelled"
        );
    }
}
