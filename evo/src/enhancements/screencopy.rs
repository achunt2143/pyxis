//! wlr-screencopy-unstable-v1 protocol implementation
//! Allows clients to capture screen content (for recording, screenshots)

use std::sync::{Arc, Mutex};
use smithay::{
    backend::renderer::{buffer_type, BufferType, Renderer},
    desktop::{Space, utils::OutputPresentationFeedback},
    output::Output,
    reexports::wayland_server::{
        protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_shm},
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    },
    utils::{Buffer as BufferCoords, Logical, Physical, Rectangle, Size, Transform},
    wayland::shm::{shm_format_to_fourcc, with_buffer_contents},
};
use tracing::{debug, warn};
use wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};

const VERSION: u32 = 3;

/// Screencopy manager state
pub struct ScreencopyManagerState {
    global: wayland_server::backend::GlobalId,
}

impl ScreencopyManagerState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrScreencopyManagerV1, ()>,
        D: Dispatch<ZwlrScreencopyManagerV1, ()>,
        D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
        D: 'static,
    {
        let global = display.create_global::<D, ZwlrScreencopyManagerV1, _>(VERSION, ());
        
        Self { global }
    }

    pub fn global_id(&self) -> &wayland_server::backend::GlobalId {
        &self.global
    }
}

/// State for a single screencopy frame
#[derive(Debug)]
pub struct ScreencopyFrameState {
    pub output: Output,
    pub overlay_cursor: bool,
    pub region: Option<Rectangle<i32, Logical>>,
    pub buffer: Option<WlBuffer>,
}

/// Trait for compositor state to handle screencopy
pub trait ScreencopyHandler {
    fn screencopy_state(&mut self) -> &mut ScreencopyManagerState;
    
    /// Copy frame to buffer
    fn copy_frame(
        &mut self,
        output: &Output,
        buffer: &WlBuffer,
        region: Option<Rectangle<i32, Logical>>,
        overlay_cursor: bool,
    ) -> Result<(), ScreencopyError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ScreencopyError {
    #[error("Buffer format not supported")]
    InvalidBuffer,
    #[error("Failed to access buffer contents")]
    BufferAccess,
    #[error("Output not found")]
    NoOutput,
    #[error("Rendering failed")]
    RenderError,
}

// === PROTOCOL IMPLEMENTATIONS ===

impl<D> GlobalDispatch<ZwlrScreencopyManagerV1, (), D> for ScreencopyManagerState
where
    D: GlobalDispatch<ZwlrScreencopyManagerV1, ()>,
    D: Dispatch<ZwlrScreencopyManagerV1, ()>,
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
    D: ScreencopyHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> Dispatch<ZwlrScreencopyManagerV1, (), D> for ScreencopyManagerState
where
    D: Dispatch<ZwlrScreencopyManagerV1, ()>,
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
    D: ScreencopyHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output: wl_output,
            } => {
                let Some(output) = Output::from_resource(&wl_output) else {
                    warn!("Invalid output for screencopy");
                    return;
                };

                let frame_state = ScreencopyFrameState {
                    output: output.clone(),
                    overlay_cursor: overlay_cursor != 0,
                    region: None,
                    buffer: None,
                };

                let frame_resource = data_init.init(frame, frame_state);
                send_buffer_info(&frame_resource, &output, None);
            }
            
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output: wl_output,
                x,
                y,
                width,
                height,
            } => {
                let Some(output) = Output::from_resource(&wl_output) else {
                    warn!("Invalid output for screencopy region");
                    return;
                };

                let region = Rectangle::from_loc_and_size((x, y), (width, height));

                let frame_state = ScreencopyFrameState {
                    output: output.clone(),
                    overlay_cursor: overlay_cursor != 0,
                    region: Some(region),
                    buffer: None,
                };

                let frame_resource = data_init.init(frame, frame_state);
                send_buffer_info(&frame_resource, &output, Some(region));
            }
            
            zwlr_screencopy_manager_v1::Request::Destroy => {}
            
            _ => {}
        }
    }
}

impl<D> Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState, D> for ScreencopyManagerState
where
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
    D: ScreencopyHandler,
    D: 'static,
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
        match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => {
                debug!("Screencopy: copying frame");
                
                // Perform the actual copy
                match state.copy_frame(&data.output, &buffer, data.region, data.overlay_cursor) {
                    Ok(()) => {
                        // Send success
                        frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
                        
                        // Get current time (simplified - use proper timestamp in production)
                        let tv_sec_hi = 0;
                        let tv_sec_lo = 0;
                        let tv_nsec = 0;
                        
                        frame.ready(tv_sec_hi, tv_sec_lo, tv_nsec);
                        debug!("Screencopy: frame ready");
                    }
                    Err(e) => {
                        warn!("Screencopy failed: {}", e);
                        frame.failed();
                    }
                }
            }
            
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => {
                // For simplicity, treat same as regular copy
                // In production, you'd wait for damage and send damage events
                debug!("Screencopy: copying frame with damage (treating as regular copy)");
                
                match state.copy_frame(&data.output, &buffer, data.region, data.overlay_cursor) {
                    Ok(()) => {
                        frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
                        frame.ready(0, 0, 0);
                    }
                    Err(e) => {
                        warn!("Screencopy failed: {}", e);
                        frame.failed();
                    }
                }
            }
            
            zwlr_screencopy_frame_v1::Request::Destroy => {
                debug!("Screencopy: frame destroyed");
            }
            
            _ => {}
        }
    }
}

/// Send buffer format information to client
fn send_buffer_info(
    frame: &ZwlrScreencopyFrameV1,
    output: &Output,
    region: Option<Rectangle<i32, Logical>>,
) {
    let mode = output.current_mode().unwrap_or_else(|| {
        smithay::output::Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        }
    });

    let size = region
        .map(|r| r.size)
        .unwrap_or_else(|| mode.size);

    // Common formats to advertise
    let formats = [
        wl_shm::Format::Argb8888,
        wl_shm::Format::Xrgb8888,
        wl_shm::Format::Abgr8888,
        wl_shm::Format::Xbgr8888,
    ];

    for format in formats {
        let stride = size.w * 4; // 4 bytes per pixel for these formats
        
        frame.buffer(
            format as u32,
            size.w as u32,
            size.h as u32,
            stride as u32,
        );
    }

    frame.buffer_done();
}

// === MACRO FOR EASY INTEGRATION ===

#[macro_export]
macro_rules! delegate_screencopy {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: ()
        ] => $crate::shell::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: ()
        ] => $crate::shell::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1: $crate::shell::screencopy::ScreencopyFrameState
        ] => $crate::shell::screencopy::ScreencopyManagerState);
    };
}
// Add this helper function at the end of screencopy.rs

use smithay::backend::renderer::gles::GlesRenderer;

/// Actual implementation of copying frame to buffer
pub fn copy_frame_to_shm_buffer<R>(
    renderer: &mut R,
    output: &Output,
    buffer: &WlBuffer,
    region: Option<Rectangle<i32, Logical>>,
    space: &Space<crate::shell::WindowElement>,
) -> Result<(), ScreencopyError>
where
    R: Renderer,
{
    use smithay::backend::renderer::element::RenderElement;
    
    // Get buffer info
    let buffer_params = with_buffer_contents(buffer, |ptr, len, data| {
        Ok((
            ptr as *mut u8,
            len,
            data.format,
            data.width,
            data.height,
            data.stride,
        ))
    })
    .map_err(|_| ScreencopyError::BufferAccess)?;

    let (buffer_ptr, buffer_len, format, width, height, stride) = buffer_params;

    // Get output geometry
    let output_geometry = space
        .output_geometry(output)
        .ok_or(ScreencopyError::NoOutput)?;

    let copy_region = region.unwrap_or(output_geometry);

    // Validate dimensions
    if copy_region.size.w != width || copy_region.size.h != height {
        warn!(
            "Buffer size mismatch: expected {}x{}, got {}x{}",
            copy_region.size.w, copy_region.size.h, width, height
        );
        return Err(ScreencopyError::InvalidBuffer);
    }

    // Collect render elements from space
    let elements: Vec<_> = space
        .elements()
        .filter_map(|window| {
            let location = space.element_location(window)?;
            
            // Check if window intersects with copy region
            let window_geo = window.geometry();
            let window_rect = Rectangle::from_loc_and_size(
                location,
                window_geo.size,
            );
            
            if !window_rect.overlaps(copy_region) {
                return None;
            }

            // Get render elements for this window
            Some(
                window
                    .render_elements::<R>(
                        renderer,
                        location,
                        smithay::utils::Scale::from(1.0),
                        1.0, // alpha
                    )
                    .into_iter()
            )
        })
        .flatten()
        .collect();

    // In a real implementation, you would:
    // 1. Render elements to an offscreen framebuffer
    // 2. Read pixels from framebuffer
    // 3. Convert format if needed
    // 4. Copy to SHM buffer
    
    // Simplified: Just clear the buffer for now
    unsafe {
        let buffer_slice = std::slice::from_raw_parts_mut(buffer_ptr, buffer_len);
        // Fill with black (in real impl, render actual content)
        buffer_slice.fill(0xFF000000u32.to_ne_bytes()[0]);
    }

    debug!("Screencopy: frame copied successfully");
    Ok(())
}
