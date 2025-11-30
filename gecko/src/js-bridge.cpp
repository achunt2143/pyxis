/*
 * JavaScript Bridge Implementation
 * Native function bindings and type system
 */

#include "js-bridge.h"
#include <spdlog/spdlog.h>
#include <spdlog/sinks/file_sink.h>
#include <iostream>
#include <cmath>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// JSVALUE CREATION METHODS
// ============================================================================

JSValue JavaScriptBridge::FromBool(bool value) {
    JSValue result;
    result.type = JSValueType::Boolean;
    result.value = value;
    return result;
}

JSValue JavaScriptBridge::FromNumber(double value) {
    JSValue result;
    result.type = JSValueType::Number;
    result.value = value;
    return result;
}

JSValue JavaScriptBridge::FromString(const std::string& value) {
    JSValue result;
    result.type = JSValueType::String;
    result.value = value;
    return result;
}

JSValue JavaScriptBridge::FromNull() {
    JSValue result;
    result.type = JSValueType::Null;
    return result;
}

JSValue JavaScriptBridge::FromUndefined() {
    JSValue result;
    result.type = JSValueType::Undefined;
    return result;
}

// ============================================================================
// JSVALUE EXTRACTION METHODS
// ============================================================================

bool JavaScriptBridge::ToBool(const JSValue& value) {
    if (value.type == JSValueType::Boolean) {
        try {
            return std::any_cast<bool>(value.value);
        } catch (const std::bad_any_cast&) {
            return false;
        }
    }
    return false;
}

double JavaScriptBridge::ToNumber(const JSValue& value) {
    if (value.type == JSValueType::Number) {
        try {
            return std::any_cast<double>(value.value);
        } catch (const std::bad_any_cast&) {
            return 0.0;
        }
    }
    return 0.0;
}

std::string JavaScriptBridge::ToString(const JSValue& value) {
    switch (value.type) {
        case JSValueType::String:
            try {
                return std::any_cast<std::string>(value.value);
            } catch (const std::bad_any_cast&) {
                return "";
            }
        case JSValueType::Number:
            try {
                double num = std::any_cast<double>(value.value);
                // Check if it's an integer
                if (num == std::floor(num)) {
                    return std::to_string(static_cast<long long>(num));
                } else {
                    return std::to_string(num);
                }
            } catch (const std::bad_any_cast&) {
                return "";
            }
        case JSValueType::Boolean:
            try {
                return std::any_cast<bool>(value.value) ? "true" : "false";
            } catch (const std::bad_any_cast&) {
                return "";
            }
        case JSValueType::Null:
            return "null";
        case JSValueType::Undefined:
            return "undefined";
        default:
            return "[Object]";
    }
}

// ============================================================================
// JAVASCRIPT BRIDGE IMPLEMENTATION
// ============================================================================

class JavaScriptBridge::Impl {
public:
    std::map<std::string, NativeFunction> native_functions_;
    bool initialized_ = false;
    std::shared_ptr<spdlog::logger> logger_;
    
    Impl() {
        try {
            logger_ = spdlog::get("gecko");
            if (!logger_) {
                logger_ = spdlog::stderr_color_mt("gecko");
            }
        } catch (...) {
            // Fallback to stderr if spdlog setup fails
        }
    }
    
    void Log(const std::string& message) {
        if (logger_) {
            logger_->info(message);
        }
    }
};

JavaScriptBridge::JavaScriptBridge() : impl_(std::make_unique<Impl>()) {
}

bool JavaScriptBridge::Initialize() {
    if (impl_->initialized_) {
        return true;
    }
    
    impl_->Log("Initializing JavaScript bridge...");
    
    try {
        // SpiderMonkey runtime initialization would go here
        // For now, we set up the native functions registry
        
        // Register system functions
        DefineNativeFunction("system_shutdown", NativeFunctions::SystemShutdown);
        DefineNativeFunction("system_reboot", NativeFunctions::SystemReboot);
        DefineNativeFunction("system_info", NativeFunctions::SystemGetInfo);
        
        // Register graphics functions
        DefineNativeFunction("graphics_set_resolution", NativeFunctions::GraphicsSetResolution);
        DefineNativeFunction("graphics_get_displays", NativeFunctions::GraphicsGetDisplays);
        DefineNativeFunction("graphics_get_framerate", NativeFunctions::GraphicsGetFrameRate);
        
        // Register IPC functions
        DefineNativeFunction("ipc_call_service", NativeFunctions::IPCCallService);
        DefineNativeFunction("ipc_register_service", NativeFunctions::IPCRegisterService);
        DefineNativeFunction("ipc_get_services", NativeFunctions::IPCGetServices);
        
        // Register file functions
        DefineNativeFunction("file_open", NativeFunctions::FileOpen);
        DefineNativeFunction("file_read", NativeFunctions::FileRead);
        DefineNativeFunction("file_write", NativeFunctions::FileWrite);
        DefineNativeFunction("file_close", NativeFunctions::FileClose);
        
        // Register logging functions
        DefineNativeFunction("log_info", NativeFunctions::LogInfo);
        DefineNativeFunction("log_warn", NativeFunctions::LogWarn);
        DefineNativeFunction("log_error", NativeFunctions::LogError);
        
        impl_->initialized_ = true;
        impl_->Log("JavaScript bridge initialized successfully");
        
        return true;
    } catch (const std::exception& e) {
        impl_->Log(std::string("Exception during JS bridge initialization: ") + e.what());
        return false;
    }
}

bool JavaScriptBridge::Shutdown() {
    if (!impl_->initialized_) {
        return true;
    }
    
    impl_->Log("Shutting down JavaScript bridge...");
    
    try {
        impl_->native_functions_.clear();
        impl_->initialized_ = false;
        impl_->Log("JavaScript bridge shut down successfully");
        
        return true;
    } catch (const std::exception& e) {
        impl_->Log(std::string("Exception during JS bridge shutdown: ") + e.what());
        return false;
    }
}

void JavaScriptBridge::DefineNativeFunction(const std::string& name, NativeFunction callback) {
    impl_->native_functions_[name] = callback;
    impl_->Log("Native function registered: " + name);
}

void JavaScriptBridge::DefineNativeObject(const std::string& object_name, 
                                         const std::map<std::string, NativeFunction>& methods) {
    for (const auto& method : methods) {
        std::string full_name = object_name + "." + method.first;
        impl_->native_functions_[full_name] = method.second;
    }
    impl_->Log("Native object registered: " + object_name + " with " + std::to_string(methods.size()) + " methods");
}

JSValue JavaScriptBridge::MapToJSObject(const std::map<std::string, std::string>& map) {
    JSValue result;
    result.type = JSValueType::Object;
    result.value = map;
    return result;
}

std::map<std::string, std::string> JavaScriptBridge::JSObjectToMap(const JSValue& obj) {
    if (obj.type == JSValueType::Object) {
        try {
            return std::any_cast<std::map<std::string, std::string>>(obj.value);
        } catch (const std::bad_any_cast&) {
            return {};
        }
    }
    return {};
}

JSValue JavaScriptBridge::VectorToJSArray(const std::vector<std::string>& vec) {
    JSValue result;
    result.type = JSValueType::Array;
    result.value = vec;
    return result;
}

std::vector<std::string> JavaScriptBridge::JSArrayToVector(const JSValue& arr) {
    if (arr.type == JSValueType::Array) {
        try {
            return std::any_cast<std::vector<std::string>>(arr.value);
        } catch (const std::bad_any_cast&) {
            return {};
        }
    }
    return {};
}

Result<JSValue> JavaScriptBridge::ExecuteGlobal(const std::string& script) {
    if (!impl_->initialized_) {
        return Result<JSValue>("JS bridge not initialized");
    }
    
    impl_->Log("Executing global script: " + script.substr(0, 50) + "...");
    
    try {
        // SpiderMonkey script execution would go here
        // For now, return a success result
        return Result<JSValue>(FromString("script executed"));
    } catch (const std::exception& e) {
        return Result<JSValue>(std::string("Script execution failed: ") + e.what());
    }
}

Result<JSValue> JavaScriptBridge::CallFunction(const std::string& function_name,
                                             const std::vector<JSValue>& args) {
    if (!impl_->initialized_) {
        return Result<JSValue>("JS bridge not initialized");
    }
    
    auto it = impl_->native_functions_.find(function_name);
    if (it != impl_->native_functions_.end()) {
        try {
            return Result<JSValue>(it->second(args));
        } catch (const std::exception& e) {
            return Result<JSValue>(std::string("Function call failed: ") + e.what());
        }
    }
    
    return Result<JSValue>("Function not found: " + function_name);
}

Result<JSValue> JavaScriptBridge::Evaluate(const std::string& expression) {
    return ExecuteGlobal(expression);
}

// ============================================================================
// PREDEFINED NATIVE FUNCTIONS - SYSTEM
// ============================================================================

namespace NativeFunctions {

JSValue SystemShutdown(const std::vector<JSValue>& args) {
    std::cout << "System shutdown requested from JavaScript" << std::endl;
    // TODO: Implement actual shutdown via IPC
    return FromBool(true);
}

JSValue SystemReboot(const std::vector<JSValue>& args) {
    std::cout << "System reboot requested from JavaScript" << std::endl;
    // TODO: Implement actual reboot via IPC
    return FromBool(true);
}

JSValue SystemGetInfo(const std::vector<JSValue>& args) {
    std::map<std::string, std::string> info;
    info["os"] = "GeckoOS";
    info["version"] = "1.0.0";
    info["arch"] = "x86_64";
    
    JSValue result;
    result.type = JSValueType::Object;
    result.value = info;
    return result;
}

// ============================================================================
// PREDEFINED NATIVE FUNCTIONS - GRAPHICS
// ============================================================================

JSValue GraphicsSetResolution(const std::vector<JSValue>& args) {
    if (args.size() < 2) {
        return FromBool(false);
    }
    
    uint32_t width = static_cast<uint32_t>(ToNumber(args[0]));
    uint32_t height = static_cast<uint32_t>(ToNumber(args[1]));
    
    std::cout << "Graphics resolution set to " << width << "x" << height << std::endl;
    // TODO: Implement actual resolution change
    return FromBool(true);
}

JSValue GraphicsGetDisplays(const std::vector<JSValue>& args) {
    std::vector<std::string> displays = {"HDMI-1", "DP-1"};
    return VectorToJSArray(displays);
}

JSValue GraphicsGetFrameRate(const std::vector<JSValue>& args) {
    return FromNumber(60.0);
}

// ============================================================================
// PREDEFINED NATIVE FUNCTIONS - IPC
// ============================================================================

JSValue IPCCallService(const std::vector<JSValue>& args) {
    if (args.size() < 2) {
        return FromBool(false);
    }
    
    std::string service = ToString(args[0]);
    std::string method = ToString(args[1]);
    
    std::cout << "IPC call: " << service << "." << method << std::endl;
    // TODO: Implement actual IPC call
    return FromBool(true);
}

JSValue IPCRegisterService(const std::vector<JSValue>& args) {
    if (args.size() < 1) {
        return FromBool(false);
    }
    
    std::string service_name = ToString(args[0]);
    std::cout << "Registering IPC service: " << service_name << std::endl;
    // TODO: Implement actual service registration
    return FromBool(true);
}

JSValue IPCGetServices(const std::vector<JSValue>& args) {
    std::vector<std::string> services = {"org.geckoOS.Cronus", "org.geckoOS.Echo", "org.geckoOS.Kratos"};
    return VectorToJSArray(services);
}

// ============================================================================
// PREDEFINED NATIVE FUNCTIONS - FILE
// ============================================================================

JSValue FileOpen(const std::vector<JSValue>& args) {
    if (args.size() < 1) {
        return FromNumber(-1);
    }
    
    std::string path = ToString(args[0]);
    std::cout << "File open: " << path << std::endl;
    // TODO: Implement actual file open
    return FromNumber(1);  // File descriptor
}

JSValue FileRead(const std::vector<JSValue>& args) {
    if (args.size() < 1) {
        return FromString("");
    }
    
    int fd = static_cast<int>(ToNumber(args[0]));
    // TODO: Implement actual file read
    return FromString("file contents");
}

JSValue FileWrite(const std::vector<JSValue>& args) {
    if (args.size() < 2) {
        return FromNumber(0);
    }
    
    int fd = static_cast<int>(ToNumber(args[0]));
    std::string data = ToString(args[1]);
    // TODO: Implement actual file write
    return FromNumber(data.length());
}

JSValue FileClose(const std::vector<JSValue>& args) {
    if (args.size() < 1) {
        return FromBool(false);
    }
    
    int fd = static_cast<int>(ToNumber(args[0]));
    // TODO: Implement actual file close
    return FromBool(true);
}

// ============================================================================
// PREDEFINED NATIVE FUNCTIONS - LOGGING
// ============================================================================

JSValue LogInfo(const std::vector<JSValue>& args) {
    if (args.size() < 1) {
        return FromUndefined();
    }
    
    std::string message = ToString(args[0]);
    std::cout << "[INFO] " << message << std::endl;
    return FromUndefined();
}

JSValue LogWarn(const std::vector<JSValue>& args) {
    if (args.size() < 1) {
        return FromUndefined();
    }
    
    std::string message = ToString(args[0]);
    std::cout << "[WARN] " << message << std::endl;
    return FromUndefined();
}

JSValue LogError(const std::vector<JSValue>& args) {
    if (args.size() < 1) {
        return FromUndefined();
    }
    
    std::string message = ToString(args[0]);
    std::cout << "[ERROR] " << message << std::endl;
    return FromUndefined();
}

}  // namespace NativeFunctions

}  // namespace Gecko
}  // namespace GeckoOS
