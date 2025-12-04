// Performance Layer - Optimization Engine
// File: performance/src/optimization.rs
// Purpose: Profile-based optimization and dynamic tuning

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{info, debug, warn};

use crate::PerformanceMetrics;

#[derive(Debug, Clone)]
pub struct OptimizationProfile {
    pub name: String,
    pub cpu_threshold: f32,
    pub memory_threshold: f32,
    pub cache_size: usize,
    pub batch_size: usize,
    pub prefetch_enabled: bool,
    pub gpu_acceleration: bool,
}

impl Default for OptimizationProfile {
    fn default() -> Self {
        OptimizationProfile {
            name: "default".to_string(),
            cpu_threshold: 80.0,
            memory_threshold: 85.0,
            cache_size: 1024,
            batch_size: 128,
            prefetch_enabled: true,
            gpu_acceleration: true,
        }
    }
}

pub struct OptimizationEngine {
    current_profile: Arc<RwLock<OptimizationProfile>>,
    profiles: Arc<RwLock<HashMap<String, OptimizationProfile>>>,
    metrics: Arc<RwLock<PerformanceMetrics>>,
}

impl OptimizationEngine {
    /// Create new optimization engine
    pub async fn new() -> Self {
        info!("Creating optimization engine");
        
        let mut profiles = HashMap::new();
        
        // Power profile
        profiles.insert("power".to_string(), OptimizationProfile {
            name: "power".to_string(),
            cpu_threshold: 50.0,
            memory_threshold: 60.0,
            cache_size: 512,
            batch_size: 64,
            prefetch_enabled: false,
            gpu_acceleration: false,
        });
        
        // Balanced profile
        profiles.insert("balanced".to_string(), OptimizationProfile::default());
        
        // Performance profile
        profiles.insert("performance".to_string(), OptimizationProfile {
            name: "performance".to_string(),
            cpu_threshold: 95.0,
            memory_threshold: 95.0,
            cache_size: 4096,
            batch_size: 256,
            prefetch_enabled: true,
            gpu_acceleration: true,
        });
        
        OptimizationEngine {
            current_profile: Arc::new(RwLock::new(OptimizationProfile::default())),
            profiles: Arc::new(RwLock::new(profiles)),
            metrics: Arc::new(RwLock::new(PerformanceMetrics::default())),
        }
    }
    
    /// Set optimization profile
    pub fn set_profile(&self, name: &str) -> Result<(), String> {
        let profiles = self.profiles.read().unwrap();
        
        if let Some(profile) = profiles.get(name) {
            let mut current = self.current_profile.write().unwrap();
            *current = profile.clone();
            info!("Switched to profile: {}", name);
            Ok(())
        } else {
            Err(format!("Profile not found: {}", name))
        }
    }
    
    /// Get available profiles
    pub fn list_profiles(&self) -> Vec<String> {
        self.profiles.read().unwrap().keys().cloned().collect()
    }
    
    /// Update metrics
    pub fn update_metrics(&self, metrics: PerformanceMetrics) {
        let mut m = self.metrics.write().unwrap();
        *m = metrics;
        
        debug!("Metrics updated - CPU: {:.1}%, Mem: {:.1}%, FPS: {}", 
            m.cpu_utilization, m.memory_usage_mb, m.fps);
        
        // Check thresholds and auto-tune
        self.auto_tune(&m);
    }
    
    /// Get current metrics
    pub fn get_metrics(&self) -> PerformanceMetrics {
        self.metrics.read().unwrap().clone()
    }
    
    /// Automatic tuning based on metrics
    fn auto_tune(&self, metrics: &PerformanceMetrics) {
        let profile = self.current_profile.read().unwrap();
        
        if metrics.cpu_utilization > profile.cpu_threshold {
            warn!("High CPU usage: {:.1}%", metrics.cpu_utilization);
            self.reduce_workload();
        }
        
        if metrics.memory_usage_mb > profile.memory_threshold as u64 {
            warn!("High memory usage: {:.1}%", metrics.memory_usage_mb);
            self.optimize_memory();
        }
        
        if metrics.cache_hit_rate < 70.0 {
            debug!("Low cache hit rate: {:.1}%", metrics.cache_hit_rate);
            self.increase_cache_size();
        }
        
        if metrics.fps < 30.0 {
            warn!("Low frame rate: {} FPS", metrics.fps);
            self.enable_gpu_acceleration();
        }
    }
    
    /// Reduce workload
    fn reduce_workload(&self) {
        let mut profile = self.current_profile.write().unwrap();
        profile.batch_size = (profile.batch_size / 2).max(32);
        profile.prefetch_enabled = false;
        debug!("Workload reduced, batch size: {}", profile.batch_size);
    }
    
    /// Optimize memory usage
    fn optimize_memory(&self) {
        let mut profile = self.current_profile.write().unwrap();
        profile.cache_size = (profile.cache_size / 2).max(256);
        debug!("Memory optimized, cache size: {}", profile.cache_size);
    }
    
    /// Increase cache size
    fn increase_cache_size(&self) {
        let mut profile = self.current_profile.write().unwrap();
        profile.cache_size = (profile.cache_size * 2).min(16384);
        debug!("Cache increased to: {}", profile.cache_size);
    }
    
    /// Enable GPU acceleration
    fn enable_gpu_acceleration(&self) {
        let mut profile = self.current_profile.write().unwrap();
        if !profile.gpu_acceleration {
            profile.gpu_acceleration = true;
            info!("GPU acceleration enabled");
        }
    }
    
    /// Get optimization recommendations
    pub fn get_recommendations(&self) -> Vec<String> {
        let mut recommendations = Vec::new();
        let metrics = self.metrics.read().unwrap();
        
        if metrics.cpu_utilization > 80.0 {
            recommendations.push("Consider reducing batch size".to_string());
        }
        
        if metrics.memory_usage_mb > 85.0 as u64 {
            recommendations.push("Increase cache eviction frequency".to_string());
        }
        
        if metrics.fps < 30.0 {
            recommendations.push("Enable GPU acceleration".to_string());
        }
        
        if metrics.cache_hit_rate < 50.0 {
            recommendations.push("Increase cache size or adjust prefetching".to_string());
        }
        
        recommendations
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
    
//     #[test]
//     fn test_optimization_engine_creation() {
//         let engine = OptimizationEngine::new();
//         let profiles = engine.list_profiles();
//         assert!(profiles.contains(&"power".to_string()));
//         assert!(profiles.contains(&"balanced".to_string()));
//         assert!(profiles.contains(&"performance".to_string()));
//     }
    
//     #[test]
//     fn test_profile_switching() {
//         let engine = OptimizationEngine::new();
//         assert!(engine.set_profile("performance").is_ok());
        
//         let profile = engine.current_profile.read().unwrap();
//         assert_eq!(profile.name, "performance");
//         assert!(profile.gpu_acceleration);
//     }
    
//     #[test]
//     fn test_metrics_update() {
//         let engine = OptimizationEngine::new();
//         let metrics = PerformanceMetrics {
//             cpu_utilization: 75.0,
//             memory_usage: 80.0,
//             frame_time_ms: 16.67,
//             fps: 60,
//             cache_hit_rate: 95.0,
//             gpu_utilization: 85.0,
//         };
        
//         engine.update_metrics(metrics);
//         let retrieved = engine.get_metrics();
//         assert_eq!(retrieved.cpu_utilization, 75.0);
//     }
// }
