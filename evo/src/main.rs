use tracing::{error, info};


static POSSIBLE_BACKENDS: &[&str] = &[
    #[cfg(feature = "winit")]
    "--winit : Run Evo as a X11 or Wayland client using winit.",
    #[cfg(feature = "udev")]
    "--tty-udev : Run Evo as a tty udev client (requires root if without logind).",
    #[cfg(feature = "x11")]
    "--x11 : Run Evo as an X11 client.",
];

#[cfg(feature = "profile-with-tracy-mem")]
#[global_allocator]
static GLOBAL: profiling::tracy_client::ProfiledAllocator<std::alloc::System> =
    profiling::tracy_client::ProfiledAllocator::new(std::alloc::System, 10);

fn main() {
    geckoOS_evo::install_crash_handler();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    // Initialize logging
    // if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
    //     tracing_subscriber::fmt()
    //         .compact()
    //         .with_env_filter(env_filter)
    //         .init();
    // } else {
    //     tracing_subscriber::fmt().compact().init();
    // }

    // Print GeckoOS banner
    print_gecko_banner();

    let wayland_display = std::env::var("WAYLAND_DISPLAY")
    .unwrap_or_else(|_| "wayland-0".to_string());
info!("Evo WAYLAND_DISPLAY={}", wayland_display);


std::env::set_var("WAYLAND_DISPLAY", &wayland_display);

info!("Wayland socket: {}", wayland_display);

    // Echo IPC is set up by EvoState (state.rs), which owns the service for
    // the compositor's lifetime. A second instance used to be created here,
    // registered as `org.geckoOS.evo`, and dropped a line later. The comment
    // on it claimed its broker kept running, but a dropped service closes its
    // command channel, the broker shuts down, and its connection closes.
    // All that bought was the same name registered twice, on two
    // connections, one of them already closing.

    // Initialize profiling if enabled
    #[cfg(feature = "profile-with-tracy")]
    profiling::tracy_client::Client::start();

    #[cfg(feature = "profile-with-tracy")]
    profiling::register_thread!("Main Thread");

    #[cfg(feature = "profile-with-puffin")]
    let _server =
        puffin_http::Server::new(&format!("0.0.0.0:{}", puffin_http::DEFAULT_PORT)).unwrap();

    #[cfg(feature = "profile-with-puffin")]
    profiling::puffin::set_scopes_on(true);

    // Select and run backend
    let arg = ::std::env::args().nth(1);
    match arg.as_ref().map(|s| &s[..]) {
        #[cfg(feature = "winit")]
        Some("--winit") => {
            info!("Starting GeckoOS Ouranos with winit backend");
            geckoOS_evo::winit::run_winit();
        }

        #[cfg(feature = "udev")]
        Some("--tty-udev") => {
            info!("Starting GeckoOS Ouranos on a tty using udev");
            geckoOS_evo::udev::run_udev();
        }

        #[cfg(feature = "x11")]
        Some("--x11") => {
            info!("Starting GeckoOS Ouranos with x11 backend");
            geckoOS_evo::x11::run_x11();
        }

        Some(other) => {
            error!("Unknown backend: {}", other);
            print_usage();
        }

        None => {
            print_usage();
        }
    }
}

fn print_gecko_banner() {
    println!("╔═══════════════════════════════════════════════════════╗");
    println!("║         GeckoOS Evo - Wayland Compositor              ║");
    println!("║                 Built on Smithay                      ║");
    println!("╚═══════════════════════════════════════════════════════╝");
    println!();
}

fn print_usage() {
    println!();
    println!("USAGE: geckoOS_ouranos --backend");
    println!();
    println!("Possible backends are:");
    for backend in POSSIBLE_BACKENDS {
        println!("\t{backend}");
    }
    println!();
    
    #[cfg(feature = "echo-ipc")]
    println!("✨ Echo IPC support: ENABLED");
    
    #[cfg(feature = "xwayland")]
    println!("✨ XWayland support: ENABLED");
    
    println!();
}
