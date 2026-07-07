# GeckoOS / Pyxis — Alpha 1 Release Notes

**Headline:** first end-to-end boot-to-app milestone. The system now boots on real (QEMU-virtualized) hardware, brings up its full service stack, and runs one real application — a terminal — through the complete Bloom → Synergy → Echo → Rockhopper/Gecko pipeline. Everything else in the stack is present and running, but not yet wired to user-facing functionality.

---

## Evo (compositor)

- Boots and composites windows on real hardware via the custom kernel/initramfs, with working Wayland surface creation and window decorations.
- Two-step window-create/claim contract with Echo sessions is functional and is what binds the terminal's app window today.
- CPU-rasterized window chrome (headers/borders) renders correctly.
- Known gaps: no minimize, no HiDPI scale detection, placeholder fps/memory stats, rough X11 override-redirect ordering.

## Echo (IPC bus)

- Core message routing, service registration, and request/response forwarding all work.
- New this release: real signal broadcast to remote/socket clients (previously signals only reached in-process subscribers) — this was the key fix that made cross-process events work at all.
- No permission/capability model yet — any connected client can call any registered service. This is the single biggest security gap in the stack right now.

## Bloom (desktop shell)

- Visually complete shell UI (windows, taskbar, launcher, terminal, settings, browser, files) exists and renders.
- Terminal app is now real — it talks to Synergy/Echo, not a mock — and is the one fully functional app in the OS today.
- Every other app surface is still backed by an in-memory mock (fake sessions, fake data); only the terminal path has been converted to live IPC.
- Known dead code / lint debt not yet cleaned up (a handful of unused components, a hooks-rules violation, no test coverage).

## Synergy (native↔web bridge)

- Real Unix-socket client into Echo with the full `window.geckoOS` API surface (filesystem, appManager, power, events).
- Works today for dev/extension-hosted content; does not yet work under the production `pyxis://` scheme (WebExtensions can't bind custom schemes — needs a compiled-in bridge actor instead, see Nova/Vela section).
- The custom Gecko build (`vela`) now has a working compiled-in `JSWindowActor` version of this bridge for `pyxis://` origins, proven out via the terminal.

## Rockhopper (hardware/legacy compat layer)

- Real HAL/compat daemon, boots cleanly, registers on Echo.
- Owns app lifecycle via the new `puppis` crate (launch/suspend/resume/terminate, resource tracking) — real logic, but suspend/resume isn't wired to actual window visibility yet.
- Formerly named Persephone/Shift; renamed this cycle to resolve a naming collision with the process orchestrator.

## Shift (process/boot orchestrator)

- Boots and supervises the other daemons; process supervision and signal handling are functional.
- Formerly named Cronus.
- Has a known startup-ordering warning (fails to register with Echo on first attempt because it starts before Echo's socket exists) — cosmetic today, not yet fixed.

## Vroom (GPU/perf)

- Runs (`performance-monitor`) but its stats are placeholder values, not real telemetry yet.
- No webOS-equivalent component — this is new territory for the project.
- Formerly named Kratos.

## gecko / Vela (browser runtime)

- Custom from-source Gecko build (`vela`) now boots in kiosk mode with a working `pyxis://<app-id>` origin scheme and compiled-in Synergy bridge — this is what actually renders the terminal app today.
- This was the largest single workstream this cycle (protocol handler, secure-context origin mapping, bridge actor, kiosk run mode) and is now proven working end-to-end, not just designed.
- Old libxul-embedding approach fully retired in favor of this from-source build.

## Nova (app runtime host)

- Design-only — not started. No manifest parsing, install flow, or launch-composition layer exists yet.
- Everything Nova will eventually own (app install, permissions, single-instance policy) is currently either absent or hand-wired for the one terminal app.

## Housekeeping / cleanup this cycle

- Retired `gaia` (legacy actix-web shell, fully superseded by Bloom) and `gonk` (orphaned HAL prototype, superseded by Rockhopper) — both were dead code with zero live references.
- Fixed a panicking unit test, removed a stale systemd unit, corrected stale doc comments.

---

## Overall Alpha 1 summary

This release proves the architecture end-to-end for the first time: real boot, real compositor, real IPC bus, real browser runtime, and one real app (terminal) running through the full intended pipeline rather than a mock. It is not yet a usable desktop — only the terminal works; every other Bloom app surface is still simulated, there's no app install/permission story (Nova), and Echo has no security model. The core plumbing that everything else depends on (Evo, Echo, Synergy, Vela's `pyxis://` scheme) is now validated as workable, which de-risks the rest of the roadmap significantly.
