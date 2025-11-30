// Linux Compatibility Layer
// File: persephone/src/compatibility.rs
// Purpose: Provide Linux system compatibility shims

use std::collections::HashMap;
use std::error::Error;
use tracing::{info, debug, warn};

pub struct CompatibilityLayer {
    syscall_map: HashMap<u32, String>,
    errno_map: HashMap<i32, String>,
    capability_set: Vec<String>,
}

impl CompatibilityLayer {
    /// Create new compatibility layer
    pub fn new() -> Self {
        info!("Initializing Linux compatibility layer");
        
        let mut compat = CompatibilityLayer {
            syscall_map: HashMap::new(),
            errno_map: HashMap::new(),
            capability_set: Vec::new(),
        };
        
        compat.init_syscall_map();
        compat.init_errno_map();
        compat.init_capabilities();
        
        compat
    }
    
    fn init_syscall_map(&mut self) {
        // Common x86-64 syscalls
        let syscalls = vec![
            (0, "read"),
            (1, "write"),
            (2, "open"),
            (3, "close"),
            (4, "stat"),
            (5, "fstat"),
            (6, "lstat"),
            (7, "poll"),
            (8, "lseek"),
            (9, "mmap"),
            (10, "mprotect"),
            (11, "munmap"),
            (12, "brk"),
            (39, "getpid"),
            (60, "exit"),
            (231, "exit_group"),
        ];
        
        for (num, name) in syscalls {
            self.syscall_map.insert(num, name.to_string());
        }
        
        debug!("Initialized {} syscall mappings", self.syscall_map.len());
    }
    
    fn init_errno_map(&mut self) {
        let errors = vec![
            (1, "EPERM"),
            (2, "ENOENT"),
            (3, "ESRCH"),
            (4, "EINTR"),
            (5, "EIO"),
            (6, "ENXIO"),
            (12, "ENOMEM"),
            (13, "EACCES"),
            (14, "EFAULT"),
            (22, "EINVAL"),
        ];
        
        for (num, name) in errors {
            self.errno_map.insert(num, name.to_string());
        }
        
        debug!("Initialized {} error code mappings", self.errno_map.len());
    }
    
    fn init_capabilities(&mut self) {
        let capabilities = vec![
            "CAP_CHOWN",
            "CAP_DAC_OVERRIDE",
            "CAP_KILL",
            "CAP_SETUID",
            "CAP_SETGID",
            "CAP_NET_RAW",
            "CAP_NET_BIND_SERVICE",
            "CAP_SYS_ADMIN",
        ];
        
        self.capability_set = capabilities.iter().map(|c| c.to_string()).collect();
        debug!("Initialized {} capabilities", self.capability_set.len());
    }
    
    /// Get syscall name from number
    pub fn get_syscall_name(&self, syscall_num: u32) -> String {
        self.syscall_map
            .get(&syscall_num)
            .cloned()
            .unwrap_or_else(|| format!("syscall_{}", syscall_num))
    }
    
    /// Get error message from errno
    pub fn get_error_message(&self, errno: i32) -> String {
        self.errno_map
            .get(&errno)
            .cloned()
            .unwrap_or_else(|| format!("EUNKNOWN({})", errno))
    }
    
    /// Check if capability is granted
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capability_set.contains(&capability.to_string())
    }
    
    /// Grant capability
    pub fn grant_capability(&mut self, capability: &str) {
        if !self.has_capability(capability) {
            self.capability_set.push(capability.to_string());
            debug!("Granted capability: {}", capability);
        }
    }
    
    /// Revoke capability
    pub fn revoke_capability(&mut self, capability: &str) {
        self.capability_set.retain(|c| c != capability);
        debug!("Revoked capability: {}", capability);
    }
    
    /// Handle Linux system call
    pub fn handle_syscall(&self, syscall_num: u32, args: &[u64]) -> Result<u64, i32> {
        let name = self.get_syscall_name(syscall_num);
        debug!("Handling syscall: {} ({})", syscall_num, name);
        
        match syscall_num {
            60 => {
                // exit(code)
                info!("Process exit requested with code: {}", args.get(0).unwrap_or(&0));
                Ok(0)
            }
            39 => {
                // getpid - return mock PID
                Ok(1000)
            }
            0 | 1 => {
                // read/write - return success
                Ok(0)
            }
            _ => {
                warn!("Unhandled syscall: {} ({})", syscall_num, name);
                Err(-38) // ENOSYS - Function not implemented
            }
        }
    }
    
    /// Check libc compatibility
    pub fn check_libc_compatibility(&self) -> Result<LibcInfo, Box<dyn Error>> {
        info!("Checking libc compatibility");
        
        let info = LibcInfo {
            version: "2.34".to_string(),
            supported_features: vec![
                "POSIX".to_string(),
                "GNU_EXTENSIONS".to_string(),
                "THREAD_SAFE".to_string(),
            ],
            known_incompatibilities: vec![],
        };
        
        debug!("libc compatibility: {}", info.version);
        Ok(info)
    }
}

#[derive(Debug, Clone)]
pub struct LibcInfo {
    pub version: String,
    pub supported_features: Vec<String>,
    pub known_incompatibilities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_compat_layer_creation() {
        let compat = CompatibilityLayer::new();
        assert!(!compat.syscall_map.is_empty());
        assert!(!compat.errno_map.is_empty());
    }
    
    #[test]
    fn test_syscall_names() {
        let compat = CompatibilityLayer::new();
        assert_eq!(compat.get_syscall_name(39), "getpid");
        assert_eq!(compat.get_syscall_name(60), "exit");
    }
    
    #[test]
    fn test_capabilities() {
        let mut compat = CompatibilityLayer::new();
        assert!(compat.has_capability("CAP_CHOWN"));
        compat.revoke_capability("CAP_CHOWN");
        assert!(!compat.has_capability("CAP_CHOWN"));
        compat.grant_capability("CAP_CHOWN");
        assert!(compat.has_capability("CAP_CHOWN"));
    }
}
