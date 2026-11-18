use mado_mata_desktop::application::{MAX_PROFILE_SHARE_BYTES, PROFILE_SHARE_PREFIX};
use mado_runtime_comparison::model::Fault;

#[tauri::command]
pub fn copy_profile_share_code(code: &str) -> Result<(), Fault> {
    if code.len() > MAX_PROFILE_SHARE_BYTES
        || !code.starts_with(PROFILE_SHARE_PREFIX)
        || code.contains('\0')
    {
        return Err(Fault::new(
            "ProfileShareClipboard",
            "Invalid share code for copying",
        ));
    }
    #[cfg(target_os = "macos")]
    if write_text(code) {
        return Ok(());
    }
    #[cfg(windows)]
    if crate::windows_shell::copy_text(code).is_ok() {
        return Ok(());
    }
    Err(Fault::new(
        "ProfileShareClipboard",
        "Could not copy the reviewed share code",
    ))
}

#[cfg(target_os = "macos")]
pub(super) fn write_text(text: &str) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    let pasteboard = NSPasteboard::generalPasteboard();
    // SAFETY: AppKit exports this immutable process-lifetime string type.
    #[expect(
        unsafe_code,
        reason = "audited immutable AppKit pasteboard type constant"
    )]
    let text_type = unsafe { NSPasteboardTypeString };
    let text = NSString::from_str(text);
    pasteboard.clearContents();
    pasteboard.setString_forType(&text, text_type)
}
