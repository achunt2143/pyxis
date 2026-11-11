//! Open and close animations: when a window starts one, and what it needs.
//! The machinery is `crate::anim`; this is the part that knows about
//! windows, the Space and the backend.
//!
//! - **Open** fires once per showing, at the first commit where the window is
//!   mapped (it has a buffer and is in the Space) and claims a bindable app
//!   id. The vela about:blank pre-window each launch maps first claims
//!   `vela-default`, which is not bindable, so it never animates (it would
//!   flash a card open and shut on every launch). The shell and the overlay
//!   never animate.
//! - **Close** draws a snapshot, taken before the buffer goes: when the
//!   toplevel is destroyed (`remove_window_from_workspace`), or in the pre-commit
//!   hook of a commit that attaches a null buffer (a client unmapping itself).
//!   A window that never opened (so was never seen) leaves no ghost.

use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Rectangle},
};
use smithay::desktop::space::SpaceElement;

use super::{placement::is_bindable_app_id, WindowElement};
use crate::{
    anim::{
        self,
        bloom::{self, BloomFx},
        evaporate::{self, Close, Evaporate},
        ghosts::{self, Ghost, GhostSource},
        AnimKind, TransformAnim, WindowAnim,
    },
    state::Backend,
    EvoState,
};

impl<BackendData: Backend> EvoState<BackendData> {
    /// Whether any animation is running or a ghost is being drawn. A window
    /// animating in is drawn every frame, so repaints keep coming on their
    /// own; this is for callers that need to know (idle decisions, tests).
    pub fn animations_active(&self) -> bool {
        anim::active_count() > 0
    }

    /// Whether `window` may animate at all: an app, not the shell, not the
    /// overlay, and not a window that has yet to say who it is.
    pub fn animation_eligible(&self, window: &WindowElement) -> bool {
        // The surface, not the toplevel: `toplevel_destroyed` runs with the
        // xdg_toplevel already dying (`Window::alive` is false) but the
        // surface, and its last buffer, still there, which is when a close
        // animation takes its snapshot.
        window.wl_surface().is_some_and(|s| s.alive())
            && !window.is_shell_window()
            && window
                .claimed_app_id()
                .is_some_and(|id| is_bindable_app_id(&id))
    }

    /// Start `window`'s open animation if this commit is the one that shows
    /// it. Cheap to call on every commit.
    pub fn maybe_start_open_animation(&mut self, window: &WindowElement, root: &WlSurface) {
        if window.has_opened() {
            return;
        }
        let has_buffer = with_renderer_surface_state(root, |state| state.buffer().is_some()).unwrap_or(false);
        if !has_buffer || !self.is_window_mapped(window) || !self.animation_eligible(window) {
            return;
        }
        window.set_opened(true);
        let settings = anim::settings();
        let now = anim::now();
        // Lumen's Bloom, unless Lumen is off or its shader failed: then the
        // plain open.
        if bloom::available() {
            if let Some(open) = TransformAnim::bloom(&settings, now) {
                let fx = BloomFx::new(&settings, now, self.bloom_accent(window), window.content_corner_radius());
                window.start_animation(WindowAnim::new(AnimKind::Open, open).with_bloom(fx));
            }
            return;
        }
        if let Some(open) = TransformAnim::open(&settings, now, None) {
            window.start_animation(WindowAnim::new(AnimKind::Open, open));
        }
    }

    /// The colour `window` blooms in: the accent Bloom gave its app (in
    /// `SetMinimizeTargets`), else a neutral cool white. A window that is only
    /// now appearing has no picture to take an average from.
    fn bloom_accent(&self, window: &WindowElement) -> [f32; 3] {
        super::minimize::accent_for(window.app_id().as_deref()).unwrap_or(bloom::NEUTRAL_ACCENT)
    }

    /// Draw `window` closing, from a snapshot taken now. Call before its
    /// buffer goes and before it leaves the Space.
    pub fn begin_close_animation(&mut self, window: &WindowElement) {
        if !window.has_opened() || !self.animation_eligible(window) {
            return;
        }
        // The window may be unmapped again (a client that hides and shows).
        window.set_opened(false);
        let settings = anim::settings();
        if !settings.enabled() {
            return;
        }
        let has_buffer = window
            .wl_surface()
            .and_then(|s| with_renderer_surface_state(&s, |state| state.buffer().is_some()))
            .unwrap_or(false);
        if !has_buffer || !self.is_window_mapped(window) {
            return;
        }
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        let output = self
            .space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned());
        let Some(output) = output else {
            return;
        };
        let Some(mut snapshot) = self
            .backend_data
            .snapshot_window(window, output.current_scale().fractional_scale())
        else {
            return;
        };
        // The closing card keeps the shadow it had (see `anim::depth`).
        snapshot.shadow = window.shadow_style(anim::now());
        let geometry = SpaceElement::geometry(window);
        // The window next below this one: the ghost goes right above it.
        let anchor = self
            .space
            .elements()
            .take_while(|w| *w != window)
            .last()
            .cloned();
        // The window is leaving, but whatever it was animating (an open, a
        // restore) is the pose the snapshot starts from: a window closed
        // mid-open shrinks from the size and opacity it had instead of
        // jumping to full size first.
        let now = anim::now();
        let prev = window.with_animation(|a| a.anim.clone());
        // Lumen's Evaporate in full mode, when the program works and Lumen is
        // on; else (reduced, Lumen off, a failed program) the plain scale/fade.
        let close = evaporate::choose(prev.as_ref(), now, &settings, evaporate::usable());
        window.cancel_animation();
        window.cancel_geometry();
        let (close, source) = match close {
            Some(Close::Evaporate(anim)) => {
                // Bloom's accent for the app if it sent one, else the picture's.
                let accent = super::minimize::accent_for(window.app_id().as_deref());
                (anim, GhostSource::Evaporate(Box::new(Evaporate::new(snapshot, accent))))
            }
            Some(Close::Plain(anim)) => (anim, GhostSource::Snapshot(snapshot)),
            None => return,
        };
        ghosts::add(Ghost {
            source,
            owner: Some(window.clone()),
            anim: WindowAnim::new(AnimKind::Close, close),
            anchor,
            frame: Rectangle::new(location, geometry.size),
            render_origin: location - geometry.loc,
        });
    }
}
