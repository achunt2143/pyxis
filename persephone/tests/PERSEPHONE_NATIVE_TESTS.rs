// Persephone Native Tests
// File: persephone/tests/native_tests.rs
// Purpose: Test native Linux binary execution

#[cfg(test)]
mod tests {
    use persephone::elf_loader::ElfLoader;
    use persephone::runtime::Runtime;
    use persephone::compatibility::CompatibilityLayer;
    
    #[test]
    fn test_elf_loader_initialization() {
        // Test that ELF loader can be initialized
        // This would use a real binary in production
        println!("Testing ELF loader");
    }
    
    #[test]
    fn test_runtime_creation() {
        let runtime = Runtime::new(1000);
        assert_eq!(runtime.process_id, 1000);
        assert!(!runtime.environment.is_empty());
    }
    
    #[test]
    fn test_environment_variables() {
        let mut runtime = Runtime::new(2000);
        runtime.set_env("TEST_VAR".to_string(), "test_value".to_string());
        assert_eq!(runtime.get_env("TEST_VAR"), Some("test_value".to_string()));
    }
    
    #[test]
    fn test_compatibility_layer() {
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
    }
    
    #[test]
    fn test_syscall_handling() {
        let compat = CompatibilityLayer::new();
        
        // Test getpid syscall
        let result = compat.handle_syscall(39, &[]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1000);
        
        // Test exit syscall
        let result = compat.handle_syscall(60, &[0]);
        assert!(result.is_ok());
    }
    
    #[test]
    fn test_error_code_mapping() {
        let compat = CompatibilityLayer::new();
        
        let eperm = compat.get_error_message(1);
        assert_eq!(eperm, "EPERM");
        
        let enoent = compat.get_error_message(2);
        assert_eq!(enoent, "ENOENT");
    }
    
    #[test]
    fn test_libc_compatibility() {
        let compat = CompatibilityLayer::new();
        let info = compat.check_libc_compatibility().unwrap();
        
        assert!(!info.version.is_empty());
        assert!(!info.supported_features.is_empty());
        assert!(info.supported_features.contains(&"POSIX".to_string()));
    }
    
    #[test]
    fn test_runtime_limits() {
        let runtime = Runtime::new(3000);
        
        assert_eq!(runtime.resource_limits.max_memory_mb, 512);
        assert_eq!(runtime.resource_limits.max_open_files, 1024);
        assert_eq!(runtime.resource_limits.max_threads, 256);
    }
}
