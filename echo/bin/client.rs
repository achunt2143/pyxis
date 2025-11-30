// Echo IPC Client - CLI Tool
// File: echo/bin/client.rs
// Purpose: Command-line tool for Echo IPC interaction

use std::error::Error;
use structopt::StructOpt;
use echo::echo_client::EchoClient;

#[derive(StructOpt, Debug)]
#[structopt(name = "echo-cli", about = "Echo IPC Message Bus CLI")]
enum Command {
    /// List all registered services
    #[structopt(name = "services")]
    ListServices,
    
    /// Show service information
    #[structopt(name = "service-info")]
    ServiceInfo {
        /// Service name
        service: String,
    },
    
    /// Call a service method
    #[structopt(name = "call")]
    CallMethod {
        /// Service name
        service: String,
        
        /// Method name
        method: String,
        
        /// Arguments in JSON format
        #[structopt(default_value = "{}")]
        args: String,
    },
    
    /// Listen for signals from a service
    #[structopt(name = "listen")]
    Listen {
        /// Service name
        service: String,
        
        /// Signal name
        signal: String,
    },
    
    /// Emit a signal
    #[structopt(name = "emit")]
    Emit {
        /// Service name
        service: String,
        
        /// Signal name
        signal: String,
        
        /// Signal data in JSON format
        #[structopt(default_value = "{}")]
        data: String,
    },
    
    /// Get Echo bus status
    #[structopt(name = "status")]
    Status,
    
    /// Monitor IPC traffic
    #[structopt(name = "monitor")]
    Monitor,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let command = Command::from_args();
    
    let client = EchoClient::connect().await?;
    
    match command {
        Command::ListServices => list_services(&client).await?,
        Command::ServiceInfo { service } => service_info(&client, &service).await?,
        Command::CallMethod { service, method, args } => call_method(&client, &service, &method, &args).await?,
        Command::Listen { service, signal } => listen_signal(&client, &service, &signal).await?,
        Command::Emit { service, signal, data } => emit_signal(&client, &service, &signal, &data).await?,
        Command::Status => show_status(&client).await?,
        Command::Monitor => monitor_traffic(&client).await?,
    }
    
    Ok(())
}

async fn list_services(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    println!("Echo IPC Registered Services:");
    println!();
    
    let services = client.list_services().await?;
    
    for service in services {
        println!("📦 {}", service.name);
        println!("   PID: {}", service.pid);
        println!("   Methods: {}", service.methods.join(", "));
        println!("   Signals: {}", service.signals.join(", "));
        println!();
    }
    
    Ok(())
}

async fn service_info(client: &EchoClient, service: &str) -> Result<(), Box<dyn Error>> {
    let info = client.get_service_info(service).await?;
    
    println!("Service Information: {}", service);
    println!("═════════════════════════════════════");
    println!("PID: {}", info.pid);
    println!("Owner: {}", info.owner);
    println!("Created: {}", info.created_at);
    println!();
    println!("Methods:");
    for method in &info.methods {
        println!("  - {}", method);
    }
    println!();
    println!("Signals:");
    for signal in &info.signals {
        println!("  - {}", signal);
    }
    
    Ok(())
}

async fn call_method(client: &EchoClient, service: &str, method: &str, args: &str) -> Result<(), Box<dyn Error>> {
    println!("Calling {}.{}({})", service, method, args);
    
    let result = client.call_method(service, method, args).await?;
    
    println!("✓ Method call successful");
    println!("Response: {}", result);
    
    Ok(())
}

async fn listen_signal(client: &EchoClient, service: &str, signal: &str) -> Result<(), Box<dyn Error>> {
    println!("Listening for {}.{} signal...", service, signal);
    println!("Press Ctrl+C to stop");
    println!();
    
    let mut receiver = client.listen(service, signal).await?;
    
    while let Some(event) = receiver.recv().await {
        println!("📨 Signal received: {}", event.timestamp);
        println!("   Data: {}", event.data);
    }
    
    Ok(())
}

async fn emit_signal(client: &EchoClient, service: &str, signal: &str, data: &str) -> Result<(), Box<dyn Error>> {
    println!("Emitting {}.{}", service, signal);
    
    client.emit_signal(service, signal, data).await?;
    
    println!("✓ Signal emitted");
    
    Ok(())
}

async fn show_status(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    let status = client.get_status().await?;
    
    println!("Echo IPC Bus Status");
    println!("═══════════════════════════════════════");
    println!("Status: {}", if status.is_running { "Running ✓" } else { "Stopped ✗" });
    println!("Uptime: {}", status.uptime);
    println!("Connected Services: {}", status.service_count);
    println!("Total Messages: {}", status.message_count);
    println!("Active Connections: {}", status.connection_count);
    println!();
    println!("Top Services:");
    for (name, count) in &status.top_services {
        println!("  - {}: {} messages", name, count);
    }
    
    Ok(())
}

async fn monitor_traffic(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    println!("Monitoring Echo IPC Traffic (Ctrl+C to stop)");
    println!();
    
    let mut monitor = client.create_monitor().await?;
    
    println!("{:<35} {:<20} {:<15}", "Service", "Method/Signal", "Status");
    println!("{}", "-".repeat(70));
    
    while let Some(event) = monitor.next().await {
        match event.event_type {
            echo::MonitorEventType::MethodCall => {
                println!("{:<35} {:<20} {:<15}", event.service, event.method, "CALL");
            }
            echo::MonitorEventType::MethodReturn => {
                println!("{:<35} {:<20} {:<15}", event.service, event.method, "RETURN");
            }
            echo::MonitorEventType::Signal => {
                println!("{:<35} {:<20} {:<15}", event.service, event.signal, "SIGNAL");
            }
        }
    }
    
    Ok(())
}
