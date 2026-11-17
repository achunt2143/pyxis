//! Window thumbnails for Bloom's overview and Alt+Tab switcher
//! (`org.geckoOS.evo GetWindowThumbnails`).
//!
//! Bloom's pages can't speak Wayland, so they can't use screencopy. Evo can
//! already draw any window to pixels (`Backend::snapshot_window`), so it does,
//! at the size a card needs.
//!
//! - **Small renders.** The window is drawn offscreen at
//!   `thumbnail width / frame width`, never at full size, so the readback is
//!   a few hundred KB (480x300 RGBA is 576 KB) instead of 4 MB.
//! - **Frame only.** The picture is cropped to the window's frame (SSD header
//!   included, CSD shadow margins excluded) so its aspect ratio is the
//!   frame's, which is what a card lays out around.
//! - **Two threads.** The compositor thread draws and reads back
//!   (`EvoState::serve_thumbnail_requests`); the Echo broker encodes JPEG on a
//!   blocking worker (`encode`), so a slow encode never costs a frame.
//! - **A cache**, per window, valid while the window's surface commit counter
//!   and frame size are unchanged, so an Alt+Tab walk over idle windows draws
//!   nothing. A window that commits every frame (a terminal, a video) would
//!   miss every time, so an entry younger than `GRACE` is served as is.
//! - **Minimized windows** keep their last thumbnail: one is taken as the
//!   window minimizes (`EvoState::thumbnail_at_minimize`), and a minimized
//!   window's cached entry is served however old it is.
//!
//! The pieces that need no renderer (size maths, crop, cache validity, base64,
//! the request parser) are plain functions with tests.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::Value;
use smithay::backend::renderer::utils::CommitCounter;

/// Bounds used when a caller gives none, and the ones Bloom asks for.
pub const DEFAULT_WIDTH: u32 = 480;
pub const DEFAULT_HEIGHT: u32 = 300;
/// A request is clamped to these; nothing larger is ever drawn.
pub const MAX_WIDTH: u32 = 480;
pub const MAX_HEIGHT: u32 = 480;
const MIN_SIDE: u32 = 16;
/// A cached entry younger than this is served though its window has committed
/// since.
pub const GRACE: Duration = Duration::from_millis(150);
const JPEG_QUALITY: u8 = 80;

// ---------------------------------------------------------------------------
// Size maths
// ---------------------------------------------------------------------------

/// The bounds a call asked for, clamped to what Evo will draw.
pub fn clamp_bounds(width: Option<f64>, height: Option<f64>) -> (u32, u32) {
    let clamp = |v: Option<f64>, default: u32, max: u32| match v {
        Some(v) if v.is_finite() => (v.round() as i64).clamp(MIN_SIDE as i64, max as i64) as u32,
        _ => default,
    };
    (clamp(width, DEFAULT_WIDTH, MAX_WIDTH), clamp(height, DEFAULT_HEIGHT, MAX_HEIGHT))
}

/// The largest size with `frame`'s aspect ratio that fits `bounds`. Never
/// larger than the frame: a small window is not blown up. At least 1x1.
pub fn fit(frame: (i32, i32), bounds: (u32, u32)) -> (u32, u32) {
    let (fw, fh) = (frame.0.max(1) as f64, frame.1.max(1) as f64);
    let s = (bounds.0 as f64 / fw).min(bounds.1 as f64 / fh).min(1.0);
    (((fw * s).round() as u32).max(1), ((fh * s).round() as u32).max(1))
}

/// The scale to draw a window of `frame` logical size at, so that it comes out
/// `fit(frame, bounds)` pixels wide. Not multiplied by the output scale: a
/// thumbnail is a size, not a density.
pub fn render_scale(frame: (i32, i32), bounds: (u32, u32)) -> f64 {
    fit(frame, bounds).0 as f64 / frame.0.max(1) as f64
}

/// The part of a snapshot that is the window's frame: `(x, y, w, h)` in
/// snapshot pixels, clamped to the snapshot. `frame_loc` is the frame's
/// top-left from the render origin (physical), `origin` is where the
/// snapshot's pixel (0, 0) sits from that same origin, `want` is the size the
/// frame should come out.
pub fn crop_rect(
    snapshot: (i32, i32),
    origin: (i32, i32),
    frame_loc: (i32, i32),
    want: (u32, u32),
) -> (u32, u32, u32, u32) {
    let x0 = (frame_loc.0 - origin.0).clamp(0, snapshot.0.max(1) - 1);
    let y0 = (frame_loc.1 - origin.1).clamp(0, snapshot.1.max(1) - 1);
    let w = (want.0 as i32).min(snapshot.0 - x0).max(1);
    let h = (want.1 as i32).min(snapshot.1 - y0).max(1);
    (x0 as u32, y0 as u32, w as u32, h as u32)
}

/// Copy `rect` out of `rgba` (`stride_w` pixels wide, 4 bytes each).
pub fn crop_rgba(rgba: &[u8], stride_w: u32, rect: (u32, u32, u32, u32)) -> Vec<u8> {
    let (x, y, w, h) = rect;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let start = ((row * stride_w + x) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (w * 4) as usize]);
    }
    out
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// RGBA (premultiplied, as a snapshot is) to a `data:image/jpeg` URL. The
/// alpha is dropped, so the rounded corners come out black over a card's
/// background, which clips them with its own radius. JPEG over WebP because
/// the `image` crate's WebP encoder is lossless only: five times the bytes,
/// and slower, for a picture drawn at a quarter of its size.
#[cfg(feature = "image")]
pub fn encode_jpeg_data_url(rgba: &[u8], width: u32, height: u32) -> Result<String, String> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok(format!("data:image/jpeg;base64,{}", base64(&jpeg)))
}

#[cfg(not(feature = "image"))]
pub fn encode_jpeg_data_url(_: &[u8], _: u32, _: u32) -> Result<String, String> {
    Err("evo was built without the image crate".into())
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// What a thumbnail was drawn from. Equal versions draw the same picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub commit: CommitCounter,
    /// The frame's logical size.
    pub frame: (i32, i32),
}

#[derive(Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    /// `data:image/jpeg;base64,...`
    pub data: String,
}

struct Entry {
    version: Version,
    bounds: (u32, u32),
    taken: Instant,
    thumb: Arc<Thumb>,
}

#[derive(Default)]
pub struct Cache {
    entries: HashMap<u32, Entry>,
}

impl Cache {
    /// The cached thumbnail if it still stands for the window: asked for at
    /// the same bounds, and either drawn from this version or no older than
    /// `GRACE`.
    pub fn fresh(&self, id: u32, version: Version, bounds: (u32, u32), now: Instant) -> Option<Arc<Thumb>> {
        let e = self.entries.get(&id)?;
        if e.bounds != bounds {
            return None;
        }
        (e.version == version || now.saturating_duration_since(e.taken) < GRACE).then(|| e.thumb.clone())
    }

    /// Whatever there is, however old: for a window that is minimized.
    pub fn last(&self, id: u32) -> Option<Arc<Thumb>> {
        self.entries.get(&id).map(|e| e.thumb.clone())
    }

    pub fn store(&mut self, id: u32, version: Version, bounds: (u32, u32), now: Instant, thumb: Arc<Thumb>) {
        self.entries.insert(id, Entry { version, bounds, taken: now, thumb });
    }

    /// Forget every window `alive` says is gone.
    pub fn retain(&mut self, alive: impl Fn(u32) -> bool) {
        self.entries.retain(|id, _| alive(*id));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

pub fn with_cache<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    let mut guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(Cache::default))
}

// ---------------------------------------------------------------------------
// Requests: Echo thread -> compositor thread
// ---------------------------------------------------------------------------

/// A drawn window that still needs encoding.
pub struct Raw {
    pub window_id: u32,
    pub version: Version,
    pub bounds: (u32, u32),
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub enum Slot {
    Ready(u32, Arc<Thumb>),
    Raw(Raw),
}

pub struct Request {
    /// `None`: every app window.
    pub ids: Option<Vec<u32>>,
    pub bounds: (u32, u32),
    pub reply: tokio::sync::oneshot::Sender<Vec<Slot>>,
}

static QUEUE: Mutex<Vec<Request>> = Mutex::new(Vec::new());

/// Queue a request for the compositor thread's next loop pass (at most 16 ms
/// away).
pub fn submit(ids: Option<Vec<u32>>, bounds: (u32, u32)) -> tokio::sync::oneshot::Receiver<Vec<Slot>> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    QUEUE.lock().unwrap_or_else(|p| p.into_inner()).push(Request { ids, bounds, reply });
    rx
}

pub fn take_requests() -> Vec<Request> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|p| p.into_inner()))
}

/// Parse a `GetWindowThumbnails` body: `window_ids` (optional, an array of
/// non-negative integers), `max_width`, `max_height`.
pub fn parse_request(body: &Value) -> Result<(Option<Vec<u32>>, (u32, u32)), String> {
    let ids = match body.get("window_ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| "window_ids must be non-negative integers".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(_) => return Err("window_ids must be an array".into()),
    };
    let num = |k: &str| -> Result<Option<f64>, String> {
        match body.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().map(Some).ok_or_else(|| format!("{k} must be a number")),
        }
    };
    Ok((ids, clamp_bounds(num("max_width")?, num("max_height")?)))
}

/// Encode a drawn window and file it in the cache. Runs on a worker, not the
/// compositor thread.
pub fn encode(raw: Raw) -> Result<(u32, Arc<Thumb>), String> {
    let data = encode_jpeg_data_url(&raw.rgba, raw.width, raw.height)?;
    let thumb = Arc::new(Thumb { width: raw.width, height: raw.height, data });
    with_cache(|c| c.store(raw.window_id, raw.version, raw.bounds, Instant::now(), thumb.clone()));
    Ok((raw.window_id, thumb))
}

// ---------------------------------------------------------------------------
// The compositor-thread half
// ---------------------------------------------------------------------------

use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    desktop::space::SpaceElement,
    utils::Scale,
};

use super::WindowElement;
use crate::state::{Backend, EvoState};

impl<BackendData: Backend> EvoState<BackendData> {
    /// What `window` is now, for the cache.
    fn thumbnail_version(window: &WindowElement) -> Version {
        let commit = window
            .wl_surface()
            .and_then(|s| with_renderer_surface_state(&s, |st| st.current_commit()))
            .unwrap_or_default();
        let size = SpaceElement::geometry(window).size;
        Version { commit, frame: (size.w, size.h) }
    }

    /// Draw `window` at thumbnail size and read it back, ready for `encode`.
    /// `None` when it has nothing to draw (no buffer yet) or the backend can't.
    fn draw_thumbnail(&mut self, id: u32, window: &WindowElement, bounds: (u32, u32)) -> Option<Raw> {
        use smithay::backend::renderer::utils::with_renderer_surface_state as surface_state;
        let has_buffer = window
            .wl_surface()
            .and_then(|s| surface_state(&s, |st| st.buffer().is_some()))
            .unwrap_or(false);
        if !has_buffer {
            return None;
        }
        let version = Self::thumbnail_version(window);
        if version.frame.0 <= 0 || version.frame.1 <= 0 {
            return None;
        }
        let want = fit(version.frame, bounds);
        let scale = render_scale(version.frame, bounds);
        let snapshot = self.backend_data.snapshot_window(window, scale)?;
        // Where the frame sits in what was drawn: the render origin is the
        // buffer's, and the frame starts at the inner window's geometry.
        let loc = SpaceElement::geometry(&window.0)
            .loc
            .to_physical_precise_round(Scale::from(scale));
        let rect = crop_rect(
            (snapshot.size.w, snapshot.size.h),
            (snapshot.origin.x, snapshot.origin.y),
            (loc.x, loc.y),
            want,
        );
        let rgba = crop_rgba(snapshot.pixels(), snapshot.size.w as u32, rect);
        Some(Raw { window_id: id, version, bounds, width: rect.2, height: rect.3, rgba })
    }

    /// The thumbnail of one window: the cache, or a fresh drawing.
    fn thumbnail_slot(&mut self, id: u32, window: &WindowElement, bounds: (u32, u32)) -> Option<Slot> {
        let now = Instant::now();
        let minimized = self.workspace_manager.is_minimized(window);
        let cached = with_cache(|c| {
            if minimized {
                c.last(id)
            } else {
                c.fresh(id, Self::thumbnail_version(window), bounds, now)
            }
        });
        if let Some(thumb) = cached {
            return Some(Slot::Ready(id, thumb));
        }
        self.draw_thumbnail(id, window, bounds).map(Slot::Raw)
    }

    /// Answer the queued `GetWindowThumbnails` calls. Called every loop pass.
    pub fn serve_thumbnail_requests(&mut self) {
        let requests = take_requests();
        if requests.is_empty() {
            return;
        }
        // A picture of an app window is a picture of the user's work: none
        // while the screen is locked, however the request got here (it may
        // have been queued just before the lock).
        if super::lock::input_locked() {
            for request in requests {
                let _ = request.reply.send(Vec::new());
            }
            return;
        }
        let alive: std::collections::HashSet<u32> = self.windows_by_id.keys().copied().collect();
        with_cache(|c| c.retain(|id| alive.contains(&id)));
        for request in requests {
            let mut ids: Vec<u32> = match &request.ids {
                Some(ids) => ids.clone(),
                None => {
                    let mut all: Vec<u32> = self.windows_by_id.keys().copied().collect();
                    all.sort_unstable();
                    all
                }
            };
            ids.dedup();
            let mut slots = Vec::new();
            for id in ids {
                let Some(window) = self.windows_by_id.get(&id).cloned() else {
                    continue;
                };
                // The shell and its overlay are not cards.
                if window.is_shell_window() || window.is_overlay() {
                    continue;
                }
                if let Some(slot) = self.thumbnail_slot(id, &window, request.bounds) {
                    slots.push(slot);
                }
            }
            let _ = request.reply.send(slots);
        }
    }

    /// Keep a picture of `window` for when it is minimized and not drawable
    /// from the Space: called as it minimizes, while it still has its last
    /// frame. Skipped when the cache already stands for this frame. The
    /// encoding happens on its own thread.
    pub fn thumbnail_at_minimize(&mut self, window: &WindowElement) {
        let Some(id) = self.window_id_for(window) else {
            return;
        };
        if window.is_shell_window() || window.is_overlay() {
            return;
        }
        let bounds = (DEFAULT_WIDTH, DEFAULT_HEIGHT);
        let stale = with_cache(|c| {
            c.fresh(id, Self::thumbnail_version(window), bounds, Instant::now()).is_none()
        });
        if !stale {
            return;
        }
        if let Some(raw) = self.draw_thumbnail(id, window, bounds) {
            std::thread::spawn(move || {
                if let Err(e) = encode(raw) {
                    tracing::warn!("thumbnail at minimize: {e}");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(commit: usize, w: i32, h: i32) -> Version {
        Version { commit: CommitCounter::from(commit), frame: (w, h) }
    }

    fn thumb(tag: &str) -> Arc<Thumb> {
        Arc::new(Thumb { width: 4, height: 3, data: tag.into() })
    }

    #[test]
    fn bounds_are_clamped_and_default() {
        assert_eq!(clamp_bounds(None, None), (DEFAULT_WIDTH, DEFAULT_HEIGHT));
        assert_eq!(clamp_bounds(Some(5000.0), Some(5000.0)), (MAX_WIDTH, MAX_HEIGHT));
        assert_eq!(clamp_bounds(Some(1.0), Some(-3.0)), (MIN_SIDE, MIN_SIDE));
        assert_eq!(clamp_bounds(Some(f64::NAN), Some(200.0)), (DEFAULT_WIDTH, 200));
    }

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        // 1280x832 (800 client + 32 header) into 480x300: height limits.
        let (w, h) = fit((1280, 832), (480, 300));
        assert_eq!(h, 300);
        assert!((w as f64 / h as f64 - 1280.0 / 832.0).abs() < 0.01, "{w}x{h}");
        // Portrait: height limits at the max.
        let (w, h) = fit((400, 1000), (480, 480));
        assert_eq!(h, 480);
        assert_eq!(w, 192);
        // A small window is not enlarged.
        assert_eq!(fit((200, 100), (480, 300)), (200, 100));
        // Degenerate frames still give a pixel.
        assert_eq!(fit((0, 0), (480, 300)), (1, 1));
    }

    #[test]
    fn render_scale_lands_on_the_fitted_width() {
        for frame in [(1280, 832), (333, 777), (4000, 300), (50, 50)] {
            let (w, _) = fit(frame, (480, 300));
            let s = render_scale(frame, (480, 300));
            assert!(((frame.0 as f64 * s).round() as u32).abs_diff(w) <= 1, "{frame:?}");
            assert!(s <= 1.0);
        }
    }

    #[test]
    fn crop_stays_inside_the_snapshot() {
        // A snapshot larger than the frame (a CSD shadow margin on each side).
        assert_eq!(crop_rect((100, 80), (-10, -10), (0, 0), (80, 60)), (10, 10, 80, 60));
        // No margin.
        assert_eq!(crop_rect((100, 80), (0, 0), (0, 0), (100, 80)), (0, 0, 100, 80));
        // A frame that wants more than is there is cut short, never out of range.
        assert_eq!(crop_rect((100, 80), (0, 0), (90, 70), (50, 50)), (90, 70, 10, 10));
        // A frame left of the snapshot clamps to its edge.
        assert_eq!(crop_rect((100, 80), (5, 5), (0, 0), (20, 20)), (0, 0, 20, 20));
    }

    #[test]
    fn crop_rgba_copies_the_right_pixels() {
        // 4x2 image, pixel value = column + 10*row in the red byte.
        let mut img = Vec::new();
        for y in 0..2u8 {
            for x in 0..4u8 {
                img.extend_from_slice(&[x + 10 * y, 0, 0, 255]);
            }
        }
        let out = crop_rgba(&img, 4, (1, 1, 2, 1));
        assert_eq!(out, vec![11, 0, 0, 255, 12, 0, 0, 255]);
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[cfg(feature = "image")]
    #[test]
    fn jpeg_data_url_decodes_to_the_same_size() {
        let rgba: Vec<u8> = (0..16 * 8).flat_map(|i| [(i % 255) as u8, 80, 200, 255]).collect();
        let url = encode_jpeg_data_url(&rgba, 16, 8).unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
        let b64 = url.trim_start_matches("data:image/jpeg;base64,");
        assert!(b64.len() % 4 == 0);
        // Decode it back: the bytes are a real JPEG of the right size.
        let mut bytes = Vec::new();
        for quad in b64.as_bytes().chunks(4) {
            let v: Vec<u32> = quad
                .iter()
                .map(|c| B64.iter().position(|b| b == c).map_or(0, |p| p as u32))
                .collect();
            let n = v[0] << 18 | v[1] << 12 | v[2] << 6 | v[3];
            bytes.push((n >> 16) as u8);
            if quad[2] != b'=' {
                bytes.push((n >> 8) as u8);
            }
            if quad[3] != b'=' {
                bytes.push(n as u8);
            }
        }
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (16, 8));
    }

    #[test]
    fn cache_invalidates_on_commit_or_size_but_not_inside_the_grace() {
        let t0 = Instant::now();
        let mut c = Cache::default();
        c.store(1, v(5, 100, 80), (480, 300), t0, thumb("a"));
        let later = t0 + GRACE * 2;
        // Same version: valid however old.
        assert_eq!(c.fresh(1, v(5, 100, 80), (480, 300), later).unwrap().data, "a");
        // A commit since, past the grace: stale.
        assert!(c.fresh(1, v(6, 100, 80), (480, 300), later).is_none());
        // A resize since: stale.
        assert!(c.fresh(1, v(5, 120, 80), (480, 300), later).is_none());
        // A commit since, inside the grace: served (a busy window must not
        // be redrawn on every call).
        assert_eq!(c.fresh(1, v(9, 100, 80), (480, 300), t0 + GRACE / 2).unwrap().data, "a");
        // Different bounds never hit.
        assert!(c.fresh(1, v(5, 100, 80), (240, 150), t0).is_none());
        // Unknown window.
        assert!(c.fresh(2, v(5, 100, 80), (480, 300), t0).is_none());
    }

    #[test]
    fn minimized_windows_get_their_last_entry_whatever_its_age() {
        let t0 = Instant::now();
        let mut c = Cache::default();
        c.store(7, v(1, 10, 10), (480, 300), t0, thumb("last"));
        assert_eq!(c.last(7).unwrap().data, "last");
        assert!(c.last(8).is_none());
    }

    #[test]
    fn closed_windows_are_forgotten() {
        let t0 = Instant::now();
        let mut c = Cache::default();
        c.store(1, v(1, 1, 1), (480, 300), t0, thumb("a"));
        c.store(2, v(1, 1, 1), (480, 300), t0, thumb("b"));
        c.retain(|id| id == 2);
        assert_eq!(c.len(), 1);
        assert!(c.last(1).is_none());
        assert!(c.last(2).is_some());
    }

    #[test]
    fn requests_parse() {
        let b = serde_json::json!({ "window_ids": [3, 4], "max_width": 9999, "max_height": 120 });
        assert_eq!(parse_request(&b).unwrap(), (Some(vec![3, 4]), (MAX_WIDTH, 120)));
        assert_eq!(parse_request(&serde_json::json!({})).unwrap(), (None, (DEFAULT_WIDTH, DEFAULT_HEIGHT)));
        assert!(parse_request(&serde_json::json!({ "window_ids": "x" })).is_err());
        assert!(parse_request(&serde_json::json!({ "window_ids": [-1] })).is_err());
        assert!(parse_request(&serde_json::json!({ "max_width": "wide" })).is_err());
    }
}
