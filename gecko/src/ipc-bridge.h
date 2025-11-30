/*
 * IPC Bridge Header
 * Communication with system services via DBus
 */

#pragma once

#include "gecko.h"
#include <map>
#include <functional>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// SERVICE DEFINITIONS
// ============================================================================

struct ServiceInfo {
    std::string service_name;
    std::string object_path;
    std::string interface_name;
    std::vector<std::string> methods;
    bool is_available;
};

// ============================================================================
// IPC MESSAGE
// ============================================================================

struct IPCMessage {
    std::string service;
    std::string method;
    std::map<std::string, std::string> parameters;
    uint32_t timeout_ms = 5000;
};

struct IPCResponse {
    bool success;
    std::map<std::string, std::string> data;
    std::string error_message;
};

using IPCCallback = std::function<void(const IPCResponse&)>;

// ============================================================================
// IPC BRIDGE
// ============================================================================

class IPCBridge {
public:
    /**
     * Initialize IPC bridge and connect to system bus
     */
    bool Initialize();
    
    /**
     * Shutdown IPC bridge
     */
    bool Shutdown();
    
    // === Service Discovery ===
    
    /**
     * Get available services
     */
    std::vector<ServiceInfo> GetServices() const;
    
    /**
     * Get service information
     */
    std::optional<ServiceInfo> GetServiceInfo(const std::string& service_name) const;
    
    /**
     * Check if service is available
     */
    bool IsServiceAvailable(const std::string& service_name) const;
    
    /**
     * Watch for service availability
     */
    void WatchService(const std::string& service_name,
                     std::function<void(bool available)> callback);
    
    // === Synchronous Calls ===
    
    /**
     * Call a service method synchronously
     */
    Result<IPCResponse> CallService(const IPCMessage& message);
    
    /**
     * Call Cronus process manager
     */
    Result<IPCResponse> CallCronus(const std::string& method,
                                   const std::map<std::string, std::string>& args);
    
    /**
     * Call Echo IPC manager
     */
    Result<IPCResponse> CallEcho(const std::string& method,
                                const std::map<std::string, std::string>& args);
    
    /**
     * Call Kratos performance manager
     */
    Result<IPCResponse> CallKratos(const std::string& method,
                                   const std::map<std::string, std::string>& args);
    
    /**
     * Call OURANOS compositor
     */
    Result<IPCResponse> CallOuranos(const std::string& method,
                                    const std::map<std::string, std::string>& args);
    
    /**
     * Call Persephone compatibility layer
     */
    Result<IPCResponse> CallPersephone(const std::string& method,
                                       const std::map<std::string, std::string>& args);
    
    // === Asynchronous Calls ===
    
    /**
     * Call a service method asynchronously
     */
    void CallServiceAsync(const IPCMessage& message, IPCCallback callback);
    
    /**
     * Batch multiple async calls
     */
    void CallBatch(const std::vector<IPCMessage>& messages,
                  std::function<void(const std::vector<IPCResponse>&)> callback);
    
    // === Signal Handling ===
    
    /**
     * Listen for signals from a service
     */
    using SignalHandler = std::function<void(const std::map<std::string, std::string>&)>;
    void ListenSignal(const std::string& service, const std::string& signal,
                     SignalHandler handler);
    
    /**
     * Unlisten from signal
     */
    void UnlistenSignal(const std::string& service, const std::string& signal);
    
    // === Object Introspection ===
    
    /**
     * Get object introspection XML
     */
    std::string Introspect(const std::string& service,
                          const std::string& object_path) const;
    
    /**
     * Get method signature
     */
    std::string GetMethodSignature(const std::string& service,
                                  const std::string& method) const;

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace Gecko
}  // namespace GeckoOS
