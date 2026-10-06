use super::{
    Click, NativeCoordinateUnit, NativePickerSnapshot, ObservedWindow, Rect, changed, eligible_hit,
    unavailable,
};
use mado_mata_desktop::application::NativeCaptureArea;
use mado_runtime_comparison::model::Fault;
use objc2::{
    MainThreadMarker, MainThreadOnly,
    rc::{Retained, autoreleasepool},
};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBox, NSBoxType, NSColor, NSEvent, NSEventMask,
    NSEventType, NSFont, NSPanel, NSScreen, NSStatusWindowLevel, NSTextField, NSTitlePosition,
    NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};
use std::{cell::Cell, sync::atomic::Ordering};

struct ScreenOverlay<'a> {
    clean: &'a Cell<bool>,
    panel: Retained<NSPanel>,
    outline: Retained<NSBox>,
    label: Retained<NSTextField>,
    frame: Rect,
    scale: f64,
}

impl Drop for ScreenOverlay<'_> {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
        if self.panel.isVisible() {
            self.clean.set(false);
        }
    }
}

fn nsrect(rect: Rect) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.width, rect.height),
    )
}

fn rect(value: NSRect) -> Result<Rect, Fault> {
    Rect::checked(
        value.origin.x,
        value.origin.y,
        value.size.width,
        value.size.height,
    )
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
    reason = "AppKit window ownership and immutable system run-loop constant"
)]
fn pick_inner(
    window: &tauri::WebviewWindow,
    snapshot: &NativePickerSnapshot,
    clean: &Cell<bool>,
) -> Result<Option<String>, Fault> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| unavailable("Visual picker requires the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    if !app.isActive() {
        return Err(unavailable(
            "Return to the editor before opening visual selection",
        ));
    }
    for candidate in &snapshot.candidates {
        if candidate.bounds.unit != NativeCoordinateUnit::MacosGlobalPoints
            || candidate.capture_area != NativeCaptureArea::MacosWindow
        {
            return Err(unavailable(
                "Verified ScreenCaptureKit window geometry is required",
            ));
        }
    }
    let pointer = window
        .ns_window()
        .map_err(|_| unavailable("Editor window is unavailable"))?;
    // SAFETY: Tauri supplies a live NSWindow on its main thread. Retain it across
    // the local event loop because processing a close event may release Tauri's ownership.
    let parent = unsafe { Retained::<NSWindow>::retain(pointer.cast()) }
        .ok_or_else(|| unavailable("Editor window is unavailable"))?;
    let screens = NSScreen::screens(mtm);
    if screens.count() == 0 || screens.count() > 32 {
        return Err(unavailable(
            "Display topology is unavailable or exceeds the picker bound",
        ));
    }
    // AppKit's first screen is the menu-bar/Quartz origin screen, not mainScreen
    // (which follows the key window). Never multiply the desktop by its scale.
    let primary = rect(screens.objectAtIndex(0).frame())?;
    let primary_top = primary.y + primary.height;
    let mut overlays = Vec::with_capacity(screens.count());
    for screen in screens.iter() {
        overlays.push(create_overlay(&screen, mtm, clean)?);
    }
    let overlay_ids: Vec<u64> = overlays
        .iter()
        .map(|overlay| overlay.panel.windowNumber() as u64)
        .collect();
    let mut click = Click::default();
    let mut previous_hit = None;
    loop {
        let result = autoreleasepool(|_| -> Result<Option<Option<String>>, Fault> {
            if snapshot.cancelled.load(Ordering::Acquire) || !app.isActive() || !parent.isVisible()
            {
                return Ok(Some(None));
            }
            let point = NSEvent::mouseLocation();
            let desktop_point = (point.x, primary_top - point.y);
            let hit = picker_hit(
                snapshot,
                &overlays,
                &overlay_ids,
                desktop_point,
                primary_top,
            )?;
            if hit != previous_hit {
                draw_selection(&overlays, snapshot, hit, primary_top);
                previous_hit = hit;
            }
            let deadline = NSDate::dateWithTimeIntervalSinceNow(0.03);
            // SAFETY: Framework-exported immutable process-lifetime NSString constant.
            let mode = unsafe { NSDefaultRunLoopMode };
            let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&deadline),
                mode,
                true,
            ) else {
                return Ok(None);
            };
            let event_type = event.r#type();
            if event_type == NSEventType::KeyDown && event.keyCode() == 53 {
                return Ok(Some(None));
            }
            let owned_event = overlay_ids.contains(&(event.windowNumber() as u64));
            if owned_event {
                // Never send a picker mouse event to AppKit, the game, or a synthetic
                // input route. In particular, nonactivating panels never become key.
                if event_type == NSEventType::RightMouseUp {
                    return Ok(Some(None));
                }
                if event_type != NSEventType::LeftMouseDown
                    && event_type != NSEventType::LeftMouseUp
                {
                    return Ok(None);
                }
                let overlay = overlays
                    .iter()
                    .find(|overlay| overlay.panel.windowNumber() == event.windowNumber())
                    .ok_or_else(changed)?;
                // Use the queued click's coordinates, not a later cursor sample.
                let point = overlay.panel.convertPointToScreen(event.locationInWindow());
                let point = (point.x, primary_top - point.y);
                let hit = picker_hit(snapshot, &overlays, &overlay_ids, point, primary_top)?;
                if event_type == NSEventType::LeftMouseDown {
                    click.press(hit);
                } else if event_type == NSEventType::LeftMouseUp {
                    if let Some(index) =
                        click.release(hit, snapshot.cancelled.load(Ordering::Acquire))
                    {
                        return Ok(Some(Some(snapshot.candidates[index].id.clone())));
                    }
                }
            } else {
                app.sendEvent(&event);
            }
            Ok(None)
        })?;
        if let Some(selected) = result {
            // Drop destroys all panels before the host picker reservation is released.
            return Ok(selected);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "retained AppKit panels must not self-release on close"
)]
fn create_overlay<'a>(
    screen: &NSScreen,
    mtm: MainThreadMarker,
    clean: &'a Cell<bool>,
) -> Result<ScreenOverlay<'a>, Fault> {
    let frame = rect(screen.frame())?;
    let scale = screen.backingScaleFactor();
    if !scale.is_finite() || scale <= 0.0 {
        return Err(unavailable("Display backing scale is unavailable"));
    }
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer_screen(
        NSPanel::alloc(mtm),
        nsrect(Rect {
            x: 0.0,
            y: 0.0,
            ..frame
        }),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
        Some(screen),
    );
    // SAFETY: Retained owns the panel; close must not consume that retained reference.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setHasShadow(false);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(true);
    panel.setIgnoresMouseEvents(false);
    panel.setAcceptsMouseMovedEvents(false);
    panel.setLevel(NSStatusWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    // Nonzero alpha over the ENTIRE screen is the click shield. There are no
    // transparent hit-test holes, temporary orderOut calls, or global event taps.
    panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.0, 0.0, 0.0, 0.08,
    )));
    let content = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(frame.width, frame.height),
        ),
    );
    content.setClipsToBounds(true);
    let outline = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
    );
    outline.setBoxType(NSBoxType::Custom);
    outline.setTitlePosition(NSTitlePosition::NoTitle);
    outline.setBorderWidth(3.0);
    outline.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.15, 0.75, 1.0, 1.0,
    ));
    outline.setFillColor(&NSColor::clearColor());
    outline.setHidden(true);
    content.addSubview(&outline);
    let label = NSTextField::labelWithString(
        &NSString::from_str(
            "Point to a verified window · Click to select · Escape / right-click to cancel",
        ),
        mtm,
    );
    label.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    label.setTextColor(Some(&NSColor::whiteColor()));
    label.setBackgroundColor(Some(&NSColor::blackColor()));
    label.setDrawsBackground(true);
    label.setSelectable(false);
    label.setFrame(NSRect::new(
        NSPoint::new(16.0, (frame.height - 76.0).max(0.0)),
        NSSize::new((frame.width - 32.0).max(1.0), 52.0),
    ));
    content.addSubview(&label);
    panel.setContentView(Some(&content));
    let overlay = ScreenOverlay {
        clean,
        panel,
        outline,
        label,
        frame,
        scale,
    };
    // Set global coordinates explicitly after screen-local initialization; never
    // infer a secondary screen's origin from its backing-pixel scale.
    overlay.panel.setFrame_display(nsrect(frame), false);
    if rect(overlay.panel.frame())? != frame || overlay.panel.backingScaleFactor() != scale {
        return Err(unavailable(
            "The OS could not place a safe per-display picker shield",
        ));
    }
    overlay.panel.orderFrontRegardless();
    Ok(overlay)
}

fn draw_selection(
    overlays: &[ScreenOverlay<'_>],
    snapshot: &NativePickerSnapshot,
    hit: Option<usize>,
    primary_top: f64,
) {
    for overlay in overlays {
        if let Some(index) = hit {
            let candidate = &snapshot.candidates[index];
            let bounds = Rect::from(&candidate.bounds).appkit(primary_top);
            // Each panel is attached to its own NSScreen. AppKit handles that screen's
            // backing scale; clipping keeps the true outline, not a seam-sized rectangle.
            overlay.outline.setFrame(nsrect(Rect {
                x: bounds.x - overlay.frame.x,
                y: bounds.y - overlay.frame.y,
                ..bounds
            }));
            overlay
                .outline
                .setHidden(bounds.intersection(overlay.frame).is_none());
            overlay.label.setStringValue(&NSString::from_str(&format!(
                "{} — {}\nClick to select · Escape / right-click to cancel",
                candidate.application_label, candidate.window_label
            )));
        } else {
            overlay.outline.setHidden(true);
            overlay.label.setStringValue(&NSString::from_str(
                "No eligible window here · Escape / right-click to cancel",
            ));
        }
    }
}

fn picker_hit(
    snapshot: &NativePickerSnapshot,
    overlays: &[ScreenOverlay<'_>],
    ids: &[u64],
    point: (f64, f64),
    primary_top: f64,
) -> Result<Option<usize>, Fault> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| unavailable("Visual picker requires the main thread"))?;
    let current_screens = NSScreen::screens(mtm);
    if current_screens.count() != overlays.len() {
        return Err(changed());
    }
    for (screen, overlay) in current_screens.iter().zip(overlays) {
        if rect(screen.frame())? != overlay.frame || screen.backingScaleFactor() != overlay.scale {
            return Err(changed());
        }
    }
    for overlay in overlays {
        let label = rect(overlay.label.frame())?;
        let label = Rect {
            x: label.x + overlay.frame.x,
            y: label.y + overlay.frame.y,
            ..label
        };
        if label.contains((point.0, primary_top - point.1)) {
            return Ok(None);
        }
    }
    eligible_hit(&snapshot.candidates, front_window(point, ids)?, point)
}

#[expect(
    unsafe_code,
    reason = "CGWindow immutable metadata collection casts and exported dictionary keys"
)]
fn front_window(point: (f64, f64), overlays: &[u64]) -> Result<Option<ObservedWindow>, Fault> {
    let windows =
        CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, kCGNullWindowID)
            .ok_or_else(|| {
                unavailable("Window metadata is unavailable; visual selection cannot continue")
            })?;
    if windows.len() > 4096 {
        return Err(unavailable(
            "Desktop window metadata exceeds the picker bound",
        ));
    }
    // SAFETY: CGWindowListCopyWindowInfo returns an immutable CFArray of CFDictionary
    // objects. Each dictionary uses CFString keys and CFType values, checked below.
    let windows = unsafe { windows.cast_unchecked::<CFType>() };
    for value in windows.iter() {
        let dictionary = value.downcast_ref::<CFDictionary>().ok_or_else(changed)?;
        let dictionary = unsafe { dictionary.cast_unchecked::<CFString, CFType>() };
        let id = number(dictionary, unsafe { kCGWindowNumber })?;
        if !id.is_finite() || id < 1.0 || id.fract() != 0.0 || id > f64::from(u32::MAX) {
            return Err(changed());
        }
        let id = id as u64;
        if overlays.contains(&id) {
            continue;
        }
        let alpha = number(dictionary, unsafe { kCGWindowAlpha })?;
        if alpha == 0.0 {
            continue;
        }
        let value = dictionary
            .get(unsafe { kCGWindowBounds })
            .ok_or_else(changed)?;
        let bounds = value.downcast_ref::<CFDictionary>().ok_or_else(changed)?;
        let mut rectangle = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        // SAFETY: CGWindow supplies the immutable rectangle dictionary; the output
        // points to an initialized, exclusively borrowed stack value.
        if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &raw mut rectangle) } {
            return Err(changed());
        }
        let bounds = rect(rectangle)?;
        if !bounds.contains(point) {
            continue;
        }
        let pid = number(dictionary, unsafe { kCGWindowOwnerPID })?;
        if !pid.is_finite() || pid < 0.0 || pid.fract() != 0.0 || pid > f64::from(u32::MAX) {
            return Err(changed());
        }
        // The first covering window blocks all below, even our editor/preview or a
        // noncandidate system panel. Only known overlay IDs are removed from z-order.
        return Ok(Some(ObservedWindow {
            id,
            process_id: pid as u32,
            capture_bounds: bounds,
        }));
    }
    Ok(None)
}

fn number(dictionary: &CFDictionary<CFString, CFType>, key: &CFString) -> Result<f64, Fault> {
    dictionary
        .get(key)
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_f64))
        .ok_or_else(|| {
            unavailable("Window metadata is incomplete; visual selection cannot continue")
        })
}
