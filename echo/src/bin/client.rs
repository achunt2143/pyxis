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

    /// Print signals as they arrive (one JSON line each: interface, signal,
    /// body), optionally only those named, for `seconds` or until `count`
    /// have been printed. For suites that need to see a signal go out.
    #[command(name = "watch")]
    Watch {
        /// Only this signal name (any if empty)
        #[arg(short, long, default_value = "")]
        signal: String,

        /// Only signals from this interface (any if empty)
        #[arg(short, long, default_value = "")]
        interface: String,

        /// Stop after this many seconds
        #[arg(long, default_value_t = 10)]
        seconds: u64,

        /// Stop after this many signals (0: no limit)
        #[arg(long, default_value_t = 0)]
        count: usize,
    },
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
        Commands::Watch { signal, interface, seconds, count } => {
            watch(&client, &signal, &interface, seconds, count).await?
        }
    }

    Ok(())
}

/// Calls `org.geckoOS.system.listServices`, not `org.geckoOS.Echo`'s
/// `ListServices`.
///
/// The latter reads `ServiceRegistry` alone, and nothing but an over-the-wire
/// `RegisterService` call ever writes to it -- so on a booted device this
/// printed "(no services registered)" while a dozen in-process services were
/// live and answering. `system.listServices` merges the router's handler
/// table with the registry, which is why the keys read here are `name`/
/// `destination`/`serviceId`/`registeredAt` and not the registry's
/// `service_name`/`service_id`/`registered_at`.
async fn list_services(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    println!("Echo IPC Services:");
    println!();

    let result = client.call_method(
        "org.geckoOS.system",
        "org.geckoOS.system",
        "listServices",
        serde_json::json!({}),
    ).await?;

    let services = result["services"].as_array()
        .ok_or("Invalid response format")?;

    if services.is_empty() {
        println!("  (no services on the bus)");
        return Ok(());
    }

    let mut reachable_count = 0usize;

    for service in services {
        let name = service["name"].as_str().unwrap_or("unknown");
        let destination = service["destination"].as_str().unwrap_or("unknown");
        let kind = service["kind"].as_str().unwrap_or("unknown");
        let reachable = service["reachable"].as_bool().unwrap_or(false);
        let methods = service["methods"].as_array().map_or(0, |m| m.len());
        let declared = service["declaredMethods"].as_array().map_or(0, |m| m.len());

        if reachable {
            reachable_count += 1;
        }

        // The registry is never pruned on disconnect, so a service that
        // registered and then died is still listed. Making that visible is
        // the point of this command, so it goes on the headline rather than
        // being left for the reader to infer from a missing field.
        println!(
            "{} {}  [{}]{}",
            if reachable { "📦" } else { "💀" },
            name,
            kind,
            if reachable { "" } else { "  <- registered but NOT reachable" },
        );
        println!("   Destination: {}", destination);

        // Two different facts, two different lines: `methods` is what the
        // router will dispatch right now; `declaredMethods` is only what an
        // external client claimed in its RegisterService interface.
        if methods > 0 {
            println!("   Methods: {} dispatchable", methods);
        }
        if declared > 0 {
            println!("   Declared: {} (from its RegisterService interface)", declared);
        }
        if let Some(id) = service["serviceId"].as_str() {
            println!("   ID: {}", id);
        }
        if let Some(registered_at) = service["registeredAt"].as_str() {
            println!("   Registered: {}", registered_at);
        }
        println!();
    }

    println!(
        "Total: {} services ({} reachable, {} unreachable)",
        services.len(),
        reachable_count,
        services.len() - reachable_count,
    );
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
        "org.geckoOS.system",
        "org.geckoOS.system",
        "listServices",
        serde_json::json!({}),
    ).await?;

    let empty = Vec::new();
    let services = services_result["services"].as_array().unwrap_or(&empty);
    let total = services.len();
    let reachable: Vec<&serde_json::Value> = services
        .iter()
        .filter(|s| s["reachable"].as_bool().unwrap_or(false))
        .collect();

    println!("Echo IPC Bus Status");
    println!("═══════════════════════════════════════");
    println!("Status: Running ✓");
    // Deliberately two numbers rather than one "Registered Services: N".
    // "Registered" used to mean the registry alone, and a single count cannot
    // distinguish a live service from one whose registry entry outlived its
    // connection -- so name both and let the difference show.
    println!("Services: {} on the bus, {} reachable", total, reachable.len());
    println!();

    if !reachable.is_empty() {
        println!("Reachable:");
        for service in &reachable {
            let name = service["name"].as_str().unwrap_or("unknown");
            let kind = service["kind"].as_str().unwrap_or("unknown");
            let methods = service["methods"].as_array().map_or(0, |m| m.len());
            println!("  • {} [{}], {} methods", name, kind, methods);
        }
        println!();
    }

    if total > reachable.len() {
        println!("Registered but NOT reachable:");
        for service in services {
            if service["reachable"].as_bool().unwrap_or(false) {
                continue;
            }
            let name = service["name"].as_str().unwrap_or("unknown");
            let registered_at = service["registeredAt"].as_str().unwrap_or("unknown");
            println!("  ✗ {} (registered {})", name, registered_at);
        }
        println!();
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

/// A round-trip timing, so it calls the one method with no work behind it.
///
/// This used to call `ListServices` "as a ping", which timed a registry walk;
/// pointing it at `system.listServices` would have been worse still, timing a
/// scan of every handler on the bus plus a connection lookup per external
/// service. Neither is the daemon responsiveness the printed number claims to
/// report, so `system.ping` exists to be the cheap thing.
async fn ping(client: &EchoClient) -> Result<(), Box<dyn Error>> {
    println!("Pinging Echo daemon...");

    let start = std::time::Instant::now();

    let _ = client.call_method(
        "org.geckoOS.system",
        "org.geckoOS.system",
        "ping",
        serde_json::json!({}),
    ).await?;

    let elapsed = start.elapsed();

    println!("✓ Pong! ({:.2}ms)", elapsed.as_secs_f64() * 1000.0);
    Ok(())
}

async fn watch(
    client: &EchoClient,
    signal: &str,
    interface: &str,
    seconds: u64,
    count: usize,
) -> Result<(), Box<dyn Error>> {
    use geckoOS_echo::message::MessageType;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut seen = 0;
    loop {
        let msg = match tokio::time::timeout_at(deadline, client.next_message()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => break,
        };
        if msg.header.message_type != MessageType::Signal
            || (!signal.is_empty() && msg.header.method != signal)
            || (!interface.is_empty() && msg.header.interface != interface)
        {
            continue;
        }
        println!(
            "{}",
            serde_json::json!({ "interface": msg.header.interface, "signal": msg.header.method, "body": msg.body })
        );
        seen += 1;
        if count > 0 && seen >= count {
            break;
        }
    }
    Ok(())
}
