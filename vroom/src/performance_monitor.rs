// Performance Layer - Real-time Monitoring
// File: performance/src/performance_monitor.rs
// Purpose: Collect and analyze performance metrics

use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
use tracing::{info, debug};

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub timestamp: SystemTime,
    pub cpu_percent: f32,
    pub memory_mb: u32,
    pub frame_time_ms: f32,
    pub fps: u32,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub gpu_utilization: f32,
}

pub struct PerformanceMonitor {
    snapshots: Arc<RwLock<Vec<MetricsSnapshot>>>,
    max_history: usize,
    bottlenecks: Arc<RwLock<Vec<Bottleneck>>>,
}

#[derive(Debug, Clone)]
pub struct Bottleneck {
    pub component: String,
    pub severity: BottleneckSeverity,
    pub description: String,
    pub timestamp: SystemTime,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BottleneckSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl PerformanceMonitor {
    /// Create new performance monitor
    pub async fn new(max_history: usize) -> Self {
        info!("Creating performance monitor with {} history entries", max_history);
        
        PerformanceMonitor {
            snapshots: Arc::new(RwLock::new(Vec::new())),
            max_history,
            bottlenecks: Arc::new(RwLock::new(Vec::new())),
        }
    }
    
    /// Record metrics snapshot
    pub fn record(&self, snapshot: MetricsSnapshot) {
        let mut snapshots = self.snapshots.write().unwrap();
        
        // Check for bottlenecks
        self.detect_bottlenecks(&snapshot);
        
        snapshots.push(snapshot);
        
        // Maintain max history
        while snapshots.len() > self.max_history {
            snapshots.remove(0);
        }
        
        debug!("Metrics recorded, total snapshots: {}", snapshots.len());
    }
    
    /// Detect performance bottlenecks
    fn detect_bottlenecks(&self, snapshot: &MetricsSnapshot) {
        let mut bottlenecks = self.bottlenecks.write().unwrap();
        
        // CPU bottleneck
        if snapshot.cpu_percent > 90.0 {
            bottlenecks.push(Bottleneck {
                component: "CPU".to_string(),
                severity: BottleneckSeverity::Critical,
                description: format!("CPU usage at {:.1}%", snapshot.cpu_percent),
                timestamp: SystemTime::now(),
            });
        } else if snapshot.cpu_percent > 75.0 {
            bottlenecks.push(Bottleneck {
                component: "CPU".to_string(),
                severity: BottleneckSeverity::High,
                description: format!("High CPU usage: {:.1}%", snapshot.cpu_percent),
                timestamp: SystemTime::now(),
            });
        }
        
        // Memory bottleneck
        if snapshot.memory_mb > 400 {
            bottlenecks.push(Bottleneck {
                component: "Memory".to_string(),
                severity: BottleneckSeverity::High,
                description: format!("High memory usage: {} MB", snapshot.memory_mb),
                timestamp: SystemTime::now(),
            });
        }
        
        // Frame rate bottleneck
        if snapshot.fps < 30 {
            bottlenecks.push(Bottleneck {
                component: "GPU".to_string(),
                severity: BottleneckSeverity::Critical,
                description: format!("Low frame rate: {} FPS", snapshot.fps),
                timestamp: SystemTime::now(),
            });
        }
        
        // Keep only last 100 bottlenecks
        while bottlenecks.len() > 100 {
            bottlenecks.remove(0);
        }
    }
    
    /// Get average metrics over time window
    pub async fn get_average(&self, window: Duration) -> Option<MetricsSnapshot> {
        let snapshots = self.snapshots.read().unwrap();
        
        if snapshots.is_empty() {
            return None;
        }
        
        let now = SystemTime::now();
        let mut filtered = snapshots
            .iter()
            .filter(|s| {
                now.duration_since(s.timestamp)
                    .unwrap_or(Duration::from_secs(0))
                    < window
            })
            .collect::<Vec<_>>();
        
        if filtered.is_empty() {
            return None;
        }
        
        let count = filtered.len() as f32;
        
        Some(MetricsSnapshot {
            timestamp: SystemTime::now(),
            cpu_percent: filtered.iter().map(|s| s.cpu_percent).sum::<f32>() / count,
            memory_mb: (filtered.iter().map(|s| s.memory_mb as f32).sum::<f32>() / count) as u32,
            frame_time_ms: filtered.iter().map(|s| s.frame_time_ms).sum::<f32>() / count,
            fps: (filtered.iter().map(|s| s.fps as f32).sum::<f32>() / count) as u32,
            cache_hits: filtered.iter().map(|s| s.cache_hits).sum(),
            cache_misses: filtered.iter().map(|s| s.cache_misses).sum(),
            gpu_utilization: filtered.iter().map(|s| s.gpu_utilization).sum::<f32>() / count,
        })
    }
    
    /// Get current bottlenecks
    pub fn get_bottlenecks(&self) -> Vec<Bottleneck> {
        self.bottlenecks.read().unwrap().clone()
    }
    
    /// Get critical bottlenecks
    pub fn get_critical_bottlenecks(&self) -> Vec<Bottleneck> {
        self.bottlenecks
            .read()
            .unwrap()
            .iter()
            .filter(|b| b.severity == BottleneckSeverity::Critical)
            .cloned()
            .collect()
    }
    
    /// Get total snapshots recorded
    pub fn get_snapshot_count(&self) -> usize {
        self.snapshots.read().unwrap().len()
    }
    
    /// Clear history
    pub fn clear(&self) {
        self.snapshots.write().unwrap().clear();
        self.bottlenecks.write().unwrap().clear();
        debug!("Performance monitor cleared");
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
    
//     #[test]
//     fn test_monitor_creation() {
//         let monitor = PerformanceMonitor::new(100);
//         assert_eq!(monitor.get_snapshot_count(), 0);
//     }
    
//     #[test]
//     fn test_record_snapshot() {
//         let monitor = PerformanceMonitor::new(100);
        
//         let snapshot = MetricsSnapshot {
//             timestamp: SystemTime::now(),
//             cpu_percent: 50.0,
//             memory_mb: 200,
//             frame_time_ms: 16.67,
//             fps: 60,
//             cache_hits: 1000,
//             cache_misses: 50,
//             gpu_utilization: 75.0,
//         };
        
//         monitor.record(snapshot);
//         assert_eq!(monitor.await.get_snapshot_count(), 1);
//     }
    
//     #[test]
//     fn test_bottleneck_detection() {
//         let monitor = PerformanceMonitor::new(100);
        
//         let snapshot = MetricsSnapshot {
//             timestamp: SystemTime::now(),
//             cpu_percent: 95.0,
//             memory_mb: 500,
//             frame_time_ms: 50.0,
//             fps: 20,
//             cache_hits: 100,
//             cache_misses: 900,
//             gpu_utilization: 100.0,
//         };
        
//         monitor.record(snapshot);
//         let bottlenecks = monitor.get_bottlenecks();
//         assert!(!bottlenecks.is_empty());
//     }
// }
