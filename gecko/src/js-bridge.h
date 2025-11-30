/*
 * JavaScript Bridge Header
 * Bindings between JavaScript and C++ native functions
 */

#pragma once

#include "gecko.h"
#include <any>
#include <functional>

namespace GeckoOS {
namespace Gecko {

// ============================================================================
// JAVASCRIPT VALUE TYPES
// ============================================================================

enum class JSValueType {
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    Object,
    Array,
    Function
};

struct JSValue {
    JSValueType type;
    std::any value;
};

// ============================================================================
// JAVASCRIPT BRIDGE
// ============================================================================

class JavaScriptBridge {
public:
    /**
     * Initialize the JavaScript bridge with SpiderMonkey runtime
     */
    bool Initialize();
    
    /**
     * Shutdown the JS bridge
     */
    bool Shutdown();
    
    // === Native Function Bindings ===
    
    /**
     * Define a native C++ function callable from JavaScript
     * @param name Function name in JavaScript
     * @param callback Function to call
     */
    using NativeFunction = std::function<JSValue(const std::vector<JSValue>&)>;
    void DefineNativeFunction(const std::string& name, NativeFunction callback);
    
    /**
     * Define a native object with methods
     * @param object_name Object name (e.g., "geckoOS")
     * @param methods Map of method name to function
     */
    void DefineNativeObject(const std::string& object_name, 
                           const std::map<std::string, NativeFunction>& methods);
    
    // === Object/Array Marshaling ===
    
    /**
     * Convert C++ std::map to JavaScript object
     */
    JSValue MapToJSObject(const std::map<std::string, std::string>& map);
    
    /**
     * Convert JavaScript object to C++ std::map
     */
    std::map<std::string, std::string> JSObjectToMap(const JSValue& obj);
    
    /**
     * Convert C++ vector to JavaScript array
     */
    JSValue VectorToJSArray(const std::vector<std::string>& vec);
    
    /**
     * Convert JavaScript array to C++ vector
     */
    std::vector<std::string> JSArrayToVector(const JSValue& arr);
    
    // === Value Conversion ===
    
    /**
     * Create JS values from C++ types
     */
    static JSValue FromBool(bool value);
    static JSValue FromNumber(double value);
    static JSValue FromString(const std::string& value);
    static JSValue FromNull();
    static JSValue FromUndefined();
    
    /**
     * Extract C++ values from JS values
     */
    static bool ToBool(const JSValue& value);
    static double ToNumber(const JSValue& value);
    static std::string ToString(const JSValue& value);
    
    // === Script Execution ===
    
    /**
     * Execute JavaScript in global context
     */
    Result<JSValue> ExecuteGlobal(const std::string& script);
    
    /**
     * Call a JavaScript function with arguments
     */
    Result<JSValue> CallFunction(const std::string& function_name,
                                 const std::vector<JSValue>& args);
    
    /**
     * Evaluate JavaScript expression
     */
    Result<JSValue> Evaluate(const std::string& expression);

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

// ============================================================================
// PREDEFINED NATIVE FUNCTIONS (System bindings)
// ============================================================================

namespace NativeFunctions {

// System functions
JSValue SystemShutdown(const std::vector<JSValue>& args);
JSValue SystemReboot(const std::vector<JSValue>& args);
JSValue SystemGetInfo(const std::vector<JSValue>& args);

// Graphics functions
JSValue GraphicsSetResolution(const std::vector<JSValue>& args);
JSValue GraphicsGetDisplays(const std::vector<JSValue>& args);
JSValue GraphicsGetFrameRate(const std::vector<JSValue>& args);

// IPC functions
JSValue IPCCallService(const std::vector<JSValue>& args);
JSValue IPCRegisterService(const std::vector<JSValue>& args);
JSValue IPCGetServices(const std::vector<JSValue>& args);

// File functions
JSValue FileOpen(const std::vector<JSValue>& args);
JSValue FileRead(const std::vector<JSValue>& args);
JSValue FileWrite(const std::vector<JSValue>& args);
JSValue FileClose(const std::vector<JSValue>& args);

// Logging functions
JSValue LogInfo(const std::vector<JSValue>& args);
JSValue LogWarn(const std::vector<JSValue>& args);
JSValue LogError(const std::vector<JSValue>& args);

}  // namespace NativeFunctions

}  // namespace Gecko
}  // namespace GeckoOS
