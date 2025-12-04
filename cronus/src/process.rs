use anyhow::Result;
use serde::{Deserialize, Serialize};
use parking_lot::Mutex;
use chrono::{DateTime, Utc};
use nix::unistd::Pid;
use libc;

/// Represents a running process
pub struct Process {
    pub pid: u32,
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
    pub fn new(pid: u32, config: crate::ProcessLaunchConfig) -> Result<Self> {
        Ok(Process {
            pid,
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

    pub fn send_signal(&self, signal: i32) -> Result<()> {
        unsafe {
            if libc::kill(self.pid as libc::pid_t, signal) == 0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!("Failed to send signal {} to process {}", signal, self.pid))
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
    use std::collections::HashMap;

    #[test]
    fn test_process_creation() {
        let config = crate::ProcessLaunchConfig {
            app_id: "test-app".to_string(),
            executable: "/usr/bin/test".to_string(),
            ..Default::default()
        };
        let process = Process::new(1001, config).unwrap();
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
        let process = Process::new(1001, config).unwrap();
        process.update_state(ProcessState::Running);
        assert_eq!(process.get_state(), ProcessState::Running);
        process.update_state(ProcessState::Suspended);
        assert_eq!(process.get_state(), ProcessState::Suspended);
    }
}
