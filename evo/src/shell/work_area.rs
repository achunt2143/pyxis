//! The work area: the part of an output that app windows may use.
//!
//! The shell's top bar and dock are drawn by Bloom's overlay, which Evo keeps
//! above every app (see `super::overlay`). Left alone, a maximized app would
//! sit under them. Bloom therefore reports the edges it occupies
//! (`org.geckoOS.evo SetWorkArea`: `{top, bottom, left, right}` in logical
//! pixels), and every place that decides where a window may go uses the
//! output minus those insets: maximize, first placement, restoring from a
//! fullscreen/maximize, the move clamp and the resize clamp. Fullscreen
//! still covers the whole output.
//!
//! Until Bloom reports, the insets are zero, so a Bloom that never starts
//! leaves apps the whole screen, as before. They are process-wide, like the
//! overlay's input region: there is one shell per session.

use std::sync::RwLock;

use smithay::utils::{Logical, Point, Rectangle, Size};

use super::grabs::ResizeEdge;

/// No inset may exceed this, whatever the caller says.
pub const MAX_INSET: i32 = 2000;

/// Edges of an output the shell occupies, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Insets {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
}

static INSETS: RwLock<Insets> = RwLock::new(Insets { top: 0, bottom: 0, left: 0, right: 0 });

/// The insets in force.
pub fn insets() -> Insets {
    INSETS.read().map(|i| *i).unwrap_or_default()
}

/// Replace the insets in force. Returns whether they changed.
pub fn set_insets(new: Insets) -> bool {
    match INSETS.write() {
        Ok(mut insets) => {
            let changed = *insets != new;
            *insets = new;
            changed
        }
        Err(_) => false,
    }
}

impl Insets {
    /// `area` minus these insets. If they would leave less than half of an
    /// axis, they are ignored on that axis: a shell bug must not squeeze
    /// every window to nothing.
    pub fn shrink(&self, area: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
        let (mut x, mut w) = (area.loc.x, area.size.w);
        let (mut y, mut h) = (area.loc.y, area.size.h);
        if w - self.left - self.right >= w / 2 {
            x += self.left;
            w -= self.left + self.right;
        }
        if h - self.top - self.bottom >= h / 2 {
            y += self.top;
            h -= self.top + self.bottom;
        }
        Rectangle::new((x, y).into(), (w, h).into())
    }
}

/// The work area of an output, from its geometry and the layer-shell
/// exclusive zone (which is the whole output when no layer surface reserves
/// anything).
pub fn usable(output_geo: Rectangle<i32, Logical>, layer_zone: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    insets().shrink(Rectangle::new(output_geo.loc + layer_zone.loc, layer_zone.size))
}

/// `output_geo` minus the insets in force.
pub fn of_output(output_geo: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    insets().shrink(output_geo)
}

/// Where a dragged window's frame may be: all of it inside the work area, so
/// neither its title bar nor its resize band can end up under the shell's top
/// bar or dock, or off the screen. A frame larger than the area (one that has
/// not been fitted yet, see `fit_frame`) is pinned to the area's top-left.
pub fn clamp_move(
    location: Point<i32, Logical>,
    frame: Size<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let max_x = (area.loc.x + area.size.w - frame.w).max(area.loc.x);
    let max_y = (area.loc.y + area.size.h - frame.h).max(area.loc.y);
    (
        location.x.clamp(area.loc.x, max_x),
        location.y.clamp(area.loc.y, max_y),
    )
        .into()
}

/// The frame a floating window should have: no bigger than the work area
/// (but never smaller than `min`, the client's own minimum, which wins when
/// it is larger than the area), and placed so all of it is inside.
pub fn fit_frame(
    location: Point<i32, Logical>,
    frame: Size<i32, Logical>,
    min: Size<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> (Point<i32, Logical>, Size<i32, Logical>) {
    let size: Size<i32, Logical> = (
        frame.w.min(area.size.w).max(min.w.min(frame.w)),
        frame.h.min(area.size.h).max(min.h.min(frame.h)),
    )
        .into();
    (clamp_move(location, size, area), size)
}

/// The largest a frame being resized from `edges` may become: the edge being
/// dragged stays inside the work area (the opposite edge does not move). A
/// frame that already exceeds a limit may keep its current size.
pub fn max_resize_frame(
    edges: ResizeEdge,
    initial_location: Point<i32, Logical>,
    initial_size: Size<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Size<i32, Logical> {
    let (mut w, mut h) = (i32::MAX, i32::MAX);
    if edges.intersects(ResizeEdge::LEFT) {
        w = w.min(initial_location.x + initial_size.w - area.loc.x);
    }
    if edges.intersects(ResizeEdge::RIGHT) {
        w = w.min(area.loc.x + area.size.w - initial_location.x);
    }
    if edges.intersects(ResizeEdge::TOP) {
        h = h.min(initial_location.y + initial_size.h - area.loc.y);
    }
    if edges.intersects(ResizeEdge::BOTTOM) {
        h = h.min(area.loc.y + area.size.h - initial_location.y);
    }
    (w.max(initial_size.w), h.max(initial_size.h)).into()
}

/// Parse `SetWorkArea`'s arguments: `{ "top": 44, "bottom": 80, "left": 0,
/// "right": 0 }`. Missing edges are zero.
pub fn parse_insets(body: &serde_json::Value) -> Result<Insets, String> {
    let edge = |name: &str| -> Result<i32, String> {
        match body.get(name) {
            None | Some(serde_json::Value::Null) => Ok(0),
            Some(v) => {
                let n = v.as_f64().ok_or_else(|| format!("{name} must be a number"))?;
                if !n.is_finite() || !(0.0..=f64::from(MAX_INSET)).contains(&n) {
                    return Err(format!("{name} must be between 0 and {MAX_INSET}"));
                }
                Ok(n.round() as i32)
            }
        }
    };
    Ok(Insets {
        top: edge("top")?,
        bottom: edge("bottom")?,
        left: edge("left")?,
        right: edge("right")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn insets_shrink_the_output_from_each_edge() {
        let insets = Insets { top: 44, bottom: 80, left: 0, right: 0 };
        assert_eq!(insets.shrink(rect(0, 0, 1280, 800)), rect(0, 44, 1280, 676));
        assert_eq!(insets.shrink(rect(100, 50, 1280, 800)), rect(100, 94, 1280, 676));
    }

    #[test]
    fn absurd_insets_are_ignored_on_the_axis_they_ruin() {
        let insets = Insets { top: 700, bottom: 0, left: 10, right: 10 };
        assert_eq!(insets.shrink(rect(0, 0, 1000, 800)), rect(10, 0, 980, 800));
    }

    #[test]
    fn a_left_rail_inset_moves_the_area_right_and_keeps_windows_off_it() {
        // The launch bar as a rail: 36 px strip on top, 64 px down the left.
        let insets = Insets { top: 36, bottom: 0, left: 64, right: 0 };
        let area = insets.shrink(rect(0, 0, 1280, 800));
        assert_eq!(area, rect(64, 36, 1216, 764));
        // A maximized window's frame is the area: it starts right of the rail.
        assert_eq!((area.loc.x, area.size.w), (64, 1216));
        // A dragged window can't go under the rail, left or top.
        assert_eq!(clamp_move((-50, 0).into(), (600, 400).into(), area), (64, 36).into());
        // A frame placed at the screen's edge (a restored window) is fitted inside.
        let (loc, size) = fit_frame((0, 24).into(), (1280, 800).into(), (0, 0).into(), area);
        assert_eq!(size, (1216, 764).into());
        assert_eq!(loc, (64, 36).into());
        // A left-edge resize cannot push the window under the rail.
        let max = max_resize_frame(ResizeEdge::LEFT, (300, 100).into(), (500, 400).into(), area);
        assert_eq!(max.w, 300 + 500 - 64);
        // And the parsed `SetWorkArea` body carries the left inset.
        assert_eq!(parse_insets(&json!({ "top": 36, "bottom": 0, "left": 64 })).unwrap(), insets);
    }

    #[test]
    fn no_insets_change_nothing() {
        assert_eq!(Insets::default().shrink(rect(0, 0, 640, 480)), rect(0, 0, 640, 480));
    }

    #[test]
    fn parses_insets_and_defaults_missing_edges() {
        assert_eq!(
            parse_insets(&json!({ "top": 44, "bottom": 79.6 })).unwrap(),
            Insets { top: 44, bottom: 80, left: 0, right: 0 }
        );
        assert_eq!(parse_insets(&json!({})).unwrap(), Insets::default());
    }

    #[test]
    fn rejects_malformed_insets() {
        assert!(parse_insets(&json!({ "top": "tall" })).is_err());
        assert!(parse_insets(&json!({ "top": -1 })).is_err());
        assert!(parse_insets(&json!({ "bottom": 1e9 })).is_err());
    }

    #[test]
    fn a_dragged_window_stays_below_the_top_bar() {
        let area = rect(0, 44, 1280, 676);
        let frame = (600, 400).into();
        assert_eq!(clamp_move((100, 10).into(), frame, area), (100, 44).into());
        assert_eq!(clamp_move((100, 200).into(), frame, area), (100, 200).into());
    }

    #[test]
    fn a_dragged_window_stays_wholly_inside_the_work_area() {
        let area = rect(0, 44, 1280, 676);
        let frame = (600, 400).into();
        assert_eq!(clamp_move((-900, 100).into(), frame, area).x, 0);
        // Far right and far down: the frame's far corner meets the area's.
        assert_eq!(clamp_move((5000, 5000).into(), frame, area), (1280 - 600, 44 + 676 - 400).into());
    }

    #[test]
    fn a_frame_taller_than_the_area_is_pinned_to_its_top_left() {
        let area = rect(0, 44, 1280, 676);
        assert_eq!(clamp_move((64, 300).into(), (1152, 752).into(), area), (64, 44).into());
    }

    #[test]
    fn a_frame_is_shrunk_to_the_area_and_moved_inside_it() {
        let area = rect(0, 44, 1280, 676);
        // The restored Files window: 1152x752 at y=24.
        let (loc, size) = fit_frame((64, 24).into(), (1152, 752).into(), (0, 0).into(), area);
        assert_eq!(size, (1152, 676).into());
        assert_eq!(loc, (64, 44).into());
        // One that already fits is left alone.
        let (loc, size) = fit_frame((100, 100).into(), (800, 500).into(), (0, 0).into(), area);
        assert_eq!((loc, size), ((100, 100).into(), (800, 500).into()));
        // Too wide as well as too tall.
        let (_, size) = fit_frame((0, 0).into(), (2000, 900).into(), (0, 0).into(), area);
        assert_eq!(size, (1280, 676).into());
    }

    #[test]
    fn a_clients_own_minimum_beats_the_area() {
        let area = rect(0, 44, 1280, 676);
        let (_, size) = fit_frame((0, 44).into(), (900, 800).into(), (500, 700).into(), area);
        assert_eq!(size.h, 700, "the client can't go below its minimum");
        // A frame already smaller than its (odd) minimum is not grown.
        let (_, size) = fit_frame((0, 44).into(), (300, 300).into(), (500, 500).into(), area);
        assert_eq!(size, (300, 300).into());
    }

    #[test]
    fn a_resize_cannot_push_an_edge_out_of_the_work_area() {
        let area = rect(0, 44, 1280, 676);
        let at: Point<i32, Logical> = (100, 100).into();
        let size: Size<i32, Logical> = (300, 300).into();
        // Top edge: the bottom (at 400) stays put, so at most 400-44 tall.
        assert_eq!(max_resize_frame(ResizeEdge::TOP, at, size, area).h, 356);
        assert_eq!(max_resize_frame(ResizeEdge::TOP_LEFT, at, size, area), (400, 356).into());
        // Bottom/right edges: limited by the area's far edges.
        assert_eq!(max_resize_frame(ResizeEdge::BOTTOM, at, size, area).h, 44 + 676 - 100);
        assert_eq!(max_resize_frame(ResizeEdge::RIGHT, at, size, area).w, 1280 - 100);
        // Unrelated axes are not limited.
        assert_eq!(max_resize_frame(ResizeEdge::LEFT, at, size, area).h, i32::MAX);
    }

    #[test]
    fn a_window_already_beyond_the_limit_is_not_shrunk() {
        let area = rect(0, 44, 1280, 676);
        let size = max_resize_frame(ResizeEdge::TOP, (0, 10).into(), (300, 300).into(), area);
        assert_eq!(size.h, 300);
    }
}
