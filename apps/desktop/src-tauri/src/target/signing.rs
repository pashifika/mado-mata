//! Security owns signature parsing and dynamic-to-static identity validation.
use super::{Guard, unavailable};
use crate::target::observation::{SignedIdentity, SigningIdentity};
use mado_runtime_comparison::model::Fault;
use objc2_core_foundation::{
    CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL, Type,
};
use objc2_security::{
    SecCSFlags, SecCode, SecRequirement, SecStaticCode, errSecCSReqFailed, errSecCSUnsigned,
    kSecCSSigningInformation, kSecCodeAttributeArchitecture, kSecCodeInfoIdentifier,
    kSecCodeInfoMainExecutable, kSecCodeInfoTeamIdentifier, kSecCodeInfoUnique,
    kSecGuestAttributePid,
};
use serde_json::json;
use std::ptr::{self, NonNull};

pub(super) struct SelectedCode {
    code: CFRetained<SecStaticCode>,
    pub identity: SigningIdentity,
}

pub(super) struct RunningCode {
    code: CFRetained<SecCode>,
    pub identity: SigningIdentity,
}

fn status_error(stage: &str, status: i32) -> Fault {
    unavailable(stage).with_context(json!({"stage": stage, "os_status": status}))
}

// SAFETY: Callers pass only initialized Create/Copy outputs with +1 ownership on success.
unsafe fn copied<T: Type>(
    status: i32,
    pointer: *const T,
    stage: &str,
) -> Result<CFRetained<T>, Fault> {
    if status != 0 {
        return Err(status_error(stage, status));
    }
    let pointer = NonNull::new(pointer.cast_mut()).ok_or_else(|| unavailable(stage))?;
    // SAFETY: The successful Create/Copy call transferred this non-null reference.
    Ok(unsafe { CFRetained::from_raw(pointer) })
}

fn information(code: &SecStaticCode, guard: &Guard<'_>) -> Result<CFRetained<CFDictionary>, Fault> {
    guard.call(|| {
        let mut pointer = ptr::null();
        // SAFETY: Code is retained; the initialized output slot is writable for the call.
        let status = unsafe {
            SecCode::copy_signing_information(
                code,
                SecCSFlags(kSecCSSigningInformation),
                NonNull::from(&mut pointer),
            )
        };
        // SAFETY: The Copy API returns a retained dictionary only on success.
        unsafe { copied(status, pointer, "signing_information") }
    })?
}

fn value(dictionary: &CFDictionary, key: &CFString) -> Option<CFRetained<CFType>> {
    // SAFETY: Security signing dictionaries have CFString keys and CFType values.
    let dictionary: &CFDictionary<CFString, CFType> = unsafe { dictionary.cast_unchecked() };
    dictionary.get(key)
}

fn bounded_string(value: &CFType) -> Result<String, Fault> {
    let value = value
        .downcast_ref::<CFString>()
        .ok_or_else(|| unavailable("signing_identity"))?;
    if value.length() == 0 || value.length() > 1024 {
        return Err(unavailable("signing_identity"));
    }
    let string = value.to_string();
    if string.len() > 4096 || string.chars().any(char::is_control) {
        return Err(unavailable("signing_identity"));
    }
    Ok(string)
}

fn identity(information: &CFDictionary, validity: i32) -> Result<SigningIdentity, Fault> {
    // SAFETY: These framework-exported immutable keys are valid CFStrings.
    let (identifier, unique, team) = unsafe {
        (
            value(information, kSecCodeInfoIdentifier),
            value(information, kSecCodeInfoUnique),
            value(information, kSecCodeInfoTeamIdentifier),
        )
    };
    // This classifies the static image, not the process executing from its path.
    // Only a successful information read and the specific unsigned status permit absence.
    if identifier.is_none() && unique.is_none() && team.is_none() && validity == errSecCSUnsigned {
        return Ok(SigningIdentity::Unsigned);
    }
    if validity != 0 {
        return Err(status_error("signature_validity", validity));
    }
    let identifier = bounded_string(
        identifier
            .as_deref()
            .ok_or_else(|| unavailable("signing_identity"))?,
    )?;
    let team = team.as_deref().map(bounded_string).transpose()?;
    let unique = unique.ok_or_else(|| unavailable("code_identity"))?;
    let unique = unique
        .downcast_ref::<CFData>()
        .ok_or_else(|| unavailable("code_identity"))?;
    if unique.length() <= 0 || unique.length() > 64 {
        return Err(unavailable("code_identity"));
    }
    Ok(SigningIdentity::Signed(SignedIdentity {
        identifier,
        team,
        unique: unique.to_vec(),
    }))
}

fn confirm_running_identity(identity: &SigningIdentity, validity: i32) -> Result<(), Fault> {
    // Dynamic errSecCSUnsigned can describe an unsigned replacement on disk.
    // Public validity/status APIs cannot distinguish that from genuinely unsigned live code;
    // even a cleared VALID bit also describes invalidated signed code.
    if validity == 0 && matches!(identity, SigningIdentity::Signed(_)) {
        Ok(())
    } else {
        Err(status_error("running_signing_state", validity))
    }
}

fn running_identity(information: &CFDictionary, validity: i32) -> Result<SigningIdentity, Fault> {
    let identity = identity(information, validity)?;
    confirm_running_identity(&identity, validity)?;
    Ok(identity)
}

fn confirm_executable(
    information: &CFDictionary,
    expected: &str,
    guard: &Guard<'_>,
) -> Result<(), Fault> {
    // SAFETY: Security exports a process-lifetime immutable CFString key.
    let executable = value(information, unsafe { kSecCodeInfoMainExecutable })
        .ok_or_else(|| unavailable("signed_executable"))?;
    let url = executable
        .downcast_ref::<CFURL>()
        .ok_or_else(|| unavailable("signed_executable"))?;
    let path = guard
        .call(|| url.to_file_path())?
        .ok_or_else(|| unavailable("signed_executable"))?;
    let path = guard
        .call(|| std::fs::canonicalize(path))?
        .map_err(|_| unavailable("signed_executable"))?;
    if path != std::path::Path::new(expected) {
        return Err(unavailable("signed_executable"));
    }
    Ok(())
}

pub(super) fn selected(
    path: &str,
    architecture: i32,
    guard: &Guard<'_>,
) -> Result<SelectedCode, Fault> {
    let url = guard
        .call(|| CFURL::from_file_path(path))?
        .ok_or_else(|| unavailable("selected_code"))?;
    let architecture = CFNumber::new_i32(architecture);
    // SAFETY: Security's architecture key is an immutable CFString.
    let attributes = CFDictionary::from_slices(
        &[unsafe { kSecCodeAttributeArchitecture }],
        &[&*architecture],
    );
    let code = guard.call(|| {
        let mut pointer = ptr::null();
        // SAFETY: The attributes contain the documented CFNumber architecture; output is writable.
        let status = unsafe {
            SecStaticCode::create_with_path_and_attributes(
                &url,
                SecCSFlags::empty(),
                attributes.as_opaque(),
                NonNull::from(&mut pointer),
            )
        };
        // SAFETY: The Create API transfers one retained static code reference on success.
        unsafe { copied(status, pointer, "selected_code") }
    })??;
    let information = information(&code, guard)?;
    confirm_executable(&information, path, guard)?;
    // SAFETY: Code remains retained; no optional requirement is passed. No network access is allowed.
    let validity =
        guard.call(|| unsafe { code.check_validity(SecCSFlags::NoNetworkAccess, None) })?;
    let identity = identity(&information, validity)?;
    Ok(SelectedCode { code, identity })
}

pub(super) fn running(pid: i32, path: &str, guard: &Guard<'_>) -> Result<RunningCode, Fault> {
    let pid = CFNumber::new_i32(pid);
    // SAFETY: Security's PID key is an immutable CFString.
    let attributes = CFDictionary::from_slices(&[unsafe { kSecGuestAttributePid }], &[&*pid]);
    let code = guard.call(|| {
        let mut pointer = ptr::null_mut();
        // SAFETY: The dictionary contains a documented PID selector; output is initialized and writable.
        let status = unsafe {
            SecCode::copy_guest_with_attributes(
                None,
                Some(attributes.as_opaque()),
                SecCSFlags::empty(),
                NonNull::from(&mut pointer),
            )
        };
        // SAFETY: The Copy API transfers one retained dynamic code reference on success.
        unsafe { copied(status, pointer, "running_code") }
    })??;
    let static_code = guard.call(|| {
        let mut pointer = ptr::null();
        // SAFETY: Dynamic code is retained; default flags select its executing architecture only.
        let status =
            unsafe { code.copy_static_code(SecCSFlags::empty(), NonNull::from(&mut pointer)) };
        // SAFETY: The Copy API transfers one retained static code reference on success.
        unsafe { copied(status, pointer, "running_architecture") }
    })??;
    let information = information(&static_code, guard)?;
    confirm_executable(&information, path, guard)?;
    // SAFETY: Dynamic validation checks the live host identity against this static image, including
    // filesystem replacement. Comparing two static file hashes without this check is insufficient.
    let validity =
        guard.call(|| unsafe { code.check_validity(SecCSFlags::NoNetworkAccess, None) })?;
    let identity = running_identity(&information, validity)?;
    Ok(RunningCode { code, identity })
}

pub(super) fn requirement(
    selected: &SelectedCode,
    running: &RunningCode,
    guard: &Guard<'_>,
) -> Result<bool, Fault> {
    let requirement: CFRetained<SecRequirement> = guard.call(|| {
        let mut pointer = ptr::null_mut();
        // SAFETY: Selected signed code was validated; output is initialized and writable.
        let status = unsafe {
            SecCode::copy_designated_requirement(
                &selected.code,
                SecCSFlags::empty(),
                NonNull::from(&mut pointer),
            )
        };
        // SAFETY: The Copy API transfers one retained requirement reference on success.
        unsafe { copied(status, pointer, "designated_requirement") }
    })??;
    // SAFETY: The dynamic code and selected requirement are both retained throughout validation.
    let status = guard.call(|| unsafe {
        running
            .code
            .check_validity(SecCSFlags::NoNetworkAccess, Some(&requirement))
    })?;
    match status {
        0 => Ok(true),
        status if status == errSecCSReqFailed => Ok(false),
        _ => Err(status_error("running_requirement", status)),
    }
}

pub(super) fn revalidate(running: &RunningCode, guard: &Guard<'_>) -> Result<(), Fault> {
    // SAFETY: The running code reference is retained through this dynamic validation call.
    let status = guard.call(|| unsafe {
        running
            .code
            .check_validity(SecCSFlags::NoNetworkAccess, None)
    })?;
    confirm_running_identity(&running.identity, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_failures_and_incomplete_identity_never_become_unsigned() {
        let absent = CFDictionary::<CFString, CFType>::empty();
        assert_eq!(
            identity(absent.as_opaque(), errSecCSUnsigned).unwrap(),
            SigningIdentity::Unsigned
        );
        for status in [
            0,
            objc2_security::errSecCSSignatureFailed,
            objc2_security::errSecCSStaticCodeChanged,
        ] {
            assert!(identity(absent.as_opaque(), status).is_err());
        }
        let identifier = CFString::from_str("dev.example.application");
        // SAFETY: The exported key is immutable and the test value is a retained CFString.
        let incomplete =
            CFDictionary::from_slices(&[unsafe { kSecCodeInfoIdentifier }], &[&*identifier]);
        assert!(identity(incomplete.as_opaque(), errSecCSUnsigned).is_err());
        assert!(identity(incomplete.as_opaque(), 0).is_err());
        let wrong_type = CFNumber::new_i32(1);
        // SAFETY: The exported key is immutable; arbitrary CFType values are rejected by the reader.
        let malformed =
            CFDictionary::from_slices(&[unsafe { kSecCodeInfoIdentifier }], &[&*wrong_type]);
        assert!(identity(malformed.as_opaque(), 0).is_err());
    }

    #[test]
    fn unsigned_disk_cannot_establish_unsigned_live_code() {
        let absent = CFDictionary::<CFString, CFType>::empty();
        let installed = identity(absent.as_opaque(), errSecCSUnsigned).unwrap();
        assert_eq!(installed, SigningIdentity::Unsigned);

        // An old signed process and genuinely unsigned code can produce the same disk evidence.
        assert!(running_identity(absent.as_opaque(), errSecCSUnsigned).is_err());
        assert!(confirm_running_identity(&installed, errSecCSUnsigned).is_err());
        assert!(confirm_running_identity(&installed, 0).is_err());
    }

    #[test]
    fn live_identity_requires_complete_signed_evidence_and_successful_validation() {
        let identifier = CFString::from_str("dev.example.application");
        let unique = CFData::from_bytes(&[1; 20]);
        // SAFETY: Security's immutable keys and retained CFType values form a signing dictionary.
        let information = CFDictionary::<CFString, CFType>::from_slices(
            &[unsafe { kSecCodeInfoIdentifier }, unsafe {
                kSecCodeInfoUnique
            }],
            &[&*identifier, &*unique],
        );
        let expected = SigningIdentity::Signed(SignedIdentity {
            identifier: "dev.example.application".into(),
            team: None,
            unique: vec![1; 20],
        });
        assert_eq!(
            running_identity(information.as_opaque(), 0).unwrap(),
            expected
        );
        confirm_running_identity(&expected, 0).unwrap();

        for status in [
            errSecCSUnsigned,
            objc2_security::errSecCSGuestInvalid,
            objc2_security::errSecCSSignatureFailed,
            objc2_security::errSecCSStaticCodeChanged,
            objc2_security::errSecCSNoSuchCode,
            -1,
        ] {
            assert!(running_identity(information.as_opaque(), status).is_err());
            assert!(confirm_running_identity(&expected, status).is_err());
        }

        let absent = CFDictionary::<CFString, CFType>::empty();
        assert!(running_identity(absent.as_opaque(), 0).is_err());
        // SAFETY: A retained CFString is intentionally not a valid code-directory hash.
        let malformed = CFDictionary::from_slices(
            &[unsafe { kSecCodeInfoIdentifier }, unsafe {
                kSecCodeInfoUnique
            }],
            &[&*identifier, &*identifier],
        );
        assert!(running_identity(malformed.as_opaque(), 0).is_err());
    }
}
