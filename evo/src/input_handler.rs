use std::convert::TryInto;

use crate::{focus::PointerFocusTarget, shell::FullscreenSurface, EvoState};

#[cfg(feature = "udev")]
use crate::udev::UdevData;
#[cfg(feature = "udev")]
use smithay::backend::renderer::DebugFlags;

use smithay::{
    backend::input::{
        self, Axis, AxisSource, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
        PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{layer_map_for_output, space::SpaceElement, WindowSurfaceType},
    input::{
        keyboard::{keysyms as xkb, FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    output::Scale,
    reexports::{
        wayland_server::protocol::wl_pointer,
    },
    utils::{Logical, Point, Serial, Transform, SERIAL_COUNTER as SCOUNTER},
    wayland::{
        input_method::InputMethodSeat,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat,
        shell::wlr_layer::{KeyboardInteractivity, Layer as WlrLayer},
    },
};

#[cfg(any(feature = "winit", feature = "x11", feature = "udev"))]
use smithay::backend::input::AbsolutePositionEvent;

#[cfg(any(feature = "winit", feature = "x11"))]
use smithay::output::Output;
use tracing::{debug, error, info};

use crate::state::Backend;
#[cfg(feature = "udev")]
use smithay::{
    backend::{
        input::{
            Device, DeviceCapability, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
            GestureSwipeUpdateEvent as _, PointerMotionEvent, ProximityState, TabletToolButtonEvent,
            TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent, TabletToolTipState, TouchEvent,
        },
        session::Session,
    },
    input::{
        pointer::{
            GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
            GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
            RelativeMotionEvent,
        },
        touch::{DownEvent, UpEvent},
    },
    reexports::wayland_server::DisplayHandle,
    wayland::{
        pointer_constraints::{with_pointer_constraint, PointerConstraint},
        seat::WaylandFocus,
        tablet_manager::{TabletDescriptor, TabletSeatTrait},
    },
};

impl<BackendData: Backend> EvoState<BackendData> {
    // Allow in this method because of existing usage
    #[allow(clippy::uninlined_format_args)]
    fn process_common_key_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::None => (),

            KeyAction::CloseWindow => self.close_focused_window(),

            // Super+L. (While locked no shortcut is looked up at all, so this
            // is never reached to lock a locked screen.)
            KeyAction::Lock => self.lock_screen(crate::shell::lock::Source::Key),

            KeyAction::CycleWindows { reverse } => self.cycle_windows(reverse),

            KeyAction::CancelSwitcher => self.switcher_cancel(),

            KeyAction::Shell(name) => {
                // Only Evo knows the stacking order show desktop staggers by,
                // so it moves the windows itself; the shell hears the signal
                // all the same.
                if name == "show-desktop" {
                    self.show_desktop();
                }
                #[cfg(feature = "echo-ipc")]
                let _ = self.echo_service.read().emit_shortcut_triggered(name);
                #[cfg(not(feature = "echo-ipc"))]
                debug!(name, "shell shortcut with no Echo to tell");
            }

            #[cfg(feature = "debug")]
            KeyAction::TogglePreview => {
                self.show_window_preview = !self.show_window_preview;
            }

            #[cfg(feature = "debug")]
            KeyAction::ToggleDecorations => {
                use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1;
                for element in self.space.elements() {
                    #[allow(irrefutable_let_patterns)]
                    if let Some(toplevel) = element.0.toplevel() {
                        let mode_changed = toplevel.with_pending_state(|state| {
                            if let Some(current_mode) = state.decoration_mode {
                                let new_mode =
                                    if current_mode == zxdg_toplevel_decoration_v1::Mode::ClientSide {
                                        zxdg_toplevel_decoration_v1::Mode::ServerSide
                                    } else {
                                        zxdg_toplevel_decoration_v1::Mode::ClientSide
                                    };
                                state.decoration_mode = Some(new_mode);
                                true
                            } else {
                                false
                            }
                        });

                        if mode_changed && toplevel.is_initial_configure_sent() {
                            toplevel.send_pending_configure();
                        }
                    }
                }
            }

            KeyAction::RestoreMinimized => {
                self.restore_last_minimized();
            }

            KeyAction::MaximizeWindow => self.toggle_maximize_focused_window(),
            KeyAction::UnmaximizeOrMinimizeWindow => self.unmaximize_or_minimize_focused_window(),
            KeyAction::TileLeft => self.tile_focused_window(crate::shell::TileSide::Left),
            KeyAction::TileRight => self.tile_focused_window(crate::shell::TileSide::Right),
            KeyAction::CycleAppWindows { reverse } => self.cycle_app_windows(reverse),

            _ => unreachable!(
                "Common key action handler encountered backend specific action {:?}",
                action
            ),
        }
    }

    fn keyboard_key_to_action<B: InputBackend>(&mut self, evt: B::KeyboardKeyEvent) -> KeyAction {
        let keycode = evt.key_code();
        let state = evt.state();
        debug!(?keycode, ?state, "key");
        let serial = SCOUNTER.next_serial();
        let time = Event::time_msec(&evt);
        let mut suppressed_keys = self.suppressed_keys.clone();
        let keyboard = self.seat.get_keyboard().unwrap();

        crate::shell::lock::note_input();
        // Locked: every key is the lock page's and nothing is a shortcut.
        // Alt+Tab, Super+anything, Super+Q, show desktop, VT switching and a
        // client's keyboard-shortcuts-inhibit are all moot: no shortcut is
        // even looked up, and the keyboard is put on the overlay first, so a
        // key arriving before the compositor's next pass can't reach an app.
        if crate::shell::lock::input_locked() {
            self.enforce_lock_focus();
            self.suppressed_keys.clear();
            self.super_tap = false;
            keyboard.input::<(), _>(self, keycode, state, serial, time, |_, _, _| FilterResult::Forward);
            return KeyAction::None;
        }

        for layer in self.layer_shell_state.layer_surfaces().rev() {
            let exclusive = layer.with_cached_state(|data| {
                data.keyboard_interactivity == KeyboardInteractivity::Exclusive
                    && (data.layer == WlrLayer::Top || data.layer == WlrLayer::Overlay)
            });
            if exclusive {
                let surface = self.space.outputs().find_map(|o| {
                    let map = layer_map_for_output(o);
                    let cloned = map.layers().find(|l| l.layer_surface() == &layer).cloned();
                    cloned
                });
                if let Some(surface) = surface {
                    keyboard.set_focus(self, Some(surface.into()), serial);
                    keyboard.input::<(), _>(self, keycode, state, serial, time, |_, _, _| {
                        FilterResult::Forward
                    });
                    return KeyAction::None;
                };
            }
        }

        // The client that has the keyboard decides, not the one under the
        // pointer: shortcuts go to whoever is typing.
        let inhibited = keyboard
            .current_focus()
            .and_then(|focus| focus.wl_surface().map(|s| s.into_owned()))
            .and_then(|surface| self.seat.keyboard_shortcuts_inhibitor_for_surface(&surface))
            .map(|inhibitor| inhibitor.is_active())
            .unwrap_or(false);
        let mut super_tap = self.super_tap && !inhibited;
        let mut tapped = false;
        // Escape while Alt+Tab is on cancels it (and is not the client's).
        let switching = self.switcher_active() && !inhibited;

        let action = keyboard
            .input(self, keycode, state, serial, time, |_, modifiers, handle| {
                let keysym = handle.modified_sym();

                debug!(
                    ?state,
                    mods = ?modifiers,
                    keysym = ::xkbcommon::xkb::keysym_get_name(keysym),
                    "keysym"
                );

                // If the key is pressed and triggered a action
                // we will not forward the key to the client.
                // Additionally add the key to the suppressed keys
                // so that we can decide on a release if the key
                // should be forwarded to the client or not.
                let is_super = matches!(keysym, Keysym::Super_L | Keysym::Super_R);
                if let KeyState::Pressed = state {
                    // Super then nothing else, released, is a tap.
                    super_tap = is_super && !inhibited;
                    if !inhibited {
                        let action = if switching && keysym == Keysym::Escape {
                            Some(KeyAction::CancelSwitcher)
                        } else {
                            process_keyboard_shortcut(*modifiers, keysym)
                        };

                        if action.is_some() {
                            suppressed_keys.push(keysym);
                        }

                        action
                            .map(FilterResult::Intercept)
                            .unwrap_or(FilterResult::Forward)
                    } else {
                        FilterResult::Forward
                    }
                } else {
                    if is_super && super_tap {
                        tapped = true;
                    }
                    if is_super {
                        super_tap = false;
                    }
                    let suppressed = suppressed_keys.contains(&keysym);
                    if suppressed {
                        suppressed_keys.retain(|k| *k != keysym);
                        FilterResult::Intercept(KeyAction::None)
                    } else {
                        FilterResult::Forward
                    }
                }
            })
            .unwrap_or(KeyAction::None);

        self.suppressed_keys = suppressed_keys;
        self.super_tap = super_tap;
        if tapped && !crate::shell::lock::input_locked() {
            #[cfg(feature = "echo-ipc")]
            let _ = self.echo_service.read().emit_shortcut_triggered("launcher");
        }
        // An Alt+Tab walk ends with Alt, and Alt's release is its commit.
        if self.alt_tab.is_some() && !keyboard.modifier_state().alt {
            self.switcher_release();
        }
        action
    }

    fn on_pointer_button<B: InputBackend>(&mut self, evt: B::PointerButtonEvent) {
        let serial = SCOUNTER.next_serial();
        let button = evt.button_code();

        let state = wl_pointer::ButtonState::from(evt.state());

        // Locked: a click is the overlay's or nobody's. The pointer's focus is
        // brought to the overlay first (an app that had it must not get this
        // click), and the click moves no keyboard focus and pings no app.
        let locked = crate::shell::lock::input_locked();
        if locked {
            self.fix_locked_pointer();
        }
        if !locked && wl_pointer::ButtonState::Pressed == state {
            // Super+click is a chord, not a tap.
            self.super_tap = false;
            self.update_keyboard_focus(self.pointer.current_location(), serial);
            // A click is a moment to ask whether the app still answers.
            let location = self.pointer.current_location();
            if let Some((window, _)) = self.space.element_under(location).map(|(w, p)| (w.clone(), p)) {
                self.ping_window(&window, true);
            }
        };
        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: state.try_into().unwrap(),
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn update_keyboard_focus(&mut self, location: Point<f64, Logical>, serial: Serial) {
        // Locked: the keyboard is the overlay's (shell/lock.rs); no click or
        // touch moves it.
        if crate::shell::lock::input_locked() {
            return;
        }
        let keyboard = self.seat.get_keyboard().unwrap();
        let touch = self.seat.get_touch();
        let input_method = self.seat.input_method();
        // change the keyboard focus unless the pointer or keyboard is grabbed
        // We test for any matching surface type here but always use the root
        // (in case of a window the toplevel) surface for the focus.
        // So for example if a user clicks on a subsurface or popup the toplevel
        // will receive the keyboard focus. Directly assigning the focus to the
        // matching surface leads to issues with clients dismissing popups and
        // subsurface menus (for example firefox-wayland).
        // see here for a discussion about that issue:
        // https://gitlab.freedesktop.org/wayland/wayland/-/issues/294
        if !self.pointer.is_grabbed()
            && (!keyboard.is_grabbed() || input_method.keyboard_grabbed())
            && !touch.map(|touch| touch.is_grabbed()).unwrap_or(false)
        {
            let output = self.space.output_under(location).next().cloned();
            if let Some(output) = output.as_ref() {
                let output_geo = self.space.output_geometry(output).unwrap();
                // A click on the shell overlay (a toast) over a fullscreen app
                // is the overlay's, as it is over any window: no focus change.
                let fullscreen = output
                    .user_data()
                    .get::<FullscreenSurface>()
                    .and_then(|f| f.get());
                if fullscreen.is_some() && self.overlay_surface_under(location).is_some() {
                    return;
                }
                if let Some(window) = fullscreen {
                    if let Some((_, _)) =
                        window.surface_under(location - output_geo.loc.to_f64(), WindowSurfaceType::ALL)
                    {
                        #[cfg(feature = "xwayland")]
                        if let Some(surface) = window.0.x11_surface() {
                            if let Some(xwm) = self.xwm.as_mut() {
                                let _ = xwm.raise_window(surface);
                            }
                        }
                        keyboard.set_focus(self, Some(window.into()), serial);
                        return;
                    }
                }

                let layers = layer_map_for_output(output);
                if let Some(layer) = layers
                    .layer_under(WlrLayer::Overlay, location - output_geo.loc.to_f64())
                    .or_else(|| layers.layer_under(WlrLayer::Top, location - output_geo.loc.to_f64()))
                {
                    if layer.can_receive_keyboard_focus() {
                        if let Some((_, _)) = layer.surface_under(
                            location
                                - output_geo.loc.to_f64()
                                - layers.layer_geometry(layer).unwrap().loc.to_f64(),
                            WindowSurfaceType::ALL,
                        ) {
                            keyboard.set_focus(self, Some(layer.clone().into()), serial);
                            return;
                        }
                    }
                }
            }

            if let Some((window, _)) = self.space.element_under(location).map(|(w, p)| (w.clone(), p)) {
                if window.is_overlay() {
                    // A click on a toast is for the overlay, which gets it as
                    // pointer input. It must not take keyboard focus from the
                    // app being typed into, and there is nothing to raise:
                    // the overlay is always on top.
                    return;
                }
                // Raises in the workspace's own order too, not just the space's.
                self.raise_window(&window);
                #[cfg(feature = "xwayland")]
                if let Some(surface) = window.0.x11_surface() {
                    if let Some(xwm) = self.xwm.as_mut() {
                        let _ = xwm.raise_window(surface);
                    }
                }
                keyboard.set_focus(self, Some(window.into()), serial);
                return;
            }

            if let Some(output) = output.as_ref() {
                let output_geo = self.space.output_geometry(output).unwrap();
                let layers = layer_map_for_output(output);
                if let Some(layer) = layers
                    .layer_under(WlrLayer::Bottom, location - output_geo.loc.to_f64())
                    .or_else(|| layers.layer_under(WlrLayer::Background, location - output_geo.loc.to_f64()))
                {
                    if layer.can_receive_keyboard_focus() {
                        if let Some((_, _)) = layer.surface_under(
                            location
                                - output_geo.loc.to_f64()
                                - layers.layer_geometry(layer).unwrap().loc.to_f64(),
                            WindowSurfaceType::ALL,
                        ) {
                            keyboard.set_focus(self, Some(layer.clone().into()), serial);
                        }
                    }
                }
            };
        }
    }

    /// What the shell overlay has under `pos`, if it takes input there. Only
    /// asked while an app is fullscreen: otherwise the space finds it like any
    /// window.
    fn overlay_surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(PointerFocusTarget, Point<i32, Logical>)> {
        let overlay = self.space.elements().find(|w| w.is_overlay())?;
        let render_loc = self.space.element_location(overlay)? - overlay.geometry().loc;
        let point = pos - render_loc.to_f64();
        if !overlay.is_in_input_region(&point) {
            return None;
        }
        let (surface, loc) = overlay.surface_under(point, WindowSurfaceType::ALL)?;
        Some((surface, loc + render_loc))
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(PointerFocusTarget, Point<f64, Logical>)> {
        // Locked: the overlay, over the whole screen, or nothing. This is the
        // one function every pointer, touch and tablet event asks, so no app
        // surface, layer surface or popup can be returned while it holds.
        if crate::shell::lock::input_locked() {
            return self.locked_surface_under(pos);
        }
        let output = self.space.outputs().find(|o| {
            self.space
                .output_geometry(o)
                .is_some_and(|geometry| geometry.to_f64().contains(pos))
        })?;
        let output_geo = self.space.output_geometry(output)?;
        let layers = layer_map_for_output(output);

        let fullscreen = output
            .user_data()
            .get::<FullscreenSurface>()
            .and_then(|f| f.get());

        let mut under = None;
        // The overlay is drawn over a fullscreen app (render.rs), so it gets
        // the pointer first, where Bloom said it takes input.
        if let Some((surface, loc)) = fullscreen
            .as_ref()
            .and_then(|_| self.overlay_surface_under(pos))
        {
            under = Some((surface, loc));
        } else if let Some((surface, loc)) = fullscreen
            .and_then(|w| w.surface_under(pos - output_geo.loc.to_f64(), WindowSurfaceType::ALL))
        {
            under = Some((surface, loc + output_geo.loc));
        } else if let Some(focus) = layers
            .layer_under(WlrLayer::Overlay, pos - output_geo.loc.to_f64())
            .or_else(|| layers.layer_under(WlrLayer::Top, pos - output_geo.loc.to_f64()))
            .and_then(|layer| {
                let layer_loc = layers.layer_geometry(layer).unwrap().loc;
                layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| {
                        (
                            PointerFocusTarget::from(surface),
                            loc + layer_loc + output_geo.loc,
                        )
                    })
            })
        {
            under = Some(focus)
        } else if let Some(focus) = self.space.element_under(pos).and_then(|(window, loc)| {
            window
                .surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, surf_loc)| (surface, surf_loc + loc))
        }) {
            under = Some(focus);
        } else if let Some(focus) = layers
            .layer_under(WlrLayer::Bottom, pos - output_geo.loc.to_f64())
            .or_else(|| layers.layer_under(WlrLayer::Background, pos - output_geo.loc.to_f64()))
            .and_then(|layer| {
                let layer_loc = layers.layer_geometry(layer).unwrap().loc;
                layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| {
                        (
                            PointerFocusTarget::from(surface),
                            loc + layer_loc + output_geo.loc,
                        )
                    })
            })
        {
            under = Some(focus)
        };
        under.map(|(s, l)| (s, l.to_f64()))
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, evt: B::PointerAxisEvent) {
        if crate::shell::lock::input_locked() {
            self.fix_locked_pointer();
        }
        let horizontal_amount = evt
            .amount(input::Axis::Horizontal)
            .unwrap_or_else(|| evt.amount_v120(input::Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.);
        let vertical_amount = evt
            .amount(input::Axis::Vertical)
            .unwrap_or_else(|| evt.amount_v120(input::Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
        let horizontal_amount_discrete = evt.amount_v120(input::Axis::Horizontal);
        let vertical_amount_discrete = evt.amount_v120(input::Axis::Vertical);

        {
            let mut frame = AxisFrame::new(evt.time_msec()).source(evt.source());
            if horizontal_amount != 0.0 {
                frame = frame.relative_direction(Axis::Horizontal, evt.relative_direction(Axis::Horizontal));
                frame = frame.value(Axis::Horizontal, horizontal_amount);
                if let Some(discrete) = horizontal_amount_discrete {
                    frame = frame.v120(Axis::Horizontal, discrete as i32);
                }
            }
            if vertical_amount != 0.0 {
                frame = frame.relative_direction(Axis::Vertical, evt.relative_direction(Axis::Vertical));
                frame = frame.value(Axis::Vertical, vertical_amount);
                if let Some(discrete) = vertical_amount_discrete {
                    frame = frame.v120(Axis::Vertical, discrete as i32);
                }
            }
            if evt.source() == AxisSource::Finger {
                if evt.amount(Axis::Horizontal) == Some(0.0) {
                    frame = frame.stop(Axis::Horizontal);
                }
                if evt.amount(Axis::Vertical) == Some(0.0) {
                    frame = frame.stop(Axis::Vertical);
                }
            }
            let pointer = self.pointer.clone();
            pointer.axis(self, frame);
            pointer.frame(self);
        }
    }
}

#[cfg(any(feature = "winit", feature = "x11"))]
impl<BackendData: Backend> EvoState<BackendData> {
    pub fn process_input_event_windowed<B: InputBackend>(&mut self, event: InputEvent<B>, output_name: &str) {
        if !matches!(event, InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. }) {
            crate::shell::lock::note_input();
        }
        match event {
            InputEvent::Keyboard { event } => match self.keyboard_key_to_action::<B>(event) {
                KeyAction::ScaleUp => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = current_scale + 0.25;
                    output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                    self.fixup_positions(self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::ScaleDown => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = f64::max(1.0, current_scale - 0.25);
                    output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                    self.fixup_positions(self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::RotateOutput => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_transform = output.current_transform();
                    let new_transform = match current_transform {
                        Transform::Normal => Transform::_90,
                        Transform::_90 => Transform::_180,
                        Transform::_180 => Transform::_270,
                        Transform::_270 => Transform::Flipped,
                        Transform::Flipped => Transform::Flipped90,
                        Transform::Flipped90 => Transform::Flipped180,
                        Transform::Flipped180 => Transform::Flipped270,
                        Transform::Flipped270 => Transform::Normal,
                    };
                    tracing::info!(?current_transform, ?new_transform, output = ?output.name(), "changing output transform");
                    output.change_current_state(None, Some(new_transform), None, None);
                    self.fixup_positions(self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                action => match action {
                    KeyAction::None
                    | KeyAction::CloseWindow
                    | KeyAction::Lock
                    | KeyAction::CycleWindows { .. }
                    | KeyAction::CancelSwitcher
                    | KeyAction::Shell(_)
                    | KeyAction::TogglePreview
                    | KeyAction::ToggleDecorations
                    | KeyAction::RestoreMinimized
                    | KeyAction::MaximizeWindow
                    | KeyAction::UnmaximizeOrMinimizeWindow
                    | KeyAction::TileLeft
                    | KeyAction::TileRight
                    | KeyAction::CycleAppWindows { .. } => self.process_common_key_action(action),

                    _ => tracing::warn!(
                        ?action,
                        output_name,
                        "Key action unsupported on on output backend.",
                    ),
                },
            },

            InputEvent::PointerMotionAbsolute { event } => {
                let output = self
                    .space
                    .outputs()
                    .find(|o| o.name() == output_name)
                    .unwrap()
                    .clone();
                self.on_pointer_move_absolute_windowed::<B>(event, &output)
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            _ => (), // other events are not handled in Evo (yet)
        }
    }

    fn on_pointer_move_absolute_windowed<B: InputBackend>(
        &mut self,
        evt: B::PointerMotionAbsoluteEvent,
        output: &Output,
    ) {
        let output_geo = self.space.output_geometry(output).unwrap();

        let pos = evt.position_transformed(output_geo.size) + output_geo.loc.to_f64();
        let serial = SCOUNTER.next_serial();

        let pointer = self.pointer.clone();
        let under = self.surface_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    pub fn release_all_keys(&mut self) {
        let keyboard = self.seat.get_keyboard().unwrap();
        for keycode in keyboard.pressed_keys() {
            keyboard.input(
                self,
                keycode,
                KeyState::Released,
                SCOUNTER.next_serial(),
                0,
                |_, _, _| FilterResult::Forward::<bool>,
            );
        }
    }
}

#[cfg(feature = "udev")]
impl EvoState<UdevData> {
    pub fn process_input_event<B: InputBackend>(&mut self, dh: &DisplayHandle, event: InputEvent<B>) {
        if !matches!(event, InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. }) {
            crate::shell::lock::note_input();
        }
        match event {
            InputEvent::Keyboard { event, .. } => match self.keyboard_key_to_action::<B>(event) {
                #[cfg(feature = "udev")]
                // Never while locked (no shortcut is looked up then; this is
                // the second lock on the same door).
                KeyAction::VtSwitch(_) if crate::shell::lock::input_locked() => {}
                KeyAction::VtSwitch(vt) => {
                    info!(to = vt, "Trying to switch vt");
                    if let Err(err) = self.backend_data.session.change_vt(vt) {
                        error!(vt, "Error switching vt: {}", err);
                    }
                }
                KeyAction::Screen(num) => {
                    let geometry = self
                        .space
                        .outputs()
                        .nth(num)
                        .map(|o| self.space.output_geometry(o).unwrap());

                    if let Some(geometry) = geometry {
                        let x = geometry.loc.x as f64 + geometry.size.w as f64 / 2.0;
                        let y = geometry.size.h as f64 / 2.0;
                        let location = (x, y).into();
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                    }
                }
                KeyAction::ScaleUp => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let (output_location, scale) = (
                            self.space.output_geometry(&output).unwrap().loc,
                            output.current_scale().fractional_scale(),
                        );
                        let new_scale = scale + 0.25;
                        output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                        let rescale = scale / new_scale;
                        let output_location = output_location.to_f64();
                        let mut pointer_output_location = self.pointer.current_location() - output_location;
                        pointer_output_location.x *= rescale;
                        pointer_output_location.y *= rescale;
                        let pointer_location = output_location + pointer_output_location;

                        self.fixup_positions(pointer_location);
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(pointer_location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location: pointer_location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::ScaleDown => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let (output_location, scale) = (
                            self.space.output_geometry(&output).unwrap().loc,
                            output.current_scale().fractional_scale(),
                        );
                        let new_scale = f64::max(1.0, scale - 0.25);
                        output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                        let rescale = scale / new_scale;
                        let output_location = output_location.to_f64();
                        let mut pointer_output_location = self.pointer.current_location() - output_location;
                        pointer_output_location.x *= rescale;
                        pointer_output_location.y *= rescale;
                        let pointer_location = output_location + pointer_output_location;

                        self.fixup_positions(pointer_location);
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(pointer_location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location: pointer_location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::RotateOutput => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let current_transform = output.current_transform();
                        let new_transform = match current_transform {
                            Transform::Normal => Transform::_90,
                            Transform::_90 => Transform::_180,
                            Transform::_180 => Transform::_270,
                            Transform::_270 => Transform::Flipped,
                            Transform::Flipped => Transform::Flipped90,
                            Transform::Flipped90 => Transform::Flipped180,
                            Transform::Flipped180 => Transform::Flipped270,
                            Transform::Flipped270 => Transform::Normal,
                        };
                        output.change_current_state(None, Some(new_transform), None, None);
                        self.fixup_positions(self.pointer.current_location());
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::ToggleTint => {
                    let mut debug_flags = self.backend_data.debug_flags();
                    debug_flags.toggle(DebugFlags::TINT);
                    self.backend_data.set_debug_flags(debug_flags);
                }

                action => match action {
                    KeyAction::None
                    | KeyAction::CloseWindow
                    | KeyAction::Lock
                    | KeyAction::CycleWindows { .. }
                    | KeyAction::CancelSwitcher
                    | KeyAction::Shell(_)
                    | KeyAction::TogglePreview
                    | KeyAction::ToggleDecorations
                    | KeyAction::RestoreMinimized
                    | KeyAction::MaximizeWindow
                    | KeyAction::UnmaximizeOrMinimizeWindow
                    | KeyAction::TileLeft
                    | KeyAction::TileRight
                    | KeyAction::CycleAppWindows { .. } => self.process_common_key_action(action),

                    _ => unreachable!(),
                },
            },
            InputEvent::PointerMotion { event, .. } => self.on_pointer_move::<B>(dh, event),
            InputEvent::PointerMotionAbsolute { event, .. } => self.on_pointer_move_absolute::<B>(dh, event),
            InputEvent::PointerButton { event, .. } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<B>(event),
            InputEvent::TabletToolAxis { event, .. } => self.on_tablet_tool_axis::<B>(event),
            InputEvent::TabletToolProximity { event, .. } => self.on_tablet_tool_proximity::<B>(dh, event),
            InputEvent::TabletToolTip { event, .. } => self.on_tablet_tool_tip::<B>(event),
            InputEvent::TabletToolButton { event, .. } => self.on_tablet_button::<B>(event),
            InputEvent::GestureSwipeBegin { event, .. } => self.on_gesture_swipe_begin::<B>(event),
            InputEvent::GestureSwipeUpdate { event, .. } => self.on_gesture_swipe_update::<B>(event),
            InputEvent::GestureSwipeEnd { event, .. } => self.on_gesture_swipe_end::<B>(event),
            InputEvent::GesturePinchBegin { event, .. } => self.on_gesture_pinch_begin::<B>(event),
            InputEvent::GesturePinchUpdate { event, .. } => self.on_gesture_pinch_update::<B>(event),
            InputEvent::GesturePinchEnd { event, .. } => self.on_gesture_pinch_end::<B>(event),
            InputEvent::GestureHoldBegin { event, .. } => self.on_gesture_hold_begin::<B>(event),
            InputEvent::GestureHoldEnd { event, .. } => self.on_gesture_hold_end::<B>(event),

            InputEvent::TouchDown { event } => self.on_touch_down::<B>(event),
            InputEvent::TouchUp { event } => self.on_touch_up::<B>(event),
            InputEvent::TouchMotion { event } => self.on_touch_motion::<B>(event),
            InputEvent::TouchFrame { event } => self.on_touch_frame::<B>(event),
            InputEvent::TouchCancel { event } => self.on_touch_cancel::<B>(event),

            InputEvent::DeviceAdded { device } => {
                if device.has_capability(DeviceCapability::TabletTool) {
                    self.seat
                        .tablet_seat()
                        .add_tablet::<Self>(dh, &TabletDescriptor::from(&device));
                }
                if device.has_capability(DeviceCapability::Touch) && self.seat.get_touch().is_none() {
                    self.seat.add_touch();
                }
            }
            InputEvent::DeviceRemoved { device } => {
                if device.has_capability(DeviceCapability::TabletTool) {
                    let tablet_seat = self.seat.tablet_seat();

                    tablet_seat.remove_tablet(&TabletDescriptor::from(&device));

                    // If there are no tablets in seat we can remove all tools
                    if tablet_seat.count_tablets() == 0 {
                        tablet_seat.clear_tools();
                    }
                }
            }
            _ => {
                // other events are not handled in Evo (yet)
            }
        }
    }

    fn on_pointer_move<B: InputBackend>(&mut self, _dh: &DisplayHandle, evt: B::PointerMotionEvent) {
        let mut pointer_location = self.pointer.current_location();
        let serial = SCOUNTER.next_serial();

        let pointer = self.pointer.clone();
        let under = self.surface_under(pointer_location);

        let mut pointer_locked = false;
        let mut pointer_confined = false;
        let mut confine_region = None;
        if let Some((surface, surface_loc)) = under
            .as_ref()
            .and_then(|(target, l)| Some((target.wl_surface()?, l)))
        {
            with_pointer_constraint(&surface, &pointer, |constraint| match constraint {
                Some(constraint) if constraint.is_active() => {
                    // Constraint does not apply if not within region
                    if !constraint
                        .region()
                        .is_none_or(|x| x.contains((pointer_location - *surface_loc).to_i32_round()))
                    {
                        return;
                    }
                    match &*constraint {
                        PointerConstraint::Locked(_locked) => {
                            pointer_locked = true;
                        }
                        PointerConstraint::Confined(confine) => {
                            pointer_confined = true;
                            confine_region = confine.region().cloned();
                        }
                    }
                }
                _ => {}
            });
        }

        pointer.relative_motion(
            self,
            under.clone(),
            &RelativeMotionEvent {
                delta: evt.delta(),
                delta_unaccel: evt.delta_unaccel(),
                utime: evt.time(),
            },
        );

        // If pointer is locked, only emit relative motion
        if pointer_locked {
            pointer.frame(self);
            return;
        }

        pointer_location += evt.delta();

        // clamp to screen limits
        // this event is never generated by winit
        pointer_location = self.clamp_coords(pointer_location);

        let new_under = self.surface_under(pointer_location);

        // If confined, don't move pointer if it would go outside surface or region
        if pointer_confined {
            if let Some((surface, surface_loc)) = &under {
                if new_under.as_ref().and_then(|(under, _)| under.wl_surface()) != surface.wl_surface() {
                    pointer.frame(self);
                    return;
                }
                if let Some(region) = confine_region {
                    if !region.contains((pointer_location - *surface_loc).to_i32_round()) {
                        pointer.frame(self);
                        return;
                    }
                }
            }
        }

        pointer.motion(
            self,
            new_under.clone(),
            &MotionEvent {
                location: pointer_location,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);

        // If pointer is now in a constraint region, activate it
        // TODO Anywhere else pointer is moved needs to do this
        if let Some((under, surface_location)) =
            new_under.and_then(|(target, loc)| Some((target.wl_surface()?.into_owned(), loc)))
        {
            with_pointer_constraint(&under, &pointer, |constraint| match constraint {
                Some(constraint) if !constraint.is_active() => {
                    let point = (pointer_location - surface_location).to_i32_round();
                    if constraint.region().is_none_or(|region| region.contains(point)) {
                        constraint.activate();
                    }
                }
                _ => {}
            });
        }
    }

    fn on_pointer_move_absolute<B: InputBackend>(
        &mut self,
        _dh: &DisplayHandle,
        evt: B::PointerMotionAbsoluteEvent,
    ) {
        let serial = SCOUNTER.next_serial();

        let Some(bbox) = self
            .space
            .outputs()
            .filter_map(|o| self.space.output_geometry(o))
            .filter(|g| g.size.w > 0 && g.size.h > 0)
            .reduce(|acc, g| acc.merge(g))
        else {
            return;
        };

        let mut pointer_location = Point::from((
            bbox.loc.x as f64 + evt.x_transformed(bbox.size.w),
            bbox.loc.y as f64 + evt.y_transformed(bbox.size.h),
        ));

        // clamp to screen limits
        pointer_location = self.clamp_coords(pointer_location);

        let pointer = self.pointer.clone();
        let under = self.surface_under(pointer_location);

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pointer_location,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn on_tablet_tool_axis<B: InputBackend>(&mut self, evt: B::TabletToolAxisEvent) {
        let tablet_seat = self.seat.tablet_seat();

        if let Some(pointer_location) = self.touch_location_transformed(&evt) {
            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&evt.tool());

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: self.clock.now().as_millis(),
                },
            );

            if let (Some(tablet), Some(tool)) = (tablet, tool) {
                if evt.pressure_has_changed() {
                    tool.pressure(evt.pressure());
                }
                if evt.distance_has_changed() {
                    tool.distance(evt.distance());
                }
                if evt.tilt_has_changed() {
                    tool.tilt(evt.tilt());
                }
                if evt.slider_has_changed() {
                    tool.slider_position(evt.slider_position());
                }
                if evt.rotation_has_changed() {
                    tool.rotation(evt.rotation());
                }
                if evt.wheel_has_changed() {
                    tool.wheel(evt.wheel_delta(), evt.wheel_delta_discrete());
                }

                tool.motion(
                    pointer_location,
                    under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                    &tablet,
                    SCOUNTER.next_serial(),
                    evt.time_msec(),
                );
            }

            pointer.frame(self);
        }
    }

    fn on_tablet_tool_proximity<B: InputBackend>(
        &mut self,
        dh: &DisplayHandle,
        evt: B::TabletToolProximityEvent,
    ) {
        let tablet_seat = self.seat.tablet_seat();

        if let Some(pointer_location) = self.touch_location_transformed(&evt) {
            let tool = evt.tool();
            tablet_seat.add_tool::<Self>(self, dh, &tool);

            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&tool);

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: evt.time_msec(),
                },
            );
            pointer.frame(self);

            if let (Some(under), Some(tablet), Some(tool)) = (
                under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                tablet,
                tool,
            ) {
                match evt.state() {
                    ProximityState::In => tool.proximity_in(
                        pointer_location,
                        under,
                        &tablet,
                        SCOUNTER.next_serial(),
                        evt.time_msec(),
                    ),
                    ProximityState::Out => tool.proximity_out(evt.time_msec()),
                }
            }
        }
    }

    fn on_tablet_tool_tip<B: InputBackend>(&mut self, evt: B::TabletToolTipEvent) {
        let tool = self.seat.tablet_seat().get_tool(&evt.tool());

        if let Some(tool) = tool {
            match evt.tip_state() {
                TabletToolTipState::Down => {
                    let serial = SCOUNTER.next_serial();
                    tool.tip_down(serial, evt.time_msec());

                    // change the keyboard focus
                    self.update_keyboard_focus(self.pointer.current_location(), serial);
                }
                TabletToolTipState::Up => {
                    tool.tip_up(evt.time_msec());
                }
            }
        }
    }

    fn on_tablet_button<B: InputBackend>(&mut self, evt: B::TabletToolButtonEvent) {
        let tool = self.seat.tablet_seat().get_tool(&evt.tool());

        if let Some(tool) = tool {
            tool.button(
                evt.button(),
                evt.button_state(),
                SCOUNTER.next_serial(),
                evt.time_msec(),
            );
        }
    }

    fn on_gesture_swipe_begin<B: InputBackend>(&mut self, evt: B::GestureSwipeBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_swipe_update<B: InputBackend>(&mut self, evt: B::GestureSwipeUpdateEvent) {
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_update(
            self,
            &GestureSwipeUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
            },
        );
    }

    fn on_gesture_swipe_end<B: InputBackend>(&mut self, evt: B::GestureSwipeEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_pinch_begin<B: InputBackend>(&mut self, evt: B::GesturePinchBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_pinch_update<B: InputBackend>(&mut self, evt: B::GesturePinchUpdateEvent) {
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_update(
            self,
            &GesturePinchUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
                scale: evt.scale(),
                rotation: evt.rotation(),
            },
        );
    }

    fn on_gesture_pinch_end<B: InputBackend>(&mut self, evt: B::GesturePinchEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_hold_begin<B: InputBackend>(&mut self, evt: B::GestureHoldBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_begin(
            self,
            &GestureHoldBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_hold_end<B: InputBackend>(&mut self, evt: B::GestureHoldEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_end(
            self,
            &GestureHoldEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn touch_location_transformed<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        evt: &E,
    ) -> Option<Point<f64, Logical>> {
        let output = self
            .space
            .outputs()
            .find(|output| output.name().starts_with("eDP"))
            .or_else(|| self.space.outputs().next());

        let output = output?;
        let output_geometry = self.space.output_geometry(output)?;

        let transform = output.current_transform();
        let size = transform.invert().transform_size(output_geometry.size);
        Some(
            transform.transform_point_in(evt.position_transformed(size), &size.to_f64())
                + output_geometry.loc.to_f64(),
        )
    }

    fn on_touch_down<B: InputBackend>(&mut self, evt: B::TouchDownEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };

        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let serial = SCOUNTER.next_serial();
        self.update_keyboard_focus(touch_location, serial);

        let under = self.surface_under(touch_location);
        handle.down(
            self,
            under,
            &DownEvent {
                slot: evt.slot(),
                location: touch_location,
                serial,
                time: evt.time_msec(),
            },
        );
    }
    fn on_touch_up<B: InputBackend>(&mut self, evt: B::TouchUpEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let serial = SCOUNTER.next_serial();
        handle.up(
            self,
            &UpEvent {
                slot: evt.slot(),
                serial,
                time: evt.time_msec(),
            },
        )
    }
    fn on_touch_motion<B: InputBackend>(&mut self, evt: B::TouchMotionEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let under = self.surface_under(touch_location);
        handle.motion(
            self,
            under,
            &smithay::input::touch::MotionEvent {
                slot: evt.slot(),
                location: touch_location,
                time: evt.time_msec(),
            },
        );
    }
    fn on_touch_frame<B: InputBackend>(&mut self, _evt: B::TouchFrameEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.frame(self);
    }
    fn on_touch_cancel<B: InputBackend>(&mut self, _evt: B::TouchCancelEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.cancel(self);
    }

    fn clamp_coords(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        let rects: Vec<smithay::utils::Rectangle<i32, Logical>> = self
            .space
            .outputs()
            .filter_map(|o| self.space.output_geometry(o))
            .filter(|g| g.size.w > 0 && g.size.h > 0)
            .collect();
        clamp_point_to_rects(pos, &rects)
    }
}

/// Clamp `pos` into any arbitrary multi-monitor set of `(x, y, w, h)` output
/// rects (stacked, staggered, negative origins, or L-shaped). If `pos` is
/// already inside an output rect it is returned unchanged; otherwise it projects
/// to the closest point inside the nearest output rect.
pub(crate) fn clamp_point_to_rects(
    pos: Point<f64, Logical>,
    rects: &[smithay::utils::Rectangle<i32, Logical>],
) -> Point<f64, Logical> {
    if rects.is_empty() {
        return pos;
    }
    if rects.iter().any(|r| r.size.w > 0 && r.size.h > 0 && r.to_f64().contains(pos)) {
        return pos;
    }
    rects
        .iter()
        .filter(|r| r.size.w > 0 && r.size.h > 0)
        .map(|r| {
            let min_x = r.loc.x as f64;
            let min_y = r.loc.y as f64;
            let max_x = (r.loc.x as f64 + r.size.w as f64 - 1.0).max(min_x);
            let max_y = (r.loc.y as f64 + r.size.h as f64 - 1.0).max(min_y);
            let cx = pos.x.clamp(min_x, max_x);
            let cy = pos.y.clamp(min_y, max_y);
            let dist_sq = (cx - pos.x).powi(2) + (cy - pos.y).powi(2);
            (dist_sq, Point::from((cx, cy)))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, pt)| pt)
        .unwrap_or(pos)
}

/// Possible results of a keyboard action
#[allow(dead_code)] // some of these are only read if udev is enabled, or with `debug`
#[derive(Debug)]
enum KeyAction {
    /// Trigger a vt-switch
    VtSwitch(i32),
    /// Super+Q: ask the focused app window to close
    CloseWindow,
    /// Super+L: lock the screen
    Lock,
    /// Alt+Tab, Shift+Alt+Tab
    CycleWindows { reverse: bool },
    /// Alt+Grave, Alt+Shift+Grave: cycle windows matching focused app_id
    CycleAppWindows { reverse: bool },
    /// Escape during Alt+Tab: close the switcher, focus stays
    CancelSwitcher,
    /// Super+Up: toggle maximize/restore on active window
    MaximizeWindow,
    /// Super+Down: unmaximize/untile if maximized or tiled, else minimize
    UnmaximizeOrMinimizeWindow,
    /// Super+Left / Super+Right: snap/tile or restore active window
    TileLeft,
    TileRight,
    /// Tell the shell (`ShortcutTriggered`): it owns what these do
    Shell(&'static str),
    /// Switch the current screen
    Screen(usize),
    // Debug toggles, produced only by a build with the `debug` feature.
    ScaleUp,
    ScaleDown,
    TogglePreview,
    RotateOutput,
    ToggleTint,
    ToggleDecorations,
    /// Re-map the most recently minimized window (there is no taskbar to
    /// click yet, so this is the interim way to get a minimized window back).
    RestoreMinimized,
    /// Do nothing more
    None,
}

/// Nothing here may end the session: Evo is not restarted by anything, so a
/// quit key would be a way to lose the desktop for good.
fn process_keyboard_shortcut(modifiers: ModifiersState, keysym: Keysym) -> Option<KeyAction> {
    let raw = keysym.raw();
    if (xkb::KEY_XF86Switch_VT_1..=xkb::KEY_XF86Switch_VT_12).contains(&raw) {
        return Some(KeyAction::VtSwitch((raw - xkb::KEY_XF86Switch_VT_1 + 1) as i32));
    }
    let bare_alt = modifiers.alt && !modifiers.logo && !modifiers.ctrl;
    // Shift turns Tab into ISO_Left_Tab, so both mean "Tab" here.
    let tab = keysym == Keysym::Tab || raw == xkb::KEY_ISO_Left_Tab;
    if bare_alt && tab {
        return Some(KeyAction::CycleWindows { reverse: modifiers.shift });
    }
    if bare_alt && (keysym == Keysym::grave || keysym == Keysym::asciitilde) {
        return Some(KeyAction::CycleAppWindows { reverse: modifiers.shift });
    }
    if modifiers.logo && !modifiers.alt && !modifiers.ctrl && !modifiers.shift {
        if keysym == Keysym::q {
            return Some(KeyAction::CloseWindow);
        }
        if keysym == Keysym::l {
            return Some(KeyAction::Lock);
        }
        if tab {
            return Some(KeyAction::Shell("overview"));
        }
        if keysym == Keysym::space {
            return Some(KeyAction::Shell("search"));
        }
        if keysym == Keysym::d {
            return Some(KeyAction::Shell("show-desktop"));
        }
        if keysym == Keysym::Up || keysym == Keysym::KP_Up {
            return Some(KeyAction::MaximizeWindow);
        }
        if keysym == Keysym::Down || keysym == Keysym::KP_Down {
            return Some(KeyAction::UnmaximizeOrMinimizeWindow);
        }
        if keysym == Keysym::Left || keysym == Keysym::KP_Left {
            return Some(KeyAction::TileLeft);
        }
        if keysym == Keysym::Right || keysym == Keysym::KP_Right {
            return Some(KeyAction::TileRight);
        }
    }
    if modifiers.logo && (xkb::KEY_1..=xkb::KEY_9).contains(&raw) {
        return Some(KeyAction::Screen((raw - xkb::KEY_1) as usize));
    }
    if modifiers.logo && modifiers.shift && keysym == Keysym::U {
        return Some(KeyAction::RestoreMinimized);
    }
    #[cfg(feature = "debug")]
    if modifiers.logo && modifiers.shift {
        return match keysym {
            Keysym::M => Some(KeyAction::ScaleDown),
            Keysym::P => Some(KeyAction::ScaleUp),
            Keysym::W => Some(KeyAction::TogglePreview),
            Keysym::R => Some(KeyAction::RotateOutput),
            Keysym::T => Some(KeyAction::ToggleTint),
            Keysym::D => Some(KeyAction::ToggleDecorations),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(f: impl FnOnce(&mut ModifiersState)) -> ModifiersState {
        let mut m = ModifiersState::default();
        f(&mut m);
        m
    }

    #[test]
    fn no_shortcut_ends_the_session_or_runs_a_program() {
        let ctrl_alt = mods(|m| {
            m.ctrl = true;
            m.alt = true;
        });
        assert!(process_keyboard_shortcut(ctrl_alt, Keysym::BackSpace).is_none());
        let logo = mods(|m| m.logo = true);
        assert!(process_keyboard_shortcut(logo, Keysym::Return).is_none());
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::q), Some(KeyAction::CloseWindow)));
    }

    #[test]
    fn global_shortcuts_map_to_their_actions() {
        let alt = mods(|m| m.alt = true);
        let alt_shift = mods(|m| {
            m.alt = true;
            m.shift = true;
        });
        let logo = mods(|m| m.logo = true);
        assert!(matches!(
            process_keyboard_shortcut(alt, Keysym::Tab),
            Some(KeyAction::CycleWindows { reverse: false })
        ));
        assert!(matches!(
            process_keyboard_shortcut(alt_shift, Keysym::ISO_Left_Tab),
            Some(KeyAction::CycleWindows { reverse: true })
        ));
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::Tab), Some(KeyAction::Shell("overview"))));
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::space), Some(KeyAction::Shell("search"))));
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::d), Some(KeyAction::Shell("show-desktop"))));
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::l), Some(KeyAction::Lock)));
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::Up), Some(KeyAction::MaximizeWindow)));
        assert!(matches!(
            process_keyboard_shortcut(logo, Keysym::Down),
            Some(KeyAction::UnmaximizeOrMinimizeWindow)
        ));
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::Left), Some(KeyAction::TileLeft)));
        assert!(matches!(process_keyboard_shortcut(logo, Keysym::Right), Some(KeyAction::TileRight)));
        assert!(matches!(
            process_keyboard_shortcut(alt, Keysym::grave),
            Some(KeyAction::CycleAppWindows { reverse: false })
        ));
        assert!(matches!(
            process_keyboard_shortcut(alt_shift, Keysym::asciitilde),
            Some(KeyAction::CycleAppWindows { reverse: true })
        ));
        // Only a bare Super+L: Ctrl+Super+L or Shift+Super+L are someone else's.
        let logo_shift = mods(|m| {
            m.logo = true;
            m.shift = true;
        });
        assert!(!matches!(process_keyboard_shortcut(logo_shift, Keysym::l), Some(KeyAction::Lock)));
        assert!(process_keyboard_shortcut(alt, Keysym::l).is_none());
    }

    #[test]
    fn clamp_point_to_rects_handles_stacked_and_staggered_monitors() {
        use smithay::utils::Rectangle;
        let stacked = [
            Rectangle::new((0, 0).into(), (1920, 1080).into()),
            Rectangle::new((0, 1080).into(), (2560, 1440).into()),
        ];
        // Bottom monitor below y=1080 is reachable without clamping to y=1080!
        assert_eq!(
            clamp_point_to_rects((1500.0, 1500.0).into(), &stacked),
            Point::from((1500.0, 1500.0))
        );
        // Point off the bottom of the bottom monitor clamps to its bottom edge.
        assert_eq!(
            clamp_point_to_rects((1500.0, 3000.0).into(), &stacked),
            Point::from((1500.0, 2519.0))
        );
        // Point outside right edge of narrower top monitor clamps into top monitor or falls cleanly.
        let clamped_top_right = clamp_point_to_rects((2200.0, 200.0).into(), &stacked);
        assert!(stacked.iter().any(|r| r.contains(clamped_top_right.to_i32_round())));
    }
}
