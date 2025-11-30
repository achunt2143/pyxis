/*
 * IPC Bridge Implementation
 * DBus service communication
 */

#include "ipc-bridge.h"
#include <spdlog/spdlog.h>
#include <iostream>
#include <algorithm>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// IPC BRIDGE IMPLEMENTATION
// ============================================================================

class IPCBridge::Impl {
public:
    std::map<std::string, ServiceInfo> services_;
    bool initialized_ = false;
    
    std::map<std::string, std::vector<SignalHandler>> signal_handlers_;
    std::map<std::string, std::function<void(bool)>> service_watchers_;
    
    std::shared_ptr<spdlog::logger> logger_;
    
    Impl() {
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
            std::cout << "[IPC] " << message << std::endl;
        }
    }
};

IPCBridge::IPCBridge() : impl_(std::make_unique<Impl>()) {
}

IPCBridge::~IPCBridge() {
    Shutdown();
}

bool IPCBridge::Initialize() {
    if (impl_->initialized_) {
        return true;
    }
    
    impl_->Log("Initializing IPC bridge...");
    
    try {
        // Register known system services
        ServiceInfo cronus;
        cronus.service_name = "org.geckoOS.Cronus";
        cronus.object_path = "/org/geckoOS/Cronus";
        cronus.interface_name = "org.geckoOS.Cronus.Manager";
        cronus.methods = {"Launch", "Terminate", "GetInfo", "ListProcesses"};
        cronus.is_available = true;
        impl_->services_[cronus.service_name] = cronus;
        
        ServiceInfo echo;
        echo.service_name = "org.geckoOS.Echo";
        echo.object_path = "/org/geckoOS/Echo";
        echo.interface_name = "org.geckoOS.Echo.IPC";
        echo.methods = {"RegisterService", "UnregisterService", "ListServices", "Introspect"};
        echo.is_available = true;
        impl_->services_[echo.service_name] = echo;
        
        ServiceInfo kratos;
        kratos.service_name = "org.geckoOS.Kratos";
        kratos.object_path = "/org/geckoOS/Kratos";
        kratos.interface_name = "org.geckoOS.Kratos.Performance";
        kratos.methods = {"GetCPUUsage", "GetMemoryUsage", "GetTemperature", "SetPowerProfile"};
        kratos.is_available = true;
        impl_->services_[kratos.service_name] = kratos;
        
        ServiceInfo ouranos;
        ouranos.service_name = "org.geckoOS.OURANOS";
        ouranos.object_path = "/org/geckoOS/OURANOS";
        ouranos.interface_name = "org.geckoOS.OURANOS.Compositor";
        ouranos.methods = {"CreateWindow", "DestroyWindow", "Present", "GetDisplays"};
        ouranos.is_available = true;
        impl_->services_[ouranos.service_name] = ouranos;
        
        ServiceInfo persephone;
        persephone.service_name = "org.geckoOS.Persephone";
        persephone.object_path = "/org/geckoOS/Persephone";
        persephone.interface_name = "org.geckoOS.Persephone.Compatibility";
        persephone.methods = {"TranslateCall", "MapResource", "EmulateSystem"};
        persephone.is_available = true;
        impl_->services_[persephone.service_name] = persephone;
        
        impl_->initialized_ = true;
        impl_->Log("IPC bridge initialized with " + std::to_string(impl_->services_.size()) + " services");
        
        return true;
    } catch (const std::exception& e) {
        impl_->Log(std::string("Exception during IPC bridge initialization: ") + e.what());
        return false;
    }
}

bool IPCBridge::Shutdown() {
    if (!impl_->initialized_) {
        return true;
    }
    
    impl_->Log("Shutting down IPC bridge...");
    
    try {
        impl_->services_.clear();
        impl_->signal_handlers_.clear();
        impl_->service_watchers_.clear();
        impl_->initialized_ = false;
        impl_->Log("IPC bridge shut down successfully");
        
        return true;
    } catch (const std::exception& e) {
        impl_->Log(std::string("Exception during IPC bridge shutdown: ") + e.what());
        return false;
    }
}

// ============================================================================
// SERVICE DISCOVERY
// ============================================================================

std::vector<ServiceInfo> IPCBridge::GetServices() const {
    std::vector<ServiceInfo> services;
    for (const auto& pair : impl_->services_) {
        services.push_back(pair.second);
    }
    return services;
}

std::optional<ServiceInfo> IPCBridge::GetServiceInfo(const std::string& service_name) const {
    auto it = impl_->services_.find(service_name);
    if (it != impl_->services_.end()) {
        return it->second;
    }
    return std::nullopt;
}

bool IPCBridge::IsServiceAvailable(const std::string& service_name) const {
    auto it = impl_->services_.find(service_name);
    return it != impl_->services_.end() && it->second.is_available;
}

void IPCBridge::WatchService(const std::string& service_name,
                            std::function<void(bool available)> callback) {
    impl_->service_watchers_[service_name] = callback;
    impl_->Log("Watching service: " + service_name);
}

// ============================================================================
// SYNCHRONOUS CALLS
// ============================================================================

Result<IPCResponse> IPCBridge::CallService(const IPCMessage& message) {
    if (!IsServiceAvailable(message.service)) {
        IPCResponse response;
        response.success = false;
        response.error_message = "Service not available: " + message.service;
        return Result<IPCResponse>(response);
    }
    
    impl_->Log("IPC call: " + message.service + "." + message.method);
    
    try {
        IPCResponse response;
        response.success = true;
        response.data = message.parameters;
        response.data["result"] = "ok";
        
        return Result<IPCResponse>(response);
    } catch (const std::exception& e) {
        IPCResponse response;
        response.success = false;
        response.error_message = std::string("IPC call failed: ") + e.what();
        return Result<IPCResponse>(response);
    }
}

Result<IPCResponse> IPCBridge::CallCronus(const std::string& method,
                                        const std::map<std::string, std::string>& args) {
    IPCMessage message;
    message.service = "org.geckoOS.Cronus";
    message.method = method;
    message.parameters = args;
    return CallService(message);
}

Result<IPCResponse> IPCBridge::CallEcho(const std::string& method,
                                      const std::map<std::string, std::string>& args) {
    IPCMessage message;
    message.service = "org.geckoOS.Echo";
    message.method = method;
    message.parameters = args;
    return CallService(message);
}

Result<IPCResponse> IPCBridge::CallKratos(const std::string& method,
                                        const std::map<std::string, std::string>& args) {
    IPCMessage message;
    message.service = "org.geckoOS.Kratos";
    message.method = method;
    message.parameters = args;
    return CallService(message);
}

Result<IPCResponse> IPCBridge::CallOuranos(const std::string& method,
                                         const std::map<std::string, std::string>& args) {
    IPCMessage message;
    message.service = "org.geckoOS.OURANOS";
    message.method = method;
    message.parameters = args;
    return CallService(message);
}

Result<IPCResponse> IPCBridge::CallPersephone(const std::string& method,
                                            const std::map<std::string, std::string>& args) {
    IPCMessage message;
    message.service = "org.geckoOS.Persephone";
    message.method = method;
    message.parameters = args;
    return CallService(message);
}

// ============================================================================
// ASYNCHRONOUS CALLS
// ============================================================================

void IPCBridge::CallServiceAsync(const IPCMessage& message, IPCCallback callback) {
    impl_->Log("Async IPC call: " + message.service + "." + message.method);
    
    // Simulate async operation (in real implementation, would use DBus async methods)
    auto result = CallService(message);
    if (result.success) {
        callback(result.value);
    } else {
        IPCResponse response;
        response.success = false;
        response.error_message = result.error_message;
        callback(response);
    }
}

void IPCBridge::CallBatch(const std::vector<IPCMessage>& messages,
                         std::function<void(const std::vector<IPCResponse>&)> callback) {
    impl_->Log("Batch IPC call: " + std::to_string(messages.size()) + " messages");
    
    std::vector<IPCResponse> responses;
    for (const auto& message : messages) {
        auto result = CallService(message);
        if (result.success) {
            responses.push_back(result.value);
        } else {
            IPCResponse response;
            response.success = false;
            response.error_message = result.error_message;
            responses.push_back(response);
        }
    }
    
    callback(responses);
}

// ============================================================================
// SIGNAL HANDLING
// ============================================================================

void IPCBridge::ListenSignal(const std::string& service, const std::string& signal,
                            SignalHandler handler) {
    std::string signal_key = service + ":" + signal;
    impl_->signal_handlers_[signal_key].push_back(handler);
    impl_->Log("Listening to signal: " + signal_key);
}

void IPCBridge::UnlistenSignal(const std::string& service, const std::string& signal) {
    std::string signal_key = service + ":" + signal;
    impl_->signal_handlers_.erase(signal_key);
    impl_->Log("Unlistening from signal: " + signal_key);
}

// ============================================================================
// OBJECT INTROSPECTION
// ============================================================================

std::string IPCBridge::Introspect(const std::string& service,
                                 const std::string& object_path) const {
    impl_->Log("Introspecting: " + service + object_path);
    
    // Return basic introspection XML
    std::string xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
    xml += "<node name=\"" + object_path + "\">\n";
    
    auto service_info = GetServiceInfo(service);
    if (service_info) {
        for (const auto& method : service_info->methods) {
            xml += "  <interface name=\"" + service_info->interface_name + "\">\n";
            xml += "    <method name=\"" + method + "\"/>\n";
            xml += "  </interface>\n";
        }
    }
    
    xml += "</node>\n";
    return xml;
}

std::string IPCBridge::GetMethodSignature(const std::string& service,
                                         const std::string& method) const {
    auto service_info = GetServiceInfo(service);
    if (service_info) {
        auto it = std::find(service_info->methods.begin(), service_info->methods.end(), method);
        if (it != service_info->methods.end()) {
            return "()";  // Simplified: return empty signature
        }
    }
    return "";
}

}  // namespace Gecko
}  // namespace GeckoOS
