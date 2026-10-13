//! Native file identity and a same-user, non-replaceable installation lock.
//! The lock's DACL is supplied at creation, not repaired after publishing it.

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    ERROR_FILE_EXISTS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION,
    PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, FileIdInfo, GetFileInformationByHandleEx,
    GetFileType, OPEN_EXISTING,
};
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const LOCK: &str = "mado-mata-plugin.lock";

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "the OMP installation lock is not a normal file private to the current user",
    )
}

struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    #[expect(
        unsafe_code,
        reason = "audited release of Windows security allocations"
    )]
    fn drop(&mut self) {
        // SAFETY: each value owns exactly one allocation returned by a security
        // API documented to use LocalFree. All borrowed pointers have expired.
        unsafe { LocalFree(self.0) };
    }
}

struct User(Vec<usize>);

impl User {
    #[expect(
        unsafe_code,
        reason = "audited query of the current process token's user SID"
    )]
    fn current() -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: GetCurrentProcess is a pseudo handle borrowed only for this
        // call; token is writable output storage and TOKEN_QUERY is read-only.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful OpenProcessToken transfers one owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut bytes = 0;
        // SAFETY: null storage requests the required byte count.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                null_mut(),
                0,
                &raw mut bytes,
            )
        };
        if (bytes as usize) < size_of::<TOKEN_USER>() || bytes > 64 * 1024 {
            return Err(refused());
        }
        let mut user = Self(vec![0_usize; (bytes as usize).div_ceil(size_of::<usize>())]);
        // SAFETY: user owns aligned storage of at least bytes. The returned SID
        // points into this same allocation, which is retained by User.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                user.0.as_mut_ptr().cast(),
                bytes,
                &raw mut bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(user)
    }

    #[expect(
        unsafe_code,
        reason = "audited borrow of a SID in retained TOKEN_USER storage"
    )]
    fn sid(&self) -> PSID {
        // SAFETY: current() successfully filled aligned TOKEN_USER storage.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    #[expect(
        unsafe_code,
        reason = "audited construction of a protected user-only DACL"
    )]
    fn descriptor(&self) -> io::Result<LocalMemory> {
        let mut text = null_mut();
        // SAFETY: sid() borrows the valid retained token user for this call.
        if unsafe { ConvertSidToStringSidW(self.sid(), &raw mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _text = LocalMemory(text.cast());
        let mut length = 0;
        // SAFETY: the API returned an allocated, NUL-terminated UTF-16 SID.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: exactly length initialized units precede that terminator.
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| refused())?;
        let sddl: Vec<_> = format!("O:{sid}D:P(A;;FA;;;{sid})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = null_mut();
        // SAFETY: sddl is NUL-terminated; output receives an owned allocation.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalMemory(descriptor))
    }

    #[expect(
        unsafe_code,
        reason = "audited owner and DACL validation on a retained file handle"
    )]
    fn check(&self, file: &File) -> io::Result<()> {
        let mut owner = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: file stays open; output pointers refer to the returned
        // LocalFree allocation, retained until all checks below finish.
        let result = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                null_mut(),
                &raw mut dacl,
                null_mut(),
                &raw mut descriptor,
            )
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result.cast_signed()));
        }
        let descriptor = LocalMemory(descriptor);
        let (mut control, mut revision) = (0, 0);
        // SAFETY: returned owner/ACL/control data are valid for this allocation.
        if owner.is_null()
            || dacl.is_null()
            || unsafe { EqualSid(owner, self.sid()) } == 0
            || unsafe {
                GetSecurityDescriptorControl(descriptor.0, &raw mut control, &raw mut revision)
            } == 0
            || control & SE_DACL_PROTECTED == 0
            || unsafe { (*dacl).AceCount } != 1
        {
            return Err(refused());
        }
        let mut ace = null_mut();
        // SAFETY: dacl was returned by the security API and has one ACE.
        if unsafe { GetAce(dacl, 0, &raw mut ace) } == 0 || ace.is_null() {
            return Err(refused());
        }
        // SAFETY: GetAce points at a valid ACE header. Only the standard allow
        // layout is accessed after checking its type and minimum byte size.
        let header = unsafe { &*ace.cast::<windows_sys::Win32::Security::ACE_HEADER>() };
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
            || header.AceFlags != 0
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err(refused());
        }
        // SAFETY: the checked type/size establish ACCESS_ALLOWED_ACE layout. The
        // security API returns a valid ACL; the SID lies in its variable tail.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = (&raw const allowed.SidStart).cast_mut().cast();
        // SAFETY: the ACE is OS-validated; SID length additionally must fit this
        // ACE before comparison with the retained token's user.
        if allowed.Mask != FILE_ALL_ACCESS
            || unsafe { IsValidSid(sid) } == 0
            || offset_of!(ACCESS_ALLOWED_ACE, SidStart) + unsafe { GetLengthSid(sid) } as usize
                > usize::from(header.AceSize)
            || unsafe { EqualSid(sid, self.sid()) } == 0
        {
            return Err(refused());
        }
        Ok(())
    }
}

/// The file is never unlinked on unlock: every process and alias of the same
/// user root locks the same object. No FILE_SHARE_DELETE allows replacement
/// while any lease is retained, including the durable-fence failure case.
#[expect(
    unsafe_code,
    reason = "audited private lock creation and no-follow handle validation"
)]
pub(super) fn lock_file(user_root: &Path) -> io::Result<File> {
    let user = User::current()?;
    let descriptor = user.descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // Resolve existing directory aliases before handing a verbatim path to
    // Win32, which otherwise does not normalize slash or dot components.
    let path: Vec<_> = user_root
        .canonicalize()?
        .join(LOCK)
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    let open = |disposition| {
        // SAFETY: path is a terminated absolute path, attributes refer to live
        // security storage, and no handle is inherited or shared for deletion.
        unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &attributes,
                disposition,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        }
    };
    let mut handle = open(CREATE_NEW);
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_FILE_EXISTS.cast_signed()) {
            return Err(error);
        }
        handle = open(OPEN_EXISTING);
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: CreateFileW transferred the valid file handle, closed by File.
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    // SAFETY: only the retained file's type is queried.
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
    {
        return Err(refused());
    }
    user.check(&file)?;
    Ok(file)
}

/// Unlike size/mtime alone, the volume and 128-bit file ID detect replacement
/// even when an installer preserves the old timestamps and length.
#[expect(
    unsafe_code,
    reason = "audited FILE_ID_INFO query on a retained executable handle"
)]
pub(super) fn identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut identity = FILE_ID_INFO::default();
    // SAFETY: file is live and identity is exactly the output layout required by
    // FileIdInfo. No pointer escapes this synchronous call.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut identity).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((identity.VolumeSerialNumber, identity.FileId.Identifier))
}

pub(super) fn metadata(path: &Path) -> io::Result<(fs::Metadata, (u64, [u8; 16]))> {
    let file = File::open(path)?;
    Ok((file.metadata()?, identity(&file)?))
}

/// Compare the registration object itself, not the directory it links to. This
/// accepts ordinary/verbatim, slash and case aliases without adopting a distinct
/// registration that happens to link to the same payload.
pub(super) fn same_registration(listed: &str, expected: &Path) -> bool {
    let listed = Path::new(listed);
    if !listed.is_absolute() {
        return false;
    }
    let open = |path: &Path| {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .and_then(|file| identity(&file))
    };
    matches!((open(listed), open(expected)), (Ok(left), Ok(right)) if left == right)
}
