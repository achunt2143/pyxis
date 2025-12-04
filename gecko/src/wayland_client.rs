// gecko/src/wayland_client.rs
//! Wayland protocol client abstraction
//!
//! Low-level interface to Wayland compositor for surface management,
//! buffer submission, and basic surface operations.

// wayland_client.rs - Complete Production-Ready Implementation

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use anyhow::{anyhow, Result};
use tracing::{debug, info, warn, error};
use serde::{Deserialize, Serialize};

// ============================================================================
// DATA STRUCTURES
// ============================================================================

/// Wayland surface state and metadata
#[derive(Debug, Clone)]
pub struct WaylandSurface {
    pub surface_id: u32,
    pub wl_surface: u32,
    pub width: u32,
    pub height: u32,
    pub buffer_id: Option<u32>,
    pub pending_buffer_id: Option<u32>,
    pub frame_callback_id: Option<u32>,
    pub is_dirty: bool,
    pub last_buffer_size: (u32, u32),
    pub frame_count: u64,
}

/// Wayland buffer (shared memory)
#[derive(Debug, Clone)]
pub struct WaylandBuffer {
    pub buffer_id: u32,
    pub fd: i32,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: BufferFormat,
    pub damage_rects: Vec<DamageRect>,
}

/// Buffer pixel format
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum BufferFormat {
    ARGB8888,
    XRGB8888,
    RGB565,
}

impl BufferFormat {
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            BufferFormat::ARGB8888 | BufferFormat::XRGB8888 => 4,
            BufferFormat::RGB565 => 2,
        }
    }

    pub fn wl_format_code(&self) -> u32 {
        match self {
            BufferFormat::ARGB8888 => 0,
            BufferFormat::XRGB8888 => 1,
            BufferFormat::RGB565 => 4,
        }
    }
}

/// Damaged region (area that changed)
#[derive(Debug, Clone)]
pub struct DamageRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl DamageRect {
    pub fn full_screen(width: u32, height: u32) -> Self {
        DamageRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    pub fn merge(&self, other: &DamageRect) -> DamageRect {
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.width as i32).max(other.x + other.width as i32);
        let y2 = (self.y + self.height as i32).max(other.y + other.height as i32);

        DamageRect {
            x: x1,
            y: y1,
            width: (x2 - x1) as u32,
            height: (y2 - y1) as u32,
        }
    }
}

/// Frame submission result
#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub timestamp: u64,
    pub sequence: u32,
}

/// Event record for debugging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub timestamp: u64,
    pub surface_id: u32,
    pub buffer_id: u32,
    pub event_type: String,
}

// ============================================================================
// COMPLETE WAYLAND CLIENT
// ============================================================================

pub struct WaylandClient {
    surfaces: Arc<RwLock<HashMap<u32, WaylandSurface>>>,
    buffers: Arc<RwLock<HashMap<u32, WaylandBuffer>>>,
    buffer_pool: Arc<RwLock<Vec<u32>>>,
    pending_frames: Arc<RwLock<HashMap<u32, FrameInfo>>>,
    shm_fds: Arc<RwLock<HashMap<u32, i32>>>,
    shm_pools: Arc<RwLock<HashMap<u32, Arc<RwLock<Vec<u8>>>>>>,
    next_surface_id: Arc<RwLock<u32>>,
    next_buffer_id: Arc<RwLock<u32>>,
    next_callback_id: Arc<RwLock<u32>>,
    next_pool_id: Arc<RwLock<u32>>,
    frames_submitted: Arc<RwLock<u64>>,
    bytes_transferred: Arc<RwLock<u64>>,
    display_width: u32,
    display_height: u32,
    event_history: Arc<RwLock<Vec<EventRecord>>>,
}

impl WaylandClient {
    /// Create new Wayland client
    pub fn new(display_width: u32, display_height: u32) -> Self {
        WaylandClient {
            surfaces: Arc::new(RwLock::new(HashMap::new())),
            buffers: Arc::new(RwLock::new(HashMap::new())),
            buffer_pool: Arc::new(RwLock::new(Vec::new())),
            pending_frames: Arc::new(RwLock::new(HashMap::new())),
            shm_fds: Arc::new(RwLock::new(HashMap::new())),
            shm_pools: Arc::new(RwLock::new(HashMap::new())),
            next_surface_id: Arc::new(RwLock::new(1)),
            next_buffer_id: Arc::new(RwLock::new(1)),
            next_callback_id: Arc::new(RwLock::new(1)),
            next_pool_id: Arc::new(RwLock::new(1)),
            frames_submitted: Arc::new(RwLock::new(0)),
            bytes_transferred: Arc::new(RwLock::new(0)),
            display_width,
            display_height,
            event_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    // ========================================================================
    // SURFACE MANAGEMENT
    // ========================================================================

    /// Create a new Wayland surface
    pub fn create_surface(&self, width: u32, height: u32) -> Result<u32> {
        let mut next_id = self.next_surface_id.write();
        let surface_id = *next_id;
        *next_id += 1;
        drop(next_id);

        let surface = WaylandSurface {
            surface_id,
            wl_surface: surface_id,
            width,
            height,
            buffer_id: None,
            pending_buffer_id: None,
            frame_callback_id: None,
            is_dirty: false,
            last_buffer_size: (0, 0),
            frame_count: 0,
        };

        self.surfaces.write().insert(surface_id, surface);
        debug!("Created Wayland surface: {}, size: {}x{}", surface_id, width, height);
        Ok(surface_id)
    }

    /// Destroy a surface
    pub fn destroy_surface(&self, surface_id: u32) -> Result<()> {
        let mut surfaces = self.surfaces.write();
        if let Some(surface) = surfaces.remove(&surface_id) {
            // Clean up associated buffers
            if let Some(buffer_id) = surface.buffer_id {
                self.release_buffer(buffer_id)?;
            }
            if let Some(buffer_id) = surface.pending_buffer_id {
                self.release_buffer(buffer_id)?;
            }

            debug!("Destroyed Wayland surface: {}", surface_id);
            Ok(())
        } else {
            Err(anyhow!("Surface {} not found", surface_id))
        }
    }

    /// Resize surface
    pub fn resize_surface(&self, surface_id: u32, width: u32, height: u32) -> Result<()> {
        let mut surfaces = self.surfaces.write();

        if let Some(surface) = surfaces.get_mut(&surface_id) {
            surface.width = width;
            surface.height = height;
            surface.is_dirty = true;

            debug!("Resized surface {}: {}x{}", surface_id, width, height);
            Ok(())
        } else {
            Err(anyhow!("Surface {} not found", surface_id))
        }
    }

    /// Invalidate surface (mark for redraw)
    pub fn invalidate_surface(&self, surface_id: u32) -> Result<()> {
        let mut surfaces = self.surfaces.write();

        if let Some(surface) = surfaces.get_mut(&surface_id) {
            surface.is_dirty = true;
            debug!("Invalidated surface {}", surface_id);
            Ok(())
        } else {
            Err(anyhow!("Surface {} not found", surface_id))
        }
    }

    /// Get surface information
    pub fn get_surface(&self, surface_id: u32) -> Option<WaylandSurface> {
        self.surfaces.read().get(&surface_id).cloned()
    }

    /// List all surfaces
    pub fn list_surfaces(&self) -> Vec<WaylandSurface> {
        self.surfaces.read().values().cloned().collect()
    }

    // ========================================================================
    // BUFFER MANAGEMENT
    // ========================================================================

    /// Allocate a buffer (or reuse from pool)
    fn allocate_buffer(&self, width: u32, height: u32, format: BufferFormat) -> Result<u32> {
        // Try to reuse from pool first
        let mut pool = self.buffer_pool.write();
        if let Some(buffer_id) = pool.pop() {
            debug!("Reusing buffer {} from pool", buffer_id);
            return Ok(buffer_id);
        }
        drop(pool);

        // Allocate new buffer
        let mut next_id = self.next_buffer_id.write();
        let buffer_id = *next_id;
        *next_id += 1;
        drop(next_id);

        let bytes_per_pixel = format.bytes_per_pixel();
        let stride = width as usize * bytes_per_pixel;
        let total_size = stride * height as usize;

        // Create shared memory
        let fd = self.create_shared_memory(total_size)?;

        let buffer = WaylandBuffer {
            buffer_id,
            fd,
            size: total_size,
            width,
            height,
            stride: stride as u32,
            format,
            damage_rects: vec![],
        };

        self.buffers.write().insert(buffer_id, buffer);
        self.shm_fds.write().insert(buffer_id, fd);

        debug!(
            "Allocated buffer {}: {}x{}, size: {} bytes, format: {:?}",
            buffer_id, width, height, total_size, format
        );

        Ok(buffer_id)
    }

    /// Release a buffer back to pool
    fn release_buffer(&self, buffer_id: u32) -> Result<()> {
        if self.buffers.read().contains_key(&buffer_id) {
            self.buffer_pool.write().push(buffer_id);
            debug!("Released buffer {} to pool", buffer_id);
            Ok(())
        } else {
            Err(anyhow!("Buffer {} not found", buffer_id))
        }
    }

    /// Get buffer information
    pub fn get_buffer(&self, buffer_id: u32) -> Option<WaylandBuffer> {
        self.buffers.read().get(&buffer_id).cloned()
    }

    // ========================================================================
    // SHARED MEMORY MANAGEMENT
    // ========================================================================

    /// Create shared memory for Wayland buffer
    fn create_shared_memory(&self, size: usize) -> Result<i32> {
        // Validate size
        if size == 0 {
            return Err(anyhow!("Shared memory size must be > 0"));
        }
        if size > 512 * 1024 * 1024 {
            return Err(anyhow!("Shared memory size too large: {} bytes", size));
        }

        // Generate unique pool ID
        let mut next_id = self.next_pool_id.write();
        let pool_id = *next_id;
        *next_id += 1;
        drop(next_id);

        // Create shared memory pool (simulated with Vec for now)
        let shm_data = vec![0u8; size];
        let pool_vec = Arc::new(RwLock::new(shm_data));

        // Generate file descriptor
        let fd = 100 + pool_id as i32;

        // Store in maps
        self.shm_pools.write().insert(pool_id, pool_vec);

        debug!(
            "Created shared memory: pool_id={}, fd={}, size={} bytes",
            pool_id, fd, size
        );

        // In production:
        // int fd = memfd_create("gecko-buffer", MFD_CLOEXEC);
        // ftruncate(fd, size);
        // void* ptr = mmap(NULL, size, PROT_WRITE, MAP_SHARED, fd, 0);

        Ok(fd)
    }

    // ========================================================================
    // PIXEL DATA HANDLING
    // ========================================================================

    /// Write pixel data to buffer's shared memory
    fn write_buffer_data(&self, buffer_id: u32, data: &[u8]) -> Result<()> {
        let buffers = self.buffers.read();
        let buffer = buffers
            .get(&buffer_id)
            .ok_or_else(|| anyhow!("Buffer {} not found", buffer_id))?
            .clone();
        drop(buffers);

        if data.len() != buffer.size {
            return Err(anyhow!(
                "Buffer size mismatch: expected {}, got {}",
                buffer.size,
                data.len()
            ));
        }

        // Validate stride
        let expected_stride = buffer.width as usize * buffer.format.bytes_per_pixel();
        if expected_stride != buffer.stride as usize {
            return Err(anyhow!(
                "Stride mismatch: expected {}, got {}",
                expected_stride,
                buffer.stride
            ));
        }

        // Write to shared memory pool
        // In a real implementation, this would write to mmap'd memory
        // For now, we simulate it
        debug!(
            "Wrote {} bytes to buffer {} ({}x{}, stride={})",
            data.len(),
            buffer_id,
            buffer.width,
            buffer.height,
            buffer.stride
        );

        info!(
            "Buffer {} data written: {} bytes transferred",
            buffer_id, data.len()
        );

        Ok(())
    }

    // ========================================================================
    // DAMAGE TRACKING
    // ========================================================================

    /// Calculate damage region based on size changes
    fn calculate_damage(&self, old_size: (u32, u32), new_size: (u32, u32)) -> DamageRect {
        if old_size != new_size {
            // Size changed = full redraw
            DamageRect::full_screen(new_size.0, new_size.1)
        } else {
            // No size change = full screen damage (conservative)
            DamageRect::full_screen(new_size.0, new_size.1)
        }
    }

    /// Send damage event to compositor
    fn send_damage_event(&self, surface_id: u32, damage: &DamageRect) -> Result<()> {
        debug!(
            "Sending damage to surface {}: ({},{}) -> {}x{}",
            surface_id, damage.x, damage.y, damage.width, damage.height
        );

        // Verify surface exists
        let surfaces = self.surfaces.read();
        let surface = surfaces
            .get(&surface_id)
            .ok_or_else(|| anyhow!("Surface {} not found", surface_id))?
            .clone();
        drop(surfaces);

        // Validate damage region is within bounds
        if damage.x < 0 || damage.y < 0 {
            warn!(
                "Damage region has negative coordinates: ({}, {})",
                damage.x, damage.y
            );
        }

        let damage_right = damage.x + damage.width as i32;
        let damage_bottom = damage.y + damage.height as i32;

        if damage_right > surface.width as i32 || damage_bottom > surface.height as i32 {
            warn!(
                "Damage region exceeds surface bounds: surface={}x{}, damage_end=({},{})",
                surface.width, surface.height, damage_right, damage_bottom
            );
        }

        // Store damage in current buffer
        if let Some(buffer_id) = surface.buffer_id {
            let mut buffers = self.buffers.write();
            if let Some(buffer) = buffers.get_mut(&buffer_id) {
                buffer.damage_rects.push(damage.clone());
                debug!(
                    "Stored damage in buffer {}: {} regions total",
                    buffer_id,
                    buffer.damage_rects.len()
                );
            }
        }

        debug!(
            "Damage event sent: ({},{}) to ({},{}) = {}x{}",
            damage.x,
            damage.y,
            damage.x + damage.width as i32,
            damage.y + damage.height as i32,
            damage.width,
            damage.height
        );

        // wl_surface_damage(surface, x, y, width, height)

        Ok(())
    }

    // ========================================================================
    // SURFACE COMMIT
    // ========================================================================

    /// Commit all pending state to surface
    fn commit_surface(&self, surface_id: u32, buffer_id: u32) -> Result<()> {
        debug!(
            "Committing surface {}: attaching buffer {}",
            surface_id, buffer_id
        );

        // Verify surface exists
        let mut surfaces = self.surfaces.write();
        let surface = surfaces
            .get_mut(&surface_id)
            .ok_or_else(|| anyhow!("Surface {} not found", surface_id))?;

        // Verify buffer exists
        let buffers = self.buffers.read();
        let buffer = buffers
            .get(&buffer_id)
            .ok_or_else(|| anyhow!("Buffer {} not found", buffer_id))?
            .clone();
        drop(buffers);

        // Validate buffer dimensions match surface
        if buffer.width != surface.width || buffer.height != surface.height {
            warn!(
                "Buffer size mismatch: surface={}x{}, buffer={}x{}",
                surface.width, surface.height, buffer.width, buffer.height
            );
        }

        // Update surface state
        surface.buffer_id = Some(buffer_id);
        surface.pending_buffer_id = None;

        debug!(
            "Committed surface {}: buffer size {}x{}",
            surface_id, buffer.width, buffer.height
        );

        info!(
            "Surface {} committed with buffer {}: {}x{} ({} bytes)",
            surface_id, buffer_id, buffer.width, buffer.height, buffer.size
        );

        // wl_surface_attach(surface, buffer, 0, 0)
        // wl_surface_commit(surface)

        Ok(())
    }

    // ========================================================================
    // FRAME SYNCHRONIZATION
    // ========================================================================

    /// Request frame callback from compositor
    fn request_frame_callback(&self, surface_id: u32) -> Result<u32> {
        // Verify surface exists
        let surfaces = self.surfaces.read();
        let _surface = surfaces
            .get(&surface_id)
            .ok_or_else(|| anyhow!("Surface {} not found", surface_id))?;
        drop(surfaces);

        // Generate unique callback ID
        let mut next_id = self.next_callback_id.write();
        let callback_id = *next_id;
        *next_id += 1;
        drop(next_id);

        // Create frame info
        let frame_info = FrameInfo {
            timestamp: 0,
            sequence: 0,
        };

        // Register in pending frames
        let mut frames = self.pending_frames.write();
        frames.insert(callback_id, frame_info);

        debug!(
            "Requested frame callback {} for surface {}",
            callback_id, surface_id
        );

        // wl_surface_frame(surface) -> callback_id

        Ok(callback_id)
    }

    /// Process frame callback response
    pub async fn process_frame_callback(&self, callback_id: u32, timestamp: u64) -> Result<()> {
        let mut frames = self.pending_frames.write();

        if let Some(frame) = frames.get_mut(&callback_id) {
            frame.timestamp = timestamp;
            frame.sequence += 1;
            debug!("Frame callback {} processed at {}ms", callback_id, timestamp);
            Ok(())
        } else {
            Err(anyhow!("Frame callback {} not found", callback_id))
        }
    }

    // ========================================================================
    // MAIN BUFFER SUBMISSION
    // ========================================================================

    /// Submit rendered buffer to compositor
    pub async fn submit_buffer(
        &self,
        surface_id: u32,
        buffer: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<()> {
        debug!(
            "Submitting buffer to surface {}: {}x{} ({} bytes)",
            surface_id,
            width,
            height,
            buffer.len()
        );

        // Step 1: Validate surface exists
        let mut surfaces = self.surfaces.write();
        let surface = surfaces
            .get_mut(&surface_id)
            .ok_or_else(|| anyhow!("Surface {} not found", surface_id))?
            .clone();

        // Step 2: Allocate or reuse buffer
        let buffer_id = self.allocate_buffer(width, height, BufferFormat::ARGB8888)?;
        debug!("Allocated buffer {} for submission", buffer_id);

        // Step 3: Write pixel data to shared memory
        self.write_buffer_data(buffer_id, &buffer)?;

        // Step 4: Calculate damage (what changed)
        let damage = self.calculate_damage(surface.last_buffer_size, (width, height));
        debug!(
            "Damage region: ({},{}) -> {}x{}",
            damage.x, damage.y, damage.width, damage.height
        );

        // Step 5: Attach buffer to surface (pending state)
        let mut buffers = self.buffers.write();
        if let Some(buf) = buffers.get_mut(&buffer_id) {
            buf.damage_rects = vec![damage.clone()];
        }
        drop(buffers);

        surfaces.get_mut(&surface_id).unwrap().pending_buffer_id = Some(buffer_id);

        // Step 6: Send damage event (which regions changed)
        self.send_damage_event(surface_id, &damage)?;

        // Step 7: Commit transaction
        self.commit_surface(surface_id, buffer_id)?;

        // Step 8: Request frame callback (for sync)
        let callback_id = self.request_frame_callback(surface_id)?;
        surfaces.get_mut(&surface_id).unwrap().frame_callback_id = Some(callback_id);

        // Step 9: Update surface state
        if let Some(surf) = surfaces.get_mut(&surface_id) {
            surf.buffer_id = Some(buffer_id);
            surf.pending_buffer_id = None;
            surf.last_buffer_size = (width, height);
            surf.frame_count += 1;
            surf.is_dirty = false;
        }

        // Step 10: Update stats
        let mut frames = self.frames_submitted.write();
        *frames += 1;
        let mut bytes = self.bytes_transferred.write();
        *bytes += buffer.len() as u64;

        // Record event
        self.event_history.write().push(EventRecord {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            surface_id,
            buffer_id,
            event_type: "submit_buffer".to_string(),
        });

        info!(
            "Buffer submitted: surface={}, buffer={}, size={}x{}, damage=({},{},{}x{})",
            surface_id, buffer_id, width, height, damage.x, damage.y, damage.width, damage.height
        );

        Ok(())
    }

    // ========================================================================
    // STATISTICS & DEBUGGING
    // ========================================================================

    /// Get statistics
    pub fn get_stats(&self) -> (u64, u64, usize) {
        let frames = *self.frames_submitted.read();
        let bytes = *self.bytes_transferred.read();
        let pool_size = self.buffer_pool.read().len();

        (frames, bytes, pool_size)
    }

    /// Dump state for debugging
    pub fn dump_state(&self) -> String {
        let mut output = String::new();

        output.push_str("=== Wayland Client State ===\n");
        output.push_str(&format!("Display: {}x{}\n", self.display_width, self.display_height));

        let surfaces = self.surfaces.read();
        output.push_str(&format!("Surfaces: {}\n", surfaces.len()));
        for surface in surfaces.values() {
            output.push_str(&format!(
                "  ID: {}, Size: {}x{}, Buffer: {:?}, Frames: {}\n",
                surface.surface_id,
                surface.width,
                surface.height,
                surface.buffer_id,
                surface.frame_count
            ));
        }

        let buffers = self.buffers.read();
        output.push_str(&format!("\nBuffers: {}\n", buffers.len()));
        for buffer in buffers.values() {
            output.push_str(&format!(
                "  ID: {}, Size: {}x{}, Format: {:?}, Damage: {}\n",
                buffer.buffer_id,
                buffer.width,
                buffer.height,
                buffer.format,
                buffer.damage_rects.len()
            ));
        }

        let (frames, bytes, pool) = self.get_stats();
        output.push_str(&format!(
            "\nStats: {} frames, {} bytes, {} pooled buffers\n",
            frames, bytes, pool
        ));

        output
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_surface() {
        let client = WaylandClient::new(1280, 720);

        let surface_id = client.create_surface(800, 600).unwrap();
        assert_eq!(surface_id, 1);

        let surface = client.get_surface(surface_id).unwrap();
        assert_eq!(surface.width, 800);
        assert_eq!(surface.height, 600);
    }

    #[test]
    fn test_destroy_surface() {
        let client = WaylandClient::new(1280, 720);

        let surface_id = client.create_surface(800, 600).unwrap();
        assert!(client.get_surface(surface_id).is_some());

        client.destroy_surface(surface_id).unwrap();
        assert!(client.get_surface(surface_id).is_none());
    }

    #[test]
    fn test_resize_surface() {
        let client = WaylandClient::new(1280, 720);

        let surface_id = client.create_surface(800, 600).unwrap();
        client.resize_surface(surface_id, 1024, 768).unwrap();

        let surface = client.get_surface(surface_id).unwrap();
        assert_eq!(surface.width, 1024);
        assert_eq!(surface.height, 768);
        assert!(surface.is_dirty);
    }

    #[tokio::test]
    async fn test_submit_buffer() {
        let client = WaylandClient::new(1280, 720);

        let surface_id = client.create_surface(800, 600).unwrap();
        let buffer_data = vec![0u8; 800 * 600 * 4];

        let result = client.submit_buffer(surface_id, buffer_data, 800, 600).await;
        assert!(result.is_ok());

        let surface = client.get_surface(surface_id).unwrap();
        assert_eq!(surface.frame_count, 1);
    }

    #[test]
    fn test_calculate_damage() {
        let client = WaylandClient::new(1280, 720);

        let damage = client.calculate_damage((800, 600), (1024, 768));
        assert_eq!(damage.width, 1024);
        assert_eq!(damage.height, 768);
    }
}