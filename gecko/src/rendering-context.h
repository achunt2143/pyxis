/*
 * Rendering Context Header
 * Graphics and rendering interface for Gecko integration
 */

#pragma once

#include "gecko.h"
#include <cstdint>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// SURFACE AND BUFFER MANAGEMENT
// ============================================================================

struct Surface {
    uint32_t width;
    uint32_t height;
    uint32_t stride;  // bytes per row
    uint32_t format;  // DRM format code
    void* buffer;
    uint64_t handle;  // GPU handle
};

struct Buffer {
    uint32_t width;
    uint32_t height;
    uint32_t format;
    void* data;
    size_t size;
    bool is_gpu_memory;
};

// ============================================================================
// RENDERING CONTEXT
// ============================================================================

class RenderingContext {
public:
    /**
     * Initialize rendering context
     * @param display_width Screen width
     * @param display_height Screen height
     * @return Success status
     */
    bool Initialize(uint32_t display_width, uint32_t display_height);
    
    /**
     * Shutdown rendering context
     */
    bool Shutdown();
    
    // === Surface Management ===
    
    /**
     * Create a new surface
     * @param width Surface width
     * @param height Surface height
     * @return Surface ID or -1 on error
     */
    int32_t CreateSurface(uint32_t width, uint32_t height);
    
    /**
     * Destroy a surface
     */
    bool DestroySurface(int32_t surface_id);
    
    /**
     * Get surface information
     */
    const Surface* GetSurface(int32_t surface_id) const;
    
    /**
     * Resize surface
     */
    bool ResizeSurface(int32_t surface_id, uint32_t width, uint32_t height);
    
    // === Buffer Management ===
    
    /**
     * Allocate a buffer
     */
    Buffer* AllocateBuffer(uint32_t width, uint32_t height, uint32_t format);
    
    /**
     * Free a buffer
     */
    void FreeBuffer(Buffer* buffer);
    
    /**
     * Lock buffer for CPU access
     */
    void* LockBuffer(Buffer* buffer);
    
    /**
     * Unlock buffer after CPU access
     */
    void UnlockBuffer(Buffer* buffer);
    
    // === Drawing Operations ===
    
    /**
     * Clear surface to color
     */
    bool Clear(int32_t surface_id, uint32_t color);
    
    /**
     * Blit from source to destination surface
     */
    bool Blit(int32_t src_surface, int32_t dst_surface,
              uint32_t src_x, uint32_t src_y,
              uint32_t dst_x, uint32_t dst_y,
              uint32_t width, uint32_t height);
    
    /**
     * Fill rectangle with color
     */
    bool FillRect(int32_t surface_id,
                  uint32_t x, uint32_t y,
                  uint32_t width, uint32_t height,
                  uint32_t color);
    
    // === Rendering Control ===
    
    /**
     * Begin a frame
     */
    bool BeginFrame();
    
    /**
     * End a frame and present to display
     */
    bool EndFrame();
    
    /**
     * Present a surface to display
     */
    bool Present(int32_t surface_id);
    
    /**
     * Swap buffers
     */
    bool SwapBuffers();
    
    // === Display Management ===
    
    /**
     * Get display width
     */
    uint32_t GetDisplayWidth() const;
    
    /**
     * Get display height
     */
    uint32_t GetDisplayHeight() const;
    
    /**
     * Set display resolution
     */
    bool SetDisplayResolution(uint32_t width, uint32_t height);
    
    /**
     * Get refresh rate in Hz
     */
    uint32_t GetRefreshRate() const;
    
    // === GPU Acceleration ===
    
    /**
     * Check if GPU acceleration is available
     */
    bool IsGPUAccelerated() const;
    
    /**
     * Get GPU memory info
     */
    struct GPUMemoryInfo {
        uint64_t total_memory;
        uint64_t available_memory;
        uint64_t used_memory;
    };
    
    GPUMemoryInfo GetGPUMemoryInfo() const;
    
    // === Synchronization ===
    
    /**
     * Wait for vertical sync
     */
    void WaitVSync();
    
    /**
     * Get frame timing information
     */
    struct FrameStats {
        uint32_t frame_count;
        double fps;
        double frame_time_ms;
        double render_time_ms;
        double present_time_ms;
    };
    
    FrameStats GetFrameStats() const;

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace Gecko
}  // namespace GeckoOS
