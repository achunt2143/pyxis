// Cronus CLI - Command Line Interface
// File: cronus/bin/cli.rs
// Purpose: Command-line tool for process management

use std::env;
use std::error::Error;
use structopt::StructOpt;
use cronus::process_manager::ProcessManager;

#[derive(StructOpt, Debug)]
#[structopt(name = "cronus", about = "Process Manager CLI")]
enum Command {
    /// List all running processes
    #[structopt(name = "ps")]
    List {
        /// Show detailed information
        #[structopt(short, long)]
        detailed: bool,
    },
    
    /// Create new process
    #[structopt(name = "run")]
    Create {
        /// Application ID or path
        #[structopt(short, long)]
        app: String,
        
        /// Priority level (low, normal, high)
        #[structopt(short, long, default_value = "normal")]
        priority: String,
    },
    
    /// Terminate process
    #[structopt(name = "kill")]
    Terminate {
        /// Process ID
        pid: u32,
        
        /// Force kill
        #[structopt(short)]
        force: bool,
    },
    
    /// Get resource usage
    #[structopt(name = "info")]
    Info {
        /// Process ID
        pid: u32,
    },
    
    /// Set resource limits
    #[structopt(name = "limit")]
    SetLimit {
        /// Process ID
        pid: u32,
        
        /// CPU limit (percentage)
        #[structopt(long)]
        cpu: Option<f32>,
        
        /// Memory limit (MB)
        #[structopt(long)]
        memory: Option<u32>,
    },
    
    /// Monitor processes
    #[structopt(name = "monitor")]
    Monitor {
        /// Refresh interval (ms)
        #[structopt(short, long, default_value = "1000")]
        interval: u64,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let command = Command::from_args();
    
    match command {
        Command::List { detailed } => list_processes(detailed).await?,
        Command::Create { app, priority } => create_process(&app, &priority).await?,
        Command::Terminate { pid, force } => terminate_process(pid, force).await?,
        Command::Info { pid } => get_process_info(pid).await?,
        Command::SetLimit { pid, cpu, memory } => set_resource_limit(pid, cpu, memory).await?,
        Command::Monitor { interval } => monitor_processes(interval).await?,
    }
    
    Ok(())
}

async fn list_processes(detailed: bool) -> Result<(), Box<dyn Error>> {
    let pm = ProcessManager::connect().await?;
    let processes = pm.list_processes().await?;
    
    println!("Running Processes:");
    println!("{:<6} {:<30} {:<10} {:<8}", "PID", "Name", "Status", "Memory");
    println!("{}", "-".repeat(60));
    
    for proc in processes {
        if detailed {
            println!(
                "{:<6} {:<30} {:<10} {:<8} CPU: {:.1}%",
                proc.pid, proc.name, proc.status, proc.memory_mb, proc.cpu_percent
            );
        } else {
            println!(
                "{:<6} {:<30} {:<10} {:<8}",
                proc.pid, proc.name, proc.status, proc.memory_mb
            );
        }
    }
    
    Ok(())
}

async fn create_process(app: &str, priority: &str) -> Result<(), Box<dyn Error>> {
    let pm = ProcessManager::connect().await?;
    
    println!("Creating process for: {}", app);
    
    let pid = pm.create_process(app, priority).await?;
    
    println!("✓ Process created");
    println!("  PID: {}", pid);
    println!("  Priority: {}", priority);
    
    Ok(())
}

async fn terminate_process(pid: u32, force: bool) -> Result<(), Box<dyn Error>> {
    let pm = ProcessManager::connect().await?;
    
    println!("Terminating process: {}", pid);
    
    pm.terminate_process(pid, force).await?;
    
    println!("✓ Process terminated");
    
    Ok(())
}

async fn get_process_info(pid: u32) -> Result<(), Box<dyn Error>> {
    let pm = ProcessManager::connect().await?;
    
    let info = pm.get_process_info(pid).await?;
    
    println!("Process Information:");
    println!("  PID: {}", info.pid);
    println!("  Name: {}", info.name);
    println!("  Status: {}", info.status);
    println!("  CPU: {:.1}%", info.cpu_percent);
    println!("  Memory: {} MB", info.memory_mb);
    println!("  Start Time: {}", info.start_time);
    println!("  Uptime: {}", info.uptime);
    
    Ok(())
}

async fn set_resource_limit(pid: u32, cpu: Option<f32>, memory: Option<u32>) -> Result<(), Box<dyn Error>> {
    let pm = ProcessManager::connect().await?;
    
    println!("Setting resource limits for PID: {}", pid);
    
    if let Some(cpu_limit) = cpu {
        pm.set_cpu_limit(pid, cpu_limit).await?;
        println!("✓ CPU limit set to {:.1}%", cpu_limit);
    }
    
    if let Some(mem_limit) = memory {
        pm.set_memory_limit(pid, mem_limit).await?;
        println!("✓ Memory limit set to {} MB", mem_limit);
    }
    
    Ok(())
}

async fn monitor_processes(interval: u64) -> Result<(), Box<dyn Error>> {
    let pm = ProcessManager::connect().await?;
    
    println!("Monitoring processes (interval: {} ms, press Ctrl+C to stop)", interval);
    println!();
    
    loop {
        // Clear screen
        print!("\x1B[2J\x1B[1;1H");
        
        let processes = pm.list_processes().await?;
        
        println!("Running Processes:");
        println!("{:<6} {:<30} {:<10} {:<10} {:<8}", "PID", "Name", "CPU%", "Memory", "Status");
        println!("{}", "-".repeat(65));
        
        for proc in processes {
            println!(
                "{:<6} {:<30} {:<10} {:<10} {:<8}",
                proc.pid, proc.name, format!("{:.1}%", proc.cpu_percent), 
                format!("{}MB", proc.memory_mb), proc.status
            );
        }
        
        tokio::time::sleep(tokio::time::Duration::from_millis(interval)).await;
    }
}
