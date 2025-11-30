/*
 * Gecko Engine Wrapper Implementation
 * Core Gecko engine initialization and lifecycle management
 */

#include "gecko.h"
#include "js-bridge.h"
#include "rendering-context.h"
#include "ipc-bridge.h"
#include "websocket-server.h"

#include <iostream>
#include <spdlog/spdlog.h>
#include <spdlog/sinks/file_sink.h>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// STATIC VARIABLES
// ============================================================================

static bool g_initialized = false;
static std::shared_ptr<spdlog::logger> g_logger;

// ============================================================================
// IMPLEMENTATION
// ============================================================================

Result<std::shared_ptr<GeckoEngine>> GeckoEngine::Create(const GeckoConfig& config) {
    try {
        auto engine = std::make_shared<GeckoEngine>(config);
        if (engine->Initialize()) {
            return Result<std::shared_ptr<GeckoEngine>>(engine);
        } else {
            return Result<std::shared_ptr<GeckoEngine>>("Failed to initialize engine");
        }
    } catch (const std::exception& e) {
        return Result<std::shared_ptr<GeckoEngine>>(std::string("Exception: ") + e.what());
    }
}

GeckoEngine::GeckoEngine(const GeckoConfig& config) : config_(config) {
    // Set up logging
    try {
        g_logger = spdlog::file_logger_mt("gecko", config.log_file);
        g_logger->set_level(config.debug_mode ? spdlog::level::debug : spdlog::level::info);
    } catch (const spdlog::spdlog_ex& ex) {
        std::cerr << "Log initialization failed: " << ex.what() << std::endl;
    }
}

GeckoEngine::~GeckoEngine() {
    if (initialized_) {
        Shutdown();
    }
}

bool GeckoEngine::Initialize() {
    if (initialized_) {
        return true;
    }
    
    try {
        if (g_logger) g_logger->info("Initializing Gecko engine...");
        
        // Initialize rendering context
        rendering_context_ = std::make_shared<RenderingContext>();
        if (!rendering_context_->Initialize(config_.display_width, config_.display_height)) {
            if (g_logger) g_logger->error("Failed to initialize rendering context");
            return false;
        }
        if (g_logger) g_logger->info("Rendering context initialized");
        
        // Initialize JavaScript bridge
        js_bridge_ = std::make_shared<JavaScriptBridge>();
        if (!js_bridge_->Initialize()) {
            if (g_logger) g_logger->error("Failed to initialize JS bridge");
            return false;
        }
        if (g_logger) g_logger->info("JavaScript bridge initialized");
        
        // Initialize IPC bridge
        ipc_bridge_ = std::make_shared<IPCBridge>();
        if (!ipc_bridge_->Initialize()) {
            if (g_logger) g_logger->error("Failed to initialize IPC bridge");
            return false;
        }
        if (g_logger) g_logger->info("IPC bridge initialized");
        
        // Initialize WebSocket server
        websocket_server_ = WebSocketServer::Create(8000);
        if (!websocket_server_->Initialize()) {
            if (g_logger) g_logger->error("Failed to initialize WebSocket server");
            return false;
        }
        if (g_logger) g_logger->info("WebSocket server initialized on port 8000");
        
        initialized_ = true;
        if (g_logger) g_logger->info("Gecko engine fully initialized");
        
        return true;
    } catch (const std::exception& e) {
        if (g_logger) g_logger->error("Exception during initialization: {}", e.what());
        return false;
    }
}

bool GeckoEngine::Shutdown() {
    if (!initialized_) {
        return true;
    }
    
    try {
        if (g_logger) g_logger->info("Shutting down Gecko engine...");
        
        // Shutdown in reverse order
        if (websocket_server_) {
            websocket_server_->Shutdown();
            websocket_server_.reset();
        }
        
        if (ipc_bridge_) {
            ipc_bridge_->Shutdown();
            ipc_bridge_.reset();
        }
        
        if (js_bridge_) {
            js_bridge_->Shutdown();
            js_bridge_.reset();
        }
        
        if (rendering_context_) {
            rendering_context_->Shutdown();
            rendering_context_.reset();
        }
        
        initialized_ = false;
        if (g_logger) g_logger->info("Gecko engine shut down successfully");
        
        return true;
    } catch (const std::exception& e) {
        if (g_logger) g_logger->error("Exception during shutdown: {}", e.what());
        return false;
    }
}

bool GeckoEngine::IsInitialized() const {
    return initialized_;
}

bool GeckoEngine::LoadURL(const std::string& url) {
    if (!initialized_) {
        if (g_logger) g_logger->error("Engine not initialized");
        return false;
    }
    
    if (g_logger) g_logger->info("Loading URL: {}", url);
    
    // Execute JavaScript to navigate
    std::string script = "window.location.href = \"" + url + "\";";
    auto result = ExecuteScript(script);
    
    return result.success;
}

bool GeckoEngine::LoadHTML(const std::string& html, const std::string& base_url) {
    if (!initialized_) {
        if (g_logger) g_logger->error("Engine not initialized");
        return false;
    }
    
    if (g_logger) g_logger->info("Loading HTML content");
    
    // Load using data: URI
    std::string data_uri = "data:text/html;charset=utf-8," + html;
    return LoadURL(data_uri);
}

std::string GeckoEngine::GetCurrentURL() const {
    // Execute script to get current URL
    auto const_this = const_cast<GeckoEngine*>(this);
    auto result = const_this->ExecuteScript("window.location.href");
    
    if (result.success) {
        return result.value;
    }
    return "";
}

std::shared_ptr<RenderingContext> GeckoEngine::GetRenderingContext() const {
    return rendering_context_;
}

void GeckoEngine::RequestRender() {
    if (rendering_context_) {
        rendering_context_->BeginFrame();
        rendering_context_->EndFrame();
    }
}

uint32_t GeckoEngine::GetFrameRate() const {
    if (rendering_context_) {
        auto stats = rendering_context_->GetFrameStats();
        return static_cast<uint32_t>(stats.fps);
    }
    return 0;
}

std::shared_ptr<JavaScriptBridge> GeckoEngine::GetJSBridge() const {
    return js_bridge_;
}

Result<std::string> GeckoEngine::ExecuteScript(const std::string& script) {
    if (!js_bridge_) {
        return Result<std::string>("JS bridge not available");
    }
    
    auto result = js_bridge_->ExecuteGlobal(script);
    if (result.success) {
        return Result<std::string>(js_bridge_->ToString(result.value));
    } else {
        return Result<std::string>(result.error_message);
    }
}

std::shared_ptr<IPCBridge> GeckoEngine::GetIPCBridge() const {
    return ipc_bridge_;
}

void GeckoEngine::RegisterEventHandler(EventType type, EventHandler handler) {
    event_handlers_[type] = handler;
}

void GeckoEngine::UnregisterEventHandler(EventType type) {
    event_handlers_.erase(type);
}

void GeckoEngine::DispatchEvent(const Event& event) {
    auto it = event_handlers_.find(event.type);
    if (it != event_handlers_.end()) {
        it->second(event);
    }
}

const GeckoConfig& GeckoEngine::GetConfig() const {
    return config_;
}

void GeckoEngine::SetConfigValue(const std::string& key, const std::string& value) {
    if (g_logger) g_logger->info("Setting config: {} = {}", key, value);
    // TODO: Implement dynamic configuration
}

// ============================================================================
// GLOBAL FUNCTIONS
// ============================================================================

VersionInfo GetVersion() {
    return {1, 0, 0, "Release"};
}

Capabilities GetCapabilities() {
    return {
        true,   // gpu_acceleration_supported
        true,   // vulkan_supported
        true,   // opengl_supported
        {"h264", "vp8", "vp9", "av1"},  // supported_codecs
        {"video/mp4", "video/webm", "audio/mp3", "audio/ogg"}  // supported_media_types
    };
}

bool GlobalInitialize() {
    if (g_initialized) {
        return true;
    }
    
    try {
        // Initialize Gecko-specific subsystems
        // This would typically include:
        // - SpiderMonkey initialization
        // - Graphics subsystem
        // - Network stack
        // - etc.
        
        g_initialized = true;
        return true;
    } catch (const std::exception& e) {
        std::cerr << "Global initialization failed: " << e.what() << std::endl;
        return false;
    }
}

bool GlobalShutdown() {
    if (!g_initialized) {
        return true;
    }
    
    try {
        // Cleanup Gecko subsystems
        g_initialized = false;
        return true;
    } catch (const std::exception& e) {
        std::cerr << "Global shutdown failed: " << e.what() << std::endl;
        return false;
    }
}

}  // namespace Gecko
}  // namespace GeckoOS
