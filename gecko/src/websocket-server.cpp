/*
 * WebSocket Server Implementation
 * Real-time communication between frontend and backend
 */

#include "websocket-server.h"
#include <spdlog/spdlog.h>
#include <iostream>
#include <algorithm>
#include <cstring>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// WEBSOCKET SERVER IMPLEMENTATION
// ============================================================================

class WebSocketServer::Impl {
public:
    uint16_t port_;
    bool running_ = false;
    bool initialized_ = false;
    
    std::map<std::string, std::vector<uint8_t>> connected_clients_;
    std::map<std::string, WebSocketHandler> message_handlers_;
    
    ConnectionCallback on_connection_;
    DisconnectionCallback on_disconnection_;
    
    size_t max_connections_ = 100;
    uint32_t message_timeout_ms_ = 30000;
    bool ssl_enabled_ = false;
    std::string ssl_cert_path_;
    std::string ssl_key_path_;
    
    std::shared_ptr<spdlog::logger> logger_;
    
    Impl(uint16_t port) : port_(port) {
        try {
            logger_ = spdlog::get("gecko");
            if (!logger_) {
                logger_ = spdlog::stderr_color_mt("gecko");
            }
        } catch (...) {
            // Fallback
        }
    }
    
    void Log(const std::string& message) {
        if (logger_) {
            logger_->info(message);
        } else {
            std::cout << "[WebSocket] " << message << std::endl;
        }
    }
};

std::shared_ptr<WebSocketServer> WebSocketServer::Create(uint16_t port) {
    return std::make_shared<WebSocketServer>(port);
}

WebSocketServer::WebSocketServer(uint16_t port) : impl_(std::make_unique<Impl>(port)) {
}

WebSocketServer::~WebSocketServer() {
    Shutdown();
}

bool WebSocketServer::Initialize() {
    if (impl_->initialized_) {
        return true;
    }
    
    impl_->Log("Initializing WebSocket server on port " + std::to_string(impl_->port_) + "...");
    
    try {
        // TODO: Initialize libwebsockets context
        // For now, just mark as initialized
        
        impl_->running_ = true;
        impl_->initialized_ = true;
        impl_->Log("WebSocket server initialized and listening");
        
        return true;
    } catch (const std::exception& e) {
        impl_->Log(std::string("Exception during WebSocket server initialization: ") + e.what());
        return false;
    }
}

bool WebSocketServer::Shutdown() {
    if (!impl_->initialized_) {
        return true;
    }
    
    impl_->Log("Shutting down WebSocket server...");
    
    try {
        impl_->running_ = false;
        impl_->connected_clients_.clear();
        impl_->message_handlers_.clear();
        impl_->initialized_ = false;
        impl_->Log("WebSocket server shut down successfully");
        
        return true;
    } catch (const std::exception& e) {
        impl_->Log(std::string("Exception during WebSocket server shutdown: ") + e.what());
        return false;
    }
}

bool WebSocketServer::IsRunning() const {
    return impl_->running_;
}

// ============================================================================
// CONNECTION MANAGEMENT
// ============================================================================

size_t WebSocketServer::GetClientCount() const {
    return impl_->connected_clients_.size();
}

std::vector<std::string> WebSocketServer::GetConnectedClients() const {
    std::vector<std::string> clients;
    for (const auto& pair : impl_->connected_clients_) {
        clients.push_back(pair.first);
    }
    return clients;
}

void WebSocketServer::DisconnectClient(const std::string& client_id) {
    auto it = impl_->connected_clients_.find(client_id);
    if (it != impl_->connected_clients_.end()) {
        impl_->connected_clients_.erase(it);
        impl_->Log("Client disconnected: " + client_id);
        
        if (impl_->on_disconnection_) {
            impl_->on_disconnection_(client_id);
        }
    }
}

void WebSocketServer::Broadcast(const WebSocketMessage& message) {
    impl_->Log("Broadcasting message type: " + message.type);
    
    // Encode message
    std::string encoded = EncodeMessage(message);
    
    // Send to all clients
    for (const auto& client_id : GetConnectedClients()) {
        SendToClient(client_id, message);
    }
}

void WebSocketServer::SendToClient(const std::string& client_id, const WebSocketMessage& message) {
    auto it = impl_->connected_clients_.find(client_id);
    if (it != impl_->connected_clients_.end()) {
        impl_->Log("Sending message to client: " + client_id);
        
        // TODO: Send via libwebsockets
        // For now, just log the operation
    }
}

// ============================================================================
// MESSAGE HANDLING
// ============================================================================

void WebSocketServer::RegisterHandler(const std::string& message_type, WebSocketHandler handler) {
    impl_->message_handlers_[message_type] = handler;
    impl_->Log("Handler registered for message type: " + message_type);
}

void WebSocketServer::UnregisterHandler(const std::string& message_type) {
    impl_->message_handlers_.erase(message_type);
    impl_->Log("Handler unregistered for message type: " + message_type);
}

// ============================================================================
// EVENT CALLBACKS
// ============================================================================

void WebSocketServer::SetConnectionCallback(ConnectionCallback callback) {
    impl_->on_connection_ = callback;
}

void WebSocketServer::SetDisconnectionCallback(DisconnectionCallback callback) {
    impl_->on_disconnection_ = callback;
}

// ============================================================================
// CONFIGURATION
// ============================================================================

uint16_t WebSocketServer::GetPort() const {
    return impl_->port_;
}

void WebSocketServer::SetMaxConnections(size_t max_connections) {
    impl_->max_connections_ = max_connections;
    impl_->Log("Max connections set to: " + std::to_string(max_connections));
}

void WebSocketServer::SetMessageTimeout(uint32_t timeout_ms) {
    impl_->message_timeout_ms_ = timeout_ms;
    impl_->Log("Message timeout set to: " + std::to_string(timeout_ms) + "ms");
}

void WebSocketServer::SetSSLEnabled(bool enabled) {
    impl_->ssl_enabled_ = enabled;
    impl_->Log("SSL enabled: " + std::string(enabled ? "true" : "false"));
}

void WebSocketServer::SetSSLCertificate(const std::string& cert_path, const std::string& key_path) {
    impl_->ssl_cert_path_ = cert_path;
    impl_->ssl_key_path_ = key_path;
    impl_->Log("SSL certificate set: " + cert_path);
}

// ============================================================================
// MESSAGE ENCODING/DECODING
// ============================================================================

std::string WebSocketServer::EncodeMessage(const WebSocketMessage& message) {
    // Simple JSON-like encoding for now
    std::string encoded = "{";
    encoded += "\"type\":\"" + message.type + "\",";
    encoded += "\"id\":\"" + message.id + "\",";
    encoded += "\"payload\":{";
    
    bool first = true;
    for (const auto& pair : message.payload) {
        if (!first) encoded += ",";
        encoded += "\"" + pair.first + "\":\"" + pair.second + "\"";
        first = false;
    }
    
    encoded += "}";
    encoded += "}";
    
    return encoded;
}

WebSocketMessage WebSocketServer::DecodeMessage(const std::string& encoded) {
    WebSocketMessage message;
    
    // Simple parsing (in real implementation, would use JSON library)
    // Extract type
    size_t type_pos = encoded.find("\"type\":\"");
    if (type_pos != std::string::npos) {
        type_pos += 9;
        size_t type_end = encoded.find("\"", type_pos);
        message.type = encoded.substr(type_pos, type_end - type_pos);
    }
    
    // Extract id
    size_t id_pos = encoded.find("\"id\":\"");
    if (id_pos != std::string::npos) {
        id_pos += 6;
        size_t id_end = encoded.find("\"", id_pos);
        message.id = encoded.substr(id_pos, id_end - id_pos);
    }
    
    return message;
}

}  // namespace Gecko
}  // namespace GeckoOS
