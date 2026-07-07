use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing;
use dashmap::DashMap;
use std::sync::Arc;
use uuid::Uuid;
#[cfg(unix)]
use libc::{ptrace, PTRACE_TRACEME, PTRACE_SYSCALL, PTRACE_GETREGS};

/// Native Linux binary executor
pub struct NativeExecutor {
    running_processes: Arc<DashMap<u32, ProcessInfo>>,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub binary: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub started_at: String,
}

impl NativeExecutor {
    pub async fn new() -> Result<Self> {
        tracing::info!("Initializing Native Executor");

        Ok(NativeExecutor {
            running_processes: Arc::new(DashMap::new()),
        })
    }

    /// Execute a native Linux binary
    pub async fn exec(
        &self,
        binary: &Path,
        args: Vec<String>,
        env: Option<HashMap<String, String>>,
    ) -> Result<u32> {
        tracing::info!("Executing binary: {:?}", binary);

        let mut cmd = Command::new(binary);

        // Set arguments
        for arg in &args {
            cmd.arg(arg);
        }

        // Set environment variables
        if let Some(env_vars) = &env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }

        // Configure I/O
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        // Spawn process
        let child = cmd.spawn()?;
        let pid = child.id();

        // Track process
        let process_info = ProcessInfo {
            pid,
            binary: binary.to_string_lossy().to_string(),
            args,
            env: env.unwrap_or_default(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };

        self.running_processes.insert(pid, process_info);

        tracing::debug!("Process started with PID: {}", pid);
        Ok(pid)
    }

    /// Get process information
    pub async fn get_process(&self, pid: u32) -> Result<Option<ProcessInfo>> {
        Ok(self
            .running_processes
            .get(&pid)
            .map(|entry| entry.value().clone()))
    }

    /// List running processes
    pub async fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        Ok(self
            .running_processes
            .iter()
            .map(|entry| entry.value().clone())
            .collect())
    }

    /// Kill process
    pub async fn kill_process(&self, pid: u32) -> Result<()> {
        tracing::info!("Killing process: {}", pid);

        #[cfg(unix)]
        {
            use nix::unistd::Pid;
            use nix::sys::signal::{kill, Signal};

            kill(Pid::from_raw(pid as i32), Signal::SIGTERM)?;
        }

        self.running_processes.remove(&pid);

        Ok(())
    }

    /// Force kill process
    pub async fn kill_force(&self, pid: u32) -> Result<()> {
        tracing::info!("Force killing process: {}", pid);

        #[cfg(unix)]
        {
            use nix::unistd::Pid;
            use nix::sys::signal::{kill, Signal};

            kill(Pid::from_raw(pid as i32), Signal::SIGKILL)?;
        }

        self.running_processes.remove(&pid);

        Ok(())
    }

    /// Get process count
    pub async fn process_count(&self) -> usize {
        self.running_processes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_executor_init() {
        let executor = NativeExecutor::new().await.unwrap();
        assert_eq!(executor.process_count().await, 0);
    }
}
