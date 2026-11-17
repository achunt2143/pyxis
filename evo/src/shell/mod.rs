use std::cell::RefCell;

#[cfg(feature = "xwayland")]
use smithay::xwayland::XWaylandClientData;

#[cfg(feature = "udev")]
use smithay::wayland::drm_syncobj::DrmSyncobjCachedState;

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    desktop::{
        layer_map_for_output, space::SpaceElement, LayerSurface, PopupKind, PopupManager, Space,
        WindowSurfaceType,
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    reexports::{
        calloop::Interest,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_output, wl_surface::WlSurface},
            Client, Resource,
        },
    },
    utils::{IsAlive, Logical, Point, Rectangle, Size},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            add_blocker, add_pre_commit_hook, get_parent, is_sync_subsurface, with_states,
            with_surface_tree_upward, BufferAssignment, CompositorClientState, CompositorHandler,
            CompositorState, SurfaceAttributes, TraversalAction,
        },
        dmabuf::get_dmabuf,
        shell::{
            wlr_layer::{
                Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::XdgToplevelSurfaceData,
        },
    },
};

use crate::{
    state::{EvoState, Backend},
    ClientState,
};

mod element;
mod grabs;
mod animation;
mod geometry_anim;
pub(crate) mod busy;
pub(crate) mod frost;
pub(crate) mod identity;
pub(crate) mod lock;
pub(crate) mod minimize;
pub(crate) mod overlay;
pub(crate) mod placement;
pub(crate) mod recede;
pub(crate) mod switcher;
pub(crate) mod thumbnails;
pub(crate) mod ssd;
pub(crate) mod work_area;
#[cfg(feature = "xwayland")]
mod x11;
mod xdg;
pub mod workspace;

pub use self::element::*;
pub use self::grabs::*;
pub use workspace::*;
pub(crate) use xdg::{OVERLAY_APP_ID, SHELL_APP_ID};

#[derive(Default)]
pub struct FullscreenSurface(RefCell<Option<WindowElement>>);

impl FullscreenSurface {
    pub fn set(&self, window: WindowElement) {
        *self.0.borrow_mut() = Some(window);
    }

    /// The window that has the output to itself, if any. Never a dead one,
    /// and never the shell's: the desktop vela window asks for fullscreen
    /// while it still claims `vela-default`, before it is known to be the
    /// shell, so it can land here as an app; held, it made the output draw
    /// (and route input to) only the desktop and the overlay, over black.
    pub fn get(&self) -> Option<WindowElement> {
        let mut window = self.0.borrow_mut();
        if window.as_ref().is_some_and(|w| !w.alive() || w.is_shell_window()) {
            *window = None;
        }
        window.clone()
    }

    pub fn clear(&self) -> Option<WindowElement> {
        self.0.borrow_mut().take()
    }
}

impl<BackendData: Backend> BufferHandler for EvoState<BackendData> {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl<BackendData: Backend> CompositorHandler for EvoState<BackendData> {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        #[cfg(feature = "xwayland")]
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        panic!("Unknown client data type")
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, move |state, _dh, surface| {
            #[cfg(feature = "udev")]
            let mut acquire_point = None;
            let maybe_dmabuf = with_states(surface, |surface_data| {
                #[cfg(feature = "udev")]
                acquire_point.clone_from(
                    &surface_data
                        .cached_state
                        .get::<DrmSyncobjCachedState>()
                        .pending()
                        .acquire_point,
                );
                surface_data
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .and_then(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).cloned().ok(),
                        _ => None,
                    })
            });
            if let Some(dmabuf) = maybe_dmabuf {
                #[cfg(feature = "udev")]
                if let Some(acquire_point) = acquire_point {
                    if let Ok((blocker, source)) = acquire_point.generate_blocker() {
                        let client = surface.client().unwrap();
                        let res = state.handle.insert_source(source, move |_, _, data| {
                            let dh = data.display_handle.clone();
                            data.client_compositor_state(&client).blocker_cleared(data, &dh);
                            Ok(())
                        });
                        if res.is_ok() {
                            add_blocker(surface, blocker);
                            return;
                        }
                    }
                }
                if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                    if let Some(client) = surface.client() {
                        let res = state.handle.insert_source(source, move |_, _, data| {
                            let dh = data.display_handle.clone();
                            data.client_compositor_state(&client).blocker_cleared(data, &dh);
                            Ok(())
                        });
                        if res.is_ok() {
                            add_blocker(surface, blocker);
                        }
                    }
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.backend_data.early_import(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            // Tracked, not just mapped: a minimized window keeps committing,
            // and skipping `on_commit` for it left its geometry stale for
            // when it came back.
            if let Some(window) = self.tracked_window_for_surface(&root) {
                window.0.on_commit();
                let mapped = self.is_window_mapped(&window);
                if window.placement_pending() && mapped {
                    self.settle_placement(&window);
                }
                if mapped {
                    // A floating window bigger than the work area (or outside
                    // it) is shrunk and moved in; a no-op once it fits.
                    self.fit_to_work_area(&window);
                    self.maybe_start_open_animation(&window, &root);
                }
                if window.focus_pending() {
                    self.focus_new_window_if_mapped(&window, &root);
                }

                if &root == surface {
                    let buffer_offset = with_states(surface, |states| {
                        states
                            .cached_state
                            .get::<SurfaceAttributes>()
                            .current()
                            .buffer_delta
                            .take()
                    });

                    if let Some(buffer_offset) = buffer_offset {
                        let current_loc = self
                            .space
                            .element_location(&window)
                            .or_else(|| self.workspace_manager.state(&window).map(|s| s.location));
                        if let Some(current_loc) = current_loc {
                            self.place_window(&window, current_loc + buffer_offset, false);
                        }
                    }
                    self.handle_toplevel_commit(surface);
                }
                if !&root.alive() {
                    self.remove_window_from_workspace(&window);
                }
            }
        }
        self.popups.commit(surface);

        if matches!(&self.cursor_status, CursorImageStatus::Surface(cursor_surface) if cursor_surface == surface)
        {
            with_states(surface, |states| {
                let cursor_image_attributes = states.data_map.get::<CursorImageSurfaceData>();

                if let Some(mut cursor_image_attributes) =
                    cursor_image_attributes.map(|attrs| attrs.lock().unwrap())
                {
                    let buffer_delta = states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer_delta
                        .take();
                    if let Some(buffer_delta) = buffer_delta {
                        tracing::trace!(hotspot = ?cursor_image_attributes.hotspot, ?buffer_delta, "decrementing cursor hotspot");
                        cursor_image_attributes.hotspot -= buffer_delta;
                    }
                }
            });
        }

        if matches!(&self.dnd_icon, Some(icon) if &icon.surface == surface) {
            let dnd_icon = self.dnd_icon.as_mut().unwrap();
            with_states(&dnd_icon.surface, |states| {
                let buffer_delta = states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .buffer_delta
                    .take()
                    .unwrap_or_default();
                tracing::trace!(offset = ?dnd_icon.offset, ?buffer_delta, "moving dnd offset");
                dnd_icon.offset += buffer_delta;
            });
        }

        ensure_initial_configure(surface, &self.space, &mut self.popups)
    }
}

impl<BackendData: Backend> WlrLayerShellHandler for EvoState<BackendData> {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .unwrap_or_else(|| self.space.outputs().next().unwrap().clone());
        let mut map = layer_map_for_output(&output);
        map.map_layer(&LayerSurface::new(surface, namespace)).unwrap();
        map.arrange();
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        if let Some((mut map, layer)) = self.space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            let layer = map
                .layers()
                .find(|&layer| layer.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (map, layer))
        }) {
            map.unmap_layer(&layer);
            map.arrange();
        }
    }
}

impl<BackendData: Backend> EvoState<BackendData> {
    // pub fn window_for_surface(&self, surface: &WlSurface) -> Option<WindowElement> {
    //     self.space
    //         .elements()
    //         .find(|window| window.wl_surface().map(|s| &*s == surface).unwrap_or(false))
    //         .cloned()
    // }
}

#[derive(Default)]
pub struct SurfaceData {
    pub geometry: Option<Rectangle<i32, Logical>>,
    pub resize_state: ResizeState,
}

fn ensure_initial_configure(surface: &WlSurface, space: &Space<WindowElement>, popups: &mut PopupManager) {
    with_surface_tree_upward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| {
            states
                .data_map
                .insert_if_missing(|| RefCell::new(SurfaceData::default()));
        },
        |_, _, _| true,
    );

    if let Some(window) = space
        .elements()
        .find(|window| window.wl_surface().map(|s| &*s == surface).unwrap_or(false))
        .cloned()
    {
        // send the initial configure if relevant
        #[cfg_attr(not(feature = "xwayland"), allow(irrefutable_let_patterns))]
        if let Some(toplevel) = window.0.toplevel() {
            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .initial_configure_sent
            });
            if !initial_configure_sent {
                toplevel.send_configure();
            }
        }

        with_states(surface, |states| {
            let mut data = states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut();

            // Finish resizing.
            if let ResizeState::WaitingForCommit(_) = data.resize_state {
                data.resize_state = ResizeState::NotResizing;
            }
        });

        return;
    }

    if let Some(popup) = popups.find_popup(surface) {
        let popup = match popup {
            PopupKind::Xdg(ref popup) => popup,
            // Doesn't require configure
            PopupKind::InputMethod(ref _input_popup) => {
                return;
            }
        };

        if !popup.is_initial_configure_sent() {
            // NOTE: This should never fail as the initial configure is always
            // allowed.
            popup.send_configure().expect("initial configure failed");
        }

        return;
    };

    if let Some(output) = space.outputs().find(|o| {
        let map = layer_map_for_output(o);
        map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some()
    }) {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });

        let mut map = layer_map_for_output(output);

        // arrange the layers before sending the initial configure
        // to respect any size the client may have sent
        map.arrange();
        // send the initial configure if relevant
        if !initial_configure_sent {
            let layer = map
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .unwrap();

            layer.layer_surface().send_configure();
        }
    };
}

/// Pick a random top-left origin within the upper-left two-thirds of `area`,
/// guarding against sub-3px spans so `Uniform::new` cannot panic.
pub(crate) fn random_point_in_area(area: Rectangle<i32, Logical>) -> Point<i32, Logical> {
    use rand::distributions::{Distribution, Uniform};

    let span_x = (((area.size.w as f32) / 3.0) * 2.0) as i32;
    let span_y = (((area.size.h as f32) / 3.0) * 2.0) as i32;
    let mut rng = rand::thread_rng();
    let x = if span_x > 0 {
        Uniform::new(area.loc.x, area.loc.x + span_x).sample(&mut rng)
    } else {
        area.loc.x
    };
    let y = if span_y > 0 {
        Uniform::new(area.loc.y, area.loc.y + span_y).sample(&mut rng)
    } else {
        area.loc.y
    };
    (x, y).into()
}

/// Resolve output positions on layout fixup: preserve explicit non-overlapping
/// `(x, y)` coordinates (e.g. stacked monitors from `ApplyOutputConfigs`), and
/// only reflow into a flat horizontal strip when outputs are unpositioned /
/// overlapping.
pub(crate) fn resolve_output_positions(
    rects: &[Rectangle<i32, Logical>],
) -> Vec<Point<i32, Logical>> {
    let any_overlap = rects.iter().enumerate().any(|(i, a)| {
        a.size.w > 0
            && a.size.h > 0
            && rects[i + 1..]
                .iter()
                .any(|b| b.size.w > 0 && b.size.h > 0 && a.overlaps(*b))
    });
    if !any_overlap {
        return rects.iter().map(|r| r.loc).collect();
    }
    let mut offset_x = 0;
    rects
        .iter()
        .map(|r| {
            let loc = Point::from((offset_x, 0));
            offset_x += r.size.w.max(0);
            loc
        })
        .collect()
}

/// Whether an app window's visible frame geometry (`location` + `geometry().size`,
/// excluding negative `-8px` SSD resize band / CSD shadow offsets) is completely
/// outside every live usable output area.
pub(crate) fn is_window_orphaned(
    frame_geo: Rectangle<i32, Logical>,
    outputs: &[Rectangle<i32, Logical>],
) -> bool {
    if outputs.is_empty() {
        return false;
    }
    !outputs.iter().any(|o_geo| o_geo.overlaps(frame_geo))
}

#[allow(dead_code)]
fn place_new_window(
    space: &mut Space<WindowElement>,
    pointer_location: Point<f64, Logical>,
    window: &WindowElement,
    activate: bool,
) -> Point<i32, Logical> {
    // place the window at a random location on same output as pointer
    // or if there is not output in a [0;800]x[0;800] square
    let output = space
        .output_under(pointer_location)
        .next()
        .or_else(|| space.outputs().next())
        .cloned();
    let output_geometry = output
        .and_then(|o| {
            let geo = space.output_geometry(&o)?;
            let map = layer_map_for_output(&o);
            let zone = map.non_exclusive_zone();
            Some(work_area::usable(geo, zone))
        })
        .unwrap_or_else(|| Rectangle::from_size((800, 800).into()));

    // set the initial toplevel bounds
    #[allow(irrefutable_let_patterns)]
    if let Some(toplevel) = window.0.toplevel() {
        toplevel.with_pending_state(|state| {
            state.bounds = Some(output_geometry.size);
        });
    }

    let loc = random_point_in_area(output_geometry);
    space.map_element(window.clone(), loc, activate);
    loc
}

/// Preserve explicit non-overlapping output coordinates (or reflow into a row if
/// overlapping) and re-place app windows stranded off every output. Returns
/// the windows it moved, so the caller (`EvoState::fixup_positions`) can record
/// their new places: this has only the space.
pub fn fixup_positions(
    space: &mut Space<WindowElement>,
    pointer_location: Point<f64, Logical>,
) -> Vec<(WindowElement, Point<i32, Logical>)> {
    // fixup outputs
    let current_outputs: Vec<_> = space.outputs().cloned().collect();
    let current_rects: Vec<_> = current_outputs
        .iter()
        .map(|output| {
            space
                .output_geometry(output)
                .unwrap_or_else(|| Rectangle::from_size(Size::from((0, 0))))
        })
        .collect();
    let resolved_locs = resolve_output_positions(&current_rects);
    for ((output, old_rect), new_loc) in current_outputs
        .into_iter()
        .zip(current_rects)
        .zip(resolved_locs)
    {
        if new_loc != old_rect.loc {
            output.change_current_state(None, None, None, Some(new_loc));
        }
        space.map_output(&output, new_loc);
        layer_map_for_output(&output).arrange();
    }

    // fixup windows
    let mut orphaned_windows = Vec::new();
    let outputs = space
        .outputs()
        .flat_map(|o| {
            let geo = space.output_geometry(o)?;
            let map = layer_map_for_output(o);
            let zone = map.non_exclusive_zone();
            Some(Rectangle::new(geo.loc + zone.loc, zone.size))
        })
        .collect::<Vec<_>>();
    for window in space.elements() {
        if window.is_shell_window() {
            continue;
        }
        let Some(window_location) = space.element_location(window) else {
            continue;
        };
        let frame_size = window.geometry().size;
        let frame_geo = Rectangle::new(
            window_location,
            (frame_size.w.max(1), frame_size.h.max(1)).into(),
        );
        if is_window_orphaned(frame_geo, &outputs) {
            orphaned_windows.push(window.clone());
        }
    }
    orphaned_windows
        .into_iter()
        .map(|window| {
            let location = place_new_window(space, pointer_location, &window, false);
            (window, location)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_point_in_area_handles_tiny_dimensions_without_panicking() {
        for (w, h) in [(0, 0), (1, 1), (2, 2), (3, 1), (1, 3)] {
            let area = Rectangle::new((10, 20).into(), (w, h).into());
            let pt = random_point_in_area(area);
            assert!(pt.x >= 10);
            assert!(pt.y >= 20);
        }
        // Negative-sized Rectangles constructed directly via fields also guard cleanly.
        let negative = Rectangle {
            loc: Point::from((10, 20)),
            size: Size::from((0, 0)),
        };
        assert_eq!(random_point_in_area(negative), Point::from((10, 20)));
    }

    #[test]
    fn resolve_output_positions_preserves_disjoint_xy_and_reflows_overlaps() {
        let stacked = [
            Rectangle::new((0, 0).into(), (1920, 1080).into()),
            Rectangle::new((100, 1080).into(), (2560, 1440).into()),
        ];
        assert_eq!(
            resolve_output_positions(&stacked),
            vec![Point::from((0, 0)), Point::from((100, 1080))]
        );

        let overlapping = [
            Rectangle::new((0, 0).into(), (1920, 1080).into()),
            Rectangle::new((1000, 0).into(), (1920, 1080).into()),
        ];
        assert_eq!(
            resolve_output_positions(&overlapping),
            vec![Point::from((0, 0)), Point::from((1920, 0))]
        );
    }

    #[test]
    fn edge_flush_frame_is_not_orphaned_despite_negative_resize_band_offset() {
        let outputs = [Rectangle::new((0, 0).into(), (1920, 1080).into())];
        let edge_flush = Rectangle::new((0, 0).into(), (800, 600).into());
        assert!(!is_window_orphaned(edge_flush, &outputs));

        let off_screen = Rectangle::new((2000, 100).into(), (800, 600).into());
        assert!(is_window_orphaned(off_screen, &outputs));
    }
}
