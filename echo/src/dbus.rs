use anyhow::Result;
use tracing;
use crate::Message;

/// D-Bus compatibility layer for GeckoOS IPC Bus
pub struct DBusCompatLayer {
    supported_versions: Vec<String>,
}

impl DBusCompatLayer {
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing D-Bus Compatibility Layer");

        Ok(DBusCompatLayer {
            supported_versions: vec!["1.12".to_string(), "1.14".to_string()],
        })
    }

    /// Convert GeckoOS message to D-Bus format
    pub fn to_dbus_format(&self, message: &Message) -> Result<Vec<u8>> {
        // In production, this would:
        // 1. Use D-Bus marshalling format
        // 2. Convert type signatures
        // 3. Handle array/struct serialization

        let dbus_msg = serde_json::to_vec(message)?;
        Ok(dbus_msg)
    }

    /// Convert D-Bus format to GeckoOS message
    pub fn from_dbus_format(&self, data: &[u8]) -> Result<Message> {
        // In production, this would:
        // 1. Parse D-Bus format
        // 2. Validate signatures
        // 3. Convert to internal format

        let message: Message = serde_json::from_slice(data)?;
        Ok(message)
    }

    /// Generate D-Bus introspection XML
    pub fn generate_introspection(&self, service_name: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<node name="/org/geckoOS/{}" xmlns:tp="http://telepathy.freedesktop.org/wiki/DbusProperties">
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect">
      <arg name="data" type="s" direction="out"/>
    </method>
  </interface>
  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get">
      <arg name="interface_name" type="s" direction="in"/>
      <arg name="property_name" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
    </method>
    <method name="GetAll">
      <arg name="interface_name" type="s" direction="in"/>
      <arg name="properties" type="a{{sv}}" direction="out"/>
    </method>
    <method name="Set">
      <arg name="interface_name" type="s" direction="in"/>
      <arg name="property_name" type="s" direction="in"/>
      <arg name="value" type="v" direction="in"/>
    </method>
  </interface>
  <interface name="org.geckoOS.{}">
  </interface>
</node>"#,
            service_name, service_name
        )
    }

    /// Check if a message is D-Bus compatible
    pub fn is_compatible(&self, message: &Message) -> bool {
        // Check for D-Bus required fields
        !message.header.message_id.is_empty()
            && !message.header.destination.is_empty()
            && !message.header.interface.is_empty()
    }

    /// Get supported D-Bus versions
    pub fn supported_versions(&self) -> Vec<String> {
        self.supported_versions.clone()
    }

    /// Convert D-Bus type signature to internal type
    pub fn parse_type_signature(&self, signature: &str) -> Result<String> {
        // D-Bus type signature to internal type mapping
        let type_map = [
            ("b", "bool"),
            ("n", "i16"),
            ("q", "u16"),
            ("i", "i32"),
            ("u", "u32"),
            ("x", "i64"),
            ("t", "u64"),
            ("d", "f64"),
            ("s", "string"),
            ("o", "object_path"),
            ("g", "signature"),
            ("y", "byte"),
            ("a", "array"),
            ("(", "struct"),
            ("v", "variant"),
            ("{", "dict"),
        ];

        for (sig, internal_type) in type_map.iter() {
            if signature.starts_with(sig) {
                return Ok(internal_type.to_string());
            }
        }

        Ok(signature.to_string())
    }

    /// Generate method signature for D-Bus
    pub fn method_signature(&self, input_types: &[&str], output_types: &[&str]) -> String {
        let input_sig = input_types.join("");
        let output_sig = output_types.join("");

        if output_sig.is_empty() {
            input_sig
        } else {
            format!("{}::{}", input_sig, output_sig)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dbus_compat_init() {
        let compat = DBusCompatLayer::new().unwrap();
        assert!(!compat.supported_versions.is_empty());
    }

    #[test]
    fn test_type_signature_parsing() {
        let compat = DBusCompatLayer::new().unwrap();
        
        let bool_type = compat.parse_type_signature("b").unwrap();
        assert_eq!(bool_type, "bool");

        let string_type = compat.parse_type_signature("s").unwrap();
        assert_eq!(string_type, "string");
    }

    #[test]
    fn test_introspection_generation() {
        let compat = DBusCompatLayer::new().unwrap();
        let xml = compat.generate_introspection("Calculator");
        assert!(xml.contains("Introspectable"));
    }
}
