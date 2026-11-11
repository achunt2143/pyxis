//! `WindowTransform`: how a window is drawn this frame, and the wrapper that
//! applies it to the window's render elements (SSD header included).
//!
//! A transform is render-time only. Input, focus, `ListWindows` and the work
//! area all keep using the window's final geometry.

use smithay::{
    backend::renderer::element::{
        utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
        Element,
    },
    utils::{Logical, Physical, Point, Scale},
};

use super::curve::Vec2;

/// A window's pose for one frame, in output-logical coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowTransform {
    /// The fixed point of the scale.
    pub origin: Point<f64, Logical>,
    pub scale: Vec2,
    /// Added after scaling.
    pub offset: Vec2,
    pub alpha: f32,
}

impl WindowTransform {
    pub const IDENTITY: WindowTransform = WindowTransform {
        origin: Point::new(0.0, 0.0),
        scale: Vec2::splat(1.0),
        offset: Vec2::splat(0.0),
        alpha: 1.0,
    };

    pub fn is_identity(&self) -> bool {
        self.scale == Vec2::splat(1.0) && self.offset == Vec2::splat(0.0) && self.alpha >= 1.0
    }

    /// Whether the geometry (not just the alpha) changes.
    pub fn moves(&self) -> bool {
        self.scale != Vec2::splat(1.0) || self.offset != Vec2::splat(0.0)
    }

    /// In physical pixels relative to an output at `output_origin` (logical).
    pub fn to_physical(
        &self,
        output_origin: Point<i32, Logical>,
        output_scale: Scale<f64>,
    ) -> PhysicalTransform {
        let origin = (self.origin - output_origin.to_f64()).to_physical(output_scale);
        PhysicalTransform {
            origin: origin.to_i32_round(),
            scale: Scale::from((self.scale.x, self.scale.y)),
            offset: Point::<f64, Physical>::from((
                self.offset.x * output_scale.x,
                self.offset.y * output_scale.y,
            ))
            .to_i32_round(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalTransform {
    pub origin: Point<i32, Physical>,
    pub scale: Scale<f64>,
    pub offset: Point<i32, Physical>,
}

/// What `wrap` makes of an element.
pub type Transformed<E> = RelocateRenderElement<RescaleRenderElement<E>>;

/// Scale `element` about `t.origin`, then move it by `t.offset`. Alpha is not
/// handled here: an element's alpha is fixed when it is created, so the
/// caller passes `WindowTransform::alpha` to `render_elements`. Smithay's
/// damage tracker compares each element's alpha frame to frame, so an
/// alpha-only change is repainted without the wrapper reporting anything.
pub fn wrap<E: Element>(element: E, t: PhysicalTransform) -> Transformed<E> {
    RelocateRenderElement::from_element(
        RescaleRenderElement::from_element(element, t.origin, t.scale),
        t.offset,
        Relocate::Relative,
    )
}

pub fn wrap_all<E: Element>(
    elements: impl IntoIterator<Item = E>,
    t: PhysicalTransform,
) -> Vec<Transformed<E>> {
    elements.into_iter().map(|e| wrap(e, t)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_identity() {
        assert!(WindowTransform::IDENTITY.is_identity());
        assert!(!WindowTransform { alpha: 0.5, ..WindowTransform::IDENTITY }.is_identity());
        assert!(!WindowTransform { alpha: 0.5, ..WindowTransform::IDENTITY }.moves());
        assert!(WindowTransform { scale: Vec2::splat(0.9), ..WindowTransform::IDENTITY }.moves());
    }

    #[test]
    fn physical_conversion_is_relative_to_the_output_and_scaled() {
        let t = WindowTransform {
            origin: Point::from((1100.0, 300.0)),
            scale: Vec2::new(0.9, 0.8),
            offset: Vec2::new(0.0, 12.0),
            alpha: 0.5,
        };
        let p = t.to_physical(Point::from((100, 100)), Scale::from(2.0));
        assert_eq!(p.origin, Point::from((2000, 400)));
        assert_eq!(p.offset, Point::from((0, 24)));
        assert_eq!(p.scale, Scale::from((0.9, 0.8)));
    }

    /// The plan's open risk: does the damage tracker repaint an element whose
    /// only change is its alpha, or a wrapped one whose only change is its
    /// scale? It does, because it compares each element's alpha and geometry
    /// frame to frame, and the wrappers report the wrapped values. If a
    /// smithay bump ever changes that, this fails, and the wrapper must report
    /// its geometry as damage while animating.
    mod damage {
        use super::*;
        use smithay::{
            backend::renderer::{
                damage::OutputDamageTracker,
                element::{Id, Kind, RenderElement},
                test::{DummyFramebuffer, DummyRenderer},
                utils::{CommitCounter, DamageSet, OpaqueRegions},
            },
            utils::{Buffer, Rectangle, Size, Transform},
        };

        /// An element with an alpha and nothing else: no surface, no texture.
        #[derive(Clone)]
        struct Probe {
            id: Id,
            alpha: f32,
        }

        impl Element for Probe {
            fn id(&self) -> &Id {
                &self.id
            }
            fn current_commit(&self) -> CommitCounter {
                CommitCounter::default()
            }
            fn src(&self) -> Rectangle<f64, Buffer> {
                Rectangle::from_size((100.0, 100.0).into())
            }
            fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
                Rectangle::new((100, 100).into(), (200, 200).into())
            }
            fn damage_since(&self, _scale: Scale<f64>, _commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
                DamageSet::default()
            }
            fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
                OpaqueRegions::default()
            }
            fn alpha(&self) -> f32 {
                self.alpha
            }
            fn kind(&self) -> Kind {
                Kind::Unspecified
            }
        }

        impl RenderElement<DummyRenderer> for Probe {
            fn draw(
                &self,
                _frame: &mut smithay::backend::renderer::test::DummyFrame,
                _src: Rectangle<f64, Buffer>,
                _dst: Rectangle<i32, Physical>,
                _damage: &[Rectangle<i32, Physical>],
                _opaque: &[Rectangle<i32, Physical>],
            ) -> Result<(), <DummyRenderer as smithay::backend::renderer::RendererSuper>::Error> {
                Ok(())
            }
        }

        /// A tracker that knows which frame it is on: age 0 (an unknown
        /// buffer) damages everything, so frames after the first say their
        /// buffer is one frame old.
        struct Tracker {
            inner: OutputDamageTracker,
            frames: usize,
        }

        fn tracker() -> Tracker {
            Tracker {
                inner: OutputDamageTracker::new(Size::from((800, 600)), 1.0, Transform::Normal),
                frames: 0,
            }
        }

        /// Whether this frame has damage.
        fn damaged<E: RenderElement<DummyRenderer>>(t: &mut Tracker, element: E) -> bool {
            let age = t.frames.min(1);
            t.frames += 1;
            let mut renderer = DummyRenderer::default();
            let mut fb = DummyFramebuffer;
            t.inner
                .render_output(&mut renderer, &mut fb, age, &[element], [0.0, 0.0, 0.0, 1.0])
                .unwrap()
                .damage
                .is_some_and(|d| !d.is_empty())
        }

        #[test]
        fn alpha_changes_are_damage() {
            let id = Id::new();
            let mut t = tracker();
            assert!(damaged(&mut t, Probe { id: id.clone(), alpha: 1.0 }), "first frame");
            assert!(!damaged(&mut t, Probe { id: id.clone(), alpha: 1.0 }), "unchanged is clean");
            assert!(damaged(&mut t, Probe { id: id.clone(), alpha: 0.5 }), "alpha-only change");
            assert!(!damaged(&mut t, Probe { id, alpha: 0.5 }), "and then it is clean again");
        }

        #[test]
        fn wrapped_scale_and_offset_changes_are_damage() {
            let id = Id::new();
            let pose = |scale: f64, dy: i32| PhysicalTransform {
                origin: (200, 200).into(),
                scale: Scale::from(scale),
                offset: (0, dy).into(),
            };
            let mut t = tracker();
            assert!(damaged(&mut t, wrap(Probe { id: id.clone(), alpha: 1.0 }, pose(0.9, 12))));
            assert!(!damaged(&mut t, wrap(Probe { id: id.clone(), alpha: 1.0 }, pose(0.9, 12))));
            assert!(damaged(&mut t, wrap(Probe { id: id.clone(), alpha: 1.0 }, pose(0.95, 12))), "scale");
            assert!(damaged(&mut t, wrap(Probe { id: id.clone(), alpha: 1.0 }, pose(0.95, 6))), "offset");
        }

        #[test]
        fn wrapped_alpha_changes_are_damage() {
            let id = Id::new();
            let pose = PhysicalTransform { origin: (200, 200).into(), scale: Scale::from(0.9), offset: (0, 0).into() };
            let mut t = tracker();
            assert!(damaged(&mut t, wrap(Probe { id: id.clone(), alpha: 0.2 }, pose)));
            assert!(damaged(&mut t, wrap(Probe { id: id.clone(), alpha: 0.4 }, pose)), "alpha through the wrapper");
        }
    }
}
