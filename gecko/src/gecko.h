/*
 * GeckoOS Gecko Integration Header
 * Main interface for Gecko engine integration with GeckoOS
 * 
 * This header defines the core C++ interfaces for integrating the Gecko
 * (Firefox) rendering engine with the GeckoOS system services and GAIA
 * desktop environment.
 */

#pragma once

#include <memory>
#include <string>
#include <functional>
#include <map>
#include <vector>
#include <optional>

namespace GeckoOS {
namespace Gecko {

// Forward declarations
class GeckoEngine;
class RenderingContext;
class JavaScriptBridge;
class IPCBridge;
class WebSocketServer;

// Result type for operations
template<typename T>
struct Result {
    bool success;
    T value;
    std::string error_message;
    
    Result(const T& v) : success(true), value(v), error_message("") {}
    Result(const std::string& error) : success(false), error_message(error) {}
};

// ============================================================================
// EVENT SYSTEM
// ============================================================================

enum class EventType {
    // Lifecycle events
    WindowOpen,
    WindowClose,
    PageLoad,
    PageUnload,
    
    // Input events
    KeyPress,
    KeyRelease,
    MouseMove,
    MouseClick,
    MouseScroll,
    Touch,
    
    // System events
    SystemShutdown,
    SystemReboot,
    DisplayChange,
    FocusChange,
    
    // Error events
    NetworkError,
    RenderError,
    ScriptError
};

struct Event {
    EventType type;
    std::map<std::string, std::string> data;
    uint64_t timestamp_ms;
};

using EventHandler = std::function<void(const Event&)>;

// ============================================================================
// CONFIGURATION
// ============================================================================

struct GeckoConfig {
    // Graphics
    uint32_t display_width = 1920;
    uint32_t display_height = 1080;
    uint32_t display_refresh_rate = 60;
    bool use_gpu_acceleration = true;
    std::string rendering_backend = "vulkan";  // vulkan or opengl
    
    // Memory
    size_t max_cache_size = 512 * 1024 * 1024;  // 512MB
    size_t initial_heap_size = 128 * 1024 * 1024;  // 128MB
    
    // Performance
    bool enable_jit = true;
    bool enable_incremental_rendering = true;
    uint32_t max_frame_rate = 60;
    
    // Debug
    bool debug_mode = false;
    bool verbose_logging = false;
    std::string log_file = "/var/log/gecko.log";
};

// ============================================================================
// GECKO ENGINE MAIN CLASS
// ============================================================================

class GeckoEngine {
public:
    /**
     * Create and initialize the Gecko engine
     * @param config Configuration parameters
     * @return Result with GeckoEngine instance or error
     */
    static Result<std::shared_ptr<GeckoEngine>> Create(const GeckoConfig& config);
    
    /**
     * Destroy the Gecko engine
     */
    ~GeckoEngine();
    
    // === Lifecycle ===
    
    /**
     * Initialize the engine and all subsystems
     * @return Success status
     */
    bool Initialize();
    
    /**
     * Shutdown the engine cleanly
     * @return Success status
     */
    bool Shutdown();
    
    /**
     * Check if engine is initialized
     */
    bool IsInitialized() const;
    
    // === Content Loading ===
    
    /**
     * Load a URL in the Gecko engine
     * @param url URL to load (https://example.com or file://path)
     * @return Success status
     */
    bool LoadURL(const std::string& url);
    
    /**
     * Load HTML content directly
     * @param html HTML content as string
     * @param base_url Base URL for relative links
     * @return Success status
     */
    bool LoadHTML(const std::string& html, const std::string& base_url = "about:blank");
    
    /**
     * Get current loaded URL
     */
    std::string GetCurrentURL() const;
    
    // === Rendering ===
    
    /**
     * Get the rendering context
     */
    std::shared_ptr<RenderingContext> GetRenderingContext() const;
    
    /**
     * Request a frame render
     */
    void RequestRender();
    
    /**
     * Get current render frame rate
     */
    uint32_t GetFrameRate() const;
    
    // === JavaScript Bridge ===
    
    /**
     * Get the JavaScript bridge
     */
    std::shared_ptr<JavaScriptBridge> GetJSBridge() const;
    
    /**
     * Execute JavaScript in the page context
     * @param script JavaScript code to execute
     * @return Result with return value
     */
    Result<std::string> ExecuteScript(const std::string& script);
    
    // === IPC Communication ===
    
    /**
     * Get the IPC bridge for system service communication
     */
    std::shared_ptr<IPCBridge> GetIPCBridge() const;
    
    // === Events ===
    
    /**
     * Register an event handler
     */
    void RegisterEventHandler(EventType type, EventHandler handler);
    
    /**
     * Unregister an event handler
     */
    void UnregisterEventHandler(EventType type);
    
    /**
     * Dispatch an event
     */
    void DispatchEvent(const Event& event);
    
    // === Configuration ===
    
    /**
     * Get current configuration
     */
    const GeckoConfig& GetConfig() const;
    
    /**
     * Update configuration value
     */
    void SetConfigValue(const std::string& key, const std::string& value);

private:
    GeckoEngine(const GeckoConfig& config);
    
    GeckoConfig config_;
    bool initialized_ = false;
    
    std::shared_ptr<RenderingContext> rendering_context_;
    std::shared_ptr<JavaScriptBridge> js_bridge_;
    std::shared_ptr<IPCBridge> ipc_bridge_;
    std::shared_ptr<WebSocketServer> websocket_server_;
    
    std::map<EventType, EventHandler> event_handlers_;
};

// ============================================================================
// VERSION AND CAPABILITIES
// ============================================================================

struct VersionInfo {
    uint32_t major;
    uint32_t minor;
    uint32_t patch;
    std::string build_type;  // Debug, Release, etc.
};

struct Capabilities {
    bool gpu_acceleration_supported;
    bool vulkan_supported;
    bool opengl_supported;
    std::vector<std::string> supported_codecs;
    std::vector<std::string> supported_media_types;
};

/**
 * Get Gecko version information
 */
VersionInfo GetVersion();

/**
 * Get system capabilities
 */
Capabilities GetCapabilities();

/**
 * Initialize Gecko global subsystems (call once at startup)
 * @return Success status
 */
bool GlobalInitialize();

/**
 * Cleanup Gecko global subsystems (call once at shutdown)
 * @return Success status
 */
bool GlobalShutdown();

}  // namespace Gecko
}  // namespace GeckoOS
