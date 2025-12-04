pub mod rendering_pipeline;
pub mod gpu_acceleration;
pub mod performance_monitor;
pub mod caching;
pub mod optimization;
pub mod echo_ipc;

pub use rendering_pipeline::RenderingPipeline;
pub use gpu_acceleration::GPUAccelerator;
pub use performance_monitor::PerformanceMonitor;
pub use caching::LRUCache;
pub use optimization::OptimizationEngine;
pub use echo_ipc::KratosEchoService;

use anyhow::Result;
use std::{sync::Arc};
use parking_lot::RwLock;
use tracing;

/// Performance optimization layer for GeckoOS
/// Handles rendering pipeline optimization, GPU acceleration, and system tuning
pub struct PerformanceLayer<T> {
    rendering_pipeline: Arc<RenderingPipeline>,
    gpu_accelerator: Arc<GPUAccelerator>,
    performance_monitor: Arc<PerformanceMonitor>,
    cache_manager: Arc<LRUCache<T>>,
    optimization_engine: Arc<OptimizationEngine>,
    state: Arc<RwLock<PerformanceState>>,
}

#[derive(Debug, Clone)]
pub struct PerformanceState {
    pub gpu_enabled: bool,
    pub pipeline_optimized: bool,
    pub fps_target: u32,
    pub current_fps: f64,
    pub memory_usage_mb: u64,
    pub gpu_utilization: f32,
}

/// Configuration for performance layer
#[derive(Debug, Clone)]
pub struct PerformanceConfig {
    pub enable_gpu: bool,
    pub gpu_backend: GPUBackend,
    pub target_fps: u32,
    pub vsync_enabled: bool,
    pub cache_size_mb: u64,
    pub buffer_pool_size: usize,
    pub thread_count: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum GPUBackend {
    Vulkan,
    Metal,
    DirectX12,
    WebGPU,
    Auto,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        PerformanceConfig {
            enable_gpu: true,
            gpu_backend: GPUBackend::Auto,
            target_fps: 60,
            vsync_enabled: true,
            cache_size_mb: 256,
            buffer_pool_size: 1024,
            thread_count: num_cpus::get(),
        }
    }
}

impl<T: Clone> PerformanceLayer<T> {
    /// Initialize performance layer
    pub async fn new(config: PerformanceConfig) -> Result<Self> {
        tracing::info!("Initializing Performance Layer");

        let rendering_pipeline = Arc::new(RenderingPipeline::new(&config).await?);
        let gpu_accelerator = Arc::new(GPUAccelerator::new(&config).await?);
        let performance_monitor = Arc::new(PerformanceMonitor::new(config.buffer_pool_size).await);
        let cache_manager = Arc::new(LRUCache::new(config.cache_size_mb as usize).await.unwrap());
        let optimization_engine = Arc::new(OptimizationEngine::new().await);

        let state = Arc::new(RwLock::new(PerformanceState {
            gpu_enabled: config.enable_gpu,
            pipeline_optimized: false,
            fps_target: config.target_fps,
            current_fps: 0.0,
            memory_usage_mb: 0,
            gpu_utilization: 0.0,
        }));

        let layer = PerformanceLayer {
            rendering_pipeline,
            gpu_accelerator,
            performance_monitor,
            cache_manager,
            optimization_engine,
            state,
        };

        tracing::info!("Performance Layer initialized successfully");
        Ok(layer)
    }

    /// Optimize rendering pipeline
    pub async fn optimize_pipeline(&self) -> Result<()> {
        tracing::info!("Optimizing rendering pipeline");
        // Analyze current state
        let metrics = self.optimization_engine.get_metrics();

        // Optimize based on metrics
        self.rendering_pipeline.optimize(&metrics).await?;

        // Mark as optimized
        let mut state = self.state.write();
        state.pipeline_optimized = true;

        Ok(())
    }

    /// Enable GPU acceleration
    pub async fn enable_gpu(&self) -> Result<()> {
        tracing::info!("Enabling GPU acceleration");

        self.gpu_accelerator.initialize().await?;

        let mut state = self.state.write();
        state.gpu_enabled = true;

        Ok(())
    }

    /// Get current performance metrics
    pub async fn get_performance_metrics(&self) -> Result<PerformanceMetrics> {
        let mut state = self.state.write();
        
        let metrics = self.optimization_engine.get_metrics();
        state.current_fps = metrics.fps;
        state.memory_usage_mb = metrics.memory_usage_mb;
        state.gpu_utilization = metrics.gpu_utilization;

        Ok(metrics)
    }

    /// Get rendering pipeline
    pub fn rendering_pipeline(&self) -> Arc<RenderingPipeline> {
        self.rendering_pipeline.clone()
    }

    /// Get GPU accelerator
    pub fn gpu_accelerator(&self) -> Arc<GPUAccelerator> {
        self.gpu_accelerator.clone()
    }

    /// Get performance monitor
    pub fn performance_monitor(&self) -> Arc<PerformanceMonitor> {
        self.performance_monitor.clone()
    }

    /// Get cache manager
    pub fn cache_manager(&self) -> Arc<LRUCache<T>> {
        self.cache_manager.clone()
    }

    /// Get optimization engine
    pub fn optimization_engine(&self) -> Arc<OptimizationEngine> {
        self.optimization_engine.clone()
    }

    /// Get state
    pub fn state(&self) -> Arc<RwLock<PerformanceState>> {
        self.state.clone()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PerformanceMetrics {
    pub fps: f64,
    pub frame_time_ms: f64,
    pub memory_usage_mb: u64,
    pub gpu_utilization: f32,
    pub cpu_utilization: f32,
    pub cache_hit_rate: f32,
    pub render_passes: u32,
    pub draw_calls: u32,
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        PerformanceMetrics {
            fps: 0.0,
            frame_time_ms: 0.0,
            memory_usage_mb: 0,
            gpu_utilization: 0.0,
            cpu_utilization: 0.0,
            cache_hit_rate: 0.0,
            render_passes: 0,
            draw_calls: 0
        }
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[tokio::test]
//     async fn test_performance_layer_init() {
//         let config = PerformanceConfig::default();
//         let layer = PerformanceLayer::new(config).await.unwrap();
//         let state = layer.state.read();
//         assert_eq!(state.fps_target, 60);
//     }
// }
