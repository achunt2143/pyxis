#![warn(rust_2018_idioms)]
// If no backend is enabled, a large portion of the codebase is unused.
// So silence this useless warning for the CI.
#![cfg_attr(
    not(any(feature = "winit", feature = "x11", feature = "udev")),
    allow(dead_code, unused_imports)
)]

#[cfg(any(feature = "udev", feature = "xwayland"))]
pub mod cursor;
pub mod anim;
pub mod drawing;
pub mod focus;
pub mod input_handler;
pub mod render;
pub mod shell;
pub mod state;
pub mod trust;

#[cfg(feature = "udev")]
pub mod udev;
#[cfg(feature = "winit")]
pub mod winit;
#[cfg(feature = "x11")]
pub mod x11;

pub use state::{EvoState, ClientState};

// === GECKOOS ENHANCEMENTS ===
// Conditionally compile enhancement modules
#[cfg(any(feature = "echo-ipc", feature = "decorations", feature = "screencopy"))]
pub mod enhancements;

// Re-export commonly used enhancements
#[cfg(feature = "echo-ipc")]
pub use enhancements::echo_ipc;

/// Format a structured compositor crash diagnostic report for `/var/log/evo-crash.log` and stderr.
pub fn format_crash_report(
    payload: &str,
    location: Option<(&str, u32, u32)>,
    backtrace: &std::backtrace::Backtrace,
) -> String {
    let ts_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    let loc = location
        .map(|(file, line, col)| format!("{file}:{line}:{col}"))
        .unwrap_or_else(|| "<unknown>".to_string());
    format!(
        "=== [EVO CRASH REPORT] ===\ntimestamp_unix_s: {ts_secs}\nthread: {thread_name}\nlocation: {loc}\nmessage: {payload}\nbacktrace:\n{backtrace}\n=== [END EVO CRASH REPORT] ===\n"
    )
}

/// Install a panic hook that writes structured crash diagnostics to stderr and
/// `EVO_CRASH_LOG` (defaulting to `/var/log/evo-crash.log`).
pub fn install_crash_handler() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".to_string()
        };
        let location = info
            .location()
            .map(|l| (l.file(), l.line(), l.column()));
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format_crash_report(&payload, location, &backtrace);
        eprintln!("{report}");
        let log_path = std::env::var("EVO_CRASH_LOG")
            .unwrap_or_else(|_| "/var/log/evo-crash.log".to_string());
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            use std::io::Write;
            let _ = file.write_all(report.as_bytes());
        }
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_report_formats_structured_diagnostics() {
        let bt = std::backtrace::Backtrace::disabled();
        let report = format_crash_report("boom", Some(("evo/src/state.rs", 42, 7)), &bt);
        assert!(report.contains("[EVO CRASH REPORT]"));
        assert!(report.contains("location: evo/src/state.rs:42:7"));
        assert!(report.contains("message: boom"));
        assert!(report.contains("backtrace:"));
    }
}

