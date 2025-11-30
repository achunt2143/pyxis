/*
 * WebSocket Server Header
 * Provides WebSocket bridge between GAIA frontend and Gecko backend
 */

#pragma once

#include "gecko.h"
#include <memory>
#include <string>
#include <functional>
#include <map>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// WEBSOCKET MESSAGE
// ============================================================================

struct WebSocketMessage {
    std::string type;  // "command", "event", "response"
    std::string id;    // Message ID for tracking
    std::map<std::string, std::string> payload;
};

using WebSocketHandler = std::function<void(const WebSocketMessage&, 
                                            std::function<void(const WebSocketMessage&)>)>;

// ============================================================================
// WEBSOCKET SERVER
// ============================================================================

class WebSocketServer {
public:
    /**
     * Create WebSocket server on specified port
     * @param port Port number (default 8000)
     */
    static std::shared_ptr<WebSocketServer> Create(uint16_t port = 8000);
    
    /**
     * Initialize server
     */
    bool Initialize();
    
    /**
     * Shutdown server
     */
    bool Shutdown();
    
    /**
     * Check if server is running
     */
    bool IsRunning() const;
    
    // === Connection Management ===
    
    /**
     * Get number of connected clients
     */
    size_t GetClientCount() const;
    
    /**
     * Get list of connected clients
     */
    std::vector<std::string> GetConnectedClients() const;
    
    /**
     * Disconnect a client
     */
    void DisconnectClient(const std::string& client_id);
    
    /**
     * Broadcast message to all clients
     */
    void Broadcast(const WebSocketMessage& message);
    
    /**
     * Send message to specific client
     */
    void SendToClient(const std::string& client_id, const WebSocketMessage& message);
    
    // === Message Handling ===
    
    /**
     * Register handler for message type
     */
    void RegisterHandler(const std::string& message_type, WebSocketHandler handler);
    
    /**
     * Unregister handler
     */
    void UnregisterHandler(const std::string& message_type);
    
    // === Event Callbacks ===
    
    /**
     * Set connection callback
     */
    using ConnectionCallback = std::function<void(const std::string& client_id)>;
    void SetConnectionCallback(ConnectionCallback callback);
    
    /**
     * Set disconnection callback
     */
    using DisconnectionCallback = std::function<void(const std::string& client_id)>;
    void SetDisconnectionCallback(DisconnectionCallback callback);
    
    // === Configuration ===
    
    /**
     * Get server port
     */
    uint16_t GetPort() const;
    
    /**
     * Set maximum connections
     */
    void SetMaxConnections(size_t max_connections);
    
    /**
     * Set message timeout
     */
    void SetMessageTimeout(uint32_t timeout_ms);
    
    /**
     * Enable/disable SSL
     */
    void SetSSLEnabled(bool enabled);
    
    /**
     * Set SSL certificate path
     */
    void SetSSLCertificate(const std::string& cert_path, const std::string& key_path);

private:
    WebSocketServer(uint16_t port);
    
    class Impl;
    std::unique_ptr<Impl> impl_;
};

// ============================================================================
// STANDARD MESSAGE TYPES
// ============================================================================

namespace MessageTypes {

// Command messages (GAIA → Gecko)
const char* const CMD_SYSTEM_INFO = "cmd_system_info";
const char* const CMD_SCREENSHOT = "cmd_screenshot";
const char* const CMD_LOAD_URL = "cmd_load_url";
const char* const CMD_EXECUTE_SCRIPT = "cmd_execute_script";
const char* const CMD_IPC_CALL = "cmd_ipc_call";

// Event messages (Gecko → GAIA)
const char* const EVT_PAGE_LOADED = "evt_page_loaded";
const char* const EVT_PAGE_ERROR = "evt_page_error";
const char* const EVT_INPUT = "evt_input";
const char* const EVT_SYSTEM = "evt_system";
const char* const EVT_GRAPHICS = "evt_graphics";

// Response messages (Bidirectional)
const char* const RESP_SUCCESS = "resp_success";
const char* const RESP_ERROR = "resp_error";

}  // namespace MessageTypes

}  // namespace Gecko
}  // namespace GeckoOS
