use smithay::{
    backend::renderer::{
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            surface::WaylandSurfaceRenderElement,
            utils::{
                ConstrainAlign, ConstrainScaleBehavior, CropRenderElement, RelocateRenderElement,
                RescaleRenderElement,
            },
            AsRenderElements, RenderElement, Wrap,
        },
        Color32F, ImportAll, ImportMem, Renderer, Texture,
    },
    desktop::space::{
        constrain_space_element, ConstrainBehavior, ConstrainReference, Space, SpaceRenderElements,
    },
    output::Output,
    utils::{Point, Rectangle, Size},
};
use smithay::{
    desktop::{layer_map_for_output, LayerSurface},
    utils::Scale,
    wayland::shell::wlr_layer::Layer,
};
use smithay::desktop::space::SpaceElement;

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    anim::{
        self,
        shader::{self, GlesAccess, Programs, ShadowElement}, geometry,
        depth, genie, warp,
        glint::GlintElement,
        bloom,
        shader::GlowElement,
        ghosts::{self, DrawnGhost, GhostElement},
        transform::{wrap_all, Transformed},
    },
    drawing::{PointerRenderElement, CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN},
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
};

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElements<R> where
        R: ImportAll + ImportMem;
    Pointer=PointerRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
    #[cfg(feature = "debug")]
    // Note: We would like to borrow this element instead, but that would introduce
    // a feature-dependent lifetime, which introduces a lot more feature bounds
    // as the whole type changes and we can't have an unused lifetime (for when "debug" is disabled)
    // in the declaration.
    Fps=FpsElement<R::TextureId>,
}

impl<R: Renderer> std::fmt::Debug for CustomRenderElements<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer(arg0) => f.debug_tuple("Pointer").field(arg0).finish(),
            Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<R, E> where R: ImportAll + ImportMem + GlesAccess;
    Space=SpaceRenderElements<R, E>,
    Window=Wrap<E>,
    Custom=CustomRenderElements<R>,
    Preview=CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>,
    // A window with a running animation (see `anim`).
    Animated=Transformed<WindowRenderElement<R>>,
    // A window that has left the Space but is still being drawn.
    Ghost=GhostElement<R>,
    // A window's drop shadow (see `anim::depth`).
    Shadow=ShadowElement,
    // Lumen's Settle glint over a window ending a geometry transition.
    Glint=GlintElement,
    // Lumen's Bloom glow beneath an opening window (see `anim::bloom`).
    Glow=GlowElement,
}

impl<R: Renderer + ImportAll + ImportMem + GlesAccess, E: RenderElement<R> + std::fmt::Debug> std::fmt::Debug
    for OutputRenderElements<R, E>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Space(arg0) => f.debug_tuple("Space").field(arg0).finish(),
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Custom(arg0) => f.debug_tuple("Custom").field(arg0).finish(),
            Self::Preview(arg0) => f.debug_tuple("Preview").field(arg0).finish(),
            Self::Animated(arg0) => f.debug_tuple("Animated").field(arg0).finish(),
            Self::Ghost(arg0) => f.debug_tuple("Ghost").field(arg0).finish(),
            Self::Shadow(arg0) => f.debug_tuple("Shadow").field(arg0).finish(),
            Self::Glint(arg0) => f.debug_tuple("Glint").field(arg0).finish(),
            Self::Glow(arg0) => f.debug_tuple("Glow").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

pub fn space_preview_elements<'a, R, C>(
    renderer: &'a mut R,
    space: &'a Space<WindowElement>,
    output: &'a Output,
) -> impl Iterator<Item = C> + 'a
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Send + 'static,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>> + 'a,
{
    let constrain_behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let preview_padding = 10;

    let elements_on_space = space.elements_for_output(output).count();
    let output_scale = output.current_scale().fractional_scale();
    let output_transform = output.current_transform();
    let output_size = output
        .current_mode()
        .map(|mode| {
            output_transform
                .transform_size(mode.size)
                .to_f64()
                .to_logical(output_scale)
        })
        .unwrap_or_default();

    let max_elements_per_row = 4;
    let elements_per_row = usize::min(elements_on_space, max_elements_per_row);
    let rows = f64::ceil(elements_on_space as f64 / elements_per_row as f64);

    let preview_size = Size::from((
        f64::round(output_size.w / elements_per_row as f64) as i32 - preview_padding * 2,
        f64::round(output_size.h / rows) as i32 - preview_padding * 2,
    ));

    space
        .elements_for_output(output)
        .enumerate()
        .flat_map(move |(element_index, window)| {
            let column = element_index % elements_per_row;
            let row = element_index / elements_per_row;
            let preview_location = Point::from((
                preview_padding + (preview_padding + preview_size.w) * column as i32,
                preview_padding + (preview_padding + preview_size.h) * row as i32,
            ));
            let constrain = Rectangle::new(preview_location, preview_size);
            constrain_space_element(
                renderer,
                window,
                preview_location,
                1.0,
                output_scale,
                constrain,
                constrain_behavior,
            )
        })
}

/// The Space's windows and the output's layer surfaces, front to back, with
/// each window drawn in its animated pose and ghosts at their old z-slots.
///
/// This is `smithay::desktop::space::space_render_elements` done by hand,
/// because that function hands back one flat list with no per-window
/// transform. Its order is kept: layer surfaces above windows (Overlay, Top),
/// then the windows from the front of the stack to the back (the Space sorts
/// them by z-index, so Bloom's overlay is first and Bloom itself last), then
/// layer surfaces beneath (Bottom, Background).
///
/// Ghosts go directly above their anchor window. One whose anchor is gone is
/// drawn above the desktop (Bloom), beneath every app, as the last resort.
pub fn stack_elements<R>(
    renderer: &mut R,
    space: &Space<WindowElement>,
    output: &Output,
    now: std::time::Duration,
) -> Vec<OutputRenderElements<R, WindowRenderElement<R>>>
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };
    let output_scale = output.current_scale().fractional_scale();
    let scale = Scale::from(output_scale);
    let mut out: Vec<OutputRenderElements<R, WindowRenderElement<R>>> = Vec::new();

    let layer_map = layer_map_for_output(output);
    let (lower, upper): (Vec<&LayerSurface>, Vec<&LayerSurface>) = layer_map
        .layers()
        .rev()
        .partition(|s| matches!(s.layer(), Layer::Background | Layer::Bottom));
    let layer_elements = |renderer: &mut R, layers: Vec<&LayerSurface>| {
        layers
            .into_iter()
            .filter_map(|surface| layer_map.layer_geometry(surface).map(|geo| (geo.loc, surface)))
            .flat_map(|(loc, surface)| {
                AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(
                    surface,
                    renderer,
                    loc.to_physical_precise_round(output_scale),
                    scale,
                    1.0,
                )
                .into_iter()
                .map(|e| OutputRenderElements::Space(SpaceRenderElements::Surface(e)))
                .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    out.extend(layer_elements(renderer, upper));

    // The drop-shadow shader, compiled on the first frame; `None` (say, the
    // compile failed) draws no shadows and no rounded corners.
    let programs = shader::programs(renderer);
    let mut pending: Vec<DrawnGhost<R>> = ghosts::draw_all(renderer, output_geo.loc, output_scale, now);
    let mut emit_ghosts = |out: &mut Vec<OutputRenderElements<R, WindowRenderElement<R>>>,
                           pending: &mut Vec<DrawnGhost<R>>,
                           anchored_to: Option<&WindowElement>| {
        let mut i = 0;
        while i < pending.len() {
            let take = match (&pending[i].anchor, anchored_to) {
                (Some(a), Some(w)) => a == w,
                (None, None) => true,
                _ => false,
            };
            if take {
                let ghost = pending.remove(i);
                out.extend(ghost.elements.into_iter().map(OutputRenderElements::Ghost));
                out.extend(ghost.shadow.map(OutputRenderElements::Shadow));
            } else {
                i += 1;
            }
        }
    };

    for window in space.elements().rev() {
        let Some(location) = space.element_location(window) else {
            continue;
        };
        // The desktop is the bottom of the app stack: ghosts whose anchor
        // left with them (or that had none) land above it.
        if window.is_shell() {
            let orphans: Vec<_> = pending.drain(..).collect();
            for ghost in orphans {
                out.extend(ghost.elements.into_iter().map(OutputRenderElements::Ghost));
                out.extend(ghost.shadow.map(OutputRenderElements::Shadow));
            }
        }
        emit_ghosts(&mut out, &mut pending, Some(window));

        if !space.element_bbox(window).is_some_and(|bbox| output_geo.overlaps(bbox)) {
            continue;
        }
        let frame = Rectangle::new(location, window.geometry().size);
        let render_origin = location - window.geometry().loc - output_geo.loc;
        let physical_origin = render_origin.to_physical_precise_round(scale);
        let pose = window.animation_transform(now, frame);
        // Another kind of animation has the window (open, minimize): it wins,
        // and the geometry transition is moot.
        if pose.is_some() && window.geometry_animating() {
            window.cancel_geometry();
        }
        // The pose a geometry transition gives the shadow (the window's own
        // animation pose, if any, wins).
        let mut geometry_pose = None;
        // A restore drawn as the genie warp casts its shadow from where the
        // window will be, fading early (see `anim::genie::shadow_keep`).
        let mut warped_pose = None;
        match pose {
            Some(pose) => {
                let place = warp::Placement {
                    frame,
                    render_origin: physical_origin,
                    output_origin: output_geo.loc,
                    scale: output_scale,
                };
                // A Rise, or a show desktop's Emerge (`anim::recede`), both
                // through Lumen; `None` draws the live window under the pose.
                let warped = window
                    .with_animation(|a| {
                        a.warp
                            .as_ref()
                            .and_then(|w| w.draw(renderer, &place, &pose))
                            .or_else(|| a.recede.as_ref().and_then(|r| r.draw(renderer, &place, &pose)))
                    })
                    .flatten();
                match warped {
                    Some(drawn) => {
                        warped_pose = Some(anim::WindowTransform {
                            alpha: pose.alpha * genie::shadow_keep(drawn.progress) as f32,
                            ..anim::WindowTransform::IDENTITY
                        });
                        out.push(OutputRenderElements::Ghost(GhostElement::Warp(drawn.element)));
                    }
                    None => {
                        let mut elements: Vec<WindowRenderElement<R>> =
                            window.frame_render_elements(renderer, physical_origin, scale, pose.alpha, true);
                        // An open drawn as Lumen's Bloom: blurred, tinted,
                        // rounded live surfaces, and a glow beneath them.
                        let look = window.with_animation(|a| a.bloom.as_ref().map(|b| b.look(now))).flatten();
                        let mut glow = None;
                        if let Some(look) = look {
                            let unposed = depth::pose_frame(frame, None, output_geo.loc, output_scale);
                            elements = bloom::apply(renderer, elements, &look, unposed, scale);
                            let posed = depth::pose_frame(frame, Some(&pose), output_geo.loc, output_scale);
                            glow = bloom::glow(renderer, &look, posed, pose.alpha);
                        }
                        let physical = pose.to_physical(output_geo.loc, scale);
                        out.extend(wrap_all(elements, physical).into_iter().map(OutputRenderElements::Animated));
                        out.extend(glow.map(OutputRenderElements::Glow));
                    }
                }
            }
            None => match geometry::draw(window, renderer, frame, physical_origin, output_geo.loc, scale, now) {
                // Maximizing, unmaximizing, fullscreen, a fit: the live window
                // in front of the snapshot of its old frame, both scaled to
                // the interpolated rect (`anim::geometry`).
                Some(drawn) => {
                    geometry_pose = Some(drawn.shadow_pose);
                    out.extend(drawn.glint.map(OutputRenderElements::Glint));
                    out.extend(drawn.live.into_iter().map(OutputRenderElements::Animated));
                    out.extend(
                        drawn
                            .snapshot
                            .into_iter()
                            .map(|e| OutputRenderElements::Ghost(GhostElement::Snapshot(e))),
                    );
                }
                None => {
                    let elements: Vec<WindowRenderElement<R>> =
                        AsRenderElements::<R>::render_elements(window, renderer, physical_origin, scale, 1.0);
                    out.extend(
                        elements
                            .into_iter()
                            .map(|e| OutputRenderElements::Space(SpaceRenderElements::Element(Wrap::from(e)))),
                    );
                }
            },
        }
        // The shadow goes beneath the window (this list is front to back) and
        // above whatever is below it. It follows the window's pose, or the
        // rect a geometry transition is drawing it at.
        if let Some(shadow) = programs
            .as_ref()
            .and_then(|p| {
                window_shadow(
                    p,
                    window,
                    frame,
                    warped_pose.as_ref().or(pose.as_ref()).or(geometry_pose.as_ref()),
                    output_geo.loc,
                    output_scale,
                    now,
                )
            })
        {
            out.push(OutputRenderElements::Shadow(shadow));
        }
    }
    // No desktop window in the stack: whatever is left goes last.
    for ghost in pending {
        out.extend(ghost.elements.into_iter().map(OutputRenderElements::Ghost));
        out.extend(ghost.shadow.map(OutputRenderElements::Shadow));
    }

    out.extend(layer_elements(renderer, lower));
    out
}

/// `window`'s drop shadow this frame, `None` if it casts none (the shell, a
/// maximized window, ...) or has nothing to draw. `pose` is its animation
/// transform, which the shadow follows.
fn window_shadow(
    programs: &Programs,
    window: &WindowElement,
    frame: Rectangle<i32, smithay::utils::Logical>,
    pose: Option<&anim::WindowTransform>,
    output_origin: Point<i32, smithay::utils::Logical>,
    output_scale: f64,
    now: std::time::Duration,
) -> Option<ShadowElement> {
    let style = window.shadow_style(now)?;
    let alpha = pose.map_or(1.0, |p| p.alpha) * window.shadow_appear(now) as f32;
    let px = output_scale * pose.map_or(1.0, |p| (p.scale.x + p.scale.y) / 2.0);
    let physical = depth::pose_frame(frame, pose, output_origin, output_scale);
    let params = depth::shadow_params(physical, &style, px, alpha)?;
    Some(window.shadow_element(programs, params))
}

#[profiling::function]
pub fn output_elements<R>(
    output: &Output,
    space: &Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &mut R,
    show_window_preview: bool,
) -> (Vec<OutputRenderElements<R, WindowRenderElement<R>>>, Color32F)
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Texture + Send + 'static,
{
    // End the geometry transitions nobody can draw (a window minimized or
    // closed mid-transition), so they cannot hold a fullscreen hand-over or a
    // counter open.
    geometry::sweep(anim::now());

    // A window entering or leaving fullscreen goes through the normal path
    // (which draws the whole stack, the overlay above it) until its
    // transition ends, then hands over to the exclusive one.
    if let Some(window) = output
        .user_data()
        .get::<FullscreenSurface>()
        .and_then(|f| f.get())
        .filter(|w| !w.geometry_animating())
    {
        let scale = output.current_scale().fractional_scale().into();
        let window_render_elements: Vec<WindowRenderElement<R>> =
            AsRenderElements::<R>::render_elements(&window, renderer, (0, 0).into(), scale, 1.0);

        // The shell overlay (toasts) is still drawn, above the app: Evo's
        // exclusive fullscreen would otherwise hide every notification for as
        // long as a video plays. It sits in the space at the output's origin,
        // so its render origin is that location less its geometry's, in the
        // output's own coordinates like the fullscreen window's (0, 0).
        let output_origin = space.output_geometry(output).map(|g| g.loc).unwrap_or_default();
        let overlay_render_elements: Vec<WindowRenderElement<R>> = space
            .elements()
            .find(|w| w.is_overlay())
            .and_then(|overlay| {
                let location = space.element_location(overlay)? - overlay.geometry().loc - output_origin;
                Some(AsRenderElements::<R>::render_elements(
                    overlay,
                    renderer,
                    location.to_physical_precise_round(scale),
                    scale,
                    1.0,
                ))
            })
            .unwrap_or_default();

        // Front to back: pointer, overlay, then the fullscreen window.
        let elements = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .chain(
                overlay_render_elements
                    .into_iter()
                    .map(|e| OutputRenderElements::Window(Wrap::from(e))),
            )
            .chain(
                window_render_elements
                    .into_iter()
                    .map(|e| OutputRenderElements::Window(Wrap::from(e))),
            )
            .collect::<Vec<_>>();
        (elements, CLEAR_COLOR_FULLSCREEN)
    } else {
        let mut output_render_elements = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .collect::<Vec<_>>();

        if show_window_preview && space.elements_for_output(output).count() > 0 {
            output_render_elements.extend(space_preview_elements(renderer, space, output));
        }

        output_render_elements.extend(stack_elements(renderer, space, output, anim::now()));

        (output_render_elements, CLEAR_COLOR)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_output<'a, 'd, R>(
    output: &'a Output,
    space: &'a Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &'a mut R,
    framebuffer: &'a mut R::Framebuffer<'_>,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
    show_window_preview: bool,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R::Error>>
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let (elements, clear_color) =
        output_elements(output, space, custom_elements, renderer, show_window_preview);
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}
