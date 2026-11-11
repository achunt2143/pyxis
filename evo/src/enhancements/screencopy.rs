//! wlr-screencopy-unstable-v1: copy an output's pixels into a client's
//! wl_shm buffer.
//!
//! This is the protocol `grim` speaks, and it is how the serial harness sees
//! the screen. QMP `screendump` cannot help there, because a GL scanout
//! gives it no surface to read.
//!
//! This module handles the protocol, and the backend supplies the pixels. A
//! `copy` request only queues a [`PendingCapture`]. The udev backend fulfils
//! it during its next repaint of that output: it draws that repaint's element
//! list a second time, into an offscreen texture, and reads the texture back.
//!
//! Drawing the element list again is deliberate. The obvious alternative is
//! to read back the frame the DRM compositor just built, but that frame is not
//! a picture of the screen. The compositor is free to put elements on
//! hardware planes (the cursor, or a fullscreen client), and those never pass
//! through its primary buffer.
//!
//! Until 2026-09-25 this file was a stub, and nothing had ever compiled it.
//! It was gated on a `screencopy` feature that `Cargo.toml` did not declare,
//! and the stub filled the client's buffer with zeros.
//!
//! Only wl_shm buffers are offered. There is no linux-dmabuf path, which is
//! all grim needs. Backends that cannot capture (winit, x11) fail the frame at
//! `copy` time rather than leave the client waiting for a `ready` that never
//! comes.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use smithay::{
    backend::{
        renderer::{element::RenderElement, Color32F, ExportMem, Offscreen},
    },
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            backend::GlobalId,
            protocol::{wl_buffer::WlBuffer, wl_shm},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Buffer as BufferCoords, Logical, Physical, Rectangle, Scale, Size, Transform},
    wayland::shm,
};
use tracing::{debug, warn};

use crate::anim::snapshot::{render_to_mem, OffscreenError};

const VERSION: u32 = 3;

/// The one wl_shm format offered. Every compositor must support it for
/// wl_shm, so a client can always allocate it.
const SHM_FORMAT: wl_shm::Format = wl_shm::Format::Xrgb8888;

// The readback format is `anim::snapshot::READBACK_FORMAT`: GL_RGBA /
// GL_UNSIGNED_BYTE, the one combination every GLES implementation can read
// back, virgl included. It is converted to `SHM_FORMAT` on the CPU (see
// `swizzle_rows`).

/// Global data of the manager: who may see it.
pub struct ScreencopyGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

pub struct ScreencopyManagerState {
    global: GlobalId,
    pending: Vec<PendingCapture>,
}

impl ScreencopyManagerState {
    /// `filter` says which clients may see the global at all: a capture is
    /// the whole screen, whatever any app has on it.
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyGlobalData>
            + Dispatch<ZwlrScreencopyManagerV1, ()>
            + Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>
            + 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global = display.create_global::<D, ZwlrScreencopyManagerV1, _>(
            VERSION,
            ScreencopyGlobalData {
                filter: Box::new(filter),
            },
        );
        Self {
            global,
            pending: Vec::new(),
        }
    }

    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }

    /// Take the captures queued for `output`, dropping any whose client has
    /// gone. The backend fulfils them, and hands back with [`Self::requeue`]
    /// any that are still waiting for damage.
    pub fn take_pending(&mut self, output: &Output) -> Vec<PendingCapture> {
        let (mine, rest) = std::mem::take(&mut self.pending)
            .into_iter()
            .filter(|capture| capture.frame.is_alive())
            .partition(|capture| &capture.output == output);
        self.pending = rest;
        mine
    }

    pub fn requeue(&mut self, captures: impl IntoIterator<Item = PendingCapture>) {
        self.pending.extend(captures);
    }
}

pub trait ScreencopyHandler {
    fn screencopy_state(&mut self) -> &mut ScreencopyManagerState;

    /// Whether this backend ever calls [`ScreencopyManagerState::take_pending`].
    /// A capture queued on a backend that doesn't would never complete.
    fn screencopy_supported(&self) -> bool;
}

#[derive(Debug, thiserror::Error)]
pub enum ScreencopyError {
    #[error("output has no mode")]
    NoMode,
    #[error("buffer does not match the advertised wl_shm parameters")]
    InvalidBuffer,
    #[error("could not access the client's buffer")]
    BufferAccess,
    #[error("readback returned {got} bytes, expected {want}")]
    ShortReadback { got: usize, want: usize },
    #[error("rendering failed: {0}")]
    Render(String),
}

fn render_err(err: impl std::error::Error) -> ScreencopyError {
    ScreencopyError::Render(err.to_string())
}

/// User data of a `zwlr_screencopy_frame_v1`.
#[derive(Debug)]
pub struct ScreencopyFrameState {
    /// The output and the part of it to copy, in output buffer pixels.
    /// `None` means the frame was failed as soon as it was created. That
    /// happens when its wl_output is gone, the output has no mode, or the
    /// region misses the output entirely.
    target: Option<(Output, Rectangle<i32, BufferCoords>)>,
    overlay_cursor: bool,
    used: AtomicBool,
}

/// A `copy` waiting for the backend's next repaint of its output.
///
/// Dropping one without fulfilling it sends `failed`. That covers every exit
/// path at once, a repaint that errors out early included, so a client is
/// never left waiting on a frame nobody will complete.
#[derive(Debug)]
pub struct PendingCapture {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    output: Output,
    region: Rectangle<i32, BufferCoords>,
    overlay_cursor: bool,
    with_damage: bool,
    done: bool,
}

impl PendingCapture {
    /// Whether the client asked for the cursor to be drawn into the frame.
    pub fn overlay_cursor(&self) -> bool {
        self.overlay_cursor
    }

    /// `copy_with_damage`: the copy waits for a repaint that has damage.
    pub fn with_damage(&self) -> bool {
        self.with_damage
    }

    /// Draw `elements` offscreen, read the captured region back, and write
    /// it into the client's buffer. The client then gets `ready`, or
    /// `failed` if any step errors.
    ///
    /// `elements` are front to back, the order a damage tracker takes them
    /// in. `clear_color` should be the one the repaint used, so that the
    /// capture shows what the screen shows. `T` is the offscreen target the
    /// renderer draws into, e.g. `GlesTexture`.
    pub fn fulfil<R, T, E>(mut self, renderer: &mut R, elements: &[E], clear_color: Color32F, time: Duration)
    where
        R: Offscreen<T> + ExportMem,
        E: RenderElement<R>,
    {
        match self.copy::<R, T, E>(renderer, elements, clear_color) {
            Ok(()) => {
                self.frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
                if self.with_damage {
                    // The whole region, every time. Damage is only a hint,
                    // and exact damage would need this capture's own tracker.
                    self.frame
                        .damage(0, 0, self.region.size.w as u32, self.region.size.h as u32);
                }
                let secs = time.as_secs();
                self.frame
                    .ready((secs >> 32) as u32, secs as u32, time.subsec_nanos());
                debug!(region = ?self.region, "screencopy: frame ready");
            }
            Err(err) => {
                warn!(%err, "screencopy: capture failed");
                self.frame.failed();
            }
        }
        self.done = true;
    }

    fn copy<R, T, E>(&self, renderer: &mut R, elements: &[E], clear_color: Color32F) -> Result<(), ScreencopyError>
    where
        R: Offscreen<T> + ExportMem,
        E: RenderElement<R>,
    {
        let mode = self.output.current_mode().ok_or(ScreencopyError::NoMode)?;
        let scale = Scale::from(self.output.current_scale().fractional_scale());
        // As in smithay's damage tracker: the output transform is given in
        // surface rotation, so its inverse is how the output itself is
        // drawn (`render_to_mem` inverts it). The result is the buffer in
        // scanout orientation, which is what the protocol hands out. Clients
        // apply wl_output.transform.
        let transform = self.output.current_transform();
        // Drawing, readback and the format are the offscreen path every
        // snapshot shares; only the client's buffer is this file's.
        render_to_mem::<R, T, E, _>(
            renderer,
            elements,
            scale,
            mode.size,
            transform,
            clear_color,
            self.region,
            |pixels| {
                write_shm(&self.buffer, pixels, self.region.size)
                    .map_err(|e| OffscreenError(e.to_string()))
            },
        )
        .map_err(|e| ScreencopyError::Render(e.0))
    }
}

impl Drop for PendingCapture {
    fn drop(&mut self) {
        if !self.done && self.frame.is_alive() {
            self.frame.failed();
        }
    }
}

/// The part of the output a frame copies, in output buffer pixels. That is
/// the whole mode for `capture_output`, or, for `capture_output_region`, the
/// logical `(x, y, width, height)` scaled and clipped to it.
///
/// Returns `None` when there is nothing to copy: an empty or negative
/// region, one that misses the output, or one on a rotated output. Mapping
/// a logical rectangle into a rotated buffer is not implemented, because
/// Evo's outputs are never rotated today.
///
/// The region arrives as raw ints, not a `Rectangle`, because it comes
/// straight from the client. smithay debug-asserts that a `Size` is
/// non-negative, so building a rectangle first would let any client crash a
/// debug build of the compositor.
fn clip_region(
    mode: Size<i32, Physical>,
    scale: f64,
    transform: Transform,
    region: Option<(i32, i32, i32, i32)>,
) -> Option<Rectangle<i32, BufferCoords>> {
    let full = Rectangle::from_size(Size::<i32, BufferCoords>::from((mode.w, mode.h)));
    let Some((x, y, width, height)) = region else {
        return Some(full);
    };
    if width <= 0 || height <= 0 || transform != Transform::Normal {
        return None;
    }
    let logical = Rectangle::<i32, Logical>::new((x, y).into(), (width, height).into());
    let physical: Rectangle<i32, Physical> = logical.to_physical_precise_round(scale);
    Rectangle::<i32, BufferCoords>::new(
        (physical.loc.x, physical.loc.y).into(),
        (physical.size.w, physical.size.h).into(),
    )
    .intersection(full)
    // `intersection` alone is not enough. A region that rounds to zero
    // pixels at this scale still "overlaps" the output.
    .filter(|region| region.size.w > 0 && region.size.h > 0)
}

/// True when `buffer` is a wl_shm buffer this frame can be copied into.
fn shm_buffer_matches(buffer: &WlBuffer, size: Size<i32, BufferCoords>) -> bool {
    shm::with_buffer_contents(buffer, |_, pool_len, data| {
        matches!(data.format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888)
            && data.width == size.w
            && data.height == size.h
            && data.stride >= size.w * 4
            && (data.offset as usize) + (data.stride as usize) * (data.height as usize) <= pool_len
    })
    .unwrap_or(false)
}

/// Copy readback pixels into the client's buffer, honouring its offset
/// within the pool and a stride wider than the image.
fn write_shm(buffer: &WlBuffer, pixels: &[u8], size: Size<i32, BufferCoords>) -> Result<(), ScreencopyError> {
    let (width, height) = (size.w as usize, size.h as usize);
    let want = width * height * 4;
    if pixels.len() < want {
        return Err(ScreencopyError::ShortReadback {
            got: pixels.len(),
            want,
        });
    }
    shm::with_buffer_contents_mut(buffer, |ptr, pool_len, data| {
        let (offset, stride) = (data.offset as usize, data.stride as usize);
        if data.width as usize != width
            || data.height as usize != height
            || stride < width * 4
            || offset + stride * height > pool_len
        {
            return Err(ScreencopyError::InvalidBuffer);
        }
        // SAFETY: `ptr` is the start of a pool mapping of `pool_len` bytes,
        // and the span written, `offset..offset + stride * height`, was
        // checked to lie inside it just above.
        let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset), stride * height) };
        swizzle_rows(pixels, dst, width, stride);
        Ok(())
    })
    .map_err(|_| ScreencopyError::BufferAccess)?
}

/// Convert tightly packed `Abgr8888` rows (bytes R,G,B,A) into `Xrgb8888`
/// / `Argb8888` rows (bytes B,G,R,A on little-endian), `stride` bytes apart.
fn swizzle_rows(src: &[u8], dst: &mut [u8], width: usize, stride: usize) {
    for (src_row, dst_row) in src.chunks_exact(width * 4).zip(dst.chunks_mut(stride)) {
        for (s, d) in src_row
            .chunks_exact(4)
            .zip(dst_row[..width * 4].chunks_exact_mut(4))
        {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
    }
}

// === PROTOCOL IMPLEMENTATIONS ===

impl<D> GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyGlobalData, D> for ScreencopyManagerState
where
    D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyGlobalData>
        + Dispatch<ZwlrScreencopyManagerV1, ()>
        + Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>
        + ScreencopyHandler
        + 'static,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &ScreencopyGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ScreencopyGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrScreencopyManagerV1, (), D> for ScreencopyManagerState
where
    D: Dispatch<ZwlrScreencopyManagerV1, ()>
        + Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>
        + ScreencopyHandler
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        let (frame, overlay_cursor, wl_output, logical) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, overlay_cursor, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (frame, overlay_cursor, output, Some((x, y, width, height))),
            _ => return,
        };

        let target = Output::from_resource(&wl_output).and_then(|output| {
            let mode = output.current_mode()?;
            let region = clip_region(
                mode.size,
                output.current_scale().fractional_scale(),
                output.current_transform(),
                logical,
            )?;
            Some((output, region))
        });

        // Initialise the frame whatever happened above: a new_id that is
        // never initialised panics inside wayland-server.
        let frame = data_init.init(
            frame,
            ScreencopyFrameState {
                target: target.clone(),
                overlay_cursor: overlay_cursor != 0,
                used: AtomicBool::new(false),
            },
        );

        match target {
            Some((_, region)) => {
                frame.buffer(
                    SHM_FORMAT,
                    region.size.w as u32,
                    region.size.h as u32,
                    (region.size.w * 4) as u32,
                );
                if frame.version() >= 3 {
                    frame.buffer_done();
                }
            }
            None => {
                debug!(?logical, "screencopy: nothing to capture, failing the frame");
                frame.failed();
            }
        }
    }
}

impl<D> Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState, D> for ScreencopyManagerState
where
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState> + ScreencopyHandler + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &ScreencopyFrameState,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return,
        };

        if data.used.swap(true, Ordering::Relaxed) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "frame already copied",
            );
            return;
        }
        // Failed at creation. The client was told, so there is nothing to do.
        let Some((output, region)) = data.target.clone() else {
            return;
        };
        if !shm_buffer_matches(&buffer, region.size) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                "buffer does not match the advertised wl_shm format, size or stride",
            );
            return;
        }
        if !state.screencopy_supported() {
            frame.failed();
            return;
        }

        state.screencopy_state().pending.push(PendingCapture {
            frame: frame.clone(),
            buffer,
            output,
            region,
            overlay_cursor: data.overlay_cursor,
            with_damage,
            done: false,
        });
    }
}

#[macro_export]
macro_rules! delegate_screencopy {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: $crate::enhancements::screencopy::ScreencopyGlobalData
        ] => $crate::enhancements::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: ()
        ] => $crate::enhancements::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1: $crate::enhancements::screencopy::ScreencopyFrameState
        ] => $crate::enhancements::screencopy::ScreencopyManagerState);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, BufferCoords> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn logical(x: i32, y: i32, w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
        Some((x, y, w, h))
    }

    const MODE: (i32, i32) = (1280, 800);

    #[test]
    fn whole_output_is_the_mode() {
        assert_eq!(
            clip_region(MODE.into(), 1.0, Transform::Normal, None),
            Some(rect(0, 0, 1280, 800))
        );
    }

    #[test]
    fn region_inside_the_output_is_kept() {
        assert_eq!(
            clip_region(MODE.into(), 1.0, Transform::Normal, logical(10, 20, 300, 200)),
            Some(rect(10, 20, 300, 200))
        );
    }

    #[test]
    fn region_is_clipped_to_the_output() {
        assert_eq!(
            clip_region(MODE.into(), 1.0, Transform::Normal, logical(1200, 700, 500, 500)),
            Some(rect(1200, 700, 80, 100))
        );
    }

    #[test]
    fn region_off_the_output_or_empty_is_nothing() {
        assert_eq!(
            clip_region(MODE.into(), 1.0, Transform::Normal, logical(1280, 0, 10, 10)),
            None
        );
        assert_eq!(
            clip_region(MODE.into(), 1.0, Transform::Normal, logical(5, 5, 0, 10)),
            None
        );
        // A client can send any int. A negative width must be refused, not
        // turned into a rectangle (which would trip smithay's debug assert).
        assert_eq!(
            clip_region(MODE.into(), 1.0, Transform::Normal, logical(10, 5, -5, 10)),
            None
        );
    }

    #[test]
    fn region_that_rounds_to_nothing_is_nothing() {
        // One logical pixel at 0.4x rounds to zero physical pixels.
        assert_eq!(
            clip_region((512, 320).into(), 0.4, Transform::Normal, logical(3, 3, 1, 1)),
            None
        );
    }

    #[test]
    fn region_is_scaled_to_buffer_pixels() {
        // A 2x output: a 2560x1600 mode holds a 1280x800 logical space.
        assert_eq!(
            clip_region((2560, 1600).into(), 2.0, Transform::Normal, logical(100, 50, 200, 100)),
            Some(rect(200, 100, 400, 200))
        );
    }

    #[test]
    fn region_on_a_rotated_output_is_refused_not_guessed() {
        assert_eq!(
            clip_region(MODE.into(), 1.0, Transform::_90, logical(0, 0, 10, 10)),
            None
        );
        // A whole-output capture needs no mapping, so it still works.
        assert!(clip_region(MODE.into(), 1.0, Transform::_90, None).is_some());
    }

    #[test]
    fn swizzle_turns_rgba_into_bgra_and_honours_stride() {
        // Two rows of two pixels: red, green / blue, translucent white.
        let src = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 128,
        ];
        // Stride 12: each destination row has 4 bytes of padding that must
        // be left alone.
        let mut dst = [0xAAu8; 24];
        swizzle_rows(&src, &mut dst, 2, 12);
        assert_eq!(
            dst,
            [
                0, 0, 255, 255, 0, 255, 0, 255, 0xAA, 0xAA, 0xAA, 0xAA, //
                255, 0, 0, 255, 255, 255, 255, 128, 0xAA, 0xAA, 0xAA, 0xAA,
            ]
        );
    }
}
