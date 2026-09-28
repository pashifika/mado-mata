use super::{Backend, background};
use mado_mata_desktop::application::{
    Application, AuthoringRef, NativeCaptureBounds, NativeCoordinateUnit, NativePickerCandidate,
    NativePickerSnapshot, NativeSelectionView,
};
use mado_runtime_comparison::model::Fault;
use std::sync::{Arc, atomic::Ordering};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Renderer input contains no PID, native window handle or rectangle.
#[tauri::command]
pub async fn native_pick_window(
    owner: AuthoringRef,
    revision: String,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Backend>,
) -> Result<Option<NativeSelectionView>, Fault> {
    let application = state.bootstrap.application()?;
    let worker = application.clone();
    let expected_owner = owner.clone();
    let expected_revision = revision.clone();
    let snapshot = background(move || {
        worker.native_prepare_picker(&expected_owner, &expected_revision)?;
        worker.native_picker_snapshot(&expected_owner, &expected_revision)
    })
    .await?;
    let generation = snapshot.selection_generation;
    let reservation = Reservation {
        application: application.clone(),
        owner: owner.clone(),
        revision: revision.clone(),
        generation,
        finished: false,
    };
    if snapshot.owner != owner || snapshot.package_revision != revision {
        return Err(Fault::new(
            "StaleNativePicker",
            "Verified candidates belong to another authoring request",
        ));
    }
    let (send, mut receive) = tauri::async_runtime::channel(1);
    let parent = window.clone();
    window
        .run_on_main_thread(move || {
            // Native handles never cross threads. pick() destroys every overlay before returning,
            // including on cancellation/error. A dropped receiver cannot leave the gate open early.
            let mut reservation = reservation;
            // Once native UI creation begins, unwind or incomplete cleanup must retain
            // the host gate. Only a clean return below permits release.
            reservation.finished = true;
            let selected = pick(&parent, &snapshot);
            let result = if selected
                .as_ref()
                .is_err_and(|error| error.category == "NativePickerCleanup")
            {
                selected
            } else {
                reservation.finish().and(selected)
            };
            let _ = send.try_send(result);
        })
        .map_err(|_| unavailable("Could not enter the visual picker on the UI thread"))?;
    let selected = receive
        .recv()
        .await
        .ok_or_else(|| unavailable("Visual picker did not finish"))??;
    match selected {
        Some(id) => background(move || {
            // Selection revalidates the exact retained lifetime and geometry; it never captures.
            application.native_select_candidate(&owner, &revision, generation, &id)
        })
        .await
        .map(Some),
        None => Ok(None),
    }
}

struct Reservation {
    application: Arc<Application>,
    owner: AuthoringRef,
    revision: String,
    generation: u64,
    finished: bool,
}

impl Reservation {
    fn finish(&mut self) -> Result<(), Fault> {
        self.finished = true;
        self.application
            .native_picker_finished(&self.owner, &self.revision, self.generation)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish();
        }
    }
}

fn pick(
    window: &tauri::WebviewWindow,
    snapshot: &NativePickerSnapshot,
) -> Result<Option<String>, Fault> {
    validate_snapshot(snapshot)?;
    if snapshot.cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    #[cfg(target_os = "macos")]
    return macos::pick(window, snapshot);
    #[cfg(windows)]
    return windows::pick(window, snapshot);
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = window;
        Err(unavailable(
            "Visual window selection requires macOS or Windows",
        ))
    }
}

fn unavailable(message: &str) -> Fault {
    Fault::new("NativePickerUnavailable", message)
}

fn changed() -> Fault {
    Fault::new(
        "NativeSelectionChanged",
        "Window ownership or capture geometry changed; discover and select again",
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Rect {
    fn checked(x: f64, y: f64, width: f64, height: f64) -> Result<Self, Fault> {
        if ![x, y, width, height, x + width, y + height]
            .iter()
            .all(|v| v.is_finite())
            || width <= 0.0
            || height <= 0.0
        {
            return Err(unavailable("Window/display geometry is unavailable"));
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    fn contains(self, point: (f64, f64)) -> bool {
        point.0 >= self.x
            && point.0 < self.x + self.width
            && point.1 >= self.y
            && point.1 < self.y + self.height
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self::checked(
            x,
            y,
            (self.x + self.width).min(other.x + other.width) - x,
            (self.y + self.height).min(other.y + other.height) - y,
        )
        .ok()
    }

    /// Quartz global top-left points -> AppKit global bottom-left points. The primary
    /// screen supplies the common origin, not a global backing-pixel scale.
    #[cfg(any(target_os = "macos", test))]
    fn appkit(self, primary_top: f64) -> Self {
        Self {
            y: primary_top - self.y - self.height,
            ..self
        }
    }
}

impl From<&NativeCaptureBounds> for Rect {
    fn from(value: &NativeCaptureBounds) -> Self {
        Self {
            x: value.x,
            y: value.y,
            width: value.width,
            height: value.height,
        }
    }
}

fn validate_snapshot(snapshot: &NativePickerSnapshot) -> Result<(), Fault> {
    if snapshot.candidates.is_empty() || snapshot.candidates.len() > 64 {
        return Err(unavailable(
            "Discover between one and 64 verified windows before visual selection",
        ));
    }
    for (index, candidate) in snapshot.candidates.iter().enumerate() {
        let bounds = &candidate.bounds;
        Rect::checked(bounds.x, bounds.y, bounds.width, bounds.height)?;
        if candidate.id.is_empty()
            || candidate.native_window_id == 0
            || candidate.process_id == 0
            || candidate.process_id == std::process::id()
            || snapshot.candidates[..index].iter().any(|previous| {
                previous.id == candidate.id
                    || previous.native_window_id == candidate.native_window_id
            })
        {
            return Err(unavailable("Verified candidate snapshot is invalid"));
        }
    }
    Ok(())
}

/// An OS z-order observation is not authority. Only the first non-overlay window
/// under the pointer may match a host candidate; ineligible occluders block it.
struct ObservedWindow {
    id: u64,
    process_id: u32,
    capture_bounds: Rect,
}

fn eligible_hit(
    candidates: &[NativePickerCandidate],
    front: Option<ObservedWindow>,
    point: (f64, f64),
) -> Result<Option<usize>, Fault> {
    let Some(front) = front else { return Ok(None) };
    let Some((index, candidate)) = candidates
        .iter()
        .enumerate()
        .find(|(_, candidate)| candidate.native_window_id == front.id)
    else {
        return Ok(None);
    };
    if candidate.process_id != front.process_id
        || Rect::from(&candidate.bounds) != front.capture_bounds
    {
        return Err(changed());
    }
    Ok(front.capture_bounds.contains(point).then_some(index))
}

/// Commit only after consuming BOTH halves of the same click over unchanged
/// metadata. A press followed by movement, occlusion or cancellation cannot select.
#[derive(Default)]
struct Click {
    pressed: Option<usize>,
}

impl Click {
    fn press(&mut self, hit: Option<usize>) {
        self.pressed = hit;
    }
    fn release(&mut self, hit: Option<usize>, cancelled: bool) -> Option<usize> {
        self.pressed
            .take()
            .filter(|pressed| !cancelled && Some(*pressed) == hit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, native: u64, rect: Rect) -> NativePickerCandidate {
        NativePickerCandidate {
            id: id.into(),
            application_label: "Game".into(),
            window_label: "Window".into(),
            native_window_id: native,
            process_id: 42,
            capture_area: mado_mata_desktop::application::NativeCaptureArea::WindowsExtendedFrame,
            bounds: NativeCaptureBounds {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                unit: NativeCoordinateUnit::WindowsPhysicalPixels,
            },
        }
    }

    #[test]
    fn own_windows_and_duplicate_native_ids_cannot_enter_visual_selection() {
        let bounds = Rect::checked(0.0, 0.0, 600.0, 400.0).unwrap();
        let mut snapshot = NativePickerSnapshot {
            owner: AuthoringRef {
                workspace: mado_mata_desktop::application::WorkspaceRef {
                    workspace_id: "workspace".into(),
                    revision: 1,
                },
                token: "authoring".into(),
            },
            package_revision: "revision".into(),
            selection_generation: 1,
            candidates: vec![candidate("first", 10, bounds)],
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        snapshot.candidates[0].process_id = std::process::id();
        assert!(validate_snapshot(&snapshot).is_err());
        snapshot.candidates[0].process_id = std::process::id().wrapping_add(1).max(1);
        snapshot.candidates.push(snapshot.candidates[0].clone());
        snapshot.candidates[1].id = "second".into();
        assert!(validate_snapshot(&snapshot).is_err());
    }

    #[test]
    fn client_capture_does_not_select_decorations_or_half_open_edges() {
        let bounds = Rect::checked(-100.0, 30.0, 600.0, 400.0).unwrap();
        let candidates = [candidate("opaque", 10, bounds)];
        let observed = || {
            Some(ObservedWindow {
                id: 10,
                process_id: 42,
                capture_bounds: bounds,
            })
        };
        assert_eq!(
            eligible_hit(&candidates, observed(), (0.0, 20.0)).unwrap(),
            None
        );
        assert_eq!(
            eligible_hit(&candidates, observed(), (500.0, 40.0)).unwrap(),
            None
        );
        assert_eq!(
            eligible_hit(&candidates, observed(), (-100.0, 30.0)).unwrap(),
            Some(0)
        );
    }

    #[test]
    fn ineligible_front_window_never_falls_through_to_candidate() {
        let bounds = Rect::checked(-500.0, 0.0, 600.0, 400.0).unwrap();
        let candidates = [candidate("opaque", 10, bounds)];
        let front = ObservedWindow {
            id: 11,
            process_id: 43,
            capture_bounds: bounds,
        };
        assert_eq!(
            eligible_hit(&candidates, Some(front), (0.0, 20.0)).unwrap(),
            None
        );
    }

    #[test]
    fn reused_window_and_changed_capture_area_refuse_selection() {
        let bounds = Rect::checked(0.0, 0.0, 600.0, 400.0).unwrap();
        let candidates = [candidate("opaque", 10, bounds)];
        let reused = ObservedWindow {
            id: 10,
            process_id: 43,
            capture_bounds: bounds,
        };
        assert!(eligible_hit(&candidates, Some(reused), (20.0, 20.0)).is_err());
        let moved = ObservedWindow {
            id: 10,
            process_id: 42,
            capture_bounds: Rect { x: 1.0, ..bounds },
        };
        assert!(eligible_hit(&candidates, Some(moved), (20.0, 20.0)).is_err());
    }

    #[test]
    fn click_must_release_on_same_candidate_and_cancel_wins() {
        let mut click = Click::default();
        click.press(Some(0));
        assert_eq!(click.release(Some(1), false), None);
        click.press(Some(0));
        assert_eq!(click.release(Some(0), true), None);
        assert_eq!(click.release(Some(0), false), None);
        click.press(Some(0));
        assert_eq!(click.release(Some(0), false), Some(0));
    }

    #[test]
    fn mixed_display_intersections_keep_physical_origins_not_primary_scale() {
        let candidate = Rect::checked(-100.0, -50.0, 300.0, 200.0).unwrap();
        let left = Rect::checked(-1920.0, -1080.0, 1920.0, 2160.0).unwrap();
        let right = Rect::checked(0.0, 0.0, 2560.0, 1440.0).unwrap();
        assert_eq!(
            candidate.intersection(left).unwrap(),
            Rect {
                x: -100.0,
                y: -50.0,
                width: 100.0,
                height: 200.0
            }
        );
        assert_eq!(
            candidate.intersection(right).unwrap(),
            Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 150.0
            }
        );
        assert!(!left.contains((0.0, 10.0)));
        assert!(right.contains((0.0, 10.0)));
    }

    #[test]
    fn quartz_to_appkit_preserves_negative_origins_without_pixel_scaling() {
        let bounds = Rect::checked(-1200.0, -300.0, 800.0, 600.0).unwrap();
        assert_eq!(
            bounds.appkit(900.0),
            Rect {
                x: -1200.0,
                y: 600.0,
                width: 800.0,
                height: 600.0
            }
        );
        assert!(Rect::checked(f64::NAN, 0.0, 1.0, 1.0).is_err());
        assert!(Rect::checked(0.0, 0.0, 0.0, 1.0).is_err());
    }
}
