//! Offscreen rendering, and window snapshots built on it.
//!
//! `render_to_mem` is the offscreen path: draw a front-to-back element list
//! into a texture, read it back. Screencopy's `copy` and a window snapshot
//! both use it. It used to live in `enhancements/screencopy.rs`.
//!
//! A snapshot is kept as CPU pixels, not as a texture. Evo's udev renderer is
//! a `MultiRenderer`, whose texture type cannot wrap the `GlesTexture` an
//! offscreen draw produces; the pixels are uploaded with `import_memory` the
//! first time a ghost draws, into whatever texture type the drawing renderer
//! has. The price is one GPU readback per snapshot (about 4 MB at 1280x800,
//! 33 MB at 4K), paid once when a window closes, and the memory is freed when
//! the animation ends.

use std::{any::Any, cell::RefCell};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{Element, Id, RenderElement},
            gles::GlesTexture,
            Color32F, ExportMem, Frame, ImportAll, ImportMem, Offscreen, Renderer, Texture,
        },
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform},
};

use super::shader::GlesAccess;
use crate::shell::{WindowElement, WindowRenderElement};

/// Readback format. `GL_RGBA`/`GL_UNSIGNED_BYTE` is the one combination every
/// GLES implementation can read back, virgl included.
pub const READBACK_FORMAT: Fourcc = Fourcc::Abgr8888;

/// A snapshot is refused beyond this many pixels a side.
const MAX_SIDE: i32 = 8192;

#[derive(Debug)]
pub struct OffscreenError(pub String);

impl std::fmt::Display for OffscreenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OffscreenError {}

fn err(e: impl std::error::Error) -> OffscreenError {
    OffscreenError(e.to_string())
}

/// Draw `elements` (front to back, the order a damage tracker takes them in)
/// into an offscreen `T` of `size` pixels, read `region` back as
/// `READBACK_FORMAT` and hand the pixels to `consume`.
///
/// `transform` is the output's transform as given in surface rotation: as in
/// smithay's damage tracker, its inverse is how the target itself is drawn.
/// Nothing is damage-tracked: it draws every element, back to front, over the
/// whole target, since nothing of the target survives to the next call.
#[allow(clippy::too_many_arguments)]
pub fn render_to_mem<R, T, E, O>(
    renderer: &mut R,
    elements: &[E],
    scale: Scale<f64>,
    size: Size<i32, Physical>,
    transform: Transform,
    clear_color: Color32F,
    region: Rectangle<i32, BufferCoords>,
    consume: impl FnOnce(&[u8]) -> Result<O, OffscreenError>,
) -> Result<O, OffscreenError>
where
    R: Offscreen<T> + ExportMem,
    E: RenderElement<R>,
{
    let transform = transform.invert();
    let target_geo = Rectangle::from_size(transform.transform_size(size));
    let buffer_size = Size::<i32, BufferCoords>::from((size.w, size.h));

    let mut target = renderer.create_buffer(READBACK_FORMAT, buffer_size).map_err(err)?;
    let mut framebuffer = renderer.bind(&mut target).map_err(err)?;

    let sync = {
        let mut frame = renderer.render(&mut framebuffer, size, transform).map_err(err)?;
        frame.clear(clear_color, &[target_geo]).map_err(err)?;
        for element in elements.iter().rev() {
            let geometry = element.geometry(scale);
            let Some(mut damage) = geometry.intersection(target_geo) else {
                continue;
            };
            damage.loc -= geometry.loc;
            element
                .draw(&mut frame, element.src(), geometry, &[damage], &[])
                .map_err(err)?;
        }
        frame.finish().map_err(err)?
    };
    renderer.wait(&sync).map_err(err)?;

    let mapping = renderer
        .copy_framebuffer(&framebuffer, region, READBACK_FORMAT)
        .map_err(err)?;
    let pixels = renderer.map_texture(&mapping).map_err(err)?;
    consume(pixels)
}

/// A window drawn once, SSD included, as RGBA pixels (premultiplied, top row
/// first). `origin` is where pixel (0, 0) sits relative to the window's render
/// origin, in physical pixels: a window with a CSD shadow margin or a
/// subsurface off its edge has a snapshot larger than its frame.
pub struct Snapshot {
    pixels: Vec<u8>,
    /// Texture size in pixels.
    pub size: Size<i32, Physical>,
    pub origin: Point<i32, Physical>,
    /// The output scale it was drawn at.
    pub scale: f64,
    /// Stable across frames, so the damage tracker follows it.
    pub id: Id,
    /// The shadow the window cast when the picture was taken, if the ghost
    /// drawn from it should cast one. `None` by default: the geometry
    /// crossfade's snapshot sits over the live window, which casts its own.
    /// A closing window sets it (`begin_close_animation`).
    pub shadow: Option<super::depth::ShadowStyle>,
    /// The uploaded texture, type-erased, made on first draw.
    texture: RefCell<Option<Box<dyn Any>>>,
}

impl Snapshot {
    pub fn from_pixels(pixels: Vec<u8>, size: Size<i32, Physical>, origin: Point<i32, Physical>, scale: f64) -> Self {
        Self { pixels, size, origin, scale, id: Id::new(), shadow: None, texture: RefCell::new(None) }
    }

    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    /// The pixels, RGBA premultiplied, top row first, `size.w` wide.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The texture for `renderer`, uploading on first use. `None` if the
    /// upload fails (the ghost is then skipped, not fatal).
    pub fn texture<R>(&self, renderer: &mut R) -> Option<R::TextureId>
    where
        R: Renderer + ImportMem,
        R::TextureId: Clone + 'static,
    {
        if let Some(tex) = self
            .texture
            .borrow()
            .as_ref()
            .and_then(|t| t.downcast_ref::<R::TextureId>())
        {
            return Some(tex.clone());
        }
        let size = Size::<i32, BufferCoords>::from((self.size.w, self.size.h));
        let tex = renderer
            .import_memory(&self.pixels, READBACK_FORMAT, size, false)
            .map_err(|e| tracing::warn!("snapshot upload failed: {e}"))
            .ok()?;
        *self.texture.borrow_mut() = Some(Box::new(tex.clone()));
        Some(tex)
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot").field("size", &self.size).field("origin", &self.origin).finish()
    }
}

/// Draw `window` (SSD included, popups excluded) into a snapshot.
pub fn capture_window<R, T>(
    renderer: &mut R,
    window: &WindowElement,
    scale: Scale<f64>,
) -> Result<Snapshot, OffscreenError>
where
    R: Renderer + ImportAll + ImportMem + GlesAccess + Offscreen<T> + ExportMem,
    R::TextureId: Clone + Texture + Send + 'static,
    WindowRenderElement<R>: RenderElement<R>,
{
    // Measure first: render at the render origin and take the union of what
    // the elements cover, then draw again shifted so that union starts at 0.
    let probe: Vec<WindowRenderElement<R>> =
        window.frame_render_elements(renderer, (0, 0).into(), scale, 1.0, false);
    let bounds = probe
        .iter()
        .map(|e| e.geometry(scale))
        .reduce(|a, b| a.merge(b))
        .ok_or_else(|| OffscreenError("window draws nothing".into()))?;
    if bounds.size.w <= 0 || bounds.size.h <= 0 || bounds.size.w > MAX_SIDE || bounds.size.h > MAX_SIDE {
        return Err(OffscreenError(format!("window snapshot size {:?} out of range", bounds.size)));
    }
    drop(probe);

    let elements: Vec<WindowRenderElement<R>> =
        window.frame_render_elements(renderer, Point::from((-bounds.loc.x, -bounds.loc.y)), scale, 1.0, false);
    let region = Rectangle::from_size(Size::<i32, BufferCoords>::from((bounds.size.w, bounds.size.h)));
    let pixels = render_to_mem::<R, T, _, _>(
        renderer,
        &elements,
        scale,
        bounds.size,
        Transform::Normal,
        Color32F::TRANSPARENT,
        region,
        |pixels| Ok(pixels.to_vec()),
    )?;
    Ok(Snapshot::from_pixels(pixels, bounds.size, bounds.loc, scale.x))
}

/// The offscreen target the udev backend snapshots into.
pub type SnapshotTarget = GlesTexture;
