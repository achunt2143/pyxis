//! Server-Side Decoration (SSD) rendering for GeckoOS
//! 
//! This module handles the actual rendering of window decorations using Smithay's
//! solid color buffers, integrated with the GeckoOS decoration system for theming
//! and sophisticated hit testing.

use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            AsRenderElements, Kind,
        },
        Renderer,
    },
    desktop::WindowSurface,
    input::Seat,
    utils::{Logical, Point, Serial},
    wayland::shell::xdg::XdgShellHandler,
};
use std::cell::{RefCell, RefMut};
use tracing::{debug, warn};

use crate::{state::Backend, EvoState};
use super::WindowElement;

// Import GeckoOS decorations when feature is enabled
#[cfg(feature = "decorations")]
use crate::enhancements::decorations::{
    ControlButton, Theme, WindowDecoration, ButtonState, WindowGeometry,
};

/// Window decoration state stored in window user data
pub struct SSDState {
    pub is_ssd: bool,
    pub header_bar: HeaderBar,
    
    // GeckoOS decoration metadata (themes, hit testing, etc.)
    #[cfg(feature = "decorations")]
    pub decoration: Option<WindowDecoration>,
}

/// Header bar rendering state
#[derive(Debug, Clone)]
pub struct HeaderBar {
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub width: u32,
    pub close_button_hover: bool,
    pub maximize_button_hover: bool,
    pub minimize_button_hover: bool,  // NEW: Added minimize button support
    
    // Rendering buffers
    pub background: SolidColorBuffer,
    pub close_button: SolidColorBuffer,
    pub maximize_button: SolidColorBuffer,
    pub minimize_button: SolidColorBuffer,  // NEW: Minimize button buffer
    
    // Theme support
    #[cfg(feature = "decorations")]
    pub theme: Theme,
}

// === FALLBACK COLORS (used when decorations feature is disabled) ===
#[cfg(not(feature = "decorations"))]
const BG_COLOR: [f32; 4] = [0.75f32, 0.9f32, 0.78f32, 1f32];
#[cfg(not(feature = "decorations"))]
const MAX_COLOR: [f32; 4] = [1f32, 0.965f32, 0.71f32, 1f32];
#[cfg(not(feature = "decorations"))]
const MIN_COLOR: [f32; 4] = [0.8f32, 0.8f32, 0.8f32, 1f32];
#[cfg(not(feature = "decorations"))]
const CLOSE_COLOR: [f32; 4] = [1f32, 0.66f32, 0.612f32, 1f32];
#[cfg(not(feature = "decorations"))]
const MAX_COLOR_HOVER: [f32; 4] = [0.71f32, 0.624f32, 0f32, 1f32];
#[cfg(not(feature = "decorations"))]
const MIN_COLOR_HOVER: [f32; 4] = [0.6f32, 0.6f32, 0.6f32, 1f32];
#[cfg(not(feature = "decorations"))]
const CLOSE_COLOR_HOVER: [f32; 4] = [0.75f32, 0.11f32, 0.016f32, 1f32];

// Layout constants
pub const HEADER_BAR_HEIGHT: i32 = 32;
const BUTTON_HEIGHT: u32 = HEADER_BAR_HEIGHT as u32;
const BUTTON_WIDTH: u32 = 32;

impl HeaderBar {
    /// Create a new header bar with default or themed colors
    pub fn new() -> Self {
        Self {
            pointer_loc: None,
            width: 0,
            close_button_hover: false,
            maximize_button_hover: false,
            minimize_button_hover: false,
            background: SolidColorBuffer::default(),
            close_button: SolidColorBuffer::default(),
            maximize_button: SolidColorBuffer::default(),
            minimize_button: SolidColorBuffer::default(),
            #[cfg(feature = "decorations")]
            theme: Theme::default(),
        }
    }
    
    /// Convert RGBA tuple to f32 array for Smithay
    #[cfg(feature = "decorations")]
    fn color_to_f32(rgba: (u8, u8, u8, u8)) -> [f32; 4] {
        [
            rgba.0 as f32 / 255.0,
            rgba.1 as f32 / 255.0,
            rgba.2 as f32 / 255.0,
            rgba.3 as f32 / 255.0,
        ]
    }
    
    /// Set theme and trigger redraw
    #[cfg(feature = "decorations")]
    pub fn set_theme(&mut self, theme: Theme) {
        debug!("Setting header bar theme");
        self.theme = theme;
        self.redraw(self.width);
    }
    
    /// Update pointer location
    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
    }
    
    /// Clear pointer location
    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
    }
    
    /// Handle click with GeckoOS decoration hit testing
    #[cfg(feature = "decorations")]
    pub fn clicked_with_decoration<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
        decoration: &WindowDecoration,
    ) {
        if let Some(loc) = self.pointer_loc.as_ref() {
            // Use GeckoOS decoration hit testing (more sophisticated)
            let simple_geom = WindowGeometry {
                x: 0,
                y: 0,
                width: self.width,
                height: HEADER_BAR_HEIGHT as u32,
            };
            
            if let Some(button) = decoration.get_button_at(
                loc.x as i32, 
                loc.y as i32, 
                &simple_geom
            ) {
                match button {
                    ControlButton::Close => {
                        debug!("Close button clicked (GeckoOS decoration)");
                        self.handle_close(window);
                    }
                    ControlButton::Maximize => {
                        debug!("Maximize button clicked (GeckoOS decoration)");
                        self.handle_maximize(state, window);
                    }
                    ControlButton::Minimize => {
                        warn!("Minimize not yet fully implemented");
                        // TODO: Implement minimize functionality
                    }
                }
                return;
            }
            
            // If not on a button, handle window move
            self.handle_move(seat, state, window, serial);
        }
    }
    
    /// Handle click with fallback simple hit testing
    pub fn clicked<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
    ) {
        match self.pointer_loc.as_ref() {
            // Close button (rightmost)
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {
                debug!("Close button clicked");
                self.handle_close(window);
            }
            
            // Maximize button (second from right)
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {
                debug!("Maximize button clicked");
                self.handle_maximize(state, window);
            }
            
            // Minimize button (third from right)
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 3)) as f64 => {
                warn!("Minimize not yet fully implemented");
                // TODO: Implement minimize functionality
            }
            
            // Title bar drag area
            Some(_) => {
                self.handle_move(seat, state, window, serial);
            }
            
            _ => {}
        }
    }
    
    /// Handle close button action
    fn handle_close(&self, window: &WindowElement) {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => w.send_close(),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let _ = w.close();
            }
        };
    }
    
    /// Handle maximize button action
    fn handle_maximize<B: Backend>(&self, state: &mut EvoState<B>, window: &WindowElement) {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => state.maximize_request(w.clone()),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let surface = w.clone();
                state
                    .handle
                    .insert_idle(move |data| data.maximize_request_x11(&surface));
            }
        };
    }
    
    /// Handle window move action
    fn handle_move<B: Backend>(
        &self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
    ) {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => {
                let seat = seat.clone();
                let toplevel = w.clone();
                state
                    .handle
                    .insert_idle(move |data| data.move_request_xdg(&toplevel, &seat, serial));
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let window = w.clone();
                state
                    .handle
                    .insert_idle(move |data| data.move_request_x11(&window));
            }
        };
    }
    
    /// Handle touch down event
    pub fn touch_down<B: Backend>(
        &mut self,
        seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        serial: Serial,
    ) {
        // For touch, we only handle move (not button clicks on touch down)
        match self.pointer_loc.as_ref() {
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {}
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {}
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 3)) as f64 => {}
            Some(_) => {
                self.handle_move(seat, state, window, serial);
            }
            _ => {}
        }
    }
    
    /// Handle touch up event (actual button activation)
    pub fn touch_up<B: Backend>(
        &mut self,
        _seat: &Seat<EvoState<B>>,
        state: &mut EvoState<B>,
        window: &WindowElement,
        _serial: Serial,
    ) {
        match self.pointer_loc.as_ref() {
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {
                self.handle_close(window);
            }
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {
                self.handle_maximize(state, window);
            }
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 3)) as f64 => {
                warn!("Minimize not yet fully implemented");
            }
            _ => {}
        }
    }
    
    /// Redraw header bar with theme support
    pub fn redraw(&mut self, width: u32) {
        if width == 0 {
            self.width = 0;
            return;
        }
        
        // Get colors from theme or use fallback
        #[cfg(feature = "decorations")]
        let (bg_color, close_color, close_hover, max_color, max_hover, min_color, min_hover) = {
            (
                Self::color_to_f32(self.theme.background),
                Self::color_to_f32(self.theme.button_bg),
                Self::color_to_f32(self.theme.button_hover),
                Self::color_to_f32(self.theme.button_bg),
                Self::color_to_f32(self.theme.button_hover),
                Self::color_to_f32(self.theme.button_bg),
                Self::color_to_f32(self.theme.button_hover),
            )
        };
        
        #[cfg(not(feature = "decorations"))]
        let (bg_color, close_color, close_hover, max_color, max_hover, min_color, min_hover) = {
            (BG_COLOR, CLOSE_COLOR, CLOSE_COLOR_HOVER, MAX_COLOR, MAX_COLOR_HOVER, MIN_COLOR, MIN_COLOR_HOVER)
        };
        
        // Update background
        self.background.update((width as i32, HEADER_BAR_HEIGHT), bg_color);
        
        let mut needs_redraw_buttons = false;
        if width != self.width {
            needs_redraw_buttons = true;
            self.width = width;
        }
        
        // === CLOSE BUTTON (rightmost) ===
        if self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || !self.close_button_hover)
        {
            self.close_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                close_hover,
            );
            self.close_button_hover = true;
        } else if !self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || self.close_button_hover)
        {
            self.close_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                close_color,
            );
            self.close_button_hover = false;
        }
        
        // === MAXIMIZE BUTTON (second from right) ===
        if self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH * 2) as f64 && l.x <= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || !self.maximize_button_hover)
        {
            self.maximize_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                max_hover,
            );
            self.maximize_button_hover = true;
        } else if !self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH * 2) as f64 && l.x <= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || self.maximize_button_hover)
        {
            self.maximize_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                max_color,
            );
            self.maximize_button_hover = false;
        }
        
        // === MINIMIZE BUTTON (third from right) === NEW!
        if self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH * 3) as f64 && l.x <= (width - BUTTON_WIDTH * 2) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || !self.minimize_button_hover)
        {
            self.minimize_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                min_hover,
            );
            self.minimize_button_hover = true;
        } else if !self.pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH * 3) as f64 && l.x <= (width - BUTTON_WIDTH * 2) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || self.minimize_button_hover)
        {
            self.minimize_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                min_color,
            );
            self.minimize_button_hover = false;
        }
    }
}

impl Default for HeaderBar {
    fn default() -> Self {
        Self::new()
    }
}

// === RENDERING ===
impl<R> AsRenderElements<R> for HeaderBar
where
    R: Renderer,
{
    type RenderElement = SolidColorRenderElement;
    
    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        _renderer: &mut R,
        location: Point<i32, smithay::utils::Physical>,
        scale: smithay::utils::Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let header_end_offset: Point<i32, Logical> = Point::from((self.width as i32, 0));
        let button_offset: Point<i32, Logical> = Point::from((BUTTON_WIDTH as i32, 0));
        
        vec![
            // Background (drawn first so buttons are on top)
            SolidColorRenderElement::from_buffer(
                &self.background,
                location,
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
            
            // Minimize button (third from right) - NEW!
            SolidColorRenderElement::from_buffer(
                &self.minimize_button,
                location + (header_end_offset - button_offset.upscale(3)).to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
            
            // Maximize button (second from right)
            SolidColorRenderElement::from_buffer(
                &self.maximize_button,
                location + (header_end_offset - button_offset.upscale(2)).to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
            
            // Close button (rightmost)
            SolidColorRenderElement::from_buffer(
                &self.close_button,
                location + (header_end_offset - button_offset).to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
        ]
    }
}

// === WINDOW ELEMENT DECORATION API ===
impl WindowElement {
    /// Get or create decoration state for this window
    pub fn decoration_state(&self) -> RefMut<'_, SSDState> {
        self.user_data().insert_if_missing(|| {
            RefCell::new(SSDState {
                is_ssd: false,
                header_bar: HeaderBar::new(),
                #[cfg(feature = "decorations")]
                decoration: None,
            })
        });
        
        self.user_data()
            .get::<RefCell<SSDState>>()
            .unwrap()
            .borrow_mut()
    }
    
    /// Enable or disable server-side decorations
    pub fn set_ssd(&self, ssd: bool) {
        self.decoration_state().is_ssd = ssd;
    }
    
    /// Set decoration theme (GeckoOS enhancement)
    #[cfg(feature = "decorations")]
    pub fn set_decoration_theme(&self, theme: Theme) {
        let mut state = self.decoration_state();
        state.header_bar.set_theme(theme.clone());
        
        if let Some(decoration) = state.decoration.as_mut() {
            decoration.set_theme(theme);
        }
    }
    
    /// Initialize GeckoOS decoration metadata
    #[cfg(feature = "decorations")]
    pub fn init_decoration(&self, window_id: u32, title: String, app_id: String) {
        let mut state = self.decoration_state();
        state.decoration = Some(WindowDecoration::new(window_id, title, app_id));
        debug!("Initialized GeckoOS decoration for window {}", window_id);
    }
}
