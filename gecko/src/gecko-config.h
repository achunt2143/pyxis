/*
 * Gecko Configuration Header
 * Compile-time configuration and feature flags
 */

#pragma once

// ============================================================================
// VERSION INFORMATION
// ============================================================================

#define GECKO_VERSION_MAJOR 1
#define GECKO_VERSION_MINOR 0
#define GECKO_VERSION_PATCH 0
#define GECKO_VERSION_STRING "1.0.0"
#define GECKO_BUILD_DATE __DATE__
#define GECKO_BUILD_TIME __TIME__

// ============================================================================
// FEATURE FLAGS
// ============================================================================

// Enable GPU acceleration support
#define GECKO_ENABLE_GPU_ACCELERATION 1

// Enable Vulkan rendering backend
#define GECKO_ENABLE_VULKAN 1

// Enable OpenGL rendering backend
#define GECKO_ENABLE_OPENGL 1

// Enable WebGL support in pages
#define GECKO_ENABLE_WEBGL 1

// Enable WebAssembly (WASM) support
#define GECKO_ENABLE_WASM 1

// Enable Service Workers
#define GECKO_ENABLE_SERVICE_WORKERS 1

// Enable offline support
#define GECKO_ENABLE_OFFLINE 1

// Enable hardware video decoding
#define GECKO_ENABLE_HW_VIDEO_DECODE 1

// Enable hardware audio acceleration
#define GECKO_ENABLE_HW_AUDIO 1

// Enable incremental rendering optimization
#define GECKO_ENABLE_INCREMENTAL_RENDERING 1

// Enable dirty region tracking
#define GECKO_ENABLE_DIRTY_REGION_TRACKING 1

// Enable memory compression
#define GECKO_ENABLE_MEMORY_COMPRESSION 1

// Enable GPU memory pooling
#define GECKO_ENABLE_MEMORY_POOLING 1

// Enable JIT compilation for JavaScript
#define GECKO_ENABLE_JIT 1

// ============================================================================
// DEBUG CONFIGURATION
// ============================================================================

#ifdef DEBUG
    #define GECKO_DEBUG_MODE 1
    #define GECKO_VERBOSE_LOGGING 1
    #define GECKO_ENABLE_PROFILING 1
    #define GECKO_ENABLE_MEMORY_TRACKING 1
#else
    #define GECKO_DEBUG_MODE 0
    #define GECKO_VERBOSE_LOGGING 0
    #define GECKO_ENABLE_PROFILING 0
    #define GECKO_ENABLE_MEMORY_TRACKING 0
#endif

// ============================================================================
// MEMORY CONFIGURATION
// ============================================================================

// Initial JavaScript heap size in MB
#define GECKO_JS_HEAP_INITIAL_MB 128

// Maximum JavaScript heap size in MB
#define GECKO_JS_HEAP_MAX_MB 1024

// Maximum page cache size in MB
#define GECKO_PAGE_CACHE_MAX_MB 512

// GPU memory pool size in MB
#define GECKO_GPU_MEMORY_POOL_MB 256

// Buffer pool allocation chunk size in bytes
#define GECKO_BUFFER_POOL_CHUNK_SIZE (64 * 1024)

// ============================================================================
// PERFORMANCE TUNING
// ============================================================================

// Maximum frame rate (0 = unlimited)
#define GECKO_MAX_FRAME_RATE 60

// Target frame time in milliseconds
#define GECKO_TARGET_FRAME_TIME_MS (1000.0 / GECKO_MAX_FRAME_RATE)

// IPC message batch size before sending
#define GECKO_IPC_BATCH_SIZE 100

// IPC batch timeout in milliseconds
#define GECKO_IPC_BATCH_TIMEOUT_MS 50

// DBus call timeout in milliseconds
#define GECKO_DBUS_TIMEOUT_MS 5000

// WebSocket message buffer size
#define GECKO_WEBSOCKET_BUFFER_SIZE (64 * 1024)

// WebSocket message queue size
#define GECKO_WEBSOCKET_QUEUE_SIZE 1000

// ============================================================================
// DISPLAY CONFIGURATION
// ============================================================================

// Default display width in pixels
#define GECKO_DEFAULT_DISPLAY_WIDTH 1920

// Default display height in pixels
#define GECKO_DEFAULT_DISPLAY_HEIGHT 1080

// Default display refresh rate in Hz
#define GECKO_DEFAULT_REFRESH_RATE 60

// DRM device path
#define GECKO_DRM_DEVICE "/dev/dri/card0"

// DRM render device path (for GPU operations)
#define GECKO_DRM_RENDER_DEVICE "/dev/dri/renderD128"

// ============================================================================
// LOGGING CONFIGURATION
// ============================================================================

// Log file path
#define GECKO_LOG_FILE "/var/log/geckoOS/gecko.log"

// Maximum log file size in MB before rotation
#define GECKO_LOG_MAX_SIZE_MB 100

// Number of rotated log files to keep
#define GECKO_LOG_RETENTION_COUNT 10

// ============================================================================
// PLATFORM-SPECIFIC SETTINGS
// ============================================================================

// Wayland socket path
#define GECKO_WAYLAND_SOCKET_PATH "/run/wayland"

// DBus session bus socket
#define GECKO_DBUS_SESSION_SOCKET "/run/user/1000/bus"

// DBus system bus address
#define GECKO_DBUS_SYSTEM_BUS "unix:path=/run/dbus/system_bus_socket"

// Temporary files directory
#define GECKO_TEMP_DIR "/tmp/geckoOS"

// Cache directory
#define GECKO_CACHE_DIR "/var/cache/geckoOS"

// ============================================================================
// COMPILATION SETTINGS
// ============================================================================

// Use C++17 features
#define GECKO_CXX_STANDARD 17

// Enable all warnings
#define GECKO_ENABLE_WARNINGS 1

// Enable position-independent code
#define GECKO_ENABLE_PIE 1

// Enable stack protection
#define GECKO_ENABLE_STACK_PROTECTION 1

// ============================================================================
// MACRO UTILITIES
// ============================================================================

// Stringify macro helper
#define GECKO_STRINGIFY_IMPL(x) #x
#define GECKO_STRINGIFY(x) GECKO_STRINGIFY_IMPL(x)

// Concatenate macro helper
#define GECKO_CONCAT_IMPL(a, b) a ## b
#define GECKO_CONCAT(a, b) GECKO_CONCAT_IMPL(a, b)

// Alignment macro
#define GECKO_ALIGNED(alignment) __attribute__((aligned(alignment)))

// Likely/unlikely branch optimization
#define GECKO_LIKELY(x) __builtin_expect(!!(x), 1)
#define GECKO_UNLIKELY(x) __builtin_expect(!!(x), 0)

// Deprecated attribute
#define GECKO_DEPRECATED(msg) __attribute__((deprecated(msg)))

// Packed structure
#define GECKO_PACKED __attribute__((packed))

// No-inline function
#define GECKO_NO_INLINE __attribute__((noinline))

// Always-inline function
#define GECKO_ALWAYS_INLINE __attribute__((always_inline)) inline

// ============================================================================
// OPTIONAL EXTENSIONS
// ============================================================================

// Enable codec support
#define GECKO_CODEC_H264 1
#define GECKO_CODEC_VP8 1
#define GECKO_CODEC_VP9 1
#define GECKO_CODEC_AV1 1

// Enable media format support
#define GECKO_FORMAT_MP4 1
#define GECKO_FORMAT_WEBM 1
#define GECKO_FORMAT_OGG 1
#define GECKO_FORMAT_MKV 1

#endif // GECKOOS_GECKO_CONFIG_H
