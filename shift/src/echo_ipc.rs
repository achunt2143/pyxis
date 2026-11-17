//! CRONUS registration and communication with ECHO IPC bus
//!
//! This module handles:
//! - Registration of Shift service on ECHO bus
//! - Method call handling for process operations
//! - Signal emission for process lifecycle events

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use parking_lot::{Mutex as ParkingMutex, RwLock};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, warn};

use geckoOS_echo::{
    ArgumentDefinition, EchoClient, MethodDefinition, ServiceInterface, SignalDefinition,
};

use crate::lifecycle::{LifecycleEvent, ProcessLifecycle};
use crate::process::{Process, ProcessState};
use crate::resources::{ResourceLimit, ResourceManager};
use crate::signals::SignalHandler;
use crate::tree::ProcessTree;
use crate::ProcessLaunchConfig;

/// ECHO IPC service for process management
#[derive(Clone)]
pub struct ShiftEchoService {
    service_name: String,
    client: Option<Arc<TokioMutex<EchoClient>>>,
    processes: Arc<DashMap<u32, Arc<Process>>>,
    process_tree: Arc<RwLock<ProcessTree>>,
    resource_manager: Arc<ResourceManager>,
    signal_handler: Arc<SignalHandler>,
    lifecycle: Arc<ProcessLifecycle>,
    next_pid: Arc<ParkingMutex<u32>>,
}

impl ShiftEchoService {
    /// Create new Shift ECHO service with isolated state (does not connect yet).
    pub fn new() -> Result<Self> {
        let processes = Arc::new(DashMap::new());
        let process_tree = Arc::new(RwLock::new(ProcessTree::new()));
        let resource_manager = Arc::new(ResourceManager::new()?);
        let signal_handler = Arc::new(SignalHandler::new()?);
        let lifecycle = Arc::new(ProcessLifecycle::new());
        let next_pid = Arc::new(ParkingMutex::new(1000));
        Self::with_state(
            processes,
            process_tree,
            resource_manager,
            signal_handler,
            lifecycle,
            next_pid,
        )
    }

    /// Create a Shift ECHO service sharing state with a `Shift` instance.
    pub fn with_state(
        processes: Arc<DashMap<u32, Arc<Process>>>,
        process_tree: Arc<RwLock<ProcessTree>>,
        resource_manager: Arc<ResourceManager>,
        signal_handler: Arc<SignalHandler>,
        lifecycle: Arc<ProcessLifecycle>,
        next_pid: Arc<ParkingMutex<u32>>,
    ) -> Result<Self> {
        debug!("Initializing Shift ECHO service");

        let service = ShiftEchoService {
            service_name: "org.geckoOS.Shift".to_string(),
            client: None,
            processes,
            process_tree,
            resource_manager,
            signal_handler,
            lifecycle,
            next_pid,
        };

        info!("Shift service created: {}", service.service_name);
        Ok(service)
    }

    fn allocate_pid(&self) -> u32 {
        let mut next = self.next_pid.lock();
        let pid = *next;
        *next += 1;
        if *next > 65535 {
            *next = 1000;
        }
        pid
    }

    /// Register with ECHO bus (async)
    pub async fn register(&mut self) -> Result<()> {
        debug!("Registering CRONUS with ECHO bus");
        
        // Connect to Echo daemon
        let mut client = EchoClient::connect().await?;

        // Define methods that Shift exposes
        let methods = vec![
            // Process Launch & Termination
            MethodDefinition {
                name: "LaunchProcess".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "app_id".to_string(), arg_type: "s".to_string() },
                    ArgumentDefinition { name: "args".to_string(), arg_type: "s".to_string() },
                    ArgumentDefinition { name: "env".to_string(), arg_type: "v".to_string() },
                ],
                output_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
            },
            
            MethodDefinition {
                name: "TerminateProcess".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
                output_args: vec![
                    ArgumentDefinition { name: "exit_code".to_string(), arg_type: "i".to_string() },
                ],
            },
            
            MethodDefinition {
                name: "KillProcess".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "signal".to_string(), arg_type: "i".to_string() },
                ],
                output_args: vec![],
            },
            
            // Process Control
            MethodDefinition {
                name: "SuspendProcess".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
                output_args: vec![],
            },
            
            MethodDefinition {
                name: "ResumeProcess".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
                output_args: vec![],
            },
            
            // Process Information
            MethodDefinition {
                name: "GetProcess".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
                output_args: vec![
                    ArgumentDefinition { name: "process_info".to_string(), arg_type: "v".to_string() },
                ],
            },
            
            MethodDefinition {
                name: "ListProcesses".to_string(),
                input_args: vec![],
                output_args: vec![
                    ArgumentDefinition { name: "processes".to_string(), arg_type: "v".to_string() },
                ],
            },
            
            MethodDefinition {
                name: "GetProcessTree".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "parent_pid".to_string(), arg_type: "u".to_string() },
                ],
                output_args: vec![
                    ArgumentDefinition { name: "tree".to_string(), arg_type: "v".to_string() },
                ],
            },
            
            // Resource Management
            MethodDefinition {
                name: "GetResourceUsage".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                ],
                output_args: vec![
                    ArgumentDefinition { name: "usage".to_string(), arg_type: "v".to_string() },
                ],
            },
            
            MethodDefinition {
                name: "SetResourceLimits".to_string(),
                input_args: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "limits".to_string(), arg_type: "v".to_string() },
                ],
                output_args: vec![],
            },
        ];

        // Define signals that Shift emits
        let signals = vec![
            SignalDefinition {
                name: "ProcessCreated".to_string(),
                arguments: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "app_id".to_string(), arg_type: "s".to_string() },
                    ArgumentDefinition { name: "timestamp".to_string(), arg_type: "s".to_string() },
                ],
            },
            
            SignalDefinition {
                name: "ProcessTerminated".to_string(),
                arguments: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "exit_code".to_string(), arg_type: "i".to_string() },
                    ArgumentDefinition { name: "timestamp".to_string(), arg_type: "s".to_string() },
                ],
            },
            
            SignalDefinition {
                name: "ProcessSuspended".to_string(),
                arguments: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "timestamp".to_string(), arg_type: "s".to_string() },
                ],
            },
            
            SignalDefinition {
                name: "ProcessResumed".to_string(),
                arguments: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "timestamp".to_string(), arg_type: "s".to_string() },
                ],
            },
            
            SignalDefinition {
                name: "ProcessFailed".to_string(),
                arguments: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "error".to_string(), arg_type: "s".to_string() },
                    ArgumentDefinition { name: "timestamp".to_string(), arg_type: "s".to_string() },
                ],
            },
            
            SignalDefinition {
                name: "ResourceLimitExceeded".to_string(),
                arguments: vec![
                    ArgumentDefinition { name: "pid".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "resource".to_string(), arg_type: "s".to_string() },
                    ArgumentDefinition { name: "limit".to_string(), arg_type: "u".to_string() },
                    ArgumentDefinition { name: "current".to_string(), arg_type: "u".to_string() },
                ],
            },
        ];

        // Create service interface
        let interface = ServiceInterface {
            name: self.service_name.clone(),
            methods,
            signals,
            properties: vec![],
        };

        // Register service
        let _service_id = client.register_service(&self.service_name, interface).await?;
        self.client = Some(Arc::new(TokioMutex::new(client)));

        info!("CRONUS successfully registered on ECHO bus");
        Ok(())
    }

    // ============================================================================
    // SIGNAL EMISSION METHODS
    // ============================================================================

    /// Emit ProcessCreated signal
    pub async fn emit_process_created(&self, pid: u32, app_id: &str) -> Result<()> {
        debug!("Emitting ProcessCreated signal: pid={}, app_id={}", pid, app_id);
        
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "ProcessCreated",
                json!({
                    "pid": pid,
                    "app_id": app_id,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            ).await?;
        } else {
            warn!("Shift not connected to Echo bus");
        }
        
        Ok(())
    }

    /// Emit ProcessTerminated signal
    pub async fn emit_process_terminated(&self, pid: u32, exit_code: i32) -> Result<()> {
        debug!("Emitting ProcessTerminated signal: pid={}, exit_code={}", pid, exit_code);
        
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "ProcessTerminated",
                json!({
                    "pid": pid,
                    "exit_code": exit_code,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            ).await?;
        } else {
            warn!("Shift not connected to Echo bus");
        }
        
        Ok(())
    }

    /// Emit ProcessSuspended signal
    pub async fn emit_process_suspended(&self, pid: u32) -> Result<()> {
        debug!("Emitting ProcessSuspended signal: pid={}", pid);
        
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "ProcessSuspended",
                json!({
                    "pid": pid,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            ).await?;
        } else {
            warn!("Shift not connected to Echo bus");
        }
        
        Ok(())
    }

    /// Emit ProcessResumed signal
    pub async fn emit_process_resumed(&self, pid: u32) -> Result<()> {
        debug!("Emitting ProcessResumed signal: pid={}", pid);
        
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "ProcessResumed",
                json!({
                    "pid": pid,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            ).await?;
        } else {
            warn!("Shift not connected to Echo bus");
        }
        
        Ok(())
    }

    /// Emit ProcessFailed signal
    pub async fn emit_process_failed(&self, pid: u32, error: &str) -> Result<()> {
        debug!("Emitting ProcessFailed signal: pid={}, error={}", pid, error);
        
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "ProcessFailed",
                json!({
                    "pid": pid,
                    "error": error,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            ).await?;
        } else {
            warn!("Shift not connected to Echo bus");
        }
        
        Ok(())
    }

    /// Emit ResourceLimitExceeded signal
    pub async fn emit_resource_limit_exceeded(
        &self,
        pid: u32,
        resource: &str,
        limit: u32,
        current: u32,
    ) -> Result<()> {
        debug!(
            "Emitting ResourceLimitExceeded signal: pid={}, resource={}, limit={}, current={}",
            pid, resource, limit, current
        );
        
        if let Some(client) = &self.client {
            let client = client.lock().await;
            client.emit_signal(
                &self.service_name,
                "ResourceLimitExceeded",
                json!({
                    "pid": pid,
                    "resource": resource,
                    "limit": limit,
                    "current": current,
                }),
            ).await?;
        } else {
            warn!("Shift not connected to Echo bus");
        }
        
        Ok(())
    }

    // ============================================================================
    // METHOD HANDLERS
    // ============================================================================

    /// Handle LaunchProcess method call
    pub async fn handle_launch_process(
        &self,
        app_id: &str,
        args: &str,
        env: serde_json::Value,
    ) -> Result<u32> {
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return Err(anyhow!("LaunchProcess requires non-empty app_id"));
        }
        debug!("LaunchProcess called: app_id={}, args={}", app_id, args);

        let parsed_args: Vec<String> = if args.trim().is_empty() {
            Vec::new()
        } else {
            args.split_whitespace().map(str::to_string).collect()
        };
        let mut env_map = HashMap::new();
        if let Some(obj) = env.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    env_map.insert(k.clone(), s.to_string());
                } else {
                    env_map.insert(k.clone(), v.to_string());
                }
            }
        }
        let parent_pid = env
            .get("parent_pid")
            .and_then(|v| v.as_u64())
            .map(|p| p as u32);

        let pid = self.allocate_pid();
        let config = ProcessLaunchConfig {
            app_id: app_id.to_string(),
            executable: app_id.to_string(),
            args: parsed_args,
            env: env_map,
            parent_pid,
            ..Default::default()
        };

        let process = Process::new(pid, None, config)?;
        process.update_state(ProcessState::Running);
        self.processes.insert(pid, Arc::new(process));
        {
            let mut tree = self.process_tree.write();
            tree.add_process(pid, parent_pid, app_id)?;
        }
        self.resource_manager.account_process(pid)?;
        self.signal_handler.register_handler(pid)?;
        self.lifecycle.record_event(pid, LifecycleEvent::Started)?;

        self.emit_process_created(pid, app_id).await?;

        info!("Process launched: app_id={}, pid={}", app_id, pid);
        Ok(pid)
    }

    fn cleanup_terminated_process(&self, pid: u32, process: &Process, signal: i32) -> Result<()> {
        if process.os_pid.is_some() {
            process.send_signal(signal)?;
        }
        self.lifecycle.record_event(pid, LifecycleEvent::Terminated)?;
        self.resource_manager.cleanup_process(pid)?;
        self.signal_handler.unregister_handler(pid)?;
        let mut tree = self.process_tree.write();
        let _ = tree.remove_process(pid);
        Ok(())
    }

    /// Handle TerminateProcess method call
    pub async fn handle_terminate_process(&self, pid: u32) -> Result<i32> {
        debug!("TerminateProcess called: pid={}", pid);

        let (_k, process) = self
            .processes
            .remove(&pid)
            .ok_or_else(|| anyhow!("Process {} not found", pid))?;
        self.cleanup_terminated_process(pid, &process, libc::SIGTERM)?;

        let exit_code = 0;
        self.emit_process_terminated(pid, exit_code).await?;

        info!("Process terminated: pid={}, exit_code={}", pid, exit_code);
        Ok(exit_code)
    }

    /// Handle KillProcess method call
    pub async fn handle_kill_process(&self, pid: u32, signal: i32) -> Result<()> {
        debug!("KillProcess called: pid={}, signal={}", pid, signal);

        if signal == libc::SIGKILL || signal == libc::SIGTERM {
            let (_k, process) = self
                .processes
                .remove(&pid)
                .ok_or_else(|| anyhow!("Process {} not found", pid))?;
            self.cleanup_terminated_process(pid, &process, signal)?;
            self.emit_process_terminated(pid, 128 + signal).await?;
        } else {
            let process = self
                .processes
                .get(&pid)
                .ok_or_else(|| anyhow!("Process {} not found", pid))?;
            process.send_signal(signal)?;
        }

        info!("Process killed: pid={}, signal={}", pid, signal);
        Ok(())
    }

    /// Handle SuspendProcess method call
    pub async fn handle_suspend_process(&self, pid: u32) -> Result<()> {
        debug!("SuspendProcess called: pid={}", pid);

        let process = self
            .processes
            .get(&pid)
            .ok_or_else(|| anyhow!("Process {} not found", pid))?;
        if process.os_pid.is_some() {
            process.send_signal(libc::SIGSTOP)?;
        }
        process.update_state(ProcessState::Suspended);
        self.lifecycle.record_event(pid, LifecycleEvent::Suspended)?;

        self.emit_process_suspended(pid).await?;

        info!("Process suspended: pid={}", pid);
        Ok(())
    }

    /// Handle ResumeProcess method call
    pub async fn handle_resume_process(&self, pid: u32) -> Result<()> {
        debug!("ResumeProcess called: pid={}", pid);

        let process = self
            .processes
            .get(&pid)
            .ok_or_else(|| anyhow!("Process {} not found", pid))?;
        if process.os_pid.is_some() {
            process.send_signal(libc::SIGCONT)?;
        }
        process.update_state(ProcessState::Running);
        self.lifecycle.record_event(pid, LifecycleEvent::Resumed)?;

        self.emit_process_resumed(pid).await?;

        info!("Process resumed: pid={}", pid);
        Ok(())
    }

    /// Handle GetProcess method call
    pub async fn handle_get_process(&self, pid: u32) -> Result<serde_json::Value> {
        debug!("GetProcess called: pid={}", pid);

        let process = self
            .processes
            .get(&pid)
            .ok_or_else(|| anyhow!("Process {} not found", pid))?;
        let info = process.get_info();
        let usage = self.resource_manager.get_usage(pid).ok();
        let limits = self.resource_manager.get_limits(pid);

        Ok(json!({
            "pid": info.pid,
            "os_pid": process.os_pid,
            "app_id": info.app_id,
            "executable": info.executable,
            "state": info.state_string().to_lowercase(),
            "created_at": info.created_at,
            "uptime_seconds": info.uptime_seconds,
            "memory_mb": usage.as_ref().map(|u| u.memory_mb).unwrap_or(0),
            "cpu_percent": usage.as_ref().map(|u| u.cpu_percent).unwrap_or(0.0),
            "limits": limits,
        }))
    }

    /// Handle ListProcesses method call
    pub async fn handle_list_processes(&self) -> Result<serde_json::Value> {
        debug!("ListProcesses called");

        let mut list: Vec<serde_json::Value> = self
            .processes
            .iter()
            .map(|entry| {
                let p = entry.value();
                let info = p.get_info();
                json!({
                    "pid": info.pid,
                    "os_pid": p.os_pid,
                    "app_id": info.app_id,
                    "executable": info.executable,
                    "state": info.state_string().to_lowercase(),
                    "created_at": info.created_at,
                    "uptime_seconds": info.uptime_seconds,
                })
            })
            .collect();
        list.sort_by_key(|v| v["pid"].as_u64().unwrap_or(0));

        Ok(serde_json::Value::Array(list))
    }

    /// Handle GetProcessTree method call
    pub async fn handle_get_process_tree(&self, parent_pid: u32) -> Result<serde_json::Value> {
        debug!("GetProcessTree called: parent_pid={}", parent_pid);

        let tree = self.process_tree.read();
        if parent_pid == 0 {
            let nodes = tree.get_tree_structure();
            return Ok(json!({ "nodes": nodes }));
        }
        let node = tree.get_process_node(parent_pid);
        let children = tree.get_children(parent_pid);
        let descendants = tree.get_descendants(parent_pid);

        Ok(json!({
            "pid": parent_pid,
            "node": node,
            "children": children,
            "descendants": descendants,
        }))
    }

    /// Handle GetResourceUsage method call
    pub async fn handle_get_resource_usage(&self, pid: u32) -> Result<serde_json::Value> {
        debug!("GetResourceUsage called: pid={}", pid);

        let usage = self.resource_manager.get_usage(pid)?;
        Ok(json!({
            "pid": usage.pid,
            "memory_mb": usage.memory_mb,
            "cpu_percent": usage.cpu_percent,
            "cpu_time_ms": usage.cpu_time_ms,
            "io_read_bytes": usage.io_read_bytes,
            "io_write_bytes": usage.io_write_bytes,
            "fds": usage.num_fds,
            "threads": usage.num_threads,
        }))
    }

    /// Handle SetResourceLimits method call
    pub async fn handle_set_resource_limits(
        &self,
        pid: u32,
        limits: serde_json::Value,
    ) -> Result<()> {
        debug!("SetResourceLimits called: pid={}, limits={:?}", pid, limits);

        if !self.processes.contains_key(&pid) {
            return Err(anyhow!("Process {} not found", pid));
        }

        let current = self.resource_manager.get_limits(pid).unwrap_or_default();
        let updated = ResourceLimit {
            memory_limit_mb: limits
                .get("memory_limit_mb")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(current.memory_limit_mb),
            cpu_shares: limits
                .get("cpu_shares")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(current.cpu_shares),
            io_priority: limits
                .get("io_priority")
                .and_then(|v| v.as_u64())
                .map(|v| v as u8)
                .unwrap_or(current.io_priority),
            max_fds: limits
                .get("max_fds")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(current.max_fds),
            max_threads: limits
                .get("max_threads")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(current.max_threads),
        };
        self.resource_manager.set_limits(pid, updated)?;
        info!("Resource limits set for pid={}", pid);
        Ok(())
    }

    // ============================================================================
    // UTILITY METHODS
    // ============================================================================

    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    pub fn is_registered(&self) -> bool {
        self.client.is_some()
    }
}

impl Default for ShiftEchoService {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_creation() {
        let service = ShiftEchoService::new();
        assert!(service.is_ok());
    }

    #[test]
    fn test_service_name() {
        let service = ShiftEchoService::new().unwrap();
        assert_eq!(service.service_name(), "org.geckoOS.Shift");
    }

    #[test]
    fn test_not_registered_initially() {
        let service = ShiftEchoService::new().unwrap();
        assert!(!service.is_registered());
    }

    #[tokio::test]
    async fn test_signal_emission() {
        let service = ShiftEchoService::new().unwrap();
        // Note: Will emit warnings about not being connected to Echo bus
        assert!(service.emit_process_created(1234, "test.app").await.is_ok());
        assert!(service.emit_process_terminated(1234, 0).await.is_ok());
        assert!(service.emit_process_suspended(1234).await.is_ok());
        assert!(service.emit_process_resumed(1234).await.is_ok());
        assert!(service.emit_process_failed(1234, "Out of memory").await.is_ok());
    }

    #[tokio::test]
    async fn test_method_handlers() {
        let service = ShiftEchoService::new().unwrap();

        // Starts empty — no fabricated processes.
        let initial = service.handle_list_processes().await.unwrap();
        assert_eq!(initial.as_array().unwrap().len(), 0);
        assert!(service.handle_get_process(1000).await.is_err());

        let pid = service
            .handle_launch_process("files", "--open /etc", json!({}))
            .await
            .unwrap();
        assert_eq!(pid, 1000);

        let child_pid = service
            .handle_launch_process("notes", "", json!({ "parent_pid": pid }))
            .await
            .unwrap();
        assert_eq!(child_pid, 1001);

        let listed = service.handle_list_processes().await.unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 2);

        let info = service.handle_get_process(pid).await.unwrap();
        assert_eq!(info["app_id"], "files");
        assert_eq!(info["state"], "running");

        let tree = service.handle_get_process_tree(pid).await.unwrap();
        assert_eq!(tree["children"], json!([child_pid]));

        service
            .handle_set_resource_limits(pid, json!({ "memory_limit_mb": 512 }))
            .await
            .unwrap();
        let info_after_limits = service.handle_get_process(pid).await.unwrap();
        assert_eq!(info_after_limits["limits"]["memory_limit_mb"], 512);

        let usage = service.handle_get_resource_usage(pid).await.unwrap();
        assert_eq!(usage["pid"], pid);

        service.handle_suspend_process(pid).await.unwrap();
        assert_eq!(service.handle_get_process(pid).await.unwrap()["state"], "suspended");

        service.handle_resume_process(pid).await.unwrap();
        assert_eq!(service.handle_get_process(pid).await.unwrap()["state"], "running");

        service.handle_terminate_process(child_pid).await.unwrap();
        service.handle_terminate_process(pid).await.unwrap();
        assert!(service.handle_get_process(pid).await.is_err());
        assert_eq!(
            service.handle_list_processes().await.unwrap().as_array().unwrap().len(),
            0
        );
    }
}
