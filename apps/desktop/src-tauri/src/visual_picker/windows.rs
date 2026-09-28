use super::{
    Click, NativeCoordinateUnit, NativePickerSnapshot, ObservedWindow, Rect, changed, eligible_hit,
    unavailable,
};
use mado_mata_desktop::application::NativeCaptureArea;
use mado_runtime_comparison::model::Fault;
use std::{
    cell::Cell,
    ptr::{null, null_mut},
    sync::atomic::Ordering,
};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        Gdi::*,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
            SetThreadDpiAwarenessContext,
        },
        WindowsAndMessaging::*,
    },
};
use windows_sys::core::BOOL;

const CLASS_NAME: windows_sys::core::PCWSTR =
    windows_sys::core::w!("MadoMata.AuthoringWindowPicker");
const DISPLAY_CHANGED: u32 = WM_APP + 71;
const ACCENT: u32 = 0x00ffbf26;

struct DpiContext(DPI_AWARENESS_CONTEXT);
impl Drop for DpiContext {
    #[expect(unsafe_code, reason = "restore the UI thread DPI context on all exits")]
    fn drop(&mut self) {
        // SAFETY: Saved from a successful SetThreadDpiAwarenessContext on this thread.
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

struct Overlays<'a> {
    clean: &'a Cell<bool>,
    instance: HINSTANCE,
    brush: HBRUSH,
    windows: Vec<HWND>,
}

impl Drop for Overlays<'_> {
    #[expect(
        unsafe_code,
        reason = "destroy only windows and GDI resources owned by this UI-thread scope"
    )]
    fn drop(&mut self) {
        // SAFETY: Handles remain on their creating thread. No callback retains Rust
        // state. Destroy all windows before unregistering their class/deleting its brush.
        unsafe {
            for window in self.windows.iter().rev() {
                if DestroyWindow(*window) == 0 && IsWindow(*window) != 0 {
                    self.clean.set(false);
                }
            }
            // If a native window survived, its class/brush must survive with it.
            // The host gate remains occupied until app shutdown.
            if self.clean.get() && UnregisterClassW(CLASS_NAME, self.instance) != 0 {
                DeleteObject(self.brush);
            }
        }
    }
}

struct Display {
    bounds: Rect,
    shield: HWND,
    borders: [HWND; 4],
    label: HWND,
    label_bounds: Rect,
    scale: f64,
}

pub(super) fn pick(
    window: &tauri::WebviewWindow,
    snapshot: &NativePickerSnapshot,
) -> Result<Option<String>, Fault> {
    let clean = Cell::new(true);
    let result = pick_inner(window, snapshot, &clean);
    if !clean.get() {
        return Err(Fault::new(
            "NativePickerCleanup",
            "Picker overlay removal was incomplete; authoring remains reserved",
        ));
    }
    result
}

#[expect(
    unsafe_code,
    reason = "bounded nonactivating Win32 picker windows and local UI-thread message loop"
)]
fn pick_inner(
    window: &tauri::WebviewWindow,
    snapshot: &NativePickerSnapshot,
    clean: &Cell<bool>,
) -> Result<Option<String>, Fault> {
    for candidate in &snapshot.candidates {
        if candidate.bounds.unit != NativeCoordinateUnit::WindowsPhysicalPixels
            || !matches!(
                candidate.capture_area,
                NativeCaptureArea::WindowsExtendedFrame | NativeCaptureArea::WindowsClient
            )
        {
            return Err(unavailable(
                "Verified Windows capture-area geometry is required",
            ));
        }
        integer_rect(Rect::from(&candidate.bounds))?;
    }
    let owner = window
        .hwnd()
        .map_err(|_| unavailable("Editor window is unavailable"))?
        .0 as HWND;
    // SAFETY: The scope runs on the HWND's UI thread and retains the Tauri owner.
    // Per-monitor-v2 makes every desktop rectangle physical, including negative
    // origins; no primary-display scale is applied to another display.
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous.is_null() {
        return Err(unavailable("Per-monitor DPI awareness is unavailable"));
    }
    let _dpi = DpiContext(previous);
    if !foreground_is_ours() {
        return Err(unavailable("Return to the editor before visual selection"));
    }
    let monitors = monitors()?;
    let mut overlays = create_class(clean)?;
    let mut displays = Vec::with_capacity(monitors.len());
    for bounds in monitors {
        let shield = create_window(&mut overlays, owner, bounds, true)?;
        // SAFETY: shield is an owned window created in per-monitor-v2 context.
        let dpi = unsafe { GetDpiForWindow(shield) };
        if dpi == 0 {
            return Err(unavailable("Display DPI is unavailable"));
        }
        let scale = f64::from(dpi) / 96.0;
        let seed = Rect {
            width: 1.0,
            height: 1.0,
            ..bounds
        };
        let borders = [
            create_window(&mut overlays, owner, seed, false)?,
            create_window(&mut overlays, owner, seed, false)?,
            create_window(&mut overlays, owner, seed, false)?,
            create_window(&mut overlays, owner, seed, false)?,
        ];
        let label_bounds = Rect {
            x: bounds.x + (16.0 * scale).round(),
            y: bounds.y + (16.0 * scale).round(),
            width: (bounds.width - 32.0 * scale).round().max(1.0),
            height: (64.0 * scale).round(),
        };
        let label = create_window(&mut overlays, owner, label_bounds, false)?;
        set_label(
            label,
            "Point to a verified window · Click to select · Escape / right-click to cancel",
        )?;
        // Nonzero alpha shields ALL pixels. No WS_EX_TRANSPARENT, color key,
        // screen sampling, hit-test hiding, keyboard hook or target activation.
        unsafe {
            if SetLayeredWindowAttributes(shield, 0, 20, LWA_ALPHA) == 0 {
                return Err(unavailable("Could not establish the picker click shield"));
            }
            ShowWindow(shield, SW_SHOWNOACTIVATE);
            ShowWindow(label, SW_SHOWNOACTIVATE);
        }
        displays.push(Display {
            bounds,
            shield,
            borders,
            label,
            label_bounds,
            scale,
        });
    }
    let mut click = Click::default();
    let mut previous_hit = None;
    loop {
        // SAFETY: Reads only our owner and overlay handles. Focus loss cancels,
        // rather than trying to recover focus or continue over another application.
        if snapshot.cancelled.load(Ordering::Acquire)
            || !foreground_is_ours()
            || unsafe { IsWindow(owner) == 0 || IsWindowVisible(owner) == 0 }
        {
            return Ok(None);
        }
        let point = cursor()?;
        let current_hit = hit(snapshot, &overlays.windows, &displays, point)?;
        if current_hit != previous_hit {
            draw_selection(&displays, snapshot, current_hit)?;
            previous_hit = current_hit;
        }
        let mut message = MSG::default();
        // SAFETY: This is the creating UI thread's queue. Non-picker messages are
        // dispatched normally; picker clicks/Escape are consumed, never replayed.
        while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_QUIT {
                unsafe { PostQuitMessage(message.wParam as i32) };
                return Ok(None);
            }
            if message.message == DISPLAY_CHANGED || message.message == WM_DISPLAYCHANGE {
                return Err(changed());
            }
            if message.message == WM_KEYDOWN && message.wParam == 0x1b {
                return Ok(None);
            }
            if overlays.windows.contains(&message.hwnd)
                && (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&message.message)
            {
                match message.message {
                    WM_LBUTTONDOWN | WM_LBUTTONUP => {
                        // MSG.pt belongs to this queued click, unlike GetCursorPos.
                        let point = (f64::from(message.pt.x), f64::from(message.pt.y));
                        let hit = hit(snapshot, &overlays.windows, &displays, point)?;
                        if message.message == WM_LBUTTONDOWN {
                            click.press(hit);
                        } else if let Some(index) =
                            click.release(hit, snapshot.cancelled.load(Ordering::Acquire))
                        {
                            return Ok(Some(snapshot.candidates[index].id.clone()));
                        }
                    }
                    WM_RBUTTONUP => return Ok(None),
                    _ => {}
                }
            } else {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            if snapshot.cancelled.load(Ordering::Acquire) || !foreground_is_ours() {
                return Ok(None);
            }
        }
        unsafe { MsgWaitForMultipleObjectsEx(0, null(), 30, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
    }
}

#[expect(unsafe_code, reason = "read-only foreground process attribution")]
fn foreground_is_ours() -> bool {
    let mut pid = 0;
    // SAFETY: Only local output is written; no process/window activation occurs.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid == std::process::id()
}

#[expect(unsafe_code, reason = "read-only cursor desktop position")]
fn cursor() -> Result<(f64, f64), Fault> {
    let mut point = POINT::default();
    // SAFETY: Valid stack output pointer in physical per-monitor-v2 context.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err(unavailable("Pointer position is unavailable"));
    }
    Ok((f64::from(point.x), f64::from(point.y)))
}

fn from_rect(rect: RECT) -> Result<Rect, Fault> {
    Rect::checked(
        f64::from(rect.left),
        f64::from(rect.top),
        f64::from(rect.right) - f64::from(rect.left),
        f64::from(rect.bottom) - f64::from(rect.top),
    )
}

fn integer_rect(rect: Rect) -> Result<(i32, i32, i32, i32), Fault> {
    if ![
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        rect.x + rect.width,
        rect.y + rect.height,
    ]
    .iter()
    .all(|value| {
        value.is_finite() && *value >= f64::from(i32::MIN) && *value <= f64::from(i32::MAX)
    }) || rect.width < 1.0
        || rect.height < 1.0
    {
        return Err(unavailable(
            "Physical display geometry cannot be represented",
        ));
    }
    Ok((
        rect.x.round() as i32,
        rect.y.round() as i32,
        rect.width.round() as i32,
        rect.height.round() as i32,
    ))
}

struct MonitorList {
    bounds: Vec<Rect>,
    failed: bool,
}

#[expect(
    unsafe_code,
    reason = "EnumDisplayMonitors borrows this stack context only for its synchronous callback"
)]
unsafe extern "system" fn monitor_callback(
    _: HMONITOR,
    _: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: monitors passes a live unique MonitorList; Win32 supplies RECT for
    // this callback only. Neither reference escapes or is stored in a window.
    let list = unsafe { &mut *(data as *mut MonitorList) };
    if rect.is_null() || list.bounds.len() == 32 {
        list.failed = true;
        return 0;
    }
    match from_rect(unsafe { *rect }) {
        Ok(bounds) => list.bounds.push(bounds),
        Err(_) => {
            list.failed = true;
            return 0;
        }
    }
    1
}

#[expect(unsafe_code, reason = "bounded read-only monitor enumeration")]
fn monitors() -> Result<Vec<Rect>, Fault> {
    let mut list = MonitorList {
        bounds: Vec::new(),
        failed: false,
    };
    // SAFETY: The synchronous callback cannot outlive the stack list.
    let success = unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(monitor_callback),
            (&raw mut list) as LPARAM,
        )
    };
    if success == 0 || list.failed || list.bounds.is_empty() {
        return Err(unavailable(
            "Display topology is unavailable or exceeds the picker bound",
        ));
    }
    Ok(list.bounds)
}

#[expect(
    unsafe_code,
    reason = "register a private class whose callbacks retain no Rust pointers"
)]
fn create_class(clean: &Cell<bool>) -> Result<Overlays<'_>, Fault> {
    // SAFETY: Static class name and callback outlive registration. Brush ownership
    // transfers to Overlays only after registration; all failure paths free it.
    unsafe {
        let instance = GetModuleHandleW(null());
        let brush = CreateSolidBrush(ACCENT);
        if instance.is_null() || brush.is_null() {
            if !brush.is_null() {
                DeleteObject(brush);
            }
            return Err(unavailable("Could not allocate visual picker resources"));
        }
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_CROSS),
            hbrBackground: brush,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            DeleteObject(brush);
            return Err(unavailable(
                "Visual picker is already open or its window class is unavailable",
            ));
        }
        Ok(Overlays {
            clean,
            instance,
            brush,
            windows: Vec::new(),
        })
    }
}

#[expect(unsafe_code, reason = "create only owned nonactivating popup windows")]
fn create_window(
    overlays: &mut Overlays<'_>,
    owner: HWND,
    bounds: Rect,
    shield: bool,
) -> Result<HWND, Fault> {
    let (x, y, width, height) = integer_rect(bounds)?;
    // SAFETY: All pointers are static or borrowed by this synchronous call. No
    // userdata/callback points into Rust allocation. WS_EX_NOACTIVATE and the
    // mouse-activate handler forbid changing the target's focus.
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE
                | if shield { WS_EX_LAYERED } else { 0 },
            CLASS_NAME,
            windows_sys::core::w!(""),
            WS_POPUP,
            x,
            y,
            width,
            height,
            owner,
            null_mut(),
            overlays.instance,
            null(),
        )
    };
    if window.is_null() {
        return Err(unavailable("Could not create a safe visual picker window"));
    }
    overlays.windows.push(window);
    let mut actual = RECT::default();
    // SAFETY: Read only the newly owned window. Refuse OS-adjusted geometry rather
    // than leave an uncovered strip which could forward a click to another app.
    if unsafe { GetWindowRect(window, &mut actual) } == 0
        || actual.left != x
        || actual.top != y
        || i64::from(actual.right) - i64::from(actual.left) != i64::from(width)
        || i64::from(actual.bottom) - i64::from(actual.top) != i64::from(height)
    {
        return Err(unavailable(
            "The OS could not place a safe per-display picker shield",
        ));
    }
    Ok(window)
}

#[expect(
    unsafe_code,
    reason = "native callback uses only Win32-owned window state and bounded stack buffers"
)]
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Win32 owns the callback window during dispatch; no Rust state is
    // dereferenced and no callback survives class unregistration.
    unsafe {
        match message {
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                PostMessageW(window, DISPLAY_CHANGED, 0, 0);
                0
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(window, &mut paint);
                if !dc.is_null() {
                    let mut text = [0_u16; 2048];
                    let length = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32);
                    if length > 0 {
                        let mut bounds = RECT::default();
                        GetClientRect(window, &mut bounds);
                        SetBkMode(dc, TRANSPARENT as i32);
                        SetTextColor(dc, 0);
                        DrawTextW(
                            dc,
                            text.as_ptr(),
                            length,
                            &mut bounds,
                            DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
                        );
                    }
                }
                EndPaint(window, &paint);
                0
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

#[expect(
    unsafe_code,
    reason = "set bounded text on our own identity-feedback window"
)]
fn set_label(window: HWND, text: &str) -> Result<(), Fault> {
    let text: Vec<u16> = text.encode_utf16().take(2047).chain(Some(0)).collect();
    // SAFETY: UTF-16 buffer is terminated and lives through the synchronous copy.
    if unsafe { SetWindowTextW(window, text.as_ptr()) } == 0 {
        return Err(unavailable("Picker identity feedback is unavailable"));
    }
    unsafe { InvalidateRect(window, null(), 1) };
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "position only owned outline bars in physical desktop coordinates"
)]
fn draw_selection(
    displays: &[Display],
    snapshot: &NativePickerSnapshot,
    hit: Option<usize>,
) -> Result<(), Fault> {
    for display in displays {
        let Some(index) = hit else {
            for border in display.borders {
                unsafe { ShowWindow(border, SW_HIDE) };
            }
            set_label(
                display.label,
                "No eligible window here · Escape / right-click to cancel",
            )?;
            continue;
        };
        let candidate = &snapshot.candidates[index];
        let bounds = Rect::from(&candidate.bounds);
        let stroke = (3.0 * display.scale)
            .round()
            .max(1.0)
            .min(bounds.width)
            .min(bounds.height);
        let bars = [
            Rect {
                height: stroke,
                ..bounds
            },
            Rect {
                y: bounds.y + bounds.height - stroke,
                height: stroke,
                ..bounds
            },
            Rect {
                width: stroke,
                ..bounds
            },
            Rect {
                x: bounds.x + bounds.width - stroke,
                width: stroke,
                ..bounds
            },
        ];
        for (window, bar) in display.borders.into_iter().zip(bars) {
            if let Some(clipped) = bar.intersection(display.bounds) {
                let (x, y, width, height) = integer_rect(clipped)?;
                // SAFETY: window is our border, never the selected game HWND.
                if unsafe {
                    SetWindowPos(
                        window,
                        HWND_TOPMOST,
                        x,
                        y,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    )
                } == 0
                {
                    return Err(unavailable("Could not update visual selection outline"));
                }
            } else {
                unsafe { ShowWindow(window, SW_HIDE) };
            }
        }
        set_label(
            display.label,
            &format!(
                "{} — {}\nClick to select · Escape / right-click to cancel",
                candidate.application_label, candidate.window_label
            ),
        )?;
    }
    Ok(())
}

struct HitTest<'a> {
    snapshot: &'a NativePickerSnapshot,
    overlays: &'a [HWND],
    point: (f64, f64),
    front: Option<ObservedWindow>,
    failure: Option<Fault>,
    count: usize,
}

#[expect(
    unsafe_code,
    reason = "EnumWindows synchronously borrows a private hit-test context"
)]
unsafe extern "system" fn window_callback(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: hit passes a live unique HitTest for this synchronous enumeration.
    let state = unsafe { &mut *(data as *mut HitTest<'_>) };
    state.count += 1;
    if state.count > 4096 {
        state.failure = Some(unavailable(
            "Desktop window metadata exceeds the picker bound",
        ));
        return 0;
    }
    // SAFETY: Only metadata reads. The OS validates HWNDs; failure refuses the hit.
    unsafe {
        if state.overlays.contains(&window) || IsWindowVisible(window) == 0 || IsIconic(window) != 0
        {
            return 1;
        }
        let mut cloaked = 0_u32;
        if DwmGetWindowAttribute(window, DWMWA_CLOAKED as u32, (&raw mut cloaked).cast(), 4) >= 0
            && cloaked != 0
        {
            return 1;
        }
        let mut outer = RECT::default();
        if GetWindowRect(window, &mut outer) == 0 {
            state.failure = Some(changed());
            return 0;
        }
        let Ok(outer) = from_rect(outer) else {
            return 1;
        };
        if !outer.contains(state.point) {
            return 1;
        }
        let id = window as usize as u64;
        let mut process_id = 0;
        if GetWindowThreadProcessId(window, &mut process_id) == 0 {
            state.failure = Some(changed());
            return 0;
        }
        let bounds = if let Some(candidate) = state
            .snapshot
            .candidates
            .iter()
            .find(|candidate| candidate.native_window_id == id)
        {
            match capture_bounds(window, candidate.capture_area) {
                Ok(bounds) => bounds,
                Err(error) => {
                    state.failure = Some(error);
                    return 0;
                }
            }
        } else {
            outer
        };
        // EnumWindows is front-to-back. Never search below an ineligible cover,
        // including MadoMata's editor/preview; only our known shield/bar IDs skip.
        state.front = Some(ObservedWindow {
            id,
            process_id,
            capture_bounds: bounds,
        });
    }
    0
}

#[expect(
    unsafe_code,
    reason = "read only the exact capture-area metadata kind selected by the host"
)]
fn capture_bounds(window: HWND, area: NativeCaptureArea) -> Result<Rect, Fault> {
    let mut bounds = RECT::default();
    // SAFETY: Valid stack outputs; a stale HWND/API failure cannot produce selection.
    unsafe {
        match area {
            NativeCaptureArea::WindowsExtendedFrame => {
                if DwmGetWindowAttribute(
                    window,
                    DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                    (&raw mut bounds).cast(),
                    std::mem::size_of::<RECT>() as u32,
                ) < 0
                {
                    return Err(changed());
                }
            }
            NativeCaptureArea::WindowsClient => {
                if GetClientRect(window, &mut bounds) == 0 {
                    return Err(changed());
                }
                let mut top_left = POINT {
                    x: bounds.left,
                    y: bounds.top,
                };
                let mut bottom_right = POINT {
                    x: bounds.right,
                    y: bounds.bottom,
                };
                if ClientToScreen(window, &mut top_left) == 0
                    || ClientToScreen(window, &mut bottom_right) == 0
                {
                    return Err(changed());
                }
                bounds = RECT {
                    left: top_left.x,
                    top: top_left.y,
                    right: bottom_right.x,
                    bottom: bottom_right.y,
                };
            }
            NativeCaptureArea::MacosWindow => {
                return Err(unavailable("Capture geometry belongs to another platform"));
            }
        }
    }
    from_rect(bounds)
}

#[expect(
    unsafe_code,
    reason = "read-only z-order enumeration without hiding overlays"
)]
fn hit(
    snapshot: &NativePickerSnapshot,
    overlays: &[HWND],
    displays: &[Display],
    point: (f64, f64),
) -> Result<Option<usize>, Fault> {
    validate_displays(displays)?;
    if displays
        .iter()
        .any(|display| display.label_bounds.contains(point))
    {
        return Ok(None);
    }
    let mut state = HitTest {
        snapshot,
        overlays,
        point,
        front: None,
        failure: None,
        count: 0,
    };
    // SAFETY: Callback borrows state synchronously; it never retains a pointer.
    let completed = unsafe { EnumWindows(Some(window_callback), (&raw mut state) as LPARAM) };
    if let Some(error) = state.failure {
        return Err(error);
    }
    if completed == 0 && state.front.is_none() {
        return Err(unavailable("Desktop z-order is unavailable"));
    }
    eligible_hit(&snapshot.candidates, state.front, point)
}

struct DisplayCheck<'a> {
    displays: &'a [Display],
    count: usize,
    changed: bool,
}

#[expect(
    unsafe_code,
    reason = "synchronous monitor callback borrows a bounded display snapshot"
)]
unsafe extern "system" fn display_check_callback(
    _: HMONITOR,
    _: HDC,
    bounds: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: validate_displays supplies a unique stack context for this call only.
    let check = unsafe { &mut *(data as *mut DisplayCheck<'_>) };
    let Some(expected) = check.displays.get(check.count) else {
        check.changed = true;
        return 0;
    };
    if bounds.is_null() || from_rect(unsafe { *bounds }).ok() != Some(expected.bounds) {
        check.changed = true;
        return 0;
    }
    check.count += 1;
    1
}

#[expect(
    unsafe_code,
    reason = "metadata-only per-monitor topology and DPI revalidation"
)]
fn validate_displays(displays: &[Display]) -> Result<(), Fault> {
    let mut check = DisplayCheck {
        displays,
        count: 0,
        changed: false,
    };
    // SAFETY: Callback borrows check synchronously and never retains it.
    let complete = unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(display_check_callback),
            (&raw mut check) as LPARAM,
        )
    };
    if complete == 0 || check.changed || check.count != displays.len() {
        return Err(changed());
    }
    for display in displays {
        // SAFETY: Only our retained shield's DPI is queried.
        if unsafe { GetDpiForWindow(display.shield) } != (display.scale * 96.0) as u32 {
            return Err(changed());
        }
    }
    Ok(())
}
