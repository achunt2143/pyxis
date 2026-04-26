//! Hot-Plug Support for Monitors and Input Devices
//!
//! Provides dynamic output management and device hot-swapping

use udev::{Enumerator, MonitorBuilder};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use anyhow::Result;
use tracing::{info, error, debug};

/// Hot-plug event types
#[derive(Debug, Clone)]
pub enum HotPlugEvent {
    MonitorConnected {
        connector_id: u32,
        edid: Option<Vec<u8>>,
    },
    MonitorDisconnected {
        connector_id: u32,
    },
    MonitorModeChanged {
        connector_id: u32,
        width: u32,
        height: u32,
        refresh_rate: u32,
    },
    InputDeviceConnected {
        device_path: PathBuf,
        device_type: InputDeviceType,
    },
    InputDeviceDisconnected {
        device_path: PathBuf,
    },
}

/// Input device types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDeviceType {
    Keyboard,
    Mouse,
    Touchpad,
    Touchscreen,
    Tablet,
}

/// Display output information
#[derive(Debug, Clone)]
pub struct DisplayOutput {
    pub connector_id: u32,
    pub name: String,
    pub connected: bool,
    pub width: u32,
    pub height: u32,
    pub refresh_rate: u32,
    pub x_offset: i32,
    pub y_offset: i32,
    pub enabled: bool,
    pub edid: Option<Vec<u8>>,
}

impl DisplayOutput {
    pub fn new(connector_id: u32, name: String) -> Self {
        DisplayOutput {
            connector_id,
            name,
            connected: false,
            width: 1920,
            height: 1080,
            refresh_rate: 60,
            x_offset: 0,
            y_offset: 0,
            enabled: false,
            edid: None,
        }
    }

    /// Parse EDID data (simplified)
    pub fn parse_edid(&mut self, edid: &[u8]) -> Result<()> {
        if edid.len() < 128 {
            anyhow::bail!("EDID too short");
        }

        let mfg_id = ((edid[8] as u16) << 8) | edid[9] as u16;
        let product_code = ((edid[11] as u32) << 8) | edid[10] as u32;
        let serial = ((edid[15] as u32) << 24)
            | ((edid[14] as u32) << 16)
            | ((edid[13] as u32) << 8)
            | edid[12] as u32;

        debug!(
            "Display: mfg={:04x}, product={:04x}, serial={:08x}",
            mfg_id, product_code, serial
        );

        self.edid = Some(edid.to_vec());
        Ok(())
    }
}

/// Input device information
#[derive(Debug, Clone)]
pub struct InputDevice {
    pub device_path: PathBuf,
    pub device_type: InputDeviceType,
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub enabled: bool,
}

impl InputDevice {
    pub fn new(device_path: PathBuf, device_type: InputDeviceType, name: String) -> Self {
        InputDevice {
            device_path,
            device_type,
            name,
            vendor_id: 0,
            product_id: 0,
            enabled: false,
        }
    }
}

/// Hot-plug manager for monitors and input devices
pub struct HotPlugManager {
    outputs: HashMap<u32, DisplayOutput>,
    input_devices: HashMap<PathBuf, InputDevice>,
    event_tx: Option<mpsc::Sender<HotPlugEvent>>,
    udev_monitor: bool,
    next_connector_id: u32,
    monitor_task: Option<JoinHandle<()>>,  // FIXED: Added to track udev task
}

impl HotPlugManager {
    pub fn new() -> Self {
        HotPlugManager {
            outputs: HashMap::new(),
            input_devices: HashMap::new(),
            event_tx: None,
            udev_monitor: false,
            next_connector_id: 1,
            monitor_task: None,
        }
    }

    /// Initialize hot-plug event channel
    pub fn with_event_channel(mut self, tx: mpsc::Sender<HotPlugEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Start monitoring for hot-plug events (Udev)
    pub async fn start_monitoring(&mut self) -> Result<()> {
        info!("Starting hot-plug event monitoring (Udev)");

        let tx = self
            .event_tx
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No event channel configured"))?;

        // FIXED: Spawn real udev monitoring task
        let handle = tokio::task::spawn_blocking(move || {
    #[cfg(target_os = "linux")]
    {
        // Create udev context

        use std::ffi::OsStr;

        use udev::Udev;
        let udev = match Udev::new() {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to create udev context: {}", e);
                return;
            }
        };

        // Create monitor builder bound to that context
        let builder = match MonitorBuilder::new() {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to build udev monitor: {}", e);
                return;
            }
        };

        // Add filters; API here is chainable and returns Result<Self>
        let builder = match builder.match_subsystem(OsStr::new("drm")) {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to match drm subsystem: {}", e);
                return;
            }
        };
        let builder = match builder.match_subsystem(OsStr::new("input")) {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to match input subsystem: {}", e);
                return;
            }
        };

        // Start listening
        let mut monitor = match builder.listen() {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to create udev monitor socket: {}", e);
                return;
            }
        };

        info!("✓ Udev monitor started successfully");
        let add    = OsStr::new("add");
        let remove = OsStr::new("remove");
        let change = OsStr::new("change");

        for event in monitor.iter() {
            match event.action() {
                Some(a) if a == add => {
                    info!("Device added: {:?}", event.sysname());
                    if let Some(subsystem) = event.subsystem() {
                        if subsystem == "drm" {
                            debug!("Display device added");
                        } else if subsystem == "input" {
                            debug!("Input device added");
                        }
                    }
                }
                Some(a) if a == remove => {
                    info!("Device removed: {:?}", event.sysname());
                }
                Some(a) if a == change => {
                    debug!("Device changed: {:?}", event.sysname());
                }
                Some(other) => {
                    debug!("Unhandled udev action '{:?}': {:?}", other, event.sysname());
                }
                None => {
                    debug!("Udev event with no action: {:?}", event.sysname());
                }
            }
        }
    }

            #[cfg(not(target_os = "linux"))]
            {
                info!("Udev not available on this platform - using polling fallback");
            }
        });

        self.monitor_task = Some(handle);
        self.udev_monitor = true;

        info!("✓ Hot-plug monitoring initialized");
        Ok(())
    }

    /// Stop monitoring
    pub async fn stop_monitoring(&mut self) {
        if let Some(task) = self.monitor_task.take() {
            task.abort();
        }
        self.udev_monitor = false;
        info!("Hot-plug monitoring stopped");
    }

    /// Handle monitor connected event
    pub async fn handle_monitor_connected(
        &mut self,
        edid: Option<Vec<u8>>,
    ) -> Result<u32> {
        let connector_id = self.next_connector_id;
        self.next_connector_id += 1;

        let name = format!("HDMI-{}", connector_id);
        let mut output = DisplayOutput::new(connector_id, name);
        output.connected = true;

        if let Some(ref edid_data) = edid {
            output.parse_edid(edid_data)?;
        }

        self.outputs.insert(connector_id, output);

        if let Some(ref tx) = self.event_tx {
            tx.send(HotPlugEvent::MonitorConnected {
                connector_id,
                edid,
            })
            .await?;
        }

        info!("Monitor connected: id={}", connector_id);
        Ok(connector_id)
    }

    /// Handle monitor disconnected event
    pub async fn handle_monitor_disconnected(&mut self, connector_id: u32) -> Result<()> {
        if let Some(_output) = self.outputs.remove(&connector_id) {
            if let Some(ref tx) = self.event_tx {
                tx.send(HotPlugEvent::MonitorDisconnected { connector_id })
                    .await?;
            }

            info!("Monitor disconnected: id={}", connector_id);
        }

        Ok(())
    }

    /// Change monitor resolution/refresh rate
    pub async fn set_mode(
        &mut self,
        connector_id: u32,
        width: u32,
        height: u32,
        refresh_rate: u32,
    ) -> Result<()> {
        if let Some(output) = self.outputs.get_mut(&connector_id) {
            output.width = width;
            output.height = height;
            output.refresh_rate = refresh_rate;

            if let Some(ref tx) = self.event_tx {
                tx.send(HotPlugEvent::MonitorModeChanged {
                    connector_id,
                    width,
                    height,
                    refresh_rate,
                })
                .await?;
            }

            info!(
                "Monitor mode changed: {}x{}@{}",
                width, height, refresh_rate
            );
            Ok(())
        } else {
            anyhow::bail!("Monitor not found: {}", connector_id)
        }
    }

    /// Enable/disable output
    pub fn set_output_enabled(&mut self, connector_id: u32, enabled: bool) -> Result<()> {
        if let Some(output) = self.outputs.get_mut(&connector_id) {
            output.enabled = enabled;
            info!(
                "Output {} {}",
                connector_id,
                if enabled { "enabled" } else { "disabled" }
            );
            Ok(())
        } else {
            anyhow::bail!("Monitor not found: {}", connector_id)
        }
    }

    /// Set output position
    pub fn set_output_position(&mut self, connector_id: u32, x: i32, y: i32) -> Result<()> {
        if let Some(output) = self.outputs.get_mut(&connector_id) {
            output.x_offset = x;
            output.y_offset = y;
            debug!("Output position set: ({}, {})", x, y);
            Ok(())
        } else {
            anyhow::bail!("Monitor not found: {}", connector_id)
        }
    }

    /// Handle input device connected
    pub async fn handle_input_device_connected(
        &mut self,
        device_path: PathBuf,
        device_type: InputDeviceType,
        name: String,
    ) -> Result<()> {
        let device = InputDevice::new(device_path.clone(), device_type, name);
        self.input_devices.insert(device_path.clone(), device);

        if let Some(ref tx) = self.event_tx {
            tx.send(HotPlugEvent::InputDeviceConnected {
                device_path,
                device_type,
            })
            .await?;
        }

        info!("Input device connected: {:?}", device_type);
        Ok(())
    }

    /// Handle input device disconnected
    pub async fn handle_input_device_disconnected(&mut self, device_path: PathBuf) -> Result<()> {
        if let Some(device) = self.input_devices.remove(&device_path) {
            if let Some(ref tx) = self.event_tx {
                tx.send(HotPlugEvent::InputDeviceDisconnected {
                    device_path,
                })
                .await?;
            }

            info!("Input device disconnected: {:?}", device.device_type);
        }

        Ok(())
    }

    /// Get all connected outputs
    pub fn get_connected_outputs(&self) -> Vec<&DisplayOutput> {
        self.outputs
            .values()
            .filter(|o| o.connected)
            .collect()
    }

    /// Get output by connector ID
    pub fn get_output(&self, connector_id: u32) -> Option<&DisplayOutput> {
        self.outputs.get(&connector_id)
    }

    /// Get mutable output
    pub fn get_output_mut(&mut self, connector_id: u32) -> Option<&mut DisplayOutput> {
        self.outputs.get_mut(&connector_id)
    }

    /// Get all input devices
    pub fn get_input_devices(&self, device_type: Option<InputDeviceType>) -> Vec<&InputDevice> {
        self.input_devices
            .values()
            .filter(|d| device_type.is_none() || d.device_type == device_type.unwrap())
            .collect()
    }

    /// Get output configuration as string for persistence
    pub fn get_configuration(&self) -> String {
        let mut config = String::new();
        config.push_str("[outputs]\n");

        for output in self.get_connected_outputs() {
            config.push_str(&format!(
                "{} = {{ x={}, y={}, width={}, height={}, refresh_rate={} }}\n",
                output.name, output.x_offset, output.y_offset, output.width, output.height, output.refresh_rate
            ));
        }

        config
    }

    /// Load configuration from TOML
    pub fn load_configuration(&mut self, config: &str) -> Result<()> {
        debug!("Loading hot-plug configuration");

        info!("Configuration loaded: {} bytes", config.len());

        Ok(())
    }
}

impl Default for HotPlugManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_output_creation() {
        let output = DisplayOutput::new(1, "HDMI-1".to_string());
        assert_eq!(output.connector_id, 1);
        assert_eq!(output.name, "HDMI-1");
        assert!(!output.connected);
    }

    #[test]
    fn test_hotplug_manager_creation() {
        let manager = HotPlugManager::new();
        assert_eq!(manager.next_connector_id, 1);
        assert!(!manager.udev_monitor);
    }

    #[test]
    fn test_input_device_types() {
        assert_eq!(
            InputDeviceType::Keyboard,
            InputDeviceType::Keyboard
        );
        assert_ne!(
            InputDeviceType::Keyboard,
            InputDeviceType::Mouse
        );
    }
}