use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing;
use uuid::Uuid;
use chrono::Utc;

use crate::{ServiceInterface, MethodDefinition, SignalDefinition, PropertyDefinition};

/// Service registry for tracking all services on the bus
pub struct ServiceRegistry {
    services: Arc<DashMap<String, ServiceInfo>>,
    name_to_id: Arc<DashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub service_id: String,
    pub service_name: String,
    pub interface: ServiceInterface,
    pub pid: Option<u32>,
    pub registered_at: String,
    pub owner_uid: Option<u32>,
    pub introspection: String,
}

impl ServiceRegistry {
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing Service Registry");

        Ok(ServiceRegistry {
            services: Arc::new(DashMap::new()),
            name_to_id: Arc::new(DashMap::new()),
        })
    }

    pub async fn register_service(
        &self,
        service_name: &str,
        interface: ServiceInterface,
    ) -> Result<String> {
        let service_id = Uuid::new_v4().to_string();

        let info = ServiceInfo {
            service_id: service_id.clone(),
            service_name: service_name.to_string(),
            interface: interface.clone(),
            pid: None,
            registered_at: Utc::now().to_rfc3339(),
            owner_uid: None,
            introspection: Self::generate_introspection(service_name, &interface),
        };

        self.services.insert(service_id.clone(), info);
        self.name_to_id.insert(service_name.to_string(), service_id.clone());

        tracing::info!("Service registered: {} ({})", service_name, service_id.clone());
        Ok(service_id)
    }

    pub async fn unregister_service(&self, service_id: &str) -> Result<()> {
        if let Some((_, info)) = self.services.remove(service_id) {
            self.name_to_id.remove(&info.service_name);

            tracing::info!("Service unregistered: {} ({})", info.service_name, service_id);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Service {} not found", service_id))
        }
    }

    pub fn remove_service_by_name(&self, service_name: &str) -> bool {
        if let Some((_, service_id)) = self.name_to_id.remove(service_name) {
            self.services.remove(&service_id);
            tracing::info!(
                "Removed external service from registry: {} ({})",
                service_name,
                service_id
            );
            true
        } else {
            false
        }
    }

    pub async fn get_service(&self, service_name: &str) -> Result<ServiceInfo> {
        if let Some(service_id) = self.name_to_id.get(service_name) {
            self.services
                .get(service_id.value())
                .map(|entry| entry.clone())
                .ok_or_else(|| anyhow::anyhow!("Service {} not found", service_name))
        } else {
            Err(anyhow::anyhow!("Service {} not registered", service_name))
        }
    }

    pub async fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        Ok(self
            .services
            .iter()
            .map(|entry| entry.value().clone())
            .collect())
    }

    pub async fn service_exists(&self, service_name: &str) -> Result<bool> {
        Ok(self.name_to_id.contains_key(service_name))
    }

    pub async fn service_count(&self) -> Result<usize> {
        Ok(self.services.len())
    }

    pub async fn get_service_methods(&self, service_name: &str) -> Result<Vec<MethodDefinition>> {
        let service = self.get_service(service_name).await?;
        Ok(service.interface.methods)
    }

    pub async fn get_service_signals(&self, service_name: &str) -> Result<Vec<SignalDefinition>> {
        let service = self.get_service(service_name).await?;
        Ok(service.interface.signals)
    }

    pub async fn get_service_properties(&self, service_name: &str) -> Result<Vec<PropertyDefinition>> {
        let service = self.get_service(service_name).await?;
        Ok(service.interface.properties)
    }

    pub async fn update_service_pid(&self, service_name: &str, pid: u32) -> Result<()> {
        if let Some(service_id) = self.name_to_id.get(service_name) {
            if let Some(mut service) = self.services.get_mut(service_id.value()) {
                service.pid = Some(pid);
                tracing::debug!("Updated service {} PID to {}", service_name, pid);
                Ok(())
            } else {
                Err(anyhow::anyhow!("Service {} not found", service_name))
            }
        } else {
            Err(anyhow::anyhow!("Service {} not registered", service_name))
        }
    }

    /// Generate D-Bus introspection XML for a service
    fn generate_introspection(service_name: &str, interface: &ServiceInterface) -> String {
        let mut xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<node name="{}" xmlns:tp="http://telepathy.freedesktop.org/wiki/DbusProperties">
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect">
      <arg name="data" type="s" direction="out"/>
    </method>
  </interface>
  <interface name="org.geckoOS.{}">
"#,
            service_name, interface.name
        );

        // Add methods
        for method in &interface.methods {
            xml.push_str(&format!("    <method name=\"{}\">\n", method.name));
            for arg in &method.input_args {
                xml.push_str(&format!(
                    "      <arg name=\"{}\" type=\"{}\" direction=\"in\"/>\n",
                    arg.name, arg.arg_type
                ));
            }
            for arg in &method.output_args {
                xml.push_str(&format!(
                    "      <arg name=\"{}\" type=\"{}\" direction=\"out\"/>\n",
                    arg.name, arg.arg_type
                ));
            }
            xml.push_str("    </method>\n");
        }

        // Add signals
        for signal in &interface.signals {
            xml.push_str(&format!("    <signal name=\"{}\">\n", signal.name));
            for arg in &signal.arguments {
                xml.push_str(&format!(
                    "      <arg name=\"{}\" type=\"{}\"/>\n",
                    arg.name, arg.arg_type
                ));
            }
            xml.push_str("    </signal>\n");
        }

        // Add properties
        for prop in &interface.properties {
            let access = match prop.access {
                crate::PropertyAccess::Read => "read",
                crate::PropertyAccess::Write => "write",
                crate::PropertyAccess::ReadWrite => "readwrite",
            };
            xml.push_str(&format!(
                "    <property name=\"{}\" type=\"{}\" access=\"{}\"/>\n",
                prop.name, prop.property_type, access
            ));
        }

        xml.push_str("  </interface>\n</node>\n");
        xml
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_init() {
        let registry = ServiceRegistry::new().unwrap();
        let count = registry.service_count().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_service_registration() {
        let registry = ServiceRegistry::new().unwrap();
        let interface = ServiceInterface {
            name: "Calculator".to_string(),
            methods: vec![],
            signals: vec![],
            properties: vec![],
        };

        let id = registry
            .register_service("org.geckoOS.calculator", interface)
            .await
            .unwrap();

        assert!(!id.is_empty());
        let count = registry.service_count().await.unwrap();
        assert_eq!(count, 1);
    }
}
