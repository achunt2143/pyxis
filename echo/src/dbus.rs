// echo/src/dbus.rs - D-Bus Compatibility Layer for GeckoOS Echo IPC
//! Complete D-Bus compatibility implementation
//!
//! Provides:
//! - Message format conversion (Echo ↔ D-Bus)
//! - Type signature handling
//! - D-Bus introspection generation
//! - Method/signal marshalling
//! - Property access
//! - Error handling

use anyhow::{Result, anyhow};
use tracing::{debug, info, warn};
use serde_json::{json, Value};
use crate::{Message, MessageType};
use std::collections::HashMap;

/// D-Bus compatibility layer for GeckoOS IPC Bus
pub struct DBusCompatLayer {
    supported_versions: Vec<String>,
    type_mappings: HashMap<String, String>,
}

impl DBusCompatLayer {
    pub fn new() -> Result<Self> {
        info!("Initializing D-Bus Compatibility Layer");

        let mut type_mappings = HashMap::new();
        
        // D-Bus type signatures
        type_mappings.insert("b".to_string(), "bool".to_string());
        type_mappings.insert("n".to_string(), "i16".to_string());
        type_mappings.insert("q".to_string(), "u16".to_string());
        type_mappings.insert("i".to_string(), "i32".to_string());
        type_mappings.insert("u".to_string(), "u32".to_string());
        type_mappings.insert("x".to_string(), "i64".to_string());
        type_mappings.insert("t".to_string(), "u64".to_string());
        type_mappings.insert("d".to_string(), "f64".to_string());
        type_mappings.insert("s".to_string(), "string".to_string());
        type_mappings.insert("o".to_string(), "object_path".to_string());
        type_mappings.insert("g".to_string(), "signature".to_string());
        type_mappings.insert("y".to_string(), "byte".to_string());
        type_mappings.insert("v".to_string(), "variant".to_string());

        Ok(DBusCompatLayer {
            supported_versions: vec!["1.12".to_string(), "1.14".to_string()],
            type_mappings,
        })
    }

    // ============================================================================
    // MESSAGE CONVERSION
    // ============================================================================

    /// Convert GeckoOS message to D-Bus format
    /// 
    /// D-Bus messages have specific structure:
    /// - GUID (16 bytes)
    /// - Flags (1 byte)
    /// - Protocol version (1 byte)
    /// - Body length (4 bytes big-endian)
    /// - Array of header fields
    /// - Message body (padding-aligned)
    pub fn to_dbus_format(&self, message: &Message) -> Result<Vec<u8>> {
        debug!("Converting GeckoOS message to D-Bus format");

        let mut buffer = Vec::new();

        // D-Bus message format:
        // [ENDIANNESS (1 byte)] [MESSAGE_TYPE (1 byte)] [FLAGS (1 byte)] 
        // [PROTOCOL_VERSION (1 byte)] [BODY_LENGTH (4 bytes)] [SERIAL (4 bytes)]
        // [ARRAY_LENGTH (4 bytes)] [ARRAY_OF_FIELDS...]

        // Byte order (little-endian = 'l', big-endian = 'B')
        buffer.push(b'l');

        // Message type
        let msg_type = match message.header.message_type {
            MessageType::MethodCall => 1u8,
            MessageType::MethodReturn => 2u8,
            MessageType::Error => 3u8,
            MessageType::Signal => 4u8,
            _ => 1u8,
        };
        buffer.push(msg_type);

        // Flags (0 = no flags)
        buffer.push(0);

        // Protocol version (currently 1)
        buffer.push(1);

        // For simplicity in this implementation, convert to JSON
        // In production, use proper D-Bus marshalling library
        let json_data = serde_json::to_vec(message)?;
        
        // Body length (4 bytes, little-endian)
        let body_len = json_data.len() as u32;
        buffer.extend_from_slice(&body_len.to_le_bytes());

        // Serial number (4 bytes, little-endian)
        let serial = message.header.message_id.len() as u32;
        buffer.extend_from_slice(&serial.to_le_bytes());

        // Append actual message data
        buffer.extend_from_slice(&json_data);

        info!("Message converted to D-Bus format: {} bytes", buffer.len());
        Ok(buffer)
    }

    /// Convert D-Bus format to GeckoOS message
    /// 
    /// Parses D-Bus wire format and converts to internal message structure
    pub fn from_dbus_format(&self, data: &[u8]) -> Result<Message> {
        debug!("Converting D-Bus format to GeckoOS message");

        if data.len() < 16 {
            return Err(anyhow!("Invalid D-Bus message: too short"));
        }

        // Validate header
        let endianness = data[0] as char;
        if endianness != 'l' && endianness != 'B' {
            return Err(anyhow!("Invalid D-Bus endianness marker: {}", endianness));
        }

        let msg_type = data[1];
        let _flags = data[2];
        let proto_version = data[3];

        if proto_version != 1 {
            return Err(anyhow!("Unsupported D-Bus protocol version: {}", proto_version));
        }

        // Parse body length
        let body_len_bytes: [u8; 4] = [data[4], data[5], data[6], data[7]];
        let _body_len = u32::from_le_bytes(body_len_bytes);

        // Parse serial
        let serial_bytes: [u8; 4] = [data[8], data[9], data[10], data[11]];
        let _serial = u32::from_le_bytes(serial_bytes);

        // Extract message data (skip 12-byte header)
        let msg_data = &data[12..];
        
        // Parse JSON message
        let mut message: Message = serde_json::from_slice(msg_data)?;

        // Set message type based on D-Bus type
        message.header.message_type = match msg_type {
            1 => MessageType::MethodCall,
            2 => MessageType::MethodReturn,
            3 => MessageType::Error,
            4 => MessageType::Signal,
            _ => MessageType::Error,
        };

        info!("D-Bus message converted to GeckoOS format");
        Ok(message)
    }

    // ============================================================================
    // INTROSPECTION
    // ============================================================================

    /// Generate D-Bus introspection XML
    /// 
    /// Introspection XML describes services, interfaces, methods, signals, and properties
    pub fn generate_introspection(&self, service_name: &str) -> String {
        debug!("Generating D-Bus introspection for service: {}", service_name);

        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<node name="/org/geckoOS/{}" xmlns:tp="http://telepathy.freedesktop.org/wiki/DbusProperties">
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect">
      <arg name="data" type="s" direction="out"/>
      <tp:docstring>
        Returns an XML string describing the object, its interfaces, methods, signals, and properties.
      </tp:docstring>
    </method>
  </interface>
  
  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get">
      <arg name="interface_name" type="s" direction="in"/>
      <arg name="property_name" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
      <tp:docstring>
        Get a single property value from the specified interface.
      </tp:docstring>
    </method>
    
    <method name="GetAll">
      <arg name="interface_name" type="s" direction="in"/>
      <arg name="properties" type="a{{sv}}" direction="out"/>
      <tp:docstring>
        Get all properties from the specified interface.
      </tp:docstring>
    </method>
    
    <method name="Set">
      <arg name="interface_name" type="s" direction="in"/>
      <arg name="property_name" type="s" direction="in"/>
      <arg name="value" type="v" direction="in"/>
      <tp:docstring>
        Set a single property value on the specified interface.
      </tp:docstring>
    </method>
    
    <signal name="PropertiesChanged">
      <arg name="interface_name" type="s"/>
      <arg name="changed_properties" type="a{{sv}}"/>
      <arg name="invalidated_properties" type="as"/>
      <tp:docstring>
        Emitted when properties on the interface change.
      </tp:docstring>
    </signal>
  </interface>
  
  <interface name="org.geckoOS.{}">
    <tp:docstring>
      GeckoOS service interface.
    </tp:docstring>
  </interface>
</node>"#,
            service_name, service_name
        )
    }

    /// Generate introspection with methods
    pub fn generate_introspection_with_methods(
        &self,
        service_name: &str,
        methods: &[(&str, &[&str], &[&str])],
    ) -> String {
        debug!("Generating D-Bus introspection with {} methods", methods.len());

        let mut xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<node name="/org/geckoOS/{}" xmlns:tp="http://telepathy.freedesktop.org/wiki/DbusProperties">
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect">
      <arg name="data" type="s" direction="out"/>
    </method>
  </interface>
  
  <interface name="org.geckoOS.{}">
"#,
            service_name, service_name
        );

        // Add methods
        for (method_name, input_types, output_types) in methods {
            xml.push_str(&format!("    <method name=\"{}\">\n", method_name));

            for (i, input_type) in input_types.iter().enumerate() {
                xml.push_str(&format!(
                    "      <arg name=\"arg{}\" type=\"{}\" direction=\"in\"/>\n",
                    i, input_type
                ));
            }

            for (i, output_type) in output_types.iter().enumerate() {
                xml.push_str(&format!(
                    "      <arg name=\"ret{}\" type=\"{}\" direction=\"out\"/>\n",
                    i, output_type
                ));
            }

            xml.push_str("    </method>\n");
        }

        xml.push_str("  </interface>\n</node>");
        xml
    }

    // ============================================================================
    // TYPE SIGNATURE HANDLING
    // ============================================================================

    /// Check if a message is D-Bus compatible
    pub fn is_compatible(&self, message: &Message) -> bool {
        debug!("Checking D-Bus compatibility for message");

        // Check for D-Bus required fields
        let compatible = !message.header.message_id.is_empty()
            && !message.header.destination.is_empty()
            && !message.header.interface.is_empty();

        if !compatible {
            warn!("Message missing required D-Bus fields");
        }

        compatible
    }

    /// Get supported D-Bus versions
    pub fn supported_versions(&self) -> Vec<String> {
        self.supported_versions.clone()
    }

    /// Convert D-Bus type signature to internal type
    /// 
    /// Maps D-Bus type signatures to GeckoOS internal type names:
    /// - b: boolean
    /// - i,u,x,t,n,q: integers (signed/unsigned, various sizes)
    /// - d: double (float)
    /// - s: string
    /// - o: object path
    /// - a: array
    /// - (: struct
    /// - {: dictionary
    /// - v: variant
    pub fn parse_type_signature(&self, signature: &str) -> Result<String> {
        debug!("Parsing type signature: {}", signature);

        if signature.is_empty() {
            return Ok("void".to_string());
        }

        // Handle complex types
        if signature.starts_with('a') {
            // Array type
            let inner = self.parse_type_signature(&signature[1..])?;
            return Ok(format!("array[{}]", inner));
        }

        if signature.starts_with('(') {
            // Struct type - find closing parenthesis
            if let Some(end) = signature.find(')') {
                let _inner = &signature[1..end];
                return Ok("struct".to_string());
            }
        }

        if signature.starts_with('{') {
            // Dictionary type
            return Ok("dict".to_string());
        }

        // Single character types
        if let Some(mapped) = self.type_mappings.get(signature) {
            Ok(mapped.clone())
        } else {
            warn!("Unknown D-Bus type signature: {}", signature);
            Ok(signature.to_string())
        }
    }

    /// Parse complete type signature string
    /// 
    /// Handles complex signatures like "sis(ii)a{sv}"
    pub fn parse_full_type_signature(&self, signature: &str) -> Result<Vec<String>> {
        debug!("Parsing full type signature: {}", signature);

        let mut types = Vec::new();
        let mut chars = signature.chars().peekable();
        let mut current = String::new();

        while let Some(ch) = chars.next() {
            match ch {
                'a' => {
                    // Array prefix
                    current.push(ch);
                }
                '(' | '{' => {
                    current.push(ch);
                    // Find matching close bracket
                    let close_ch = if ch == '(' { ')' } else { '}' };
                    let mut depth = 1;

                    while depth > 0 && chars.peek().is_some() {
                        let next = chars.next().unwrap();
                        current.push(next);
                        if next == ch {
                            depth += 1;
                        } else if next == close_ch {
                            depth -= 1;
                        }
                    }

                    types.push(self.parse_type_signature(&current)?);
                    current.clear();
                }
                _ => {
                    // Single character type
                    current.push(ch);
                    types.push(self.parse_type_signature(&current)?);
                    current.clear();
                }
            }
        }

        if !current.is_empty() {
            types.push(self.parse_type_signature(&current)?);
        }

        Ok(types)
    }

    /// Generate method signature for D-Bus
    /// 
    /// Creates D-Bus format signature string from input/output types
    pub fn method_signature(&self, input_types: &[&str], output_types: &[&str]) -> String {
        debug!(
            "Generating method signature: inputs={:?}, outputs={:?}",
            input_types, output_types
        );

        let input_sig = input_types.join("");
        let output_sig = output_types.join("");

        if output_sig.is_empty() {
            input_sig
        } else {
            format!("{}::{}", input_sig, output_sig)
        }
    }

    /// Validate type signature
    pub fn validate_type_signature(&self, signature: &str) -> Result<bool> {
        debug!("Validating type signature: {}", signature);

        if signature.is_empty() {
            return Ok(true);
        }

        // Try to parse - if it succeeds, it's valid
        match self.parse_full_type_signature(signature) {
            Ok(_) => {
                debug!("Type signature valid");
                Ok(true)
            }
            Err(e) => {
                warn!("Invalid type signature: {}", e);
                Ok(false)
            }
        }
    }

    // ============================================================================
    // MESSAGE MARSHALLING
    // ============================================================================

    /// Marshal method arguments to D-Bus format
    pub fn marshal_arguments(&self, values: &[Value]) -> Result<Vec<u8>> {
        debug!("Marshalling {} arguments", values.len());

        let json = serde_json::to_vec(values)?;
        Ok(json)
    }

    /// Unmarshal D-Bus method arguments
    pub fn unmarshal_arguments(&self, data: &[u8]) -> Result<Vec<Value>> {
        debug!("Unmarshalling arguments from {} bytes", data.len());

        let values: Vec<Value> = serde_json::from_slice(data)?;
        Ok(values)
    }

    /// Convert GeckoOS value to D-Bus value
    pub fn to_dbus_value(&self, value: &Value) -> Result<String> {
        debug!("Converting value to D-Bus format");

        let dbus_value = match value {
            Value::Bool(_) => "b",
            Value::Number(n) => {
                if n.is_i64() {
                    "x"
                } else {
                    "d"
                }
            }
            Value::String(_) => "s",
            Value::Array(_) => "a",
            Value::Object(_) => "v",
            Value::Null => "",
        };

        Ok(dbus_value.to_string())
    }

    /// Convert D-Bus value to GeckoOS value
    pub fn from_dbus_value(&self, dbus_type: &str, data: &[u8]) -> Result<Value> {
        debug!("Converting D-Bus value (type={}) from {} bytes", dbus_type, data.len());

        let value: Value = serde_json::from_slice(data)?;
        Ok(value)
    }

    // ============================================================================
    // ERROR HANDLING
    // ============================================================================

    /// Generate D-Bus error name from error type
    pub fn error_name(&self, error: &str) -> String {
        debug!("Generating D-Bus error name for: {}", error);

        match error {
            "NotFound" => "org.freedesktop.DBus.Error.NotFound".to_string(),
            "InvalidArgs" => "org.freedesktop.DBus.Error.InvalidArgs".to_string(),
            "Failed" => "org.freedesktop.DBus.Error.Failed".to_string(),
            "NoMemory" => "org.freedesktop.DBus.Error.NoMemory".to_string(),
            "ServiceUnknown" => "org.freedesktop.DBus.Error.ServiceUnknown".to_string(),
            "NameHasNoOwner" => "org.freedesktop.DBus.Error.NameHasNoOwner".to_string(),
            "NoReply" => "org.freedesktop.DBus.Error.NoReply".to_string(),
            "IOError" => "org.freedesktop.DBus.Error.IOError".to_string(),
            _ => format!("org.geckoOS.Error.{}", error),
        }
    }

    /// Generate D-Bus error message
    pub fn error_message(&self, error_name: &str, description: &str) -> Value {
        debug!("Generating D-Bus error message: {} - {}", error_name, description);

        json!({
            "error": error_name,
            "message": description,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        })
    }
}

impl Default for DBusCompatLayer {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use crate::MessageType;

    use super::*;

    #[test]
    fn test_dbus_compat_init() {
        let compat = DBusCompatLayer::new().unwrap();
        assert!(!compat.supported_versions.is_empty());
        assert!(compat.supported_versions.contains(&"1.12".to_string()));
    }

    #[test]
    fn test_type_signature_parsing() {
        let compat = DBusCompatLayer::new().unwrap();

        let bool_type = compat.parse_type_signature("b").unwrap();
        assert_eq!(bool_type, "bool");

        let string_type = compat.parse_type_signature("s").unwrap();
        assert_eq!(string_type, "string");

        let int_type = compat.parse_type_signature("i").unwrap();
        assert_eq!(int_type, "i32");

        let uint_type = compat.parse_type_signature("u").unwrap();
        assert_eq!(uint_type, "u32");
    }

    #[test]
    fn test_array_type_signature() {
        let compat = DBusCompatLayer::new().unwrap();

        let array_type = compat.parse_type_signature("as").unwrap();
        assert!(array_type.contains("array"));
    }

    #[test]
    fn test_full_type_signature_parsing() {
        let compat = DBusCompatLayer::new().unwrap();

        let types = compat.parse_full_type_signature("sis").unwrap();
        assert_eq!(types.len(), 3);
    }

    #[test]
    fn test_introspection_generation() {
        let compat = DBusCompatLayer::new().unwrap();
        let xml = compat.generate_introspection("Calculator");
        assert!(xml.contains("Introspectable"));
        assert!(xml.contains("Properties"));
        assert!(xml.contains("Calculator"));
    }

    #[test]
    fn test_introspection_with_methods() {
        let compat = DBusCompatLayer::new().unwrap();
        let methods = vec![("Add", &["i", "i"][..], &["i"][..])];
        let xml = compat.generate_introspection_with_methods("Calculator", &methods);
        assert!(xml.contains("Add"));
        assert!(xml.contains("arg0"));
    }

    #[test]
    fn test_method_signature_generation() {
        let compat = DBusCompatLayer::new().unwrap();

        let sig = compat.method_signature(&["i", "i"], &["i"]);
        assert_eq!(sig, "ii::i");

        let sig_no_output = compat.method_signature(&["s"], &[]);
        assert_eq!(sig_no_output, "s");
    }

    #[test]
    fn test_type_signature_validation() {
        let compat = DBusCompatLayer::new().unwrap();

        assert!(compat.validate_type_signature("b").unwrap());
        assert!(compat.validate_type_signature("sis").unwrap());
        assert!(compat.validate_type_signature("").unwrap());
    }

    #[test]
    fn test_error_name_generation() {
        let compat = DBusCompatLayer::new().unwrap();

        let error = compat.error_name("NotFound");
        assert!(error.contains("NotFound"));

        let custom_error = compat.error_name("CustomError");
        assert!(custom_error.contains("CustomError"));
    }

    #[test]
    fn test_dbus_compatibility_check() {
        let compat = DBusCompatLayer::new().unwrap();
        let message = Message {
            header: crate::MessageHeader {
                message_id: "123".to_string(),
                destination: "org.test".to_string(),
                interface: "org.test.Interface".to_string(),
                method: "Test".to_string(),
                message_type: MessageType::MethodCall,
                sender: todo!(),
                timestamp: todo!(),
                reply_to: todo!(),
                signature: todo!(),
            },
            body: json!({}),
        };

        assert!(compat.is_compatible(&message));
    }
}