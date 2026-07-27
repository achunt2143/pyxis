# Pyxis OS — Architecture Recap

**Public Name:** Pyxis (Internal/Development Name: GeckoOS)

---

## Overview

Pyxis is a full desktop operating system built from the ground up with a 2000s-adjacent aesthetic. Its architecture draws inspiration from two lineages: the FirefoxOS/B2G platform model (web-tech-based app runtime, IPC-centric service bus) and the webOS system manager model (layer-based Wayland compositing, card-style window management). The OS is built primarily in Rust for core components, with React/Vite powering all UI surfaces.

The central design philosophy is a **compositor-first, IPC-driven** system: every major subsystem communicates through a shared message bus (Echo), all windowing flows through a Wayland compositor (Evo), all shell UI is rendered via web tech (Bloom/Flow), and all apps — whether native or web-based — are treated as first-class Wayland clients.

---

## System Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                         BLOOM (Shell UI)                    │
│          Wallpaper · System Layer · Windows · Overlay       │
├──────────────┬──────────────┬──────────────┬────────────────┤
│    Synergy   │   pyxis-hostd│  System Apps │  Pyxis Apps    │
│ (IPC Bridge) │ (App Runtime)│  (Rust/Web)  │ (Gecko-based)  │
├──────────────┴──────────────┴──────────────┴────────────────┤
│                     ECHO (IPC Message Bus)                  │
├─────────────────────────────────────────────────────────────┤
│                    EVO (Wayland Compositor)                  │
├──────────────┬──────────────┬──────────────────────────────┤
│     Shift    │    Cronus    │         Linux Kernel          │
│ (HW Abstact.)│ (Sys. Orch.) │       (Mainline 7.x)         │
└──────────────┴──────────────┴──────────────────────────────┘
```

---

## Module Breakdown

---

### Evo (formerly Ouranos)
**Role: Wayland Compositor & Window Manager**

Evo is the foundation of all display and input handling in Pyxis. It is written in Rust and built on the Smithay library, forked/rebased from the COSMIC compositor. Evo acts as the **central hub** of the entire desktop — every process that renders anything must go through it.

**Responsibilities:**
- Hosts the Wayland socket that all clients (apps, shell, Firefox instances) connect to
- Tracks and manages all Wayland surfaces/windows, assigns them to workspaces
- Renders window decorations, composites layers, and manages input routing
- Maintains a persistent **Echo IPC connection** to receive and emit desktop events
- Listens for new Wayland client connections (the "Wayland listener") and automatically registers them in the window manager — no app needs to announce itself
- Emits `wm.window.created`, `wm.window.destroyed`, and workspace-change signals over Echo

**App Launch Paths:**
1. **Native apps** — An exec/binary is spawned by Synergy or the system; it connects to the Wayland socket automatically; Evo hears the client connect and adds it to the window manager. Nothing else is required.
2. **Pyxis web apps** — Synergy/Echo instructs Evo (or pyxis-hostd) to launch `firefox --wayland <app-url>`; Firefox connects to the Wayland socket; Evo manages the resulting window exactly like a native app. The web app itself requires no special Wayland code — Firefox handles all of that.

**Key design constraint:** Evo does *not* create windows on behalf of apps. Windows appear when a Wayland client connects. Evo's job is to hear that connection and manage the resulting surface.

**Repository:** `github.com/achunt2143/pyxis-evo`

---

### Echo
**Role: IPC Message Bus / System Daemon**

Echo is the nervous system of Pyxis — a persistent daemon that all subsystems and apps connect to for inter-process communication. It is modeled on the D-Bus / Firefox OS IAC architecture: a message router with named service destinations, interface definitions, and method dispatch.

**Responsibilities:**
- Runs as a Unix socket daemon at system startup
- Routes `EchoMessage` packets between all connected clients using a `destination` + `interface` + `method` addressing scheme
- Acts as a broker/proxy: services register themselves with Echo at startup; Echo forwards incoming requests to the correct registered service
- Handles persistent connections with handshake negotiation
- Core system service destinations include:
  - `org.pyxis.Evo` / `org.geckoOS.Gecko` — window manager operations (e.g., `CreateWindow`, `ListWindows`, `FocusWindow`)
  - `org.geckoOS.appManager` — app lifecycle (e.g., `launch`, `kill`)
  - `org.pyxis.shell` / Bloom shell — notifications, status bar events
  - `org.pyxis.Synergy` — native-to-web IPC bridge messages
- Emits system-wide signals (e.g., `BatteryLow`, `NetworkChanged`) that any subscriber can consume

**Design principle:** Window surface creation and app launching are **separate concerns** on the Echo bus. `appManager.launch` spawns the process; `Gecko.CreateWindow` / Evo's Wayland listener handles the surface. These should never be conflated into a single call.

**Implementation:** Rust, with `EchoMessage` structs serialized via bincode. Handlers register with `handler.register_handler("destination", callback)`.

---

### Bloom
**Role: Shell / Desktop Environment**

Bloom is the Pyxis desktop shell — the system UI environment that the user sees and interacts with. It is built entirely in web tech (React + Vite + Flow component library) and rendered inside a dedicated Gecko/Firefox instance that Evo treats as the authoritative system-layer Wayland client.

**Four Wayland Compositor Layers (top to bottom):**
1. **Wallpaper** — Static or animated background, lowest z-order
2. **System Layer** — Status bar, dock, notification toasts; rendered by Bloom's system surface
3. **Windows** — Managed app windows; controlled by Evo
4. **Overlay** — App launcher, quick settings panel, system search; appears above all windows

**Shell Components:**
- **Status Bar** — Clock, battery, network, system tray; subscribes to Echo signals for live updates
- **Dock** — Pinned and running app icons; communicates with `appManager` over Echo to launch apps
- **App Launcher / Overlay** — Activated by gesture or hotkey; queries app registry and dispatches `pyxis.app.launch` over Echo; typing queries the search service
- **Notification Center** — Receives `notifyd` events over Echo; manages toast and persistent notification display
- **Workspace/Card View** — Displays running windows in a webOS-card-style layout; receives window list from Echo (`wm.window.list`)

**Bloom is NOT a monolithic app.** It is structured as a set of lightweight, independent surface processes that each connect to Echo independently. Each component subscribes to only the signals it needs.

**Flow** is the React component library / design system powering all Bloom UI (and potentially system apps). It provides the visual language — motion, spacing, component primitives — consistent with the 2000s-adjacent Pyxis aesthetic.

---

### Synergy (formerly Native Host / Bridge)
**Role: Native-to-Web IPC Gateway**

Synergy is the bridge that allows Pyxis web apps (running inside Gecko/Firefox) to communicate with native system services over Echo. It is the equivalent of Firefox OS's `mozApps` / system message bridge.

**Responsibilities:**
- Runs as a native Rust process that holds an Echo connection on one side and a WebExtension/native messaging host on the Gecko side
- Translates web-originated API calls (e.g., `navigator.pyxis.launch('calculator')`) into properly addressed Echo messages directed at the correct service destination
- Forwards system signals from Echo back into the Gecko runtime as JS-accessible events
- Handles session/handshake negotiation between web app instances and native services
- Acts as the security boundary — Synergy can enforce capability/permission checks before forwarding messages

**App launch flow with Synergy:**
```
Web App JS → Synergy (native messaging) → Echo → appManager/Evo → Firefox --wayland <url> → Evo window
```

---

### Shift (formerly Persephone)
**Role: Hardware Abstraction & Legacy Compatibility Layer**

Shift sits below Echo and provides an abstraction over hardware interfaces — battery, input devices, display brightness, sensors — so that upper-layer services like Echo handlers and Bloom can query hardware state without knowing the underlying implementation.

**Responsibilities:**
- Exposes hardware state (battery level, AC status, network interfaces, brightness) as Echo-accessible services
- Provides a compatibility shim for FirefoxOS-style legacy API surfaces, allowing old Gecko/web components to call into modern Echo/Evo infrastructure
- Acts as the HAL (Hardware Abstraction Layer) equivalent in the Pyxis stack

---

### Cronus
**Role: System Orchestrator / Master Controller Daemon**

Cronus is the low-level system daemon responsible for process supervision and system-level lifecycle management. It sits below Echo and handles operations that no other service should touch directly.

**Responsibilities:**
- Supervises critical system processes (Evo, Echo, Bloom surfaces, Synergy) and restarts them if they crash
- Handles system power states: shutdown, reboot, suspend
- Manages privileged operations: killing unresponsive processes, modifying system-level configurations
- Coordinates the startup sequence — ensuring Echo is live before Evo starts, and Evo is live before Bloom launches

---

### pyxis-hostd (App Runtime Host)
**Role: Thin Runtime Layer Between Installed Apps and Gecko**

`pyxis-hostd` is a small Rust daemon that sits between installed Pyxis app packages and the Gecko runtime. It is not an app store or a full app manager — it is the thin production-grade host that reads app manifests, maps origins, supervises Gecko instances per app, and connects each session to Echo.

**Responsibilities:**
- Parses `manifest.json` for each installed Pyxis app package (app ID, origin URL, permissions, display name)
- Maps app origins to local HTTP servers or packaged asset paths so Gecko has a proper web origin to load
- Spawns and supervises Gecko instances per app with the correct `WAYLAND_DISPLAY`, `PYXIS_APP_ID`, and origin environment variables
- Tracks running app sessions and enforces single-instance rules
- Exposes Echo service methods: `pyxis.app.launch`, `pyxis.app.install`, `pyxis.app.kill`
- Tags launched Gecko processes with `PYXIS_APP_ID` so Evo can correlate Wayland surfaces back to app identities

**Key files (Rust module layout):**
- `main.rs` — Wires config, registry, launcher, session store, Echo server surface
- `ipc.rs` — Echo request/response shape definitions
- `launcher.rs` — Builds and spawns Gecko runtime with Wayland and app-origin context
- `manifest.rs` — App manifest parsing and validation
- `origin.rs` — Origin-to-URL mapping
- `session.rs` — Running session tracking and single-instance enforcement

---

## Inter-Module Communication Matrix

| From | To | Channel | Example Messages |
|------|----|---------|-----------------|
| Bloom (shell) | Evo | Echo → `org.pyxis.Evo` | `ListWindows`, `FocusWindow`, `SetWorkspace` |
| Bloom (launcher) | appManager | Echo → `org.geckoOS.appManager` | `launch`, `kill` |
| appManager / Synergy | pyxis-hostd | Echo → `pyxis.app.*` | `pyxis.app.launch`, `pyxis.app.install` |
| pyxis-hostd | Gecko | Process spawn + env vars | `firefox --wayland <origin-url>` |
| Gecko (Firefox) | Evo | Wayland protocol | Surface creation, input events |
| Evo | Bloom / All | Echo signals | `wm.window.created`, `wm.window.destroyed` |
| Shift | Bloom / Echo | Echo signals | `BatteryLow`, `NetworkChanged` |
| Cronus | All | Direct process supervision | Restart Evo, restart Echo |

---

## Technology Stack

| Layer | Technology |
|-------|-----------|
| Core daemons (Evo, Echo, Shift, Cronus, Synergy, pyxis-hostd) | Rust |
| Wayland compositor base | Smithay (forked from COSMIC) |
| Shell UI (Bloom) | React + Vite + Flow component library |
| App runtime | Firefox / Gecko |
| IPC serialization | bincode |
| Build system | Cargo workspace (unified) |
| OS base | Debian (debootstrap), Linux kernel 7.x mainline |
| Virtualization / testing | QEMU |

---

## Development Tooling Notes

- GPT is used for build scripts and filesystem generation tasks
- Sonnet (Claude) is used for core coding components: Bloom, Evo, Echo
- All core development remains local; `github.com/achunt2143/pyxis-evo` is the primary tracked repository
- The project uses a unified Cargo workspace to prevent dependency version conflicts between Evo and Echo crates

---

## Current Development Status (as of mid-2026)

- **Evo** — Compositor base exists; Echo integration scaffolded; window tracking and workspace management in progress; no code pushed to remote repository yet
- **Echo** — IPC daemon scaffolded; handler registration and message routing functional; service proxy/forwarding for external services in progress
- **Bloom** — Shell structure defined; main shell pieces (status bar, dock, launcher, overlay) remain the primary unimplemented surface; system app shells are next after core shell
- **Synergy** — Architecture defined; native messaging bridge in design
- **pyxis-hostd** — First-pass Rust scaffold created; Echo integration and single-instance enforcement are the next priorities
- **Shift / Cronus** — Defined architecturally; implementation follows core IPC and compositor stabilization
- **Rootfs / Kernel** — `create-rootfs.sh` (debootstrap-based) exists and has been debugged; kernel build script targets Linux 7.x mainline; QEMU-based testing pipeline in use
