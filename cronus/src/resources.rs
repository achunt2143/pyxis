use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing;

/// Resource management for processes
pub struct ResourceManager {
    limits: Arc<DashMap<u32, ResourceLimit>>,
    usage: Arc<DashMap<u32, ResourceUsage>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimit {
    pub memory_limit_mb: u32,
    pub cpu_shares: u32,
    pub io_priority: u8,
    pub max_fds: u32,
    pub max_threads: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub pid: u32,
    pub memory_mb: u32,
    pub cpu_percent: f32,
    pub cpu_time_ms: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub num_threads: u32,
    pub num_fds: u32,
}

impl Default for ResourceLimit {
    fn default() -> Self {
        ResourceLimit {
            memory_limit_mb: 256,
            cpu_shares: 1024,
            io_priority: 4,
            max_fds: 1024,
            max_threads: 64,
        }
    }
}

impl ResourceManager {
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing Resource Manager");

        Ok(ResourceManager {
            limits: Arc::new(DashMap::new()),
            usage: Arc::new(DashMap::new()),
        })
    }

    pub fn account_process(&self, pid: u32) -> Result<()> {
        self.limits.insert(pid, ResourceLimit::default());
        self.usage.insert(
            pid,
            ResourceUsage {
                pid,
                memory_mb: 0,
                cpu_percent: 0.0,
                cpu_time_ms: 0,
                io_read_bytes: 0,
                io_write_bytes: 0,
                num_threads: 1,
                num_fds: 3,
            },
        );

        tracing::debug!("Resource accounting started for process {}", pid);
        Ok(())
    }

    pub fn cleanup_process(&self, pid: u32) -> Result<()> {
        self.limits.remove(&pid);
        self.usage.remove(&pid);

        tracing::debug!("Resource cleanup for process {}", pid);
        Ok(())
    }

    pub fn set_limits(&self, pid: u32, limits: ResourceLimit) -> Result<()> {
        // In production, this would:
        // 1. Apply cgroup limits
        // 2. Set CPU shares
        // 3. Set memory limit
        // 4. Configure I/O scheduler

        self.limits.insert(pid, limits.clone());

        tracing::info!(
            "Resource limits set for process {}: memory_mb={}, cpu_shares={}",
            pid,
            limits.memory_limit_mb,
            limits.cpu_shares
        );
        Ok(())
    }

    pub fn get_limits(&self, pid: u32) -> Option<ResourceLimit> {
        self.limits.get(&pid).map(|entry| entry.clone())
    }

    pub fn get_usage(&self, pid: u32) -> Result<ResourceUsage> {
        self.usage
            .get(&pid)
            .map(|entry| entry.clone())
            .ok_or_else(|| anyhow::anyhow!("Process {} not found", pid))
    }

    pub fn update_usage(&self, pid: u32, usage: ResourceUsage) -> Result<()> {
        self.usage.insert(pid, usage);
        Ok(())
    }

    pub fn sample_usage(&self) -> Vec<ResourceUsage> {
        // In production, this would:
        // 1. Read /proc for each process
        // 2. Parse memory, CPU, I/O stats
        // 3. Calculate percentages

        self.usage
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub fn check_limits(&self, pid: u32) -> Result<LimitStatus> {
        let usage = self.get_usage(pid)?;
        let limits = self.get_limits(pid).ok_or_else(|| anyhow::anyhow!("Limits not found"))?;

        let memory_ok = usage.memory_mb <= limits.memory_limit_mb;
        let threads_ok = usage.num_threads <= limits.max_threads;
        let fds_ok = usage.num_fds <= limits.max_fds;

        Ok(LimitStatus {
            memory_ok,
            threads_ok,
            fds_ok,
            all_ok: memory_ok && threads_ok && fds_ok,
        })
    }

    pub fn all_processes_usage(&self) -> Vec<ResourceUsage> {
        self.sample_usage()
    }
}

#[derive(Debug, Clone)]
pub struct LimitStatus {
    pub memory_ok: bool,
    pub threads_ok: bool,
    pub fds_ok: bool,
    pub all_ok: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_manager_init() {
        let rm = ResourceManager::new().unwrap();
        rm.account_process(1001).unwrap();

        let usage = rm.get_usage(1001).unwrap();
        assert_eq!(usage.pid, 1001);
    }

    #[test]
    fn test_resource_limits() {
        let rm = ResourceManager::new().unwrap();
        rm.account_process(1001).unwrap();

        let limits = ResourceLimit {
            memory_limit_mb: 512,
            cpu_shares: 2048,
            ..Default::default()
        };

        rm.set_limits(1001, limits.clone()).unwrap();
        let retrieved = rm.get_limits(1001).unwrap();
        assert_eq!(retrieved.memory_limit_mb, 512);
    }

    #[test]
    fn test_limit_checking() {
        let rm = ResourceManager::new().unwrap();
        rm.account_process(1001).unwrap();

        let status = rm.check_limits(1001).unwrap();
        assert!(status.all_ok);
    }
}
