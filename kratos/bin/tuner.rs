// Performance Tuner - Interactive CLI Tool
// File: performance/bin/tuner.rs
// Purpose: Tune system performance interactively

use std::error::Error;
use std::io::{self, BufRead};
use structopt::StructOpt;
use performance::optimization::{OptimizationEngine, OptimizationProfile};

#[derive(StructOpt, Debug)]
#[structopt(name = "performance-tuner", about = "Interactive Performance Tuning")]
struct Args {
    /// Start in interactive mode
    #[structopt(short, long)]
    interactive: bool,
    
    /// Set profile (power, balanced, performance)
    #[structopt(short, long)]
    profile: Option<String>,
    
    /// Show recommendations
    #[structopt(short, long)]
    recommend: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::from_args();
    
    let engine = OptimizationEngine::new();
    
    if args.interactive {
        interactive_tuner(&engine).await?;
    } else if let Some(profile) = args.profile {
        engine.set_profile(&profile)?;
        println!("✓ Profile set to: {}", profile);
    } else if args.recommend {
        show_recommendations(&engine);
    } else {
        print_help();
    }
    
    Ok(())
}

async fn interactive_tuner(engine: &OptimizationEngine) -> Result<(), Box<dyn Error>> {
    println!();
    println!("╔════════════════════════════════════╗");
    println!("║   Performance Tuning System v2.0   ║");
    println!("╚════════════════════════════════════╝");
    println!();
    println!("Available commands:");
    println!("  profiles          - Show available profiles");
    println!("  set <profile>     - Set profile");
    println!("  metrics           - Show current metrics");
    println!("  recommend         - Get recommendations");
    println!("  tune <param>=<val> - Tune parameter");
    println!("  save <name>       - Save current profile");
    println!("  load <name>       - Load profile");
    println!("  help              - Show help");
    println!("  exit              - Exit tuner");
    println!();
    
    let stdin = io::stdin();
    let reader = stdin.lock();
    
    println!("tuner> ");
    
    for line in reader.lines() {
        let input = line?;
        let parts: Vec<&str> = input.trim().split_whitespace().collect();
        
        match parts.as_slice() {
            ["profiles"] => {
                println!("Available profiles:");
                for profile in engine.list_profiles() {
                    println!("  - {}", profile);
                }
            }
            ["set", profile] => {
                match engine.set_profile(profile) {
                    Ok(_) => println!("✓ Profile set to: {}", profile),
                    Err(e) => println!("✗ Error: {}", e),
                }
            }
            ["metrics"] => {
                let metrics = engine.get_metrics();
                println!("Current metrics:");
                println!("  CPU: {:.1}%", metrics.cpu_usage);
                println!("  Memory: {:.1}%", metrics.memory_usage);
                println!("  FPS: {}", metrics.fps);
                println!("  Cache hit rate: {:.1}%", metrics.cache_hit_rate);
                println!("  GPU utilization: {:.1}%", metrics.gpu_utilization);
            }
            ["recommend"] => {
                show_recommendations(engine);
            }
            ["tune", param_val] => {
                if let Some(idx) = param_val.find('=') {
                    let param = &param_val[..idx];
                    let value = &param_val[idx+1..];
                    println!("Tuning {} to {}", param, value);
                } else {
                    println!("Usage: tune <param>=<value>");
                }
            }
            ["save", name] => {
                println!("✓ Profile '{}' saved", name);
            }
            ["load", name] => {
                println!("✓ Profile '{}' loaded", name);
            }
            ["help"] => {
                println!("Available commands: profiles, set, metrics, recommend, tune, save, load, exit");
            }
            ["exit"] => {
                println!("Exiting tuner...");
                break;
            }
            [""] => {
                print!("tuner> ");
            }
            _ => {
                println!("Unknown command. Type 'help' for available commands.");
            }
        }
        
        print!("tuner> ");
    }
    
    Ok(())
}

fn show_recommendations(engine: &OptimizationEngine) {
    let recommendations = engine.get_recommendations();
    
    if recommendations.is_empty() {
        println!("✓ System is well-optimized");
    } else {
        println!("Recommendations:");
        for (i, rec) in recommendations.iter().enumerate() {
            println!("  {}. {}", i + 1, rec);
        }
    }
}

fn print_help() {
    println!("Performance Tuner v2.0");
    println!();
    println!("USAGE:");
    println!("  performance-tuner [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("  -i, --interactive    Enter interactive tuning mode");
    println!("  -p, --profile <name> Set optimization profile (power, balanced, performance)");
    println!("  -r, --recommend      Show tuning recommendations");
    println!("  -h, --help          Show this help message");
    println!();
    println!("EXAMPLES:");
    println!("  performance-tuner -i                     # Interactive mode");
    println!("  performance-tuner -p performance         # Set performance profile");
    println!("  performance-tuner -r                     # Show recommendations");
}
