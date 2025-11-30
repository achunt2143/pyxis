pub mod process;
pub mod lifecycle;
pub mod resources;
pub mod tree;
pub mod signals;
pub mod ipc;
pub mod echo_ipc;

pub use process::{Process, ProcessState, ProcessInfo};
pub use lifecycle::{ProcessLifecycle, LifecycleEvent};
pub use resources::{ResourceManager, ResourceLimit, ResourceUsage};
pub use tree::{ProcessTree, ProcessNode};
pub use signals::SignalHandler;
pub use ipc::IPCServer;
pub use echo_ipc::CronusEchoService;


use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing;
use uuid::Uuid;
use std::collections::HashMap;

/// Cronus: Process Manager for GeckoOS
/// Manages application lifecycle, resource accounting, and process hierarchy
pub struct Cronus {
    processes: Arc<DashMap<u32, Arc<Process>>>,
    process_tree: Arc<RwLock<ProcessTree>>,
    resource_manager: Arc<ResourceManager>,
    signal_handler: Arc<SignalHandler>,
    lifecycle: Arc<ProcessLifecycle>,
    ipc: Arc<IPCServer>,
    next_pid: Arc<parking_lot::Mutex<u32>>,
}

impl Cronus {
    /// Initialize Cronus process manager
    pub async fn new() -> Result<Self> {
        tracing::info!("Initializing Cronus Process Manager");

        let processes = Arc::new(DashMap::new());
        let process_tree = Arc::new(RwLock::new(ProcessTree::new()));
        let resource_manager = Arc::new(ResourceManager::new()?);
        let signal_handler = Arc::new(SignalHandler::new()?);
        let lifecycle = Arc::new(ProcessLifecycle::new());
        let ipc = Arc::new(IPCServer::new()?);

        let cronus = Cronus {
            processes,
            process_tree,
            resource_manager,
            signal_handler,
            lifecycle,
            ipc,
            next_pid: Arc::new(parking_lot::Mutex::new(1000)),
        };

        tracing::info!("Cronus initialized successfully");
        Ok(cronus)
    }

    /// Launch a process
    pub async fn launch(&self, config: ProcessLaunchConfig) -> Result<u32> {
        tracing::info!("Launching process: app_id={}", config.app_id);

        // Allocate process ID
        let pid = self.allocate_pid()?;

        // Create process structure
        let process = Process::new(pid, config.clone())?;
        let process_arc = Arc::new(process);

        // Fork and exec
        let child_pid = self.fork_and_exec(&process_arc, &config).await?;

        // Record process
        self.processes.insert(pid, process_arc.clone());

        // Add to process tree
        {
            let mut tree = self.process_tree.write();
            tree.add_process(pid, config.parent_pid, &config.app_id)?;
        }

        // Initialize resource accounting
        self.resource_manager.account_process(pid)?;

        // Setup signal handlers
        self.signal_handler.register_handler(pid)?;

        // Record lifecycle event
        self.lifecycle.record_event(pid, LifecycleEvent::Started)?;

        tracing::info!("Process launched: pid={}, child_pid={}", pid, child_pid);
        Ok(pid)
    }

    /// Terminate a process
    pub async fn terminate(&self, pid: u32, signal: i32) -> Result<()> {
        tracing::info!("Terminating process: pid={}, signal={}", pid, signal);

        if let Some((_k, process)) = self.processes.remove(&pid) {
            // Send signal
            process.send_signal(signal)?;

            // Record lifecycle event
            self.lifecycle.record_event(pid, LifecycleEvent::Terminated)?;

            // Cleanup resources
            self.resource_manager.cleanup_process(pid)?;

            // Remove from tree
            {
                let mut tree = self.process_tree.write();
                tree.remove_process(pid)?;
            }

            tracing::info!("Process terminated: pid={}", pid);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Process {} not found", pid))
        }
    }

    /// Suspend a process
    pub async fn suspend(&self, pid: u32) -> Result<()> {
        if let Some(process) = self.processes.get(&pid) {
            process.send_signal(libc::SIGSTOP)?;
            self.lifecycle.record_event(pid, LifecycleEvent::Suspended)?;
            tracing::debug!("Process suspended: pid={}", pid);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Process {} not found", pid))
        }
    }

    /// Resume a process
    pub async fn resume(&self, pid: u32) -> Result<()> {
        if let Some(process) = self.processes.get(&pid) {
            process.send_signal(libc::SIGCONT)?;
            self.lifecycle.record_event(pid, LifecycleEvent::Resumed)?;
            tracing::debug!("Process resumed: pid={}", pid);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Process {} not found", pid))
        }
    }

    /// Get process information
    pub fn get_process_info(&self, pid: u32) -> Option<ProcessInfo> {
        self.processes.get(&pid).map(|p| p.get_info())
    }

    /// List all processes
    pub fn list_processes(&self) -> Vec<ProcessInfo> {
        self.processes
            .iter()
            .map(|entry| entry.value().get_info())
            .collect()
    }

    /// Get process tree
    pub fn get_process_tree(&self) -> Vec<ProcessNode> {
        let tree = self.process_tree.read();
        tree.get_tree_structure()
    }

    /// Get resource usage for process
    pub fn get_resource_usage(&self, pid: u32) -> Result<ResourceUsage> {
        self.resource_manager.get_usage(pid)
    }

    /// Set resource limits for process
    pub fn set_resource_limits(&self, pid: u32, limits: ResourceLimit) -> Result<()> {
        self.resource_manager.set_limits(pid, limits)
    }

    /// Wait for process termination
    pub async fn wait(&self, pid: u32) -> Result<i32> {
        // Poll until process exits
        loop {
            if !self.processes.contains_key(&pid) {
                // Process has been removed (terminated)
                return Ok(0);
            }

            // Check if process still exists in kernel
            match nix::sys::wait::waitpid(
                nix::unistd::Pid::from_raw(pid as i32),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            ) {
                Ok(nix::sys::wait::WaitStatus::Exited(_, status)) => {
                    // Process exited
                    self.processes.remove(&pid);
                    self.resource_manager.cleanup_process(pid)?;
                    self.lifecycle.record_event(pid, LifecycleEvent::Exited)?;
                    return Ok(status as i32);
                }
                Ok(_) => {
                    // Process still running or other status
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                Err(_) => {
                    // Process doesn't exist
                    self.processes.remove(&pid);
                    return Err(anyhow::anyhow!("Process {} not found", pid));
                }
            }
        }
    }

    /// Get process children
    pub fn get_children(&self, pid: u32) -> Vec<u32> {
        let tree = self.process_tree.read();
        tree.get_children(pid)
    }

    /// Get process parent
    pub fn get_parent(&self, pid: u32) -> Option<u32> {
        let tree = self.process_tree.read();
        tree.get_parent(pid)
    }

    /// Get process ancestors
    pub fn get_ancestors(&self, pid: u32) -> Vec<u32> {
        let tree = self.process_tree.read();
        tree.get_ancestors(pid)
    }

    /// Get process descendants
    pub fn get_descendants(&self, pid: u32) -> Vec<u32> {
        let tree = self.process_tree.read();
        tree.get_descendants(pid)
    }

    /// Handle system signals
    pub async fn handle_signals(&self) -> Result<()> {
        self.signal_handler.listen().await
    }

    // Private methods

    async fn fork_and_exec(
        &self,
        process: &Arc<Process>,
        config: &ProcessLaunchConfig,
    ) -> Result<u32> {
        // In production, this would:
        // 1. Fork child process
        // 2. Set up environment
        // 3. Configure resource limits
        // 4. Exec binary

        tracing::trace!(
            "Fork/exec for process: app_id={}, executable={}",
            config.app_id,
            config.executable
        );

        // For now, return mock child PID
        Ok(process.pid + 10000)
    }

    fn allocate_pid(&self) -> Result<u32> {
        let mut next = self.next_pid.lock();
        let pid = *next;
        *next += 1;
        if *next > 65535 {
            *next = 1000;
        }
        Ok(pid)
    }
}

/// Configuration for launching a process
#[derive(Debug, Clone)]
pub struct ProcessLaunchConfig {
    pub app_id: String,
    pub executable: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_dir: String,
    pub parent_pid: Option<u32>,
    pub memory_limit_mb: u32,
    pub cpu_shares: u32,
    pub io_priority: u8,
    pub nice: i32,
}

impl Default for ProcessLaunchConfig {
    fn default() -> Self {
        ProcessLaunchConfig {
            app_id: String::new(),
            executable: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            working_dir: "/".to_string(),
            parent_pid: None,
            memory_limit_mb: 256,
            cpu_shares: 1024,
            io_priority: 4,
            nice: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cronus_initialization() {
        let cronus = Cronus::new().await.unwrap();
        assert!(cronus.list_processes().is_empty());
    }

    #[tokio::test]
    async fn test_process_allocation() {
        let cronus = Cronus::new().await.unwrap();
        let pid1 = cronus.allocate_pid().unwrap();
        let pid2 = cronus.allocate_pid().unwrap();
        assert!(pid2 > pid1);
    }
}
