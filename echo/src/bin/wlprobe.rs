//! pyxis-wlprobe — a Wayland test client for the trust-class suite
//! (`scripts/serial-tests/alpha5-wayland-trust.txt`). Not shipped for users.
//!
//!   pyxis-wlprobe globals              list every global this client can see
//!   pyxis-wlprobe bind NAME INTERFACE  bind registry name NAME as INTERFACE
//!   pyxis-wlprobe toplevel APP_ID SECS open a toplevel that claims APP_ID,
//!                                      and hold it for SECS seconds
//!   pyxis-wlprobe window APP_ID SECS   the same, mapped: a buffer, server-side
//!                                      decoration, title "pyxis-wlprobe"
//!
//! `--no-pong` (after the command) stops answering `xdg_wm_base.ping`, so
//! Evo marks the client busy and draws the spinner in its title bar
//! (`scripts/serial-tests/alpha5-identity-evo.txt`). `--pong-after SECS`
//! holds the pings for SECS seconds, then answers them, so the busy state
//! can be seen to clear.
//!
//! `bind` is the point: a global a client can't see must also be refused
//! when the client guesses its name, and the guess is the number a trusted
//! client's `globals` printed. A refusal is a protocol error from the
//! compositor, so it prints `BIND-REFUSED`; success prints `BIND-OK`.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::process::exit;
use std::time::{Duration, Instant};

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::wp::security_context::v1::client::wp_security_context_manager_v1::WpSecurityContextManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::{zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

struct State {
    /// Answer pings (off with `--no-pong`, and until `--pong-after`).
    pong: bool,
    /// Pings held unanswered, to answer when `--pong-after` comes round.
    held: Vec<(xdg_wm_base::XdgWmBase, u32)>,
    /// Pings seen, answered or not.
    pings: u32,
    /// The size the last configure asked for (0: the client's choice).
    size: Option<(i32, i32)>,
    /// A configure has arrived and wants a fresh buffer.
    configured: bool,
}

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
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
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
impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state.size = Some((width, height));
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        state: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            state.pings += 1;
            if state.pong {
                base.pong(serial);
            } else {
                println!("PING-IGNORED {}", state.pings);
                state.held.push((base.clone(), serial));
            }
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

/// A `w`×`h` buffer of one colour, in a memfd.
fn solid_buffer(shm: &wl_shm::WlShm, qh: &QueueHandle<State>, w: i32, h: i32, argb: u32) -> wl_buffer::WlBuffer {
    let stride = w * 4;
    let len = (stride * h) as usize;
    // SAFETY: memfd_create returns a fresh fd we own, or -1.
    let raw = unsafe { libc::memfd_create(c"pyxis-wlprobe".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw >= 0, "memfd_create");
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let file = std::fs::File::from(fd);
    let pixels: Vec<u8> = std::iter::repeat_n(argb.to_le_bytes(), (w * h) as usize).flatten().collect();
    std::io::Write::write_all(&mut &file, &pixels).expect("write buffer");
    let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
    let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    buffer
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let no_pong = args.iter().any(|a| a == "--no-pong");
    args.retain(|a| a != "--no-pong");
    let pong_after = args.iter().position(|a| a == "--pong-after").map(|i| {
        let secs: u64 = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(10);
        args.drain(i..(i + 2).min(args.len()));
        Instant::now() + Duration::from_secs(secs)
    });
    let conn = Connection::connect_to_env().unwrap_or_else(|e| {
        eprintln!("CONNECT-FAILED {e}");
        exit(1);
    });
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let mut state =
        State { pong: !no_pong && pong_after.is_none(), held: Vec::new(), pings: 0, size: None, configured: false };

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
        ["window", app_id, secs] => {
            let compositor: wl_compositor::WlCompositor =
                globals.bind(&qh, 1..=4, ()).expect("wl_compositor");
            let wm_base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=2, ()).expect("xdg_wm_base");
            let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
            let surface = compositor.create_surface(&qh, ());
            let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
            let toplevel = xdg.get_toplevel(&qh, ());
            toplevel.set_app_id(app_id.to_string());
            toplevel.set_title("pyxis-wlprobe".to_string());
            // Ask for Evo's frame, so the title bar (and its spinner) is drawn.
            match globals.bind::<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, _, _>(&qh, 1..=1, ()) {
                Ok(manager) => manager
                    .get_toplevel_decoration(&toplevel, &qh, ())
                    .set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide),
                Err(_) => println!("NO-XDG-DECORATION"),
            }
            surface.commit();
            let until = Instant::now() + Duration::from_secs(secs.parse().unwrap_or(5));
            let mut drawn: Option<(i32, i32)> = None;
            let mut announced = false;
            while Instant::now() < until {
                if queue.roundtrip(&mut state).is_err() {
                    println!("TOPLEVEL-DROPPED");
                    return;
                }
                if state.configured {
                    state.configured = false;
                    let (w, h) = match state.size {
                        Some((w, h)) if w > 0 && h > 0 => (w, h),
                        _ => (480, 300),
                    };
                    if drawn != Some((w, h)) {
                        let buffer = solid_buffer(&shm, &qh, w, h, 0xff34_4a5e);
                        surface.attach(Some(&buffer), 0, 0);
                        surface.damage_buffer(0, 0, w, h);
                        surface.commit();
                        drawn = Some((w, h));
                    }
                    if !announced {
                        println!("WINDOW-MAPPED {app_id} {w}x{h} pong={}", state.pong);
                        announced = true;
                    }
                }
                if pong_after.is_some_and(|t| Instant::now() >= t) && !state.pong {
                    for (base, serial) in state.held.drain(..) {
                        base.pong(serial);
                    }
                    state.pong = true;
                    println!("PONG-RESUMED");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        _ => {
            eprintln!("usage: pyxis-wlprobe globals | bind NAME INTERFACE | toplevel APP_ID SECS | window APP_ID SECS [--no-pong | --pong-after SECS]");
            exit(2);
        }
    }
}
