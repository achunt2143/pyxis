//! Cronus CLI - Command Line Interface for Process Management
//!
//! Provides user-facing commands to interact with the Cronus daemon.

use clap::{Parser, Subcommand};
use geckoOS_cronus::{Cronus, ProcessLaunchConfig};
use std::collections::HashMap;

#[derive(Parser)]
#[command(name = "cronus", about = "GeckoOS Process Manager CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all running processes
    #[command(name = "ps")]
    List {
        /// Show detailed information
        #[arg(short, long)]
        detailed: bool,
    },

    /// Launch a new process
    #[command(name = "launch")]
    Launch {
        /// Application ID
        #[arg(short, long)]
        app: String,

        /// Executable path
        #[arg(short, long)]
        exec: String,

        /// Command-line arguments
        #[arg(short = 'a', long)]
        args: Vec<String>,

        /// Memory limit in MB
        #[arg(short, long, default_value = "256")]
        memory: u32,
    },

    /// Terminate a process gracefully
    #[command(name = "kill")]
    Kill {
        /// Process ID
        pid: u32,

        /// Force kill (SIGKILL instead of SIGTERM)
        #[arg(short, long)]
        force: bool,
    },

    /// Get detailed process information
    #[command(name = "info")]
    Info {
        /// Process ID
        pid: u32,
    },

    /// Show process tree hierarchy
    #[command(name = "tree")]
    Tree,

    /// Suspend a running process
    #[command(name = "suspend")]
    Suspend {
        /// Process ID
        pid: u32,
    },

    /// Resume a suspended process
    #[command(name = "resume")]
    Resume {
        /// Process ID
        pid: u32,
    },

    /// Show resource usage for a process
    #[command(name = "resources")]
    Resources {
        /// Process ID
        pid: u32,
    },

    /// Monitor all processes continuously
    #[command(name = "monitor")]
    Monitor {
        /// Refresh interval in milliseconds
        #[arg(short, long, default_value = "1000")]
        interval: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // Create Cronus instance (in production, this would connect to daemon via IPC)
    let cronus = Cronus::new().await?;

    match cli.command {
        Commands::List { detailed } => list_processes(&cronus, detailed).await?,
        Commands::Launch { app, exec, args, memory } => {
            launch_process(&cronus, &app, &exec, args, memory).await?
        }
        Commands::Kill { pid, force } => kill_process(&cronus, pid, force).await?,
        Commands::Info { pid } => show_process_info(&cronus, pid).await?,
        Commands::Tree => show_process_tree(&cronus).await?,
        Commands::Suspend { pid } => suspend_process(&cronus, pid).await?,
        Commands::Resume { pid } => resume_process(&cronus, pid).await?,
        Commands::Resources { pid } => show_resources(&cronus, pid).await?,
        Commands::Monitor { interval } => monitor_processes(&cronus, interval).await?,
    }

    Ok(())
}

async fn list_processes(cronus: &Cronus, detailed: bool) -> anyhow::Result<()> {
    let processes = cronus.list_processes();

    println!("Running Processes:");
    println!("{:<6} {:<30} {:<12} {:<12}", "PID", "App ID", "State", "Uptime");
    println!("{}", "-".repeat(70));

    for proc in &processes {
        if detailed {
            println!(
                "{:<6} {:<30} {:<12} {}s",
                proc.pid,
                proc.app_id,
                proc.state_string(),
                proc.uptime_seconds
            );
            println!("  Executable: {}", proc.executable);
            println!("  Created: {}", proc.created_at);
        } else {
            println!(
                "{:<6} {:<30} {:<12} {}s",
                proc.pid,
                proc.app_id,
                proc.state_string(),
                proc.uptime_seconds
            );
        }
    }

    println!("\nTotal: {} processes", processes.len());
    Ok(())
}

async fn launch_process(
    cronus: &Cronus,
    app: &str,
    exec: &str,
    args: Vec<String>,
    memory: u32,
) -> anyhow::Result<()> {
    println!("Launching process: {}", app);

    let config = ProcessLaunchConfig {
        app_id: app.to_string(),
        executable: exec.to_string(),
        args,
        env: HashMap::new(),
        working_dir: "/".to_string(),
        parent_pid: None,
        memory_limit_mb: memory,
        cpu_shares: 1024,
        io_priority: 4,
        nice: 0,
    };

    let pid = cronus.launch(config).await?;

    println!("✓ Process launched successfully");
    println!("  PID: {}", pid);
    println!("  App ID: {}", app);
    println!("  Memory Limit: {} MB", memory);

    Ok(())
}

async fn kill_process(cronus: &Cronus, pid: u32, force: bool) -> anyhow::Result<()> {
    println!("Terminating process: {}", pid);

    if force {
        cronus.kill(pid).await?;
        println!("✓ Process killed (SIGKILL)");
    } else {
        cronus.terminate(pid, libc::SIGTERM).await?;
        println!("✓ Process terminated (SIGTERM)");
    }

    Ok(())
}

async fn show_process_info(cronus: &Cronus, pid: u32) -> anyhow::Result<()> {
    let info = cronus
        .get_process_info(pid)
        .ok_or_else(|| anyhow::anyhow!("Process {} not found", pid))?;

    println!("Process Information:");
    println!("  PID: {}", info.pid);
    println!("  App ID: {}", info.app_id);
    println!("  Executable: {}", info.executable);
    println!("  State: {}", info.state_string());
    println!("  Created: {}", info.created_at);
    println!("  Uptime: {} seconds", info.uptime_seconds);

    // Show parent/children
    if let Some(parent) = cronus.get_parent(pid) {
        println!("  Parent PID: {}", parent);
    }

    let children = cronus.get_children(pid);
    if !children.is_empty() {
        println!("  Children PIDs: {:?}", children);
    }

    // Show resource usage
    if let Ok(usage) = cronus.get_resource_usage(pid) {
        println!("\nResource Usage:");
        println!("  Memory: {} MB", usage.memory_mb);
        println!("  CPU: {:.1}%", usage.cpu_percent);
        println!("  Threads: {}", usage.num_threads);
        println!("  File Descriptors: {}", usage.num_fds);
    }

    Ok(())
}

async fn show_process_tree(cronus: &Cronus) -> anyhow::Result<()> {
    let tree = cronus.get_process_tree();

    println!("Process Tree:");
    println!("{}", "-".repeat(70));

    // Find root processes
    let roots: Vec<_> = tree.iter().filter(|n| n.parent_pid.is_none()).collect();

    for root in roots {
        print_tree_node(root, &tree, 0);
    }

    Ok(())
}

fn print_tree_node(node: &geckoOS_cronus::ProcessNode, all_nodes: &[geckoOS_cronus::ProcessNode], depth: usize) {
    let indent = "  ".repeat(depth);
    println!("{}{} ({}) - PID: {}", indent, node.app_id, "Running", node.pid);

    for child_pid in &node.children {
        if let Some(child) = all_nodes.iter().find(|n| n.pid == *child_pid) {
            print_tree_node(child, all_nodes, depth + 1);
        }
    }
}

async fn suspend_process(cronus: &Cronus, pid: u32) -> anyhow::Result<()> {
    println!("Suspending process: {}", pid);
    cronus.suspend(pid).await?;
    println!("✓ Process suspended");
    Ok(())
}

async fn resume_process(cronus: &Cronus, pid: u32) -> anyhow::Result<()> {
    println!("Resuming process: {}", pid);
    cronus.resume(pid).await?;
    println!("✓ Process resumed");
    Ok(())
}

async fn show_resources(cronus: &Cronus, pid: u32) -> anyhow::Result<()> {
    let usage = cronus.get_resource_usage(pid)?;

    println!("Resource Usage for PID {}:", pid);
    println!("  Memory: {} MB", usage.memory_mb);
    println!("  CPU: {:.1}%", usage.cpu_percent);
    println!("  CPU Time: {} ms", usage.cpu_time_ms);
    println!("  I/O Read: {} bytes", usage.io_read_bytes);
    println!("  I/O Write: {} bytes", usage.io_write_bytes);
    println!("  Threads: {}", usage.num_threads);
    println!("  File Descriptors: {}", usage.num_fds);

    Ok(())
}

async fn monitor_processes(cronus: &Cronus, interval: u64) -> anyhow::Result<()> {
    println!("Monitoring processes (interval: {} ms, press Ctrl+C to stop)", interval);
    println!();

    loop {
        // Clear screen
        print!("\x1B[2J\x1B[1;1H");

        let processes = cronus.list_processes();
        let usage_stats = cronus.get_all_resource_usage();

        println!("Cronus Process Monitor");
        println!("{}", "=".repeat(80));
        println!(
            "{:<6} {:<25} {:<10} {:<8} {:<8} {:<8}",
            "PID", "App ID", "State", "CPU%", "Memory", "Threads"
        );
        println!("{}", "-".repeat(80));

        for proc in &processes {
            if let Some(usage) = usage_stats.iter().find(|u| u.pid == proc.pid) {
                println!(
                    "{:<6} {:<25} {:<10} {:<8.1} {:<8} {:<8}",
                    proc.pid,
                    proc.app_id,
                    proc.state_string(),
                    usage.cpu_percent,
                    format!("{}MB", usage.memory_mb),
                    usage.num_threads
                );
            }
        }

        println!();
        println!("Total Processes: {}", processes.len());

        tokio::time::sleep(tokio::time::Duration::from_millis(interval)).await;
    }
}
