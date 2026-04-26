//! Echo IPC Client - CLI Tool
//!
//! Command-line interface for interacting with the Echo IPC bus.

use std::error::Error;
use clap::{Parser, Subcommand};
use geckoOS_echo::{EchoClient, ServiceInterface, MethodDefinition, SignalDefinition, ArgumentDefinition};

#[derive(Parser)]
#[command(name = "echo-cli", about = "Echo IPC Message Bus CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all registered services
    #[command(name = "services")]
    ListServices,

    /// Show service information
    #[command(name = "info")]
    ServiceInfo {
        /// Service name
        service: String,
    },

    /// Call a service method
    #[command(name = "call")]
    CallMethod {
        /// Service name
        service: String,

        /// Interface name
        #[arg(short, long, default_value = "")]
        interface: String,

        /// Method name
        method: String,

        /// Arguments in JSON format
        #[arg(short, long, default_value = "{}")]
        args: String,
    },

    /// Emit a signal
    #[command(name = "emit")]
    EmitSignal {
        /// Interface name
        interface: String,

        /// Signal name
        signal: String,

        /// Signal data in JSON format
        #[arg(short, long, default_value = "{}")]
        data: String,
    },

    /// Get Echo bus status
    #[command(name = "status")]
    Status,

    /// Register a test service
    #[command(name = "register")]
    RegisterService {
        /// Service name
        name: String,
    },

    /// Ping the Echo daemon
    #[command(name = "ping")]
    Ping,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();

    let cli = Cli::parse();

    let client = match EchoClient::connect().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Failed to connect to Echo daemon: {}", e);
            eprintln!("   Make sure echo-daemon is running.");
            std::process::exit(1);
        }
    };

    match cli.command {
        Commands::ListServices => list_services(&client).await?,
        Commands::ServiceInfo { service } => service_info(&client, &service).await?,
        Commands::CallMethod { service, interface, method, args } => {
            call_method(&client, &service, &interface, &method, &args).await?
        }
        Commands::EmitSignal { interface, signal, data } => {
            emit_signal(&client, &interface, &signal, &data).await?
        }
        Commands::Status => show_status(&client).await?,
        Commands::RegisterService { name } => register_service(&client, &name).await?,
        Commands::Ping => ping(&client).await?,
    }

    Ok(())
}

async fn list_services(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    println!("Echo IPC Registered Services:");
    println!();

    let result = client.call_method(
        "org.geckoOS.Echo",
        "Bus",
        "ListServices",
        serde_json::json!({}),
    ).await?;

    let services = result["services"].as_array()
        .ok_or("Invalid response format")?;

    if services.is_empty() {
        println!("  (no services registered)");
    } else {
        for service in services {
            let name = service["service_name"].as_str().unwrap_or("unknown");
            let id = service["service_id"].as_str().unwrap_or("unknown");
            let registered_at = service["registered_at"].as_str().unwrap_or("unknown");
            
            println!("📦 {}", name);
            println!("   ID: {}", id);
            println!("   Registered: {}", registered_at);
            println!();
        }
    }

    println!("Total: {} services", services.len());
    Ok(())
}

async fn service_info(client: &EchoClient, service: &str) -> Result<(), Box<dyn Error>> {
    let result = client.call_method(
        "org.geckoOS.Echo",
        "Bus",
        "GetServiceInfo",
        serde_json::json!({
            "service_name": service
        }),
    ).await?;

    println!("Service Information: {}", service);
    println!("═════════════════════════════════════");
    println!("Service ID: {}", result["service_id"].as_str().unwrap_or("unknown"));
    println!("Registered: {}", result["registered_at"].as_str().unwrap_or("unknown"));
    println!();

    if let Some(interface) = result.get("interface") {
        if let Some(methods) = interface["methods"].as_array() {
            if !methods.is_empty() {
                println!("Methods:");
                for method in methods {
                    let name = method["name"].as_str().unwrap_or("unknown");
                    println!("  • {}", name);
                }
                println!();
            }
        }

        if let Some(signals) = interface["signals"].as_array() {
            if !signals.is_empty() {
                println!("Signals:");
                for signal in signals {
                    let name = signal["name"].as_str().unwrap_or("unknown");
                    println!("  • {}", name);
                }
                println!();
            }
        }
    }

    Ok(())
}

async fn call_method(
    client: &EchoClient,
    service: &str,
    interface: &str,
    method: &str,
    args: &str,
) -> Result<(), Box<dyn Error>> {
    let interface = if interface.is_empty() {
        service.to_string()
    } else {
        interface.to_string()
    };

    println!("Calling {}.{}.{}({}) ...", service, interface, method, args);

    let args_json: serde_json::Value = serde_json::from_str(args)?;

    let result = client.call_method(service, &interface, method, args_json).await?;

    println!("✓ Method call successful");
    println!();
    println!("Response:");
    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

async fn emit_signal(
    client: &EchoClient,
    interface: &str,
    signal: &str,
    data: &str,
) -> Result<(), Box<dyn Error>> {
    println!("Emitting {}.{} ...", interface, signal);

    let data_json: serde_json::Value = serde_json::from_str(data)?;
    client.emit_signal(interface, signal, data_json).await?;

    println!("✓ Signal emitted");
    Ok(())
}

async fn show_status(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    let services_result = client.call_method(
        "org.geckoOS.Echo",
        "Bus",
        "ListServices",
        serde_json::json!({}),
    ).await?;

    let service_count = services_result["count"].as_u64().unwrap_or(0);

    println!("Echo IPC Bus Status");
    println!("═══════════════════════════════════════");
    println!("Status: Running ✓");
    println!("Registered Services: {}", service_count);
    println!();

    if service_count > 0 {
        println!("Active Services:");
        if let Some(services) = services_result["services"].as_array() {
            for service in services {
                let name = service["service_name"].as_str().unwrap_or("unknown");
                println!("  • {}", name);
            }
        }
    }

    Ok(())
}

async fn register_service(client: &EchoClient, name: &str) -> Result<(), Box<dyn Error>> {
    println!("Registering test service: {}", name);

    let interface = ServiceInterface {
        name: name.to_string(),
        methods: vec![
            MethodDefinition {
                name: "TestMethod".to_string(),
                input_args: vec![
                    ArgumentDefinition {
                        name: "arg1".to_string(),
                        arg_type: "string".to_string(),
                    }
                ],
                output_args: vec![
                    ArgumentDefinition {
                        name: "result".to_string(),
                        arg_type: "string".to_string(),
                    }
                ],
            }
        ],
        signals: vec![
            SignalDefinition {
                name: "TestSignal".to_string(),
                arguments: vec![],
            }
        ],
        properties: vec![],
    };

    let result = client.call_method(
        "org.geckoOS.Echo",
        "Bus",
        "RegisterService",
        serde_json::json!({
            "name": name,
            "interface": interface,
        }),
    ).await?;

    println!("✓ Service registered");
    println!("  Service ID: {}", result["service_id"].as_str().unwrap_or("unknown"));
    println!("  Status: {}", result["status"].as_str().unwrap_or("unknown"));

    Ok(())
}

async fn ping(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    println!("Pinging Echo daemon...");

    let start = std::time::Instant::now();
    
    // Try to list services as a ping
    let _ = client.call_method(
        "org.geckoOS.Echo",
        "Bus",
        "ListServices",
        serde_json::json!({}),
    ).await?;

    let elapsed = start.elapsed();

    println!("✓ Pong! ({:.2}ms)", elapsed.as_secs_f64() * 1000.0);
    Ok(())
}
