// ============================================================================
// UI WEBSOCKET MODULE
// File: gaia/src/ui/websocket.rs
// Purpose: WebSocket communication for real-time updates
// ============================================================================

use actix_web_actors::ws;
use actix::prelude::*;
use std::time::{Duration, Instant};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct WsConnection {
    hb: Instant,
}

impl Actor for WsConnection {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.hb(ctx);
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsConnection {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            Ok(ws::Message::Pong(_)) => {
                self.hb = Instant::now();
            }
            Ok(ws::Message::Text(text)) => {
                let response = format!("Echo: {}", text);
                ctx.text(response);
            }
            Ok(ws::Message::Binary(bin)) => ctx.binary(bin),
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
                ctx.stop();
            }
            Ok(ws::Message::Continuation(_)) => {
                ctx.stop();
            }
            _ => ctx.stop(),
        }
    }
}

impl WsConnection {
    pub fn new() -> Self {
        WsConnection {
            hb: Instant::now(),
        }
    }

    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            if Instant::now().duration_since(act.hb) > CLIENT_TIMEOUT {
                ctx.stop();
                return;
            }

            ctx.ping(b"");
        });
    }
}

// ============================================================================
// SUPPORT MODULE
// File: gaia/src/support.rs
// Purpose: Helper utilities and common functions
// ============================================================================

use log::info;

pub fn initialize_logger() {
    env_logger::builder()
        .format_timestamp_millis()
        .init();

    info!("📝 Logger initialized");
}

pub fn format_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    format!("{}h {}m {}s", hours, minutes, seconds)
}

pub fn get_system_info() -> SystemInfo {
    SystemInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_count: num_cpus::get(),
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub arch: String,
    pub cpu_count: usize,
}

pub fn validate_path(path: &str) -> bool {
    !path.contains("..") && !path.contains("//")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(3661), "1h 1m 1s");
    }

    #[test]
    fn test_validate_path() {
        assert!(validate_path("/home/user/files"));
        assert!(!validate_path("../etc/passwd"));
        assert!(!validate_path("/home//user"));
    }
}
