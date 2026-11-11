//! pyxis-wlprobe — a Wayland test client for the trust-class suite
//! (`scripts/serial-tests/alpha5-wayland-trust.txt`). Not shipped for users.
//!
//!   pyxis-wlprobe globals              list every global this client can see
//!   pyxis-wlprobe bind NAME INTERFACE  bind registry name NAME as INTERFACE
//!   pyxis-wlprobe toplevel APP_ID SECS open a toplevel that claims APP_ID,
//!                                      and hold it for SECS seconds
//!
//! `bind` is the point: a global a client can't see must also be refused
//! when the client guesses its name, and the guess is the number a trusted
//! client's `globals` printed. A refusal is a protocol error from the
//! compositor, so it prints `BIND-REFUSED`; success prints `BIND-OK`.

use std::process::exit;
use std::time::{Duration, Instant};

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_compositor, wl_registry, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::wp::security_context::v1::client::wp_security_context_manager_v1::WpSecurityContextManagerV1;
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

struct State;

/// Interfaces whose events this client has no use for.
macro_rules! ignore_events {
    ($($iface:ty),* $(,)?) => {$(
        impl Dispatch<$iface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}
ignore_events!(
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    xdg_toplevel::XdgToplevel,
    WpSecurityContextManagerV1,
    ZwpVirtualKeyboardManagerV1,
    ZwlrDataControlManagerV1,
    ZwlrLayerShellV1,
    ZwlrScreencopyManagerV1,
);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let conn = Connection::connect_to_env().unwrap_or_else(|e| {
        eprintln!("CONNECT-FAILED {e}");
        exit(1);
    });
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let mut state = State;

    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["globals"] => globals.contents().with_list(|list| {
            for g in list {
                println!("GLOBAL {} {} v{}", g.name, g.interface, g.version);
            }
        }),
        ["bind", name, interface] => {
            let name: u32 = name.parse().expect("registry name");
            let registry = globals.registry();
            // Version 1 binds every one of these; what matters is whether
            // the compositor lets the bind stand.
            match *interface {
                "zwlr_screencopy_manager_v1" => {
                    registry.bind::<ZwlrScreencopyManagerV1, _, _>(name, 1, &qh, ());
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    registry.bind::<ZwpVirtualKeyboardManagerV1, _, _>(name, 1, &qh, ());
                }
                "zwlr_data_control_manager_v1" => {
                    registry.bind::<ZwlrDataControlManagerV1, _, _>(name, 1, &qh, ());
                }
                "wp_security_context_manager_v1" => {
                    registry.bind::<WpSecurityContextManagerV1, _, _>(name, 1, &qh, ());
                }
                "zwlr_layer_shell_v1" => {
                    registry.bind::<ZwlrLayerShellV1, _, _>(name, 1, &qh, ());
                }
                other => {
                    eprintln!("unknown interface {other}");
                    exit(2);
                }
            }
            match queue.roundtrip(&mut state) {
                Ok(_) => println!("BIND-OK {interface}"),
                Err(e) => println!("BIND-REFUSED {interface}: {e}"),
            }
        }
        ["toplevel", app_id, secs] => {
            let compositor: wl_compositor::WlCompositor =
                globals.bind(&qh, 1..=4, ()).expect("wl_compositor");
            let wm_base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=2, ()).expect("xdg_wm_base");
            let surface = compositor.create_surface(&qh, ());
            let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
            let toplevel = xdg.get_toplevel(&qh, ());
            toplevel.set_app_id(app_id.to_string());
            toplevel.set_title("pyxis-wlprobe".to_string());
            surface.commit();
            queue.roundtrip(&mut state).expect("roundtrip");
            println!("TOPLEVEL-OPEN {app_id}");
            let until = Instant::now() + Duration::from_secs(secs.parse().unwrap_or(5));
            while Instant::now() < until {
                if queue.roundtrip(&mut state).is_err() {
                    println!("TOPLEVEL-DROPPED");
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        _ => {
            eprintln!("usage: pyxis-wlprobe globals | bind NAME INTERFACE | toplevel APP_ID SECS");
            exit(2);
        }
    }
}
