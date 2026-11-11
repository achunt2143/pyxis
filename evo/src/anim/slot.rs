//! A window's animation state: the `WindowAnim` slot and the open marker,
//! kept in its `user_data`. Methods on `WindowElement`, here so `element.rs`
//! stays about the window.

use std::{
    cell::{Cell, RefCell},
    time::Duration,
};

use smithay::utils::{Logical, Rectangle};

use super::{transform::WindowTransform, window::WindowAnim};
use crate::shell::WindowElement;

#[derive(Default)]
struct AnimSlot {
    running: RefCell<Option<WindowAnim>>,
    /// The window has been shown (its open animation has been decided on).
    opened: Cell<bool>,
}

impl WindowElement {
    fn anim_slot(&self) -> &AnimSlot {
        self.user_data().insert_if_missing(AnimSlot::default);
        self.user_data().get::<AnimSlot>().expect("just inserted")
    }

    /// Run `anim` on this window. Any animation already running is dropped,
    /// which counts it completed (use `with_animation` to retarget it
    /// instead).
    pub fn start_animation(&self, anim: WindowAnim) {
        *self.anim_slot().running.borrow_mut() = Some(anim);
    }

    /// Whether an animation is running (finished ones are cleared by the next
    /// `animation_transform`, so this can lag one frame behind the clock).
    pub fn animating(&self) -> bool {
        self.user_data()
            .get::<AnimSlot>()
            .is_some_and(|slot| slot.running.borrow().is_some())
    }

    /// Stop animating, completing the animation.
    pub fn cancel_animation(&self) {
        if let Some(slot) = self.user_data().get::<AnimSlot>() {
            slot.running.borrow_mut().take();
        }
    }

    /// Edit the running animation (retarget it, say). `None` if none runs.
    pub fn with_animation<T>(&self, f: impl FnOnce(&mut WindowAnim) -> T) -> Option<T> {
        let slot = self.user_data().get::<AnimSlot>()?;
        let mut running = slot.running.borrow_mut();
        running.as_mut().map(f)
    }

    /// The pose to draw this window in at `now`, `None` when it is drawn
    /// plain. A finished animation is dropped here, which completes it.
    /// `frame` is the window's frame, output-logical.
    pub fn animation_transform(&self, now: Duration, frame: Rectangle<i32, Logical>) -> Option<WindowTransform> {
        let slot = self.user_data().get::<AnimSlot>()?;
        let mut running = slot.running.borrow_mut();
        let anim = running.as_ref()?;
        match anim.sample(now, frame) {
            Some(t) => Some(t),
            None => {
                *running = None;
                None
            }
        }
    }

    /// Whether the window has been shown yet. The open animation fires once,
    /// at the first mapped commit with a bindable app id.
    pub fn has_opened(&self) -> bool {
        self.user_data()
            .get::<AnimSlot>()
            .is_some_and(|slot| slot.opened.get())
    }

    pub fn set_opened(&self, opened: bool) {
        self.anim_slot().opened.set(opened);
    }
}
