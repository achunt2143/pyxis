//! Ghosts: windows that have left the Space but are still drawn.
//!
//! A closing window is a `Snapshot` taken before its buffer went; a
//! minimizing one (phase 2) is the live `WindowElement`, whose surface lives
//! on. Either way a ghost is drawn at the z-slot its window had: directly
//! above the `anchor`, the window that was next below it in the Space when it
//! left. If the anchor has gone too, it is drawn above the desktop, beneath
//! every app. A ghost takes no input: it never enters the Space.
//!
//! The list is thread-local. Everything that adds, draws and prunes ghosts
//! runs on the compositor thread, and `WindowElement` is not `Send`.

use std::{cell::RefCell, time::Duration};

use smithay::{
    backend::renderer::{
        element::{texture::TextureRenderElement, Id, Kind},
        utils::CommitCounter,
        ImportAll, ImportMem, Renderer, Texture,
    },
    render_elements,
    utils::{IsAlive, Logical, Point, Rectangle, Transform},
};

use super::{
    depth,
    evaporate::{self, Evaporate, EvaporateElement},
    genie,
    shader::{GlesAccess, ShadowElement},
    snapshot::Snapshot,
    transform::{wrap, wrap_all, Transformed, WindowTransform},
    warp::{self, WarpElement},
    window::WindowAnim,
};
use crate::shell::{WindowElement, WindowRenderElement};

pub enum GhostSource {
    /// A closing window, drawn from pixels.
    Snapshot(Snapshot),
    /// A closing window drawn as Lumen's Evaporate (`anim::evaporate`), whose
    /// snapshot is drawn plain if the program can't be.
    Evaporate(Box<Evaporate>),
    /// A window that is out of the Space but still has a live surface.
    Live(WindowElement),
}

pub struct Ghost {
    pub source: GhostSource,
    /// The window this ghost stands for. Needed to keep the stack's order when
    /// that window is some other ghost's anchor (see [`add`]).
    pub owner: Option<WindowElement>,
    pub anim: WindowAnim,
    /// The window next below this one when it left the Space.
    pub anchor: Option<WindowElement>,
    /// The visible frame, output-logical (global space) coordinates.
    pub frame: Rectangle<i32, Logical>,
    /// The window's render origin (where its buffer's top-left is), global
    /// space coordinates.
    pub render_origin: Point<i32, Logical>,
}

render_elements! {
    pub GhostElement<R> where R: ImportAll + ImportMem + GlesAccess;
    Snapshot=Transformed<TextureRenderElement<R::TextureId>>,
    Live=Transformed<WindowRenderElement<R>>,
    /// A minimize or restore drawn as the genie mesh (`anim::warp`).
    Warp=WarpElement,
    /// A close drawn as Evaporate.
    Evaporate=EvaporateElement,
}

impl<R: Renderer> std::fmt::Debug for GhostElement<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Snapshot(a) => f.debug_tuple("Snapshot").field(a).finish(),
            Self::Live(a) => f.debug_tuple("Live").field(a).finish(),
            Self::Warp(a) => f.debug_tuple("Warp").field(a).finish(),
            Self::Evaporate(a) => f.debug_tuple("Evaporate").field(a).finish(),
            Self::_GenericCatcher(a) => f.debug_tuple("_GenericCatcher").field(a).finish(),
        }
    }
}

thread_local! {
    static GHOSTS: RefCell<Vec<Ghost>> = const { RefCell::new(Vec::new()) };
}

/// Start drawing a ghost.
///
/// **The list is kept in stack order, back to front**, and a ghost goes in at
/// the z-slot its window had. Two things need doing for that when a window
/// leaves the Space:
///
/// - a ghost anchored to the leaving window (it sat directly above it) would
///   otherwise lose its anchor and fall to the bottom of the stack, so it is
///   re-anchored to what the leaving window was anchored to;
/// - and it stays *above* the new ghost, which goes in just below the first of
///   them. Before this, ghosts of two separate minimizes stacked newest on top
///   whatever order their windows were in.
///
/// With no such ghost the new one goes on top of its anchor's other ghosts
/// (it is the newest).
pub fn add(ghost: Ghost) {
    GHOSTS.with(|g| {
        let mut g = g.borrow_mut();
        let at = match &ghost.owner {
            Some(owner) => {
                let mut first = None;
                for (i, other) in g.iter_mut().enumerate() {
                    if reanchor(&mut other.anchor, owner, &ghost.anchor) {
                        first.get_or_insert(i);
                    }
                }
                first.unwrap_or(g.len())
            }
            None => g.len(),
        };
        g.insert(at, ghost);
    });
}

/// If `anchor` is `leaving`, replace it with `new` and say so. The ordering
/// rule of [`add`], on plain values so it can be tested without windows.
fn reanchor<A: PartialEq + Clone>(anchor: &mut Option<A>, leaving: &A, new: &Option<A>) -> bool {
    if anchor.as_ref() == Some(leaving) {
        *anchor = new.clone();
        true
    } else {
        false
    }
}

/// How many ghosts are being drawn.
pub fn count() -> usize {
    GHOSTS.with(|g| g.borrow().len())
}

/// Drop every ghost (completing their animations).
pub fn clear() {
    GHOSTS.with(|g| g.borrow_mut().clear());
}

/// Drop every ghost that is not an animation of `kind`, completing it. The
/// locked screen draws only the Veil's ghosts: a window closing or minimizing
/// as the lock begins must not be drawn for even a frame.
pub fn retain_kind(kind: super::window::AnimKind) {
    GHOSTS.with(|g| g.borrow_mut().retain(|ghost| ghost.anim.kind == kind));
}

/// Whether some ghost is still drawn on behalf of `window`.
pub fn has_ghost_of(window: &WindowElement) -> bool {
    GHOSTS.with(|g| {
        g.borrow().iter().any(|ghost| matches!(&ghost.source, GhostSource::Live(w) if w == window))
    })
}

/// Take the live ghost of `window` out of the list, if it has one: a window
/// restored mid-minimize turns that ghost's animation round instead of
/// starting over. Dropping the returned ghost completes its animation.
pub fn take_live(window: &WindowElement) -> Option<Ghost> {
    GHOSTS.with(|g| {
        let mut g = g.borrow_mut();
        let at = g
            .iter()
            .position(|ghost| matches!(&ghost.source, GhostSource::Live(w) if w == window))?;
        Some(g.remove(at))
    })
}

/// One ghost's elements for this frame, and where in the stack they go.
pub struct DrawnGhost<R: Renderer + ImportAll + ImportMem + GlesAccess> {
    pub anchor: Option<WindowElement>,
    pub elements: Vec<GhostElement<R>>,
    /// The drop shadow, drawn directly beneath `elements` (so after them: the
    /// list is front to back). Follows the ghost's pose.
    pub shadow: Option<ShadowElement>,
}

/// Prune the ghosts that have finished at `now` (freeing their snapshots) and
/// draw the rest, front-most first (the list is back to front, see [`add`]).
pub fn draw_all<R>(
    renderer: &mut R,
    output_origin: Point<i32, Logical>,
    output_scale: f64,
    now: Duration,
) -> Vec<DrawnGhost<R>>
where
    R: Renderer + ImportAll + ImportMem + GlesAccess,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let programs = super::shader::programs(renderer);
    GHOSTS.with(|ghosts| {
        let mut ghosts = ghosts.borrow_mut();
        // A live ghost whose window died (closed while minimizing) has no
        // surface left to draw.
        ghosts.retain(|g| {
            g.anim.sample(now, g.frame).is_some()
                && !matches!(&g.source, GhostSource::Live(w) if !w.alive())
        });
        let scale = smithay::utils::Scale::from(output_scale);
        ghosts
            .iter()
            .rev()
            .filter_map(|ghost| {
                let t = ghost.anim.sample(now, ghost.frame)?;
                let phys = t.to_physical(output_origin, scale);
                let render_origin = (ghost.render_origin - output_origin).to_physical_precise_round(scale);
                // A minimize drawn as the genie mesh, when it has the picture
                // for it and the program works; else the transform of the live
                // window below (genie-lite), which the same spring drives.
                let place = warp::Placement {
                    frame: ghost.frame,
                    render_origin,
                    output_origin,
                    scale: output_scale,
                };
                // A show desktop's Recede draws through Lumen the same way
                // (`anim::recede`), and falls back the same way.
                let warped = ghost
                    .anim
                    .warp
                    .as_ref()
                    .and_then(|w| w.draw(renderer, &place, &t))
                    .or_else(|| ghost.anim.recede.as_ref().and_then(|r| r.draw(renderer, &place, &t)))
                    // The lock's Veil, the same way (`anim::veil`).
                    .or_else(|| ghost.anim.veil.as_ref().and_then(|v| v.draw(renderer, &place, &t)));
                let progress = warped.as_ref().map(|d| d.progress);
                // A close drawn as Evaporate: its progress, and its frame (or
                // `None` to draw the snapshot plain, fading evenly).
                let evaporating = match &ghost.source {
                    GhostSource::Evaporate(ev) => {
                        let p = evaporate::progress(&ghost.anim.anim, now);
                        let place = evaporate::Placement {
                            frame: ghost.frame,
                            render_origin: ghost.render_origin,
                            output_origin,
                            scale: output_scale,
                        };
                        Some((p, ev.draw(renderer, &place, &t, p)))
                    }
                    _ => None,
                };
                let evaporate_p = evaporating.as_ref().map(|(p, _)| *p);
                let elements = match (&ghost.source, warped) {
                    (_, Some(drawn)) => vec![GhostElement::Warp(drawn.element)],
                    (GhostSource::Evaporate(ev), None) => {
                        match evaporating.and_then(|(_, e)| e) {
                            Some(element) => vec![GhostElement::Evaporate(element)],
                            None => {
                                let snap = ev.snapshot();
                                let texture = snap.texture(renderer)?;
                                let size = snap.size.to_f64().to_logical(scale).to_i32_round();
                                let alpha = t.alpha * evaporate::fallback_alpha(evaporate_p.unwrap_or(1.0)) as f32;
                                let element = TextureRenderElement::from_static_texture(
                                    snap.id.clone(),
                                    renderer.context_id(),
                                    (render_origin + snap.origin).to_f64(),
                                    texture,
                                    1,
                                    Transform::Normal,
                                    Some(alpha),
                                    None,
                                    Some(size),
                                    None,
                                    Kind::Unspecified,
                                );
                                vec![GhostElement::Snapshot(wrap(element, phys))]
                            }
                        }
                    }
                    (GhostSource::Snapshot(snap), None) => {
                        let texture = snap.texture(renderer)?;
                        let size = snap.size.to_f64().to_logical(scale).to_i32_round();
                        let element = TextureRenderElement::from_static_texture(
                            snap.id.clone(),
                            renderer.context_id(),
                            (render_origin + snap.origin).to_f64(),
                            texture,
                            1,
                            Transform::Normal,
                            Some(t.alpha),
                            None,
                            Some(size),
                            None,
                            Kind::Unspecified,
                        );
                        vec![GhostElement::Snapshot(wrap(element, phys))]
                    }
                    (GhostSource::Live(window), None) => {
                        let live: Vec<WindowRenderElement<R>> =
                            window.frame_render_elements(renderer, render_origin, scale, t.alpha, true);
                        wrap_all(live, phys).into_iter().map(GhostElement::Live).collect()
                    }
                };
                // A live ghost (minimize) casts the shadow its window had; a
                // snapshot only if it was taken with one (close).
                let style = match &ghost.source {
                    GhostSource::Snapshot(snap) => snap.shadow,
                    GhostSource::Evaporate(ev) => ev.snapshot().shadow,
                    GhostSource::Live(window) => window.shadow_style(now),
                };
                let shadow = style.zip(programs.as_ref()).and_then(|(style, programs)| {
                    // A warped silhouette has no frame for the blur to follow:
                    // the shadow stays where the window was and fades early.
                    let pose = match (progress, evaporate_p) {
                        (Some(p), _) => WindowTransform {
                            alpha: t.alpha * genie::shadow_keep(p) as f32,
                            ..WindowTransform::IDENTITY
                        },
                        // Evaporate's shadow follows the swell and is gone by 30%.
                        (None, Some(p)) => WindowTransform {
                            alpha: t.alpha * evaporate::shadow_keep(p) as f32,
                            ..t
                        },
                        (None, None) => t,
                    };
                    let frame = depth::pose_frame(ghost.frame, Some(&pose), output_origin, output_scale);
                    let px = output_scale * (pose.scale.x + pose.scale.y) / 2.0;
                    let params = depth::shadow_params(frame, &style, px, pose.alpha)?;
                    Some(ShadowElement::new(
                        programs.shadow.clone(),
                        Id::new(),
                        CommitCounter::default(),
                        params,
                    ))
                });
                Some(DrawnGhost { anchor: ghost.anchor.clone(), elements, shadow })
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stack-order rule of `add`, on (owner, anchor) pairs: back to front.
    fn add_to(list: &mut Vec<(u32, Option<u32>)>, owner: u32, anchor: Option<u32>) {
        let mut first = None;
        for (i, other) in list.iter_mut().enumerate() {
            if reanchor(&mut other.1, &owner, &anchor) {
                first.get_or_insert(i);
            }
        }
        list.insert(first.unwrap_or(list.len()), (owner, anchor));
    }

    /// Emission order for a stack `desk(0) < c(3) < b(2) < a(1)` of windows
    /// that are in the Space, the rest ghosts: what `stack_elements` does, with
    /// ghosts taken per anchor in list order.
    fn z_order(list: &[(u32, Option<u32>)], space: &[u32]) -> Vec<u32> {
        let mut out = Vec::new();
        for &w in space {
            out.push(w);
            out.extend(list.iter().filter(|(_, a)| *a == Some(w)).map(|(o, _)| *o));
        }
        out
    }

    #[test]
    fn two_separate_minimizes_keep_the_windows_stack_order() {
        // Stack, back to front: desk, C, B, A. Minimize B (anchor C), then C
        // (anchor desk). B was above C, so its ghost must stay above C's.
        let mut list = Vec::new();
        add_to(&mut list, 2, Some(3)); // B leaves
        add_to(&mut list, 3, Some(0)); // C leaves
        assert_eq!(list, vec![(3, Some(0)), (2, Some(0))], "C's ghost under B's");
        // desk, then its ghosts back to front, then A on top.
        assert_eq!(z_order(&list, &[0, 1]), vec![0, 3, 2, 1]);
    }

    #[test]
    fn the_other_order_needs_no_fixing() {
        // C first, then B: B's anchor C is already gone, so it anchors to desk.
        let mut list = Vec::new();
        add_to(&mut list, 3, Some(0));
        add_to(&mut list, 2, Some(0));
        assert_eq!(z_order(&list, &[0, 1]), vec![0, 3, 2, 1]);
    }

    #[test]
    fn a_chain_of_three_is_kept_through_the_middle_one_leaving_last() {
        // Stack desk, D, C, B, A. Minimize A (anchor B), B (anchor C), C
        // (anchor D) in turn: each ghost sat above the next.
        let mut list = Vec::new();
        add_to(&mut list, 1, Some(2)); // A
        add_to(&mut list, 2, Some(3)); // B
        add_to(&mut list, 3, Some(4)); // C
        assert_eq!(z_order(&list, &[0, 4]), vec![0, 4, 3, 2, 1]);
    }

    #[test]
    fn show_desktop_back_to_front_keeps_the_order() {
        // desk, C, B, A all leave back to front, each anchored to the desk
        // (what is left in the Space below it).
        let mut list = Vec::new();
        for w in [3, 2, 1] {
            add_to(&mut list, w, Some(0));
        }
        assert_eq!(z_order(&list, &[0]), vec![0, 3, 2, 1]);
    }

    #[test]
    fn a_ghost_with_nothing_anchored_to_it_goes_on_top() {
        let mut list = vec![(7, Some(0))];
        add_to(&mut list, 8, Some(0));
        assert_eq!(list.last(), Some(&(8, Some(0))));
    }
}
