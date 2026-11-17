// Rockhopper Runtime - Application Execution Environment
// File: rockhopper/src/runtime.rs
// Purpose: Manage application runtime environment

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use tracing::{info, debug, error};

pub struct Runtime {
    pub process_id: u32,
    pub environment: HashMap<String, String>,
    pub working_directory: String,
    pub resource_limits: ResourceLimits,
}

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub max_memory_mb: u32,
    pub max_cpu_percent: f32,
    pub max_open_files: u32,
    pub max_threads: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        ResourceLimits {
            max_memory_mb: 512,
            max_cpu_percent: 100.0,
            max_open_files: 1024,
            max_threads: 256,
        }
    }
}

impl Runtime {
    /// Create new runtime environment
    pub fn new(process_id: u32) -> Self {
        info!("Creating runtime environment for PID: {}", process_id);
        
        let mut environment = std::env::vars().collect::<HashMap<_, _>>();
        
        // Set process-specific environment variables
        environment.insert("GECKO_PID".to_string(), process_id.to_string());
        environment.insert("GECKO_VERSION".to_string(), "2.0.0".to_string());
        
        Runtime {
            process_id,
            environment,
            working_directory: std::env::current_dir()
                .ok()
                .and_then(|p| p.to_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "/".to_string()),
            resource_limits: ResourceLimits::default(),
        }
    }
    
    /// Set environment variable
    pub fn set_env(&mut self, key: String, value: String) {
        debug!("Setting environment variable: {}={}", key, value);
        self.environment.insert(key, value);
    }
    
    /// Get environment variable
    pub fn get_env(&self, key: &str) -> Option<String> {
        self.environment.get(key).cloned()
    }
    
    /// Set working directory
    pub fn set_working_dir(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        if Path::new(path).exists() {
            self.working_directory = path.to_string();
            debug!("Working directory set to: {}", path);
            Ok(())
        } else {
            error!("Working directory does not exist: {}", path);
            Err(format!("Path not found: {}", path).into())
        }
    }
    
    /// Apply resource limits
    pub fn apply_limits(&self) -> Result<(), Box<dyn Error>> {
        info!("Applying resource limits for PID: {}", self.process_id);
        
        #[cfg(unix)]
        {
            use nix::sys::resource::{setrlimit, Resource};
            // use nix::sys::resource::Resource;
            
            let mem_limit = (self.resource_limits.max_memory_mb as u64) * 1024 * 1024;
            setrlimit(Resource::RLIMIT_AS, mem_limit, mem_limit)?;
            debug!("Memory limit set to: {} MB", self.resource_limits.max_memory_mb);
            
            setrlimit(Resource::RLIMIT_NOFILE, self.resource_limits.max_open_files.into(), self.resource_limits.max_open_files.into())?;
            debug!("Open files limit set to: {}", self.resource_limits.max_open_files);
            
            setrlimit(Resource::RLIMIT_NPROC, self.resource_limits.max_threads.into(), self.resource_limits.max_threads.into())?;
            debug!("Thread limit set to: {}", self.resource_limits.max_threads);
        }
        
        Ok(())
    }
    
    /// Get runtime information
    pub fn get_info(&self) -> RuntimeInfo {
        RuntimeInfo {
            pid: self.process_id,
            cwd: self.working_directory.clone(),
            env_count: self.environment.len(),
            memory_limit: self.resource_limits.max_memory_mb,
            cpu_limit: self.resource_limits.max_cpu_percent,
        }
    }
}

#[derive(Debug)]
pub struct RuntimeInfo {
    pub pid: u32,
    pub cwd: String,
    pub env_count: usize,
    pub memory_limit: u32,
    pub cpu_limit: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_runtime_creation() {
        let runtime = Runtime::new(1234);
        assert_eq!(runtime.process_id, 1234);
        assert!(!runtime.environment.is_empty());
    }
    
    #[test]
    fn test_environment_variables() {
        let mut runtime = Runtime::new(5678);
        runtime.set_env("TEST_VAR".to_string(), "test_value".to_string());
        assert_eq!(runtime.get_env("TEST_VAR"), Some("test_value".to_string()));
    }
}
