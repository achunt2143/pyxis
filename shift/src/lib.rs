pub mod process;
pub mod lifecycle;
pub mod resources;
pub mod tree;
pub mod signals;
pub mod echo_ipc;

pub use process::{Process, ProcessState, ProcessInfo};
pub use lifecycle::{ProcessLifecycle, LifecycleEvent};
pub use resources::{ResourceManager, ResourceLimit, ResourceUsage};
pub use tree::{ProcessTree, ProcessNode};
pub use signals::SignalHandler;
pub use echo_ipc::ShiftEchoService;

use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing::{info, debug, error};
use std::collections::HashMap;

/// Shift: Process Manager for GeckoOS
/// Manages application lifecycle, resource accounting, and process hierarchy
pub struct Shift {
    processes: Arc<DashMap<u32, Arc<Process>>>,
    process_tree: Arc<RwLock<ProcessTree>>,
    resource_manager: Arc<ResourceManager>,
    signal_handler: Arc<SignalHandler>,
    lifecycle: Arc<ProcessLifecycle>,
    echo_service: Arc<ShiftEchoService>,
    next_pid: Arc<parking_lot::Mutex<u32>>,
}

impl Shift {
    /// Initialize Shift process manager
    pub async fn new() -> Result<Self> {
        tracing::info!("Initializing Shift Process Manager");
        
        let processes = Arc::new(DashMap::new());
        let process_tree = Arc::new(RwLock::new(ProcessTree::new()));
        let resource_manager = Arc::new(ResourceManager::new()?);
        let signal_handler = Arc::new(SignalHandler::new()?);
        let lifecycle = Arc::new(ProcessLifecycle::new());
        let echo_service = Arc::new(ShiftEchoService::new()?);

        let shift = Shift {
            processes,
            process_tree,
            resource_manager,
            signal_handler,
            lifecycle,
            echo_service,
            next_pid: Arc::new(parking_lot::Mutex::new(1000)),
        };

        tracing::info!("Shift initialized successfully");
        Ok(shift)
    }

    /// Register with ECHO IPC bus
    pub async fn register_echo(&self) -> Result<()> {
        let mut serv = self.echo_service().clone();
        serv.register().await
    }

    /// Launch a process
    pub async fn launch(&self, config: ProcessLaunchConfig) -> Result<u32> {
        tracing::info!("Launching process: app_id={}", config.app_id);

        // Allocate process ID -- Shift's bookkeeping id, not a kernel pid
        let pid = self.allocate_pid()?;

        // Fork and exec. `None` means nothing was spawned; see fork_and_exec.
        let os_pid = self.fork_and_exec(pid, &config).await?;

        // Create process structure
        let process = Process::new(pid, os_pid, config.clone())?;
        let process_arc = Arc::new(process);

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

        // Update process state
        process_arc.update_state(ProcessState::Running);

        // Emit ECHO signal
        let process_info = process_arc.get_info();
        self.echo_service.emit_process_created(pid, &config.app_id).await?;

        tracing::info!("Process launched: pid={}, os_pid={:?}", pid, os_pid);
        Ok(pid)
    }

    /// Terminate a process
    pub async fn terminate(&self, pid: u32, signal: i32) -> Result<()> {
        tracing::info!("Terminating process: pid={}, signal={}", pid, signal);

        if let Some((_k, process)) = self.processes.remove(&pid) {
            // Send signal -- if there is anything to send it to. A process with
            // no OS pid was never spawned, so there is nothing to stop and the
            // bookkeeping below is all that terminating it means.
            if process.os_pid.is_some() {
                process.send_signal(signal)?;
            } else {
                tracing::warn!(
                    "terminate: process {} ({}) has no OS process; nothing to signal",
                    pid,
                    process.app_id
                );
            }

            // Record lifecycle event
            self.lifecycle.record_event(pid, LifecycleEvent::Terminated)?;

            // Cleanup resources
            self.resource_manager.cleanup_process(pid)?;

            // Remove from tree
            {
                let mut tree = self.process_tree.write();
                tree.remove_process(pid)?;
            }

            // Emit ECHO signal
            self.echo_service.emit_process_terminated(pid, 0).await?;

            tracing::info!("Process terminated: pid={}", pid);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Process {} not found", pid))
        }
    }

    /// Kill a process forcefully
    pub async fn kill(&self, pid: u32) -> Result<()> {
        self.terminate(pid, libc::SIGKILL).await
    }

    /// Suspend a process
    pub async fn suspend(&self, pid: u32) -> Result<()> {
        if let Some(process) = self.processes.get(&pid) {
            process.send_signal(libc::SIGSTOP)?;
            process.update_state(ProcessState::Suspended);
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
            process.update_state(ProcessState::Running);
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

    /// Update resource usage statistics
    pub async fn update_resource_usage(&self) -> Result<()> {
        for entry in self.processes.iter() {
            let pid = *entry.key();
            
            // In production, read from /proc or use procfs crate
            // For now, simulate resource usage
            let usage = self.sample_process_resources(pid).await?;
            self.resource_manager.update_usage(pid, usage)?;
        }
        Ok(())
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

    /// Get all resource usage statistics
    pub fn get_all_resource_usage(&self) -> Vec<ResourceUsage> {
        self.resource_manager.all_processes_usage()
    }

    /// Get lifecycle events for a process
    pub fn get_lifecycle_events(&self, pid: u32) -> Vec<lifecycle::LifecycleEventRecord> {
        self.lifecycle.get_events(pid)
    }

    /// Handle system signals
    pub async fn handle_signals(&self) -> Result<()> {
        self.signal_handler.listen().await
    }

    /// Shutdown all processes gracefully
    pub async fn shutdown_all(&self) -> Result<()> {
        tracing::info!("Shutting down all processes");
        
        let pids: Vec<u32> = self.processes.iter().map(|e| *e.key()).collect();
        
        for pid in pids {
            if let Err(e) = self.terminate(pid, libc::SIGTERM).await {
                tracing::warn!("Failed to terminate process {}: {}", pid, e);
            }
        }
        
        tracing::info!("All processes shutdown complete");
        Ok(())
    }

    /// Get ECHO service reference
    pub fn echo_service(&self) -> &ShiftEchoService {
        &self.echo_service
    }

    // Private methods

    /// Spawn the process and return its kernel pid -- or `None` if nothing was
    /// spawned, which is every call today.
    async fn fork_and_exec(
        &self,
        pid: u32,
        config: &ProcessLaunchConfig,
    ) -> Result<Option<u32>> {
        // In production, this would:
        // 1. Fork child process
        // 2. Set up environment
        // 3. Configure resource limits
        // 4. Exec binary
        tracing::debug!(
            "Fork/exec for process: pid={}, app_id={}, executable={}",
            pid,
            config.app_id,
            config.executable
        );

        // Not implemented: nothing is spawned. This used to return a made-up
        // child pid (`pid + 10000`) that was logged as though it were real.
        // `None` is the truth, and it is what stops Process::send_signal from
        // signalling a kernel pid Shift never created.
        Ok(None)
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

    async fn sample_process_resources(&self, pid: u32) -> Result<ResourceUsage> {
        // In production, use procfs crate to read /proc/{pid}/stat, /proc/{pid}/status
        // For now, return simulated data
        Ok(ResourceUsage {
            pid,
            memory_mb: 50,
            cpu_percent: 5.0,
            cpu_time_ms: 1000,
            io_read_bytes: 1024 * 100,
            io_write_bytes: 1024 * 50,
            num_threads: 2,
            num_fds: 5,
        })
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
    async fn test_shift_initialization() {
        let shift = Shift::new().await.unwrap();
        assert!(shift.list_processes().is_empty());
    }

    #[tokio::test]
    async fn test_process_allocation() {
        let shift = Shift::new().await.unwrap();
        let pid1 = shift.allocate_pid().unwrap();
        let pid2 = shift.allocate_pid().unwrap();
        assert!(pid2 > pid1);
    }

    #[tokio::test]
    async fn test_launch_and_terminate() {
        let shift = Shift::new().await.unwrap();
        
        let config = ProcessLaunchConfig {
            app_id: "test.app".to_string(),
            executable: "/bin/true".to_string(),
            ..Default::default()
        };
        
        let pid = shift.launch(config).await.unwrap();
        assert!(shift.get_process_info(pid).is_some());
        
        shift.terminate(pid, libc::SIGTERM).await.unwrap();
        assert!(shift.get_process_info(pid).is_none());
    }

    // Shift's ids are bookkeeping, not kernel pids. Signalling used to
    // kill(id, ...), hitting whatever unrelated process held OS pid 1000 --
    // test_launch_and_terminate above failed only because nothing held it on
    // the machine it ran on. Suspend is the check because, unlike terminate,
    // it has no "nothing to signal" path: it must refuse outright, and for the
    // right reason, rather than stop some other process.
    #[tokio::test]
    async fn test_never_signals_an_internal_id_as_a_kernel_pid() {
        let shift = Shift::new().await.unwrap();
        let pid = shift
            .launch(ProcessLaunchConfig {
                app_id: "test.app".to_string(),
                executable: "/bin/true".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(pid >= 1000, "ids come from allocate_pid's counter");

        let err = shift.suspend(pid).await.expect_err("there is no OS process to stop");
        assert!(
            err.to_string().contains("no OS process"),
            "must refuse for the right reason, not fail inside kill(2): {err}"
        );
    }
}
