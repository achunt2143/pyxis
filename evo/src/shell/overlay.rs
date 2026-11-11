//! Bloom's overlay window: the shell UI that has to sit above app windows.
//!
//! Bloom is the desktop, so Evo stacks it beneath every app (see
//! `WindowElement::z_index`). That is right for the desktop and wrong for
//! the parts of the shell that must be seen over apps: notification toasts
//! now, and later the lock screen. Those live in a second Bloom window, the
//! overlay. vela opens it in the shell's own process when launched with
//! `--pyxis-overlay <entry>` and gives it the app id `bloom-overlay`
//! (browser-init.js's PYXIS block). It covers the whole output and is
//! transparent wherever Bloom draws nothing.
//!
//! Gecko can't give a toplevel an input region, so it can't say which of its
//! pixels are clickable. Bloom tells Evo instead: `org.geckoOS.evo
//! SetOverlayRegion` sends the rectangles it currently draws in (a toast
//! stack, say), and the overlay takes input only there. Everywhere else a
//! click falls through to the window beneath. Until Bloom sends a region the
//! overlay takes no input at all, so a Bloom that fails to report one never
//! blocks the desktop.
//!
//! The overlay holds the whole shell UI, not only toasts: the top bar, the
//! dock and every panel (see `pyxis-experience/src/overlay`). Panels need the
//! keyboard (search typing), so Bloom can ask for it with `SetOverlayFocus`,
//! and for the edges apps must leave alone with `SetWorkArea` (see
//! `super::work_area`). Evo never gives the overlay the keyboard on its own.
//!
//! There is one overlay per session, so the region is a process-wide value.
//! The Echo thread writes it; the compositor thread reads it when it
//! hit-tests.

use std::sync::{atomic::{AtomicBool, Ordering}, RwLock};

use smithay::utils::{Logical, Point, Rectangle};

/// The most rectangles a region may hold. A toast stack needs a handful.
pub const MAX_RECTS: usize = 64;

static REGION: RwLock<Vec<Rectangle<i32, Logical>>> = RwLock::new(Vec::new());

/// Whether the overlay has the seat's keyboard. Written by the compositor
/// thread when the focus moves (`EvoState::note_overlay_keyboard`); read by
/// the Echo thread for `GetOverlayState`.
static KEYBOARD: AtomicBool = AtomicBool::new(false);

/// Whether the overlay has the keyboard.
pub fn keyboard_held() -> bool {
    KEYBOARD.load(Ordering::Relaxed)
}

/// Record whether the overlay has the keyboard. Returns whether that changed.
pub fn note_keyboard(held: bool) -> bool {
    KEYBOARD.swap(held, Ordering::Relaxed) != held
}

/// The overlay window is gone (or never was): forget what it had asked for,
/// so a Bloom that crashed does not leave apps squeezed or clicks swallowed.
pub fn reset() {
    set_region(Vec::new());
    KEYBOARD.store(false, Ordering::Relaxed);
}

/// The region in force, for `GetOverlayState`.
pub fn region() -> Vec<Rectangle<i32, Logical>> {
    REGION.read().map(|r| r.clone()).unwrap_or_default()
}

/// Replace the overlay's input region. Coordinates are the overlay's own
/// surface coordinates, which are the output's, since the overlay covers it
/// from the origin.
pub fn set_region(rects: Vec<Rectangle<i32, Logical>>) {
    if let Ok(mut region) = REGION.write() {
        *region = rects;
    }
}

/// Whether the overlay takes input at `point`, in its surface coordinates.
pub fn region_contains(point: Point<f64, Logical>) -> bool {
    REGION
        .read()
        .map(|region| region.iter().any(|rect| rect.to_f64().contains(point)))
        .unwrap_or(false)
}

/// Parse `SetOverlayRegion`'s `rects` argument:
/// `[{ "x": 0, "y": 0, "width": 10, "height": 10 }, ...]`. Rectangles with no
/// area are dropped, since they can't take input anyway.
pub fn parse_rects(value: &serde_json::Value) -> Result<Vec<Rectangle<i32, Logical>>, String> {
    let list = value
        .as_array()
        .ok_or_else(|| "rects must be an array of {x, y, width, height}".to_string())?;
    if list.len() > MAX_RECTS {
        return Err(format!("at most {MAX_RECTS} rects, got {}", list.len()));
    }
    let mut rects = Vec::with_capacity(list.len());
    for (i, rect) in list.iter().enumerate() {
        let field = |name: &str| -> Result<i32, String> {
            let n = rect
                .get(name)
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| format!("rects[{i}].{name} must be a number"))?;
            if !n.is_finite() || n.abs() > 1e6 {
                return Err(format!("rects[{i}].{name} is out of range"));
            }
            Ok(n.round() as i32)
        };
        let (x, y, w, h) = (field("x")?, field("y")?, field("width")?, field("height")?);
        if w > 0 && h > 0 {
            rects.push(Rectangle::new((x, y).into(), (w, h).into()));
        }
    }
    Ok(rects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_rects_and_drops_empty_ones() {
        let rects = parse_rects(&json!([
            { "x": 900, "y": 40, "width": 360.4, "height": 96 },
            { "x": 0, "y": 0, "width": 0, "height": 50 },
        ]))
        .unwrap();
        assert_eq!(rects, vec![Rectangle::new((900, 40).into(), (360, 96).into())]);
    }

    #[test]
    fn rejects_malformed_regions() {
        assert!(parse_rects(&json!({ "x": 1 })).is_err());
        assert!(parse_rects(&json!([{ "x": 1, "y": 2, "width": "wide", "height": 3 }])).is_err());
        assert!(parse_rects(&json!([{ "x": 1e9, "y": 2, "width": 3, "height": 3 }])).is_err());
        let too_many: Vec<_> = (0..=MAX_RECTS)
            .map(|i| json!({ "x": i, "y": 0, "width": 1, "height": 1 }))
            .collect();
        assert!(parse_rects(&json!(too_many)).is_err());
    }

    #[test]
    fn takes_input_only_inside_the_region() {
        set_region(vec![Rectangle::new((900, 40).into(), (360, 96).into())]);
        assert!(region_contains((1000.0, 100.0).into()));
        assert!(!region_contains((100.0, 100.0).into()));
        assert!(!region_contains((1000.0, 200.0).into()));
        set_region(Vec::new());
        assert!(!region_contains((1000.0, 100.0).into()));
    }
}
