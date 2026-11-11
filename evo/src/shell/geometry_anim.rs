//! Starting a geometry transition (maximize, unmaximize, fullscreen in and
//! out, fit to the work area). The machinery and the technique are
//! `crate::anim::geometry`; this is the part that knows about windows, the
//! Space and the backend.
//!
//! **Call `begin_geometry_transition` before the configure goes out and before
//! the window is placed**: it reads the window's frame as it is, and takes the
//! snapshot of it (SSD header included, which fullscreen is about to take
//! away). The handlers that change a window's frame call it first; nothing
//! else needs to know that a transition runs.

use smithay::{
    desktop::space::SpaceElement,
    utils::{Logical, Rectangle, Size},
};

use super::WindowElement;
use crate::{
    anim::{
        self,
        geometry::{GeoMotion, GeoTransition},
        snapshot::Snapshot,
    },
    state::Backend,
    EvoState,
};

impl<BackendData: Backend> EvoState<BackendData> {
    /// `window`'s frame is about to change: draw it moving from where it is to
    /// where it lands. `expected` is the frame size that has been asked for,
    /// when it is known (maximize, fullscreen, a fit): equal to the current
    /// size means only the place changes, which needs no snapshot and no
    /// waiting. `None` is "the client chooses" (an unmaximize, an unfullscreen).
    ///
    /// Does nothing (the change is instant) when animations are off, when the
    /// window has not been shown yet or is not on screen, when another
    /// animation owns it (open, minimize), or when there is no way to take a
    /// snapshot (only the udev backend can).
    pub fn begin_geometry_transition(&mut self, window: &WindowElement, expected: Option<Size<i32, Logical>>) {
        let settings = anim::settings();
        if !settings.enabled() {
            window.cancel_geometry();
            return;
        }
        if !self.animation_eligible(window)
            || !window.has_opened()
            || window.placement_pending()
            || !self.is_window_mapped(window)
            || window.animating()
        {
            return;
        }
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        let geometry = SpaceElement::geometry(window);
        let frame = Rectangle::new(location, geometry.size);
        if frame.size.w <= 0 || frame.size.h <= 0 {
            return;
        }
        let size_changes = expected != Some(frame.size);
        let wait = size_changes.then_some(frame.size);
        let now = anim::now();

        if window.geometry_animating() {
            // Already moving. A size change takes a fresh snapshot of the
            // live window and goes back to waiting, with the springs still
            // carrying on; otherwise the running one retargets by itself as
            // the live frame moves.
            if !size_changes {
                return;
            }
            let Some(snapshot) = self.snapshot_of(window) else {
                return;
            };
            let render_origin = location - geometry.loc;
            window.with_geometry(|t| {
                t.retake(snapshot, render_origin);
                t.motion.interrupt(now, frame, wait);
            });
            return;
        }

        let snapshot = if size_changes {
            match self.snapshot_of(window) {
                Some(s) => Some(s),
                // Nothing to hold the old frame: instant.
                None => return,
            }
        } else {
            None
        };
        let Some(motion) = GeoMotion::start(&settings, now, frame, snapshot.is_some(), wait, frame) else {
            return;
        };
        window.start_geometry(GeoTransition::new(motion, snapshot, location - geometry.loc));
    }

    fn snapshot_of(&mut self, window: &WindowElement) -> Option<Snapshot> {
        let output = self
            .space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())?;
        self.backend_data
            .snapshot_window(window, output.current_scale().fractional_scale())
    }
}
