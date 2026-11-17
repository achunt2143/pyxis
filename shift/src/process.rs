use anyhow::Result;
use serde::{Deserialize, Serialize};
use parking_lot::Mutex;
use chrono::{DateTime, Utc};
use libc;

/// Represents a running process
pub struct Process {
    /// Shift's own bookkeeping id, from `allocate_pid()`'s counter (starts at
    /// 1000). NOT a kernel pid -- never pass it to kill(2).
    pub pid: u32,
    /// The kernel pid, once there is one. `None` for every process today:
    /// `Shift::fork_and_exec` does not spawn anything yet.
    pub os_pid: Option<u32>,
    pub app_id: String,
    pub executable: String,
    pub state: Mutex<ProcessState>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessState {
    Created,
    Running,
    Suspended,
    Zombie,
    Exited,
}

impl Process {
    pub fn new(pid: u32, os_pid: Option<u32>, config: crate::ProcessLaunchConfig) -> Result<Self> {
        Ok(Process {
            pid,
            os_pid,
            app_id: config.app_id,
            executable: config.executable,
            state: Mutex::new(ProcessState::Created),
            created_at: Utc::now(),
        })
    }

    pub fn get_info(&self) -> ProcessInfo {
        ProcessInfo {
            pid: self.pid,
            app_id: self.app_id.clone(),
            executable: self.executable.clone(),
            state: *self.state.lock(),
            created_at: self.created_at.to_rfc3339(),
            uptime_seconds: self.get_uptime(),
        }
    }

    /// Signal the OS process behind this entry, refusing if there is none.
    ///
    /// This used to `kill(self.pid, ...)` -- and `self.pid` is the bookkeeping
    /// id, not a kernel pid, so terminate/suspend/resume signalled whatever
    /// unrelated process held OS pid 1000, 1001, ... (as root, on device).
    /// Found 2026-09-25: `test_launch_and_terminate` failed with "Failed to
    /// send signal 15 to process 1000", and only *failed* because nothing
    /// happened to hold pid 1000 on the machine it ran on.
    pub fn send_signal(&self, signal: i32) -> Result<()> {
        let Some(os_pid) = self.os_pid else {
            anyhow::bail!(
                "process {} ({}) has no OS process to signal: Shift does not spawn \
                 real processes yet, and its ids are not kernel pids",
                self.pid,
                self.app_id
            );
        };
        unsafe {
            if libc::kill(os_pid as libc::pid_t, signal) == 0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Failed to send signal {} to process {} (os pid {}): {}",
                    signal,
                    self.pid,
                    os_pid,
                    std::io::Error::last_os_error()
                ))
            }
        }
    }

    pub fn update_state(&self, new_state: ProcessState) {
        *self.state.lock() = new_state;
    }

    pub fn get_state(&self) -> ProcessState {
        *self.state.lock()
    }

    pub fn get_uptime(&self) -> u64 {
        let now = Utc::now();
        let duration = now.signed_duration_since(self.created_at);
        duration.num_seconds().max(0) as u64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub app_id: String,
    pub executable: String,
    pub state: ProcessState,
    pub created_at: String,
    pub uptime_seconds: u64,
}

impl ProcessInfo {
    pub fn state_string(&self) -> String {
        match self.state {
            ProcessState::Created => "Created".to_string(),
            ProcessState::Running => "Running".to_string(),
            ProcessState::Suspended => "Suspended".to_string(),
            ProcessState::Zombie => "Zombie".to_string(),
            ProcessState::Exited => "Exited".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_creation() {
        let config = crate::ProcessLaunchConfig {
            app_id: "test-app".to_string(),
            executable: "/usr/bin/test".to_string(),
            ..Default::default()
        };
        let process = Process::new(1001, None, config).unwrap();
        assert_eq!(process.pid, 1001);
        assert_eq!(process.app_id, "test-app");
        assert_eq!(process.get_state(), ProcessState::Created);
    }

    #[test]
    fn test_process_state_transition() {
        let config = crate::ProcessLaunchConfig {
            app_id: "test-app".to_string(),
            executable: "/usr/bin/test".to_string(),
            ..Default::default()
        };
        let process = Process::new(1001, None, config).unwrap();
        process.update_state(ProcessState::Running);
        assert_eq!(process.get_state(), ProcessState::Running);
        process.update_state(ProcessState::Suspended);
        assert_eq!(process.get_state(), ProcessState::Suspended);
    }
}
