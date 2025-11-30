//! Configuration Management
//!
//! Handles loading, parsing, and managing compositor configuration

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use anyhow::Result;

/// Main compositor configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub display: DisplayConfig,
    pub input: InputConfig,
    pub performance: PerformanceConfig,
    pub features: FeaturesConfig,
    pub logging: LoggingConfig,
}

/// Display configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayConfig {
    pub default_width: u32,
    pub default_height: u32,
    pub default_refresh_rate: u32,
    pub enable_vsync: bool,
    pub enable_damage_tracking: bool,
    pub scaling_filter: String, // "nearest", "linear", "cubic"
    pub color_space: String,     // "srgb", "linear"
}

/// Input configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    pub pointer_acceleration: f32,
    pub pointer_speed: f32,
    pub keyboard_repeat_delay: u32, // milliseconds
    pub keyboard_repeat_rate: u32,  // events per second
    pub touch_long_press_time: u32,
    pub enable_ime: bool,
    pub keyboard_layouts: Vec<String>,
}

/// Performance tuning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    pub target_fps: u32,
    pub max_frame_latency: u32, // milliseconds
    pub buffer_pool_size: usize,
    pub enable_buffer_pooling: bool,
    pub enable_gpu_acceleration: bool,
    pub texture_cache_size: usize,
}

/// Feature flags
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturesConfig {
    pub enable_server_side_decorations: bool,
    pub enable_layer_shell: bool,
    pub enable_xdg_shell: bool,
    pub enable_data_device: bool,
    pub enable_hot_plug: bool,
    pub enable_touch: bool,
    pub enable_ime: bool,
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String, // "trace", "debug", "info", "warn", "error"
    pub output: String, // "stdout", "stderr", "file"
    pub log_file: Option<PathBuf>,
    pub max_log_size: u64,
    pub color_output: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            display: DisplayConfig {
                default_width: 1920,
                default_height: 1080,
                default_refresh_rate: 60,
                enable_vsync: true,
                enable_damage_tracking: true,
                scaling_filter: "linear".to_string(),
                color_space: "srgb".to_string(),
            },
            input: InputConfig {
                pointer_acceleration: 1.0,
                pointer_speed: 1.0,
                keyboard_repeat_delay: 500,
                keyboard_repeat_rate: 30,
                touch_long_press_time: 500,
                enable_ime: true,
                keyboard_layouts: vec!["us".to_string()],
            },
            performance: PerformanceConfig {
                target_fps: 60,
                max_frame_latency: 16,
                buffer_pool_size: 4,
                enable_buffer_pooling: true,
                enable_gpu_acceleration: true,
                texture_cache_size: 100 * 1024 * 1024, // 100MB
            },
            features: FeaturesConfig {
                enable_server_side_decorations: true,
                enable_layer_shell: true,
                enable_xdg_shell: true,
                enable_data_device: true,
                enable_hot_plug: true,
                enable_touch: true,
                enable_ime: true,
            },
            logging: LoggingConfig {
                level: "info".to_string(),
                output: "stderr".to_string(),
                log_file: None,
                max_log_size: 10 * 1024 * 1024, // 10MB
                color_output: true,
            },
        }
    }
}

/// Configuration loader
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load configuration from file
    pub fn load(path: &Path) -> Result<Config> {
        let content = std::fs::read_to_string(path)?;
        
        match path.extension().and_then(|s| s.to_str()) {
            Some("toml") => {
                let config = toml::from_str(&content)?;
                Ok(config)
            }
            Some("json") => {
                let config = serde_json::from_str(&content)?;
                Ok(config)
            }
            _ => {
                tracing::warn!("Unknown config file format, using defaults");
                Ok(Config::default())
            }
        }
    }

    /// Load from default locations
    pub fn load_default() -> Config {
        let paths = vec![
            "/etc/geckoOS/compositor.toml".as_ref(),
            "~/.config/geckoOS/compositor.toml",
            "./compositor.toml",
        ];

        for path_str in paths {
            let expanded = shellexpand::tilde(path_str);
            let path = Path::new(expanded.as_ref());
            
            if path.exists() {
                match Self::load(path) {
                    Ok(config) => {
                        tracing::info!("Loaded config from: {}", path.display());
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load config from {}: {}", path.display(), e);
                    }
                }
            }
        }

        tracing::info!("Using default configuration");
        Config::default()
    }

    /// Save configuration to file
    pub fn save(config: &Config, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(config)?;
        std::fs::write(path, content)?;
        tracing::info!("Saved config to: {}", path.display());
        Ok(())
    }
}

impl Config {
    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        if self.display.default_width < 800 || self.display.default_width > 7680 {
            anyhow::bail!("Invalid display width: {}", self.display.default_width);
        }

        if self.display.default_height < 600 || self.display.default_height > 4320 {
            anyhow::bail!("Invalid display height: {}", self.display.default_height);
        }

        if self.display.default_refresh_rate < 30 || self.display.default_refresh_rate > 240 {
            anyhow::bail!("Invalid refresh rate: {}", self.display.default_refresh_rate);
        }

        if self.performance.target_fps < 30 || self.performance.target_fps > 240 {
            anyhow::bail!("Invalid target FPS: {}", self.performance.target_fps);
        }

        if self.input.keyboard_layouts.is_empty() {
            anyhow::bail!("At least one keyboard layout must be configured");
        }

        tracing::info!("Configuration validation passed");
        Ok(())
    }

    /// Get display mode description
    pub fn display_mode_string(&self) -> String {
        format!(
            "{}x{}@{}Hz",
            self.display.default_width,
            self.display.default_height,
            self.display.default_refresh_rate
        )
    }

    /// Check if feature is enabled
    pub fn is_feature_enabled(&self, feature: &str) -> bool {
        match feature {
            "ssd" => self.features.enable_server_side_decorations,
            "layer_shell" => self.features.enable_layer_shell,
            "xdg_shell" => self.features.enable_xdg_shell,
            "data_device" => self.features.enable_data_device,
            "hot_plug" => self.features.enable_hot_plug,
            "touch" => self.features.enable_touch,
            "ime" => self.features.enable_ime,
            _ => false,
        }
    }

    /// Get logging level
    pub fn log_level(&self) -> tracing::Level {
        match self.logging.level.to_lowercase().as_str() {
            "trace" => tracing::Level::TRACE,
            "debug" => tracing::Level::DEBUG,
            "info" => tracing::Level::INFO,
            "warn" => tracing::Level::WARN,
            "error" => tracing::Level::ERROR,
            _ => tracing::Level::INFO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_display_mode_string() {
        let config = Config::default();
        let mode = config.display_mode_string();
        assert_eq!(mode, "1920x1080@60Hz");
    }

    #[test]
    fn test_feature_check() {
        let config = Config::default();
        assert!(config.is_feature_enabled("ssd"));
        assert!(config.is_feature_enabled("hot_plug"));
        assert!(!config.is_feature_enabled("unknown"));
    }

    #[test]
    fn test_log_level() {
        let mut config = Config::default();
        config.logging.level = "debug".to_string();
        assert_eq!(config.log_level(), tracing::Level::DEBUG);
        
        config.logging.level = "error".to_string();
        assert_eq!(config.log_level(), tracing::Level::ERROR);
    }

    #[test]
    fn test_invalid_dimensions() {
        let mut config = Config::default();
        config.display.default_width = 100;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_fps() {
        let mut config = Config::default();
        config.performance.target_fps = 1000;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_empty_keyboard_layouts() {
        let mut config = Config::default();
        config.input.keyboard_layouts.clear();
        assert!(config.validate().is_err());
    }
}
