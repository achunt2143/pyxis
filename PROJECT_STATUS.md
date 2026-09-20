# Pyxis / GeckoOS — Project Status & Module Overview

Living status board for the whole system: what each module is, where it lives,
how it works, and what's done / in progress / to-do / backlog per module.

- **Companion docs:** [`CLAUDE.md`](CLAUDE.md) is the architectural index (read it
  for the deep component map + design rationale). This file is the *status/roadmap*
  layer on top of it. Deep-dive design docs are listed in CLAUDE.md's
  "Source documents" section.
  [`AUDIT_2026-08.md`](AUDIT_2026-08.md) is a full read-only codebase audit
  (2026-08-05) with file:line evidence; [`ALPHA_ROADMAP.md`](ALPHA_ROADMAP.md)
  turns it into the alpha3 → pre-beta release plan. **Read the audit before
  re-diagnosing any item below** — several long-standing entries turned out to
  be stale, and several "broken" features were found to be blocked on missing
  rootfs binaries rather than on the Rust.
- **Keep this current:** when a status changes, update the module section *and*
  the snapshot table. When you finish something marked 🟢, verify it on a real
  build/boot before promoting it to ✅.

## Legend

| Mark | Meaning |
|---|---|
| ✅ | Done **and** verified working |
| 🟢 | Implemented; compiles; **awaiting build + on-device verification** |
| 🚧 | In progress / partially done |
| 📋 | To-do — planned, not started |
| 🗄️ | Backlog — deferred, not scheduled |
| 💡 | Nice-to-have |
| ⚰️ | Dead / archive candidate |

---

## Snapshot — Alpha2 punch list

**Status as of the alpha2 on-device test pass** (superseding the earlier
"🟢 needs boot" markers now that a real build/boot happened). Items not called
out as broken in that pass are treated as passing; everything explicitly
reported broken is reset to 📋 **to-do** and carried into alpha3, regardless of
what was attempted this cycle — an attempted fix that didn't hold is still a
to-do, not "in progress."

| # | Item | Status | Where |
|---|------|--------|-------|
| 1a | Browser launches straight to Vela | ✅ verified on-device | Bloom `Desktop.tsx` |
| 1b | Hybrid embed browser in a web app | 🗄️ backlog (architecturally out of scope as DOM embed) | — |
| 2 | Terminal QOL (tab/history/etc.) | 📋 **to-do** — still needs work; **evaluating moving Terminal to a native app** instead of xterm.js-in-web-view | `apps/terminal`, `terminal.rs` |
| 3 | Files app — real file ops | ✅ **root cause fixed & verified on-device** — `readDirectory` on `/proc` now returns 171 entries with `created: null` instead of erroring; `$HOME`/`/tmp` list fine. base64 `readFile` + image preview added. UI-level retest pending the boot fix | `FilesApp.tsx`, `filesystem.rs` |
| 4 | Settings persistence | ✅ verified on-device | `settings.rs` |
| 5 | Kiosk-first / regular-window-after | ✅ verified on-device | `nova.rs` |
| 6 | Rebrand Firefox → Vela | 📋 to-do (needs vela rebuild) — not attempted this cycle | `gecko/` |
| 7 | **Unmaximize** (restore-down) closes app | ✅ **verified by the user 2026-09-20** on the rebuilt image — maximize then restore-down, no crash. Root cause was a click dispatched on press *and* release, with the header bar's button bands moving between the two so the release hit Close; fixed by ignoring anything but `ButtonState::Pressed`. Could not be automated: no synthetic-input channel, and an IPC-driven maximize would not touch the pointer path | `evo/shell/element.rs` |
| 8 | Thin window border (+ follows rounding) | 🚧 base implementation shipped; **general visual polish still required** (see #12). Scope confirmed: Evo SSD (see decoration decision below) | `evo/shell/ssd.rs` |
| 9 | Title bar shows app name | 🚧 base implementation shipped; **general visual polish still required** (see #12). Scope confirmed: Evo SSD | `evo/shell/ssd.rs`, `element.rs` |
| 10 | Restore from dock | ✅ **verified on-device 2026-09-20.** The three 2026-08-17 fixes had sat unconfirmed for a month; `alpha4-exit-criteria.txt` now round-trips the whole chain — no `appId: "unknown"`/`pid: 0` sessions, `nova.Focus` returns a real windowId, minimize → `suspended` → focus → `running` | `evo/state.rs`, `evo/shell/xdg.rs`, `echo/lib.rs` |
| 11 | **Window resizing** (new, found in testing) | 📋 **to-do** — not implemented at all; windows currently only have two fixed sizes (maximized / restored-default). No user-driven resize (edge/corner drag) exists yet | `evo/shell/grabs.rs`, `xdg.rs` |
| 12 | **General visual enhancements** (new, found in testing) | 📋 to-do — umbrella item, scope TBD next pass (title bar/border legibility, spacing, etc.) | Evo SSD / Bloom |
| — | Window layering (shell below apps) | ✅ verified on-device | `evo/shell/element.rs` |
| — | Installer present / install flow | ✅ **verified end to end on device 2026-09-20** — a real `.ipk` built with `pyxis-sdk`, installed through `installer.Install` → `opkg`, indexed by Nova, launched into a live Evo-composited window, and removed again. Only the *visual* confirmation in the launcher is outstanding (no headless screenshot channel) | `installer` app + service |

---

## Audit corrections (2026-08-05)

Found by the full codebase sweep in [`AUDIT_2026-08.md`](AUDIT_2026-08.md).
These change the reading of the punch list above, so they belong here rather
than only in the audit.

### Rootfs/service blockers — 🟢 fixed 2026-08-16, awaiting a real build

| Finding | Effect | Resolution |
|---|---|---|
| ⚠️ **`opkg` is not installed in the rootfs** | `installer.rs`'s `require_opkg()` fails every install/remove. The installer flow was never runnable on-device. | 🟢 **Built from source.** The audit's "add it to the apt list" fix is **not available — Debian ships no `opkg` or `opkg-utils` in any suite** (verified against the sid index; adding it would have broken the build). New `stage_opkg()` in `create-rootfs.sh` builds pinned+checksummed opkg 0.10.0 from the Yocto release tarball inside the chroot, minus curl/gpgme/libsolv (local installs only), and writes `/etc/opkg/opkg.conf` with an arch list mapped from `DEBIAN_ARCH` to Vroom's naming (`amd64`→`x86_64`). |
| ⚠️ **`nmcli` is not installed** | `NetworkService::get_status()` always reports `connected: false`; the top bar shows offline on a machine with a working DHCP lease. | 🟢 Rewritten on direct kernel reads — `/proc/net/route` for the default route, `/sys/class/net/*/device` to filter synthetic NICs, `operstate` for liveness. NetworkManager is no longer referenced at all. Unit-tested. |
| ⚠️ **`shutdown`/`systemctl` don't apply** | BusyBox is PID 1 and there's no systemd — every `power.rs` action fails. | 🟢 Repointed at `busybox poweroff`/`reboot` (with `-d` honouring the delay). `sleep()` now writes `/sys/power/state` and returns a structured `supported: false` instead of a spurious success. |
| 📋 **No audio stack** | `libasound2` (a client library) only. No PipeWire/PulseAudio/ALSA config. Unscoped subsystem. | Still open — alpha 5. |

**Also fixed in the same pass** (same "stop reporting things that aren't true"
theme, found while in the files): `power.rs`'s battery status returned a
hardcoded 50% + a constant 40°C on machines with no battery — it now reports
`present: false` and scans for the real battery instead of assuming `BAT0`;
`set_screen_brightness` piped nothing into `tee` and reported success
regardless — it now writes the sysfs backlight directly and finds the device
rather than hardcoding `intel_backlight`.

### Resolved in alpha 4 — the shell now reads Nova's registry

✅ **`useApps()` (`pyxis-experience/src/hooks/useApps.ts`) is the shell's app
source**: `nova.List`, refreshed on `AppInstalled`/`AppUninstalled`/
`RegistryChanged`. `PYXIS_APPS` is now only a presentation overlay (glyph,
accent, pin state, default size) and is read in exactly two places — pin
ordering inside `useApps`, and `appById`. The launcher, drawer, desktop
search, dock, `useWindows` and `Desktop`'s three launch sites all go through
the hook.

Verified in the browser (not yet on device): installing `weather` via the
installer makes it appear in the drawer and search with the fallback glyph,
and removing it drops it — no code change, no restart.

**The non-obvious part, don't re-derive it:** `RegistryChanged` is the
load-bearing signal, not `AppInstalled`. `nova.Install` is the *user-tier*
tarball path; an `.ipk` goes installer → `opkg` → `nova.Reload`, and Reload
emits `RegistryChanged` alone. Subscribing only to `AppInstalled` would look
correct and never fire for the case that matters.

### Root cause found and fixed for #3 (Files) — 🟢 awaiting on-device confirmation

`filesystem.rs::read_directory` used `metadata.created()?` **inside** the entry
loop. `created()` returns `Err(Unsupported)` on tmpfs/overlayfs/procfs/sysfs
and older ext4 inodes, so **one bad entry aborted the whole listing** and
`FilesApp` rendered nothing.

🟢 Fixed 2026-08-16: both timestamps are now non-fatal (serialize as `null`),
per-entry `metadata()` failures skip that entry instead of aborting, and a
dangling symlink falls back to `symlink_metadata()` so it still lists. Covered
by a regression test that lists `/proc` — the guaranteed-failure case — and
asserts a null `created` is present rather than an error.

`readFile` also now honours its `encoding` parameter (it was `read_to_string`
regardless, so any binary file errored): `base64` is supported for binary reads,
with a `maxBytes` cap checked against the file's real length before reading,
since the whole body crosses the bus as one JSON message.

🟢 **Files image preview is wired on top of it** — `FilesApp` requests
`encoding: 'base64'` for image extensions and renders a `data:` URI, instead of
falling through to the metadata card. The dev mock honours `encoding` the same
way so the two don't drift. The base64 encoder is hand-rolled (no new crate,
same call made for sha256 in `nova.rs`) and pinned by a regression test against
a real PNG encoded by an independent implementation.

### #7 / #10 — re-diagnosed on-device 2026-08-17 (both prior theories were wrong)

### #7 — ROOT-CAUSED AND FIXED 2026-08-17

**A single physical click was dispatched twice, and the two dispatches resolved
to different buttons.** `PointerTarget::button` in `evo/src/shell/element.rs`
called `HeaderBar::clicked` without checking `event.state`, so it fired on both
press *and* release. `clicked` hit-tests relative to the header bar's **current**
width, and the window changes width in between:

```
press    click lands in the maximize band of a MAXIMIZED window (width 1280)
         -> defers unmaximize_request
[idle]   window unmaximizes; header bar shrinks to its restored width, so every
         button band moves left
release  the SAME pointer x is now past `width - BUTTON_WIDTH`
         -> the CLOSE band -> send_close()
```

The client closes its only window and Gecko quits the whole app —
**confirmed on-device by `quit-application-requested reason="lastwindow"`**,
followed by `domwindowclosed ... windowclass=files sizemode=normal`.

Maximizing looked fine only because both dispatches landed on the same button
when the width hadn't changed yet. This is why the item read as
"maximize closes the app" for two cycles: the *maximize* click was never the
problem, and Evo's maximize/unmaximize handlers were correct all along
(verified: geometry saved, restored to the right point, client ACKs).

Fixed by returning early unless `event.state == ButtonState::Pressed`, matching
`input_handler.rs`'s existing idiom. Touch is unaffected (it has separate
`touch_down`/`touch_up`). The same double-dispatch also affected the titlebar
drag/move and minimize paths, so those get more predictable too.

**Awaiting on-device confirmation.**

**Also fixed 2026-08-17 (separate, real, but NOT the cause of #7).** `unmaximize_request` was skipping
`map_element` entirely whenever no pre-maximize geometry was saved — and that is
the *common* path, not a corner case: `OldGeometry` is only populated by Evo's
own `maximize_request`, but GTK/Gecko brings windows up maximized on its own
(browser-init's `sizemode="maximized"`) without ever sending
`xdg_toplevel.set_maximized`. Observed on-device as
`unmaximize_request: restored_loc=None had_saved_geo=false`. The client was told
"unmaximized, pick your own size" while Evo left the element at its maximized
origin, so the two disagreed about the window's geometry from then on. Evo now
synthesizes a centred restore position in that case. **Unverified against the
crash** — needs a rebuild and a repro.

**#7 is not a crash.** A repro capture shows the app exiting **cleanly**:
appHost emits `AppStopped`, not `AppCrashed`, which `handle_reaped` only does
for a zero exit status (or a user-initiated stop). The app's Echo connection
closes with "unexpected end of file" and is then reaped normally. Evo survives.
No `maximize_request`, `unmaximize_request` or `SSD maximize button` line
appears anywhere in the captured window, and Evo's own handlers are defensive
with no panics. So the framing carried since alpha2 — "unmaximize crashes the
app" — is wrong twice over: maximize works, and the app is *quitting*, not
crashing. Caveat: the capture's evo.log half looked thin, so "Evo logged
nothing" is suggestive rather than conclusive; confirm before building on it.

**#10 is an app-identity failure, but not the one predicted.** There is exactly
**one** appHost session per app, not two — the synthesized-second-session theory
is disproven. Apps launch correctly and their bridge works (a launched Files app
makes real `readDirectory`/`settings.get` calls). What fails is that the window
never binds: `surfaces: []`, `ListSurfaceSessions` count 0, so `nova.Focus`
correctly answers "app has no mapped compositor window yet".

### #10 — ROOT CAUSE FOUND 2026-08-17: Evo bound surfaces into the WRONG PROCESS

`evo/src/state.rs` bound each mapped Wayland surface with
`geckoOS_echo::AppHost::global().bind_surface(...)`, on the stated assumption
that it was "a same-process call — AppHost is a global — so this is cheap and
safe." **`AppHost::global()` is a `OnceLock` *process*-global, and Evo is a
separate process from `echo-daemon`.** So the compositor bound every surface
into a fresh, empty AppHost living inside itself — invisible to the daemon, and
never `set_bus`'d, so it emitted no lifecycle signals either.

That one mistake produces the entire #10 symptom set, all of which we measured:

| Observed | Explained by |
|---|---|
| session keeps `surfaces: []` | bind landed in Evo's local instance |
| `appHost.ListSurfaceSessions` count 0 | daemon never saw a bind |
| `nova.Focus` → "no mapped compositor window yet" | it reads `surfaces.first()` |
| shell never gets `AppSurfaceMapped`; dock stuck "starting" | Evo's local AppHost has no bus |

Fix part 1: Evo now calls `org.geckoOS.appHost.BindSurface`/`UnbindSurface`
over its existing Echo connection (`bind_surface_remote`/`unbind_surface_remote`
in `enhancements/echo_ipc.rs`) so binds reach the daemon's real registry.
**Confirmed working on-device** — the dock stopped reporting apps as
permanently "starting", i.e. `AppSurfaceMapped` now arrives at the shell.

**Fix part 2 (the rest of #10): bound at the wrong TIME.** With part 1 in, the
measured session state was:

```json
{ "appId": "unknown", "pid": 0,   "surfaces": [1,2,3] }   <- all surfaces here
{ "appId": "files",   "pid": 558, "surfaces": []      }   <- real app has none
```

Evo bound from the window-map path, where `XdgToplevelSurfaceData.app_id` is
still unset — GTK/vela assign it later (`windowclass` ->
`SyncAttributesToWidget` -> `nsWindow::SetWindowClass` ->
`gdk_wayland_window_set_application_id`). So every surface bound under the
literal `"unknown"` fallback, `bind_surface` synthesized a session for it, and
the real app session stayed empty — which is precisely why `nova.Focus`
(`surfaces.first()`) still answered "no mapped compositor window yet".

Binding moved to `xdg.rs::ack_configure`, the first point where a real app id
exists. It is idempotent (`bind_surface` skips a surface already listed), so
running on every ack is self-healing: whichever ack first carries a real id
binds. The shell is excluded (it is the desktop, not an app). **Awaiting
on-device confirmation.**

**Fix part 3 — the last link: in-process services could not call external
services at all.** With sessions and surfaces finally correct
(`files -> surfaces:[3]`), `nova.Focus` still failed, with:

```
Error: Method call failed: No handler registered for org.geckoOS.evo.FocusWindow
```

`FocusWindow` *is* declared in Evo's `ServiceInterface`, and Evo *was*
registered and connected. The problem was `IPCBus::call_method`: it only ever
consulted the **in-process router**. Evo is an external socket client, so there
is no router handler for it and the call was rejected. `daemon.rs` had always
done the `get_connection_for_service` -> `forward_call` lookup for messages
arriving *over a socket*; the gap was only for calls originating *inside* the
daemon — which is exactly what Nova is.

`call_method` now forwards to the owning connection when no in-process handler
exists. This affects every in-process -> external call, not just Focus, so
minimize/restore driven from the shell go through the same repaired path.
**Awaiting on-device confirmation.**

~~Also seen and worth fixing separately: `evo.MinimizeWindow` accepts an
`app_id` argument, silently ignores it, and returns
`{"success": true, "window_id": 0}`...~~ ✅ **fixed 2026-09-20.**
`FocusWindow`/`MinimizeWindow`/`RestoreWindow` all shared it — each did
`.unwrap_or(0)` and answered success regardless, then logged "Ignoring
Minimize for unknown window 0" where no caller would see it. They now go
through `resolve_window_arg`, which errors on a missing, non-numeric, or
unknown `window_id`; the missing-argument message names the likely mistake
(passing `appId`, the natural error given every *Nova* method is keyed that
way). This was not hypothetical: an alpha 4 test of mine passed `appId`,
got `success: true`, and "passed" while minimizing nothing.

⚠️ **Caveat to revisit:** both helpers do a blocking round-trip
(`block_on` + await the METHOD_RETURN) from the compositor's map path. That
matches the existing `emit_window_created` pattern next to it, but a slow or
wedged daemon can now stall Evo's event loop. A fire-and-forget variant for
bind/unbind is the right long-term shape.

### Related, still open

**The app-id theory is DISPROVEN** (instrumented and measured 2026-08-17, not
inspected). Temporary `PYXIS-DIAG` probes were added through the whole chain and
every link fires correctly for a Nova-launched app:

```
handle(): flag/env app id = files, cmdLine.state=0, preventDefault(before)=false
openPyxisAppWindow(): appId=files url=pyxis://files/index.html args.length=2 hasKey=true
browser-init: arguments=2 [1]=[...nsIWritablePropertyBag2] isBag=true hasPyxisApp=true
openPyxisAppWindow(): window opened, windowclass=files pyxisapp=files
```

Evo confirms it: `app_id=Some("files")`, `app_id=Some("notes")`. **Stamping
works; the patch is correct and is present in the built binary.** Do not spend
more time here.

**What is real:** there are additional `vela-default` toplevels that Evo
configures but which **never run `browser-init`** (all 3 probe hits had the
bag). So they are not `browser.xhtml` windows — they are some other toplevel the
vela process creates. If one of those is what the dock/appHost binds to, that
would explain the surface confusion. Identifying what they are is the open
question, not the app id.

**Shell-side defect found in the same pass (not yet fixed):**
`useAppHost`'s `refresh()` does a wholesale `setSessions(next)` replace, and
`upsert` is a blind overwrite by `sessionId` with no ordering guard. Any
refresh that resolves after a signal has updated state will clobber it with a
stale snapshot. Worth hardening once surface binding is confirmed, since it can
mask or mimic the same "stuck in starting" symptom.

**Trap that cost a cycle:** `start_daemon` truncates `/var/log/echo.log` on
every boot, so a reboot between reproducing and grepping silently destroys the
evidence. Capture to a file outside `/var/log/*.log`, or grep before rebooting.

### #10 (dock restore) — superseded by the entry above; kept for the trail

The full chain (`BottomDock` → `handleOrphanSelect` → `nova.Focus` →
`Nova::focus` → Evo `FocusWindow` → queued `WindowAction::Focus` → drained in
all three backends → `raise_window` → auto-`unminimize_window` → `WindowMinimized`
→ `handle_window_restored` → `SIGCONT`) **exists and each link is correct**;
`FocusWindow` does send a `method_return`, so there's no hang. Most likely
failure is upstream at `nova.rs:1016-1019` (`surfaces.first()` is `None`) when
vela's Wayland `app_id` doesn't match Nova's app id — `bind_surface` then
synthesizes a *second* session holding the surfaces. **Check
`appHost.ListRunning` for two sessions per app.** Fast discriminator: a failed
dock click should raise a "Couldn't reach …" snackbar — if it doesn't, the
click never reached `handleOrphanSelect` and the bus isn't the problem.

### Decoration model — RESOLVED 2026-08-16: Evo owns SSD

There was no live contradiction; there was **stale text**. The audit read
`create-rootfs.sh`'s 2026-07-08 comment block ("GTK owns app decoration, Evo
draws no SSD"), which had been superseded on 2026-07-25 and never updated.

The code has owned this since then: `evo/src/shell/xdg.rs`'s `ack_configure`
computes `is_ssd = !is_shell && (xdg_ssd || kde_mode == Server)`, and
`state.rs` creates `KdeDecorationState` with default mode `Server`. **Evo draws
the decoration; vela draws none.** The boot script's comment has been rewritten
to match, and the full model is documented in `CLAUDE.md` §"Window decoration
model".

Consequence: #8/#9/#11 keep their existing Evo-SSD scoping — they were written
correctly. #7 is an **Evo SSD bug**, not a two-parties-disagree bug, and is not
blocked on any decision.

### Stale items — already fixed, safe to stop tracking

- ✅ `echo/src/dbus.rs:637-640` `todo!()` test — **fixed**, the struct is complete.
- ✅ `ouranos.service` — **already removed** from `boot/startup/systemd/services/`.
- ✅ Bloom dead code (`AppLauncher`/`Sidebar`/`FilesModal`/`Background`, orphan
  `.scss`, `tsconfig-json.json`) — **all already deleted**.
- ✅ "12 `no-explicit-any` lint errors" — **stale**; `npx eslint .` reports
  **2** errors, both `react-hooks/set-state-in-effect` in `DesktopSearch.tsx`.
- ✅ `AppDrawer` phantom entries — **none**; it renders `PYXIS_APPS` directly.
- ✅ `stage_binaries()`'s `pyxis` binary "has no Cargo target" — **stale**;
  `vroom/Cargo.toml:29-31` defines it.
- ⚠️ `CLAUDE.md` says Nova scans `/usr/palm/applications` — code and spec both
  use **`/usr/pyxis/applications`** (`nova.rs:61`).

### Still true, still open

- `apphost.rs:20`'s "Intentionally NOT in this MVP" comment is stale (surface
  registration is real at `state.rs:1701`) — comment needs updating.
- `pyxis-experience/README.md` is still the stock Vite template.
- Zero UI test coverage (0 test files) vs. **177** Rust tests across 41 files.
- `shift/src/echo_ipc.rs:369-505` — every Echo handler is a stub returning
  fabricated data (hardcoded pid 1000, a literal
  `[{app_id:"firefox"},{app_id:"thunderbird"}]` process list, constant
  128 MB / 5% usage) while the real implementations sit unused in
  `process.rs`/`resources.rs`/`tree.rs`. `shift-daemon` *does* run at boot.

---

## Kiosk-on-relaunch — ROOT CAUSED 2026-08-17: an orphaned vela, not a kiosk bug

**Symptom:** launch an app, close it, launch it again — the second window is a
plain Firefox window with omnibar, menu and extension buttons instead of a
chromeless kiosk one.

**It is not a kiosk bug at all.** Instrumented both launches end to end and the
probe output is *byte-for-byte identical*: `handle()` reaches the pyxis branch,
`openPyxisAppWindow()` runs with the right features, the `pyxisApp` bag arrives
in `browser-init`, and the window opens with `windowclass=files`. Both times.

The chrome window is a **leaked process from the previous launch**:

```
pid=606 ppid=1(init) start=<launch 1>   cmd: /usr/lib/pyxis-gecko/vela   <- argv stripped
                                        env: PYXIS_APP_ID=files
pid=710 ppid=212(echo-daemon)           cmd: vela --profile ... --pyxis-app files ...
```

vela re-execs itself during startup, and the replacement process has a
**different pid** (and, notably, a stripped argv — which is why it renders as a
normal browser window: it no longer has `--pyxis-app`). `AppHost::close` sent
SIGTERM to the *recorded* pid, which by then no longer existed, so the signal
landed nowhere and the real process survived, reparented to init. Relaunching
then leaves two velas for one app, and the orphan owns the chrome window.

**Fixed** in `apphost.rs`:
- apps spawn with `process_group(0)`, so the app is its own process-group
  leader and any re-exec'd descendant stays in the group;
- `close`/`kill` signal the **group** (`kill(-pgid)`) with a single-pid
  fallback for synthesized sessions;
- `close` escalates to SIGKILL after 5s if the group is still alive, so a hung
  app can no longer linger invisibly.

Same class of bug as the boot-script `setsid` fix, one layer down. **Awaiting
on-device confirmation.**

## Modules

### Evo — Wayland compositor & window manager
- **Path:** `evo/` (crate `geckoOS-evo`) · ~16.5k lines · mature
- **What/how:** Smithay-based compositor. Owns all windowing: XDG + XWayland
  shells, wlr-layer-shell, workspaces, SSD (server-side decorations), input,
  DRM/udev + winit backends, screencopy. Talks to Echo over
  `enhancements/echo_ipc.rs` (own tokio broker thread; queues
  Focus/Minimize/Restore actions drained on the calloop thread). Renders via
  `render.rs` → smithay `space_render_elements`, sorted by `RenderZindex`.
- **Status:** working (windows open, launch, minimize). Verified on-device
  after alpha2 testing: window **layering** (shell stays below apps) is ✅
  fixed. Maximize/unmaximize and dock-restore attempts did **not** hold —
  reset to 📋 to-do, see below.
- **Verified working (alpha2 test pass):**
  - Window layering: shell pinned to `RenderZindex::Background`, apps at
    `Shell`, via `WindowElement::z_index` + `is_shell()`. ✅ confirmed apps no
    longer disappear behind Bloom.
  - Rounded window border + title-bar text base implementation renders (🚧
    general visual polish still called for — item #12).
- **📋 To-do — attempted this cycle, still broken:**
  - **#7 maximize→unmaximize still crashes the app.** What was tried: SSD-aware
    maximize client sizing (subtract `HEADER_BAR_HEIGHT`) + unmaximize sending
    `size=None` instead of the saved bbox. Neither resolved it. `info!`
    diagnostics were added at the click (`ssd.rs::clicked`), the
    maximize/unmaximize request dispatch (`ssd.rs::handle_maximize`), and both
    `xdg.rs::maximize_request`/`unmaximize_request` — **next session should
    capture the compositor log around a repro and read those lines first**
    rather than re-guessing. Open questions the log should answer: does
    `unmaximize_request` even fire (vs. a hit-test/close-button overshoot per
    the log's original diagnostic intent), does it panic Evo itself (all
    windows die) vs. just the one client, and what does the client-side
    (vela/GTK) log show at the same moment.
  - **Dock restore of a minimized window still doesn't work.** What was tried:
    added the missing `suspended` lifecycle state end-to-end (SDK type, dock
    orphan filter, `AppSuspended` subscription) so the dock icon no longer
    vanishes on minimize. That symptom may now be fixed, but the underlying
    *restore* action (click → `focusApp` → `nova.Focus` → Evo `FocusWindow` →
    `raise_window` → auto-`unminimize_window`) apparently still fails
    end-to-end. Needs on-device tracing of that specific call chain next —
    start by confirming the dock click actually reaches `nova.Focus` (network/
    IPC layer) before assuming the Evo-side unminimize logic is at fault.
- **📋 New from testing — window resizing not implemented.** Windows only have
  two fixed sizes today (maximized via `maximize_request`, and whatever size
  the client's own restored/default geometry is) — there is no user-driven
  edge/corner drag-resize for SSD-decorated windows. `PointerResizeSurfaceGrab`/
  `TouchResizeSurfaceGrab` exist in `shell/grabs.rs` and are wired to
  `resize_request` (client-initiated, e.g. a CSD client's own resize handles),
  but the **SSD header bar draws no resize handles/hit-regions** to trigger
  that grab from the compositor side for a server-decorated window. This is
  the actual next feature to build, not a bug to fix.
- **Pending / to-do:**
  - 📋 HiDPI scale not read from render surface (`udev.rs:1426`).
  - 📋 X11 override-redirect stacking order (`shell/x11.rs`).
  - 🚧 `GetRenderingStatus` **IPC method** still returns hardcoded fps/mem
    (`enhancements/echo_ipc.rs` `handle_incoming_message`), even though the
    signal path (`record_frame`/`current_process_rss_mb`) is real.
  - 📋 Render wlr-layer-shell layers in `output_elements` (layer surfaces are
    accepted but not composited yet — no consumer today, needed once panels/
    wallpaper daemons exist).
  - 🗄️ Remove stale `ouranos.service` systemd unit (Evo's old name).
- **Nice-to-have:** 💡 minimize/restore animations; 💡 window snapping/tiling
  (naturally follows once #11 resize lands).
- **Note:** minimize **is** implemented and works (SSD button → `minimize_window`),
  correcting CLAUDE.md's older "minimize not implemented" note — it's
  specifically *restore-from-dock* that's still broken, not minimize itself.

### Echo — IPC message bus + in-process services
- **Path:** `echo/` (crate `geckoOS-echo`; daemon `echo-daemon`)
- **What/how:** D-Bus/LS2-style bus (`destination`+`interface`+`method` routing,
  service registration, signal broadcast). Hosts most system services in-process
  (registered in `bin/daemon.rs`). Unix socket transport.
- **Status:** real; routing/registration/forwarding functional.
- **Services (all `org.geckoOS.*`):**
  | Service | File | Status |
  |---|---|---|
  | appHost | `services/apphost.rs` | ✅ process supervision, sessions, surfaces, suspend/resume (SIGSTOP/CONT) |
  | nova | `services/nova.rs` | ✅ app registry/launch/install; ✅ verified on-device: kiosk-first/regular-window-after fixed via `MOZ_NO_REMOTE` + session-restore-off |
  | installer | `services/installer.rs` | 🚧 real, **untested end-to-end** (ipk/opkg) |
  | filesystem | `services/filesystem.rs` | ✅ full CRUD implemented; 📋 **on real device, Files app is not displaying entries properly** (see #3 — root cause not yet isolated to service vs. bridge vs. UI). `readFile` is also UTF-8-only (no base64/binary → blocks Files image preview) |
  | settings | `services/settings.rs` | ✅ verified on-device: moved to `/var/lib/pyxis/settings.json` (HOME-independent), persists correctly |
  | terminal | `services/terminal.rs` | ✅ real PTY + `bash -i` backend works; 📋 QOL (#2) still needed — see Bloom terminal app note re: possible move to native |
  | network | `services/network.rs` | 🚧 status query real; depth TBD |
  | power | `services/power.rs` | 🚧 battery status real |
  | notification | `services/notification.rs` | 🚧 |
  | deviceinfo | `services/deviceinfo.rs` | 🚧 |
  | process | `services/process.rs` | 🚧 |
  | system | `services/system.rs` | ✅ added 2026-09-20 — backs the bridge's `getSystemInfo()`/`listServices()`, which had been advertised with no service behind them. `getInfo` composes `deviceinfo.rs` + `/proc` runtime state (uptime/boot time/load); `listServices` merges the router's handler table with the registry; `ping` is a deliberate no-op round trip for liveness timing |
  | appmanager | `services/appmanager.rs` | ⚰️ superseded by Nova; ⚠️ `:102` extracts tarballs with no sig check |
- **Known issues:** ⚠️ `dbus.rs:637-640` unit test uses `todo!()` (panics if run).
  ⚠️ `org.geckoOS.Echo`'s own `ListServices` still reads **only**
  `ServiceRegistry`, which nothing but the over-the-wire `RegisterService` call
  ever writes to — so on a booted device with no external clients it answers
  "(no services registered)" while a dozen in-process services are live. That
  handler is left alone deliberately (external clients may depend on its
  `service_name`/`service_id`/`registered_at` shape), so treat it as a trap for
  new code, not a bug to fix: call `org.geckoOS.system.listServices` instead.
  ✅ `echo-cli services`/`status` were repointed at it 2026-09-20 and now print
  each entry's `kind` (`builtin`/`external`/`builtin+external`), its `reachable`
  state and its method count, with `status` reporting "N on the bus, M
  reachable" rather than one ambiguous "Registered Services" number. Verified
  against a live daemon: 14 services / 98 methods listed where the old command
  printed "(no services registered)", and a registered-then-disconnected client
  renders as 💀 unreachable while the reachable count drops. `echo-cli ping`
  no longer times a service walk — it calls `system.ping`, a handler with no
  work behind it, so the printed latency is the bus round trip it claims to be.
  ⚠️ Still open: `IPCBus::remove_connection` never prunes the registry, so a
  crashed client keeps its registry entry forever; the listing now marks it
  unreachable rather than dropping it.
- **Backlog:** 🗄️ extract Nova to a standalone crate (deliberately in-process for now).

### Nova — app runtime host (headline component, lives in Echo)
- **Path:** `echo/src/services/nova.rs` (~1.9k lines, in-process)
- **What/how:** App *identity* layer above appHost — `manifest.webapp`/`appinfo.json`
  discovery + validation, `pyxis://` origin map, install/uninstall (sha256),
  single-instance policy, per-app profiles, permission vocabulary, launch
  composition (`setpriv`-wrapped `vela --pyxis-app`), suspend/resume off Evo
  visibility signals, ipk-app registry.
- **Status:** ✅ real and central. 🟢 this-session kiosk-consistency env/prefs.
- **To-do:** 📋 real package signing (currently sha256 only); 📋 Bloom should
  launch via `nova.Launch` everywhere (see `docs/internal/nova-shell-wiring.md`);
  🗄️ standalone-crate extraction.

### Bloom — desktop shell UI (sibling repo)
- **Path:** `../pyxis-experience/` (`package.json` name `"bloom"`); shell in `src/`,
  apps in `packages/apps/*`, SDK in `packages/app-sdk/`.
- **What/how:** React/Vite/MUI desktop shell (wallpaper, top bar, dock, launcher,
  quick-settings, app drawer). In compositor mode it does **not** draw app
  windows — apps are independent vela processes Evo composites; Bloom shows dock
  chrome driven by real appHost session state.
- **Status:** visually complete; talks to the real `window.geckoOS` bridge.
- **Verified on-device (alpha2 test pass):**
  - ✅ #1a Browser launches straight to vela.
- **📋 To-do — attempted this cycle, still broken on-device:**
  - **#3 Files app**: dev-mock CRUD (open/preview/new-folder/rename/delete)
    verified in the browser preview only. On the real device, **file entries
    are not displaying properly** — the mock never exercised the real
    `window.geckoOS` bridge round-trip to `filesystem.rs`'s `readDirectory`
    response shape, so this is untested territory, not a regression. Next
    session should check the browser devtools/console on-device first (bridge
    call succeeding? response shape matching what `FilesApp.tsx` expects?)
    before touching either side blind.
  - **#10 restore-from-dock**: the `suspended` lifecycle-state plumbing
    (SDK type, dock orphan-icon filter, `AppSuspended` signal subscription)
    was added so the icon should no longer vanish on minimize, but clicking it
    to restore still doesn't work end-to-end. See Evo section for the same
    item — needs on-device tracing of the dock-click → `nova.Focus` → Evo
    `FocusWindow` chain.
- **📋 To-do — terminal QOL, possible architecture change:** #2 terminal
  tab/history/QOL still needs work after the rootfs terminfo/bash-completion
  packages were added. **Under evaluation: moving Terminal from an
  xterm.js-in-a-web-view app to a native (non-web) app instead**, if web-view
  terminal emulation keeps falling short of a real terminal's feel — no
  decision made yet, flagged here so it isn't lost.
- **Apps status:**
  | App | Path | Status |
  |---|---|---|
  | files | `apps/files` | 📋 real device: entries not displaying properly (see above) |
  | terminal | `apps/terminal` | 🚧 real PTY backend works; QOL to-do; native-app rewrite under consideration |
  | browser | `apps/browser` | ✅ launches vela directly, verified on-device (toolbar shell retained for dev) |
  | settings | `apps/settings` | ✅ theme/wallpaper/network/about; persistence verified on-device |
  | notes / contacts / tasks | `apps/*` | 🚧 PIM apps, filesystem-backed |
  | installer | `apps/installer` | 📋 built but install flow untested |
- **To-do / backlog:**
  - 🗄️ delete dead code (`AppLauncher.tsx`, `Sidebar.tsx`, `FilesModal.tsx`,
    `Background.tsx`, orphan `.scss`, `tsconfig-json.json`).
  - 🗄️ remove phantom `AppDrawer` launcher entries with no backing app.
  - 📋 delete `WaylandSurfaceView` placeholder (chrome-only cards + screencopy thumbs).
  - 📋 test coverage (currently ~none); README still stock Vite template.
  - 💡 wire real battery/network/notification signals into the top bar (partly done).

### @pyxis/app-sdk — shared app runtime library
- **Path:** `../pyxis-experience/packages/app-sdk/`
- **What/how:** `EchoClient` (real `window.geckoOS` bridge + dev mock),
  `useEcho`/`useAppHost`, `AppShell`/`mountApp`, settings bus, theme hook,
  storage helpers.
- **Status:** ✅ real; drives both shell and apps.
- **Recent:** `suspended` lifecycle state + `AppSuspended` subscription added
  (fixes the dock icon vanishing on minimize; does not by itself fix restore —
  see Bloom/Evo #10). Mutable dev-mock filesystem
  (createDirectory/deleteDirectory/move/stat) — dev-mock only, not a substitute
  for on-device bridge testing (see Bloom #3).

### gecko / vela — custom Gecko build + native bridge
- **Path:** `gecko/` (crate; `mozconfig-vela`, `patches/pyxis/*.PYXIS_DIFF`,
  `BUILD_RUNBOOK.md`, bridge in `gecko/bridge/`)
- **What/how:** From-source ESR-pinned Gecko built as `vela`, with compiled-in
  `pyxis://` protocol handler, `window.geckoOS` `JSWindowActor` bridge, and
  `--pyxis-app` chromeless kiosk run mode. Native-messaging host is real.
- **Status:** builds and runs as a Wayland client (per BUILD_RUNBOOK).
- **To-do / backlog:**
  - 📋 #6 rebrand to "Vela": add `browser/branding/vela/` + `--with-branding`
    (binary is already `--with-app-name=vela`; user-facing branding still default).
    **Allowed & required** under MPL + Firefox trademark policy. Needs a rebuild.
  - 🗄️ `PYXIS_BACKGROUND=1` headless/no-window app mode (documented gap).
  - 🗄️ `pyxis://` content-process file brokering (parent-brokered channel).
  - ⚰️ `gecko/src/gecko.rs` libxul-embedding scaffolding is obsolete; repurpose
    as launcher/supervisor.

### Rockhopper — hardware abstraction + FirefoxOS-era compat
- **Path:** `rockhopper/` · ~2,300 lines · real (`wayland.rs`, `dbus_session.rs`,
  `elf_loader.rs`, `compatibility.rs`). Daemon uses Puppis's `GeckoAppLauncher`.
- **Status:** 🚧 substantial; role vs Nova/appHost worth clarifying.

### Shift — system orchestrator
- **Path:** `shift/` · ~1,800 lines · real. Process supervision, boot ordering,
  power states; own `echo_ipc.rs`. **Status:** 🚧 real.

### Puppis — Gecko native-app lifecycle
- **Path:** `puppis/` · ~730 lines · real `suspend_app`/`resume_app` freeze/thaw.
- **Status:** 🚧 real but **not yet wired** — roadmap item to drive its freeze/thaw
  from Nova/appHost on top of the existing SIGSTOP/CONT suspend.

### Vroom — Pyxis SDK/PDK packaging toolchain
- **Path:** `vroom/` (spec `docs/pyxis-sdk-pdk-spec.md`, schemas, tools)
- **What/how:** builds/packages/installs web (SDK) + native (PDK) apps as `.ipk`
  via `pyxis`/`pyxis-sdk`/`pyxis-pdk`. Phase 3 done (services, maintainer
  scripts, registry via Nova + `pyxis-appctl`).
- **Status:** ✅ real (its old GPU-layer identity was deleted).

### Supporting / inactive
- `mini/` — prebuilt kernel build support. 🚧 build-support.
- `pyxis-appctl` (`echo/src/bin/appctl.rs`) — thin Echo client (rescan/list/launch). ✅
- `../pyxis-evo/` — ⚰️ **dead** (unmodified cosmic-comp fork; archive).
- `gaia/`, `gonk/` — ⚰️ removed 2026-07-07.

---

## Cross-cutting roadmap (dependency-ordered)

Condensed from CLAUDE.md, updated with this session's progress.

0. **App-serving strategy** (`pyxis://` + from-source `vela`) — ✅ built.
1. **Unify JS bridge contract** — ✅ done (`@pyxis/app-sdk`).
2. **Build Nova** — ✅ done (in-process). Remaining: 📋 Bloom launch via `nova.Launch`.
3. **Wire Puppis card lifecycle** (freeze/thaw) behind appHost/Nova — 📋.
4. **Real status/notifications** in the shell — 🚧.
5. **Delete `WaylandSurfaceView`** placeholder — 📋.
6. **Security/capability model** — 🚧 (Nova has the permission vocabulary +
   deny-by-default bridge; still need: tarball signing, cgroup sandboxing).
7. **Finish Evo rough edges** — 🚧 (minimize ✅; HiDPI, real fps/mem IPC, X11
   ordering, layer-shell rendering pending).
8. **Housekeeping** — 🚧 (panicking `dbus.rs` test; stale `apphost.rs` doc;
   `ouranos.service`; Bloom dead code/lint/tests; archive `pyxis-evo`).

---

## Backlog & nice-to-haves (global)

### Backlog 🗄️
- **Proper desktop / dedicated wallpaper layer.** Today Bloom paints the
  wallpaper in its own DOM and sits at `RenderZindex::Background`, so
  shell+wallpaper are one surface/process. A separate compositor-drawn wallpaper
  (or a layer-shell `background` client) beneath Bloom would put the wallpaper at
  a true level 0, decoupled from the shell — enabling shell transparency, a real
  "show desktop", and a wallpaper that survives a shell restart. *(Added at user
  request.)*
- Hybrid "browser inside a web app" via compositor-composited surface region
  (webOS card-style) — the only viable form of #1b; own project.
- Extract Nova to a standalone crate.
- Archive `pyxis-evo`; remove `ouranos.service`.
- Extend Bloom test coverage beyond the Nova contract suite (components, hooks).

### Nice-to-have 💡
- Window snapping/tiling; minimize/restore animations.
- `pyxis://` headless background apps (`PYXIS_BACKGROUND`).
- Screencopy-based live dock thumbnails.
- Real fps/memory in the `GetRenderingStatus` IPC method (signal path is real).

---

## Change log

### Alpha4 fixes — memory ceiling, window-action validation (2026-09-20)

**`QEMU_MEMORY` default 2048 → 4096** in `emulate.sh`, matched by
`serial-test.py --memory`. Not a preference: at 2048 the guest saturates at
~1929/1987 MB with eight apps and the late ones stay alive but never map a
surface, with no oom-kill to explain it. That presents as a compositor fault
and is not one. Measured: all eight map at 4096, using ~2793 MB. Giving a
backgrounded app its memory back is still alpha 6's job.

**`evo.FocusWindow`/`MinimizeWindow`/`RestoreWindow` validate their
argument.** All three did `.unwrap_or(0)` on `window_id` and answered
`{"success": true, "window_id": 0}` for anything, including the natural
mistake of passing `appId` (every *Nova* method is keyed that way). They now
reject a missing or non-numeric id, and the missing-argument message names
that likely mistake.

⚠️ **A regression I caused and caught on-device — do not re-add it.** The
first version of that fix also checked the id against
`EchoIpcService::windows`, which rejected *every real window* and broke
`nova.Focus` outright. Evo has **two** window registries and they hold
different things:

| map | holds | reachable from |
|---|---|---|
| `EvoState::windows_by_id` (`state.rs`) | every live toplevel, via `register_window` on map | the compositor event loop |
| `EchoIpcService::windows` (`echo_ipc.rs`) | only windows Evo *created* itself via the two-step create/claim contract, via `track_window` | the async Echo broker task |

Nova-launched apps are in the first and never the second, and the async
broker cannot reach the first. So the IPC reply fundamentally cannot confirm
a window exists — it is sent when the action is *queued*, and existence is
resolved later in `process_pending_window_actions`. Doing better means
plumbing a response channel back through the queue. The suite now asserts
this limit (a well-formed id for a nonexistent window returns success) so it
is recorded rather than rediscovered.

**`scripts/rebuild-and-verify.sh`** — the rebuild is four steps, two needing
root, and the ordering is load-bearing (the acceptance package must reach
the rootfs before the image is built from it). One command now.

### Alpha4 close-out — rebuild, retest, and an honest exit-criteria pass (2026-09-20)

Rebuilt with the merged `org.geckoOS.system` service and ran everything
again. What this pass actually established, and what it did not.

**Merged:** `claude/system-service-3c6704` (the bridge's `getSystemInfo()`/
`listServices()` made real). It was finished but sitting **uncommitted in a
worktree** — `release/alpha3` had none of it. Rust-only, so a rootfs rebuild
picked it up without a Gecko build. Verified on device: the third-party
app's own bridge probe now records `getSystemInfo=ok` where it recorded
`FAILED ... Service not registered` this morning, and
`system.listServices` reports **16** services where `Echo.ListServices`
reports **3** — the registry-vs-router split the service exists to fix.

**#10 restore-from-dock: confirmed, finally.** Its three fixes had read
"awaiting on-device confirmation" since 2026-08-17. The whole dock chain now
round-trips on a real boot: no `appId: "unknown"` or `pid: 0` sessions,
`nova.Focus` returns a real `windowId` rather than "No handler registered
for org.geckoOS.evo.FocusWindow", and minimize → `suspended` → focus →
`running` works. Nova's WindowMinimized→suspend path fires.

**#7 maximize→unmaximize: still unconfirmed, and not confirmable here.** The
fix is in the tree and in the image. The bug is a pointer press/release
pair, so driving maximize over IPC would not touch the broken path — a pass
would be meaningless. No synthetic input, no screenshots. A human has to
click it.

**All 8 built-ins: a resource limit, not a defect.** Every app launches,
focuses, minimizes, restores and closes. All eight *simultaneously* do not
fit in the 2 GB reference VM — at 2048 MB the guest hits 1929/1987 MB used
and the last three to launch never map a surface. They are **alive and not
OOM-killed**, just unable to allocate enough to paint; `dmesg` shows no
oom-kill at all. At 4096 MB all eight map, 2793 MB used. So the answer is
alpha 6's resource isolation (cgroups; Puppis freeze/thaw so a backgrounded
app returns memory), plus probably a higher default `QEMU_MEMORY`. Nothing
to fix in alpha 4.

**Two of my own test bugs, both worth recording** because each produced a
*passing* run that proved nothing:
- The alpha4 suite hardcoded the package version. When the package went
  0.1.0 → 0.2.0 every step failed on a missing file and the run still
  exited 0, because the harness only counted timeouts as failures. Suites
  now support `#!must` / `#!must-not` assertions that fail the run, and the
  package path comes from a glob.
- The exit-criteria suite called `evo.MinimizeWindow` with `appId`. It takes
  `window_id`, so it minimized window 0 — a no-op that still answers
  `{"success": true}` (the wart already flagged under #10). The test
  "passed" while testing nothing. It now resolves a real window id first.

**Also:** the acceptance package shipped a 1×1 transparent icon from
`pyxis-sdk new`, which loads fine and draws as nothing — so "appears with
its own icon" was untestable. It now has a real 128×128 one.


### `org.geckoOS.system` — the bridge's last two dead methods (2026-09-20)

`window.geckoOS.getSystemInfo()` and `.listServices()` have been in the API
surface every pyxis:// app sees (`Object.keys(window.geckoOS)`) since the
compiled-in bridge was written, pointing at an `org.geckoOS.system` service
that was never registered anywhere. On device they returned
`Error: Method failed: Service not registered: org.geckoOS.system` while every
other namespace on that object worked — the exact "looks supported, isn't"
shape alpha 3 was about. Fixed by building the service, not by deleting the
methods: `listServices` is genuinely useful and the data already existed.

**Rust-only — no Gecko rebuild.** The bridge already sends
`("system", "getInfo")` / `("system", "listServices")`; nothing in
`gecko/patches/pyxis` changed, so this needs a rootfs rebuild only.

- ✅ `echo/src/services/system.rs` — new service, registered from
  `bin/daemon.rs`'s `register_all_services` as `register_system_handlers`.
  Method names are camelCase to match what the bridge sends (`getInfo`,
  `listServices`), not nova's PascalCase.
- ✅ `getInfo` **composes rather than duplicates**. OS/kernel/hostname/memory/
  CPU are already implemented in `services/deviceinfo.rs`; re-parsing `/proc`
  for them would have created a second source of truth for the same facts, so
  it calls that code. What it adds is what nothing else on the bus reports:
  uptime, boot time (`/proc/stat`'s `btime`, not now-minus-uptime, which
  drifts across suspend), load average and task counts.
- ✅ A failing sub-fact yields `null` plus an entry in `unavailable` saying
  why, instead of failing the whole snapshot — one missing `uname` shouldn't
  cost a caller the other six fields.
- ✅ `listServices` reads **both** of Echo's disjoint notions of "a service
  exists": `MessageRouter`'s handler table (every in-process service) and
  `ServiceRegistry` (external clients that called `RegisterService`). Nothing
  writes to both, which is why the pre-existing `Echo.ListServices` reports
  none of the in-process ones. Needed one new accessor,
  `MessageRouter::handler_entries()`.
- ✅ Each entry carries `reachable`, resolved live against the forwarding
  table rather than restated from the registry — `remove_connection` drops a
  dead client from `external_services` but leaves it in the registry, so a
  registered-then-crashed service lingers there and would otherwise be
  reported as live.
- ✅ 3 tests in `services/system.rs` (72 pass in `cargo test -p geckoOS-echo
  --lib`). The `listServices` one pins the actual regression: reading the
  registry alone reports zero in-process services.
- ✅ `echo-cli services`/`status` repointed at `system.listServices` in the
  same session, so the CLI stopped reporting "(no services registered)" on a
  live bus; they now show each entry's `kind`, `reachable` state and method
  count. `echo-cli ping` stopped using a service listing altogether and calls
  a new no-op `system.ping`, so its latency number is the round trip and not
  the cost of enumerating the bus.

**Verified locally against a real `echo-daemon`** (not yet on device):
`echo-cli call org.geckoOS.system getInfo` returns a full snapshot;
`listServices` returns 14 services / 98 methods, where the registry-only
`org.geckoOS.Echo` `ListServices` reports only whatever external clients had
registered (zero on a fresh daemon). Registering a test service and letting
the client exit produces `kind: external, reachable: false, methods: []` with
its declared method still listed under `declaredMethods`; holding the
connection open instead flips the same entry to `reachable: true`.

**Not done:** `getInfo`/`listServices` are ungated by
`PyxisBridgeParent.sys.mjs`'s `_capabilityFor` table (read-only introspection,
which that table's comment names as the no-capability case). That is
consistent with `deviceInfo`, which is entirely ungated today including
`getSerial`/`getUUID` — gating system-info reads is one decision for roadmap
item 6, not a thing to do piecemeal here.

### Alpha4 B — .ipk end to end on a real boot (2026-09-20)

The alpha 4 acceptance flow, run on the booted alpha3 image. No Rust had
changed since that image was built, so the service layer under test is
current; only the shell bundle was hot-swapped in over serial.

**New: `scripts/serial-test.py`.** Serial is this project's only
verification channel and it had no harness, so every on-device check was a
retyped manual session. The script boots the image on a qcow2 overlay (the
pristine image is never mutated), waits for the `ttyS0` root shell, runs a
list of commands, and prints each one's output. It encodes the gotchas that
have each cost hours: boot-script output never reaches serial, markers must
be split so they cannot match the shell's own echo, `echo-cli` needs
`XDG_RUNTIME_DIR` and the full destination name. It can also push a file
into the guest by chunked base64 with a sha256 check at the end — there is
no shared filesystem and no sshd, and mounting the image needs root, which
is the thing the harness exists to avoid. `scripts/serial-tests/alpha4-ipk-e2e.txt` is
the first suite.

**Verified** (`scripts/serial-tests/alpha4-ipk-e2e.txt`): `pyxis-sdk new` → `package` →
`installer.ListAvailable` → `Inspect` → `Install` → `opkg install` → Nova
reload → indexed as tier `opkg` / source `appinfo.json` / `removable: false`
→ derived native manifest + `origins.map` → visible to `appHost.ListApps`
and `pyxis-appctl` → `nova.Launch` → **live session with a mapped
compositor surface**, Evo negotiating SSD for it. Then `nova.Uninstall`
correctly refuses ("use `opkg remove`"), `installer.Remove` succeeds, and
everything is cleaned up.

**The app really runs.** A mapped surface only proves a window exists, so
the test app writes a probe through the bridge. On device it reported
`origin=pyxis://com.pyxis.hello`, `secureContext=true`, `bridge=present`,
`initialized=true` — and the write succeeded, which means Nova's
appinfo→capability mapping grants a *third-party* opkg app a real
capability, not just the built-ins.

**Two bugs found, both fixed:**

1. **Nova handed web clients a filesystem path.** `info_json`'s `icons` came
   straight from `manifest.icons`, which `load_one_appinfo` fills with an
   *absolute* path (`/usr/pyxis/applications/<id>/icon.png`) so the derived
   *native* manifest has something AppHost can read. Handing that to a
   `pyxis://` client resolves against the origin root and 404s. Two
   consumers, incompatible needs. `info_json` now projects app-root-relative
   paths via `RegistryEntry::web_icons`; the native manifest keeps its
   absolute one. Three unit tests. Bloom's `relativeIconPath` also recovers
   the relative form, because it may run against an older Nova — which is
   exactly the case on the current image.
2. **`useApps()` fetched once per consumer.** Six components call it; the
   first on-device run showed **five `nova.List` calls for one shell
   start**, and every `RegistryChanged` fanned out to all six. Rewritten as
   one module-level store read through `useSyncExternalStore`, with an
   in-flight guard. Re-verified on device: **exactly one `nova.List` per
   shell start**, rising by two on each install and each removal.

**Also found, not fixed** (out of scope, tracked): the bridge advertises
`getSystemInfo()` and `listServices()`, which call `org.geckoOS.system` — a
service registered nowhere. Any app touching them gets "Service not
registered".

**Also confirmed:** the shell's registry wiring works on the real bus, not
just the mock. `nova.List` appears in echo.log per shell start and again
after each install/remove, which a hardcoded-array Bloom would never emit.

**Still open, and it needs a human:** whether the app *visibly* appears in
the launcher. QMP `screendump` reports "no surface" on a GL scanout —
re-confirmed on QEMU 10.2.1 with `device`/`head` variants, so this is not
stale tooling and not a misconfiguration. Everything behind the pixels is
verified; the pixels are not. `make build-release && make run` and look.

**Open question, minor:** an install produces *two* `nova.List` calls, not
one, and so does a removal. Symmetric, so most likely `RegistryChanged` is
emitted twice per operation on the Rust side rather than anything in the
shell. Not chased.

**Found by the test procedure, not by the change** — worth recording because
it bears on alpha 6's "shell restart without losing session": killing the
boot-launched Bloom and relaunching it through `nova.Launch` produces
`TypeError: Property 'handleEvent' is not callable` in echo.log. It looked
at first like a regression in the new shell bundle, so it was checked
against a control: the **unmodified** alpha3 Bloom, killed and relaunched
the same way, produces it too (0 → 1), while a clean boot of either produces
none. So the trigger is the relaunch path — Bloom launched via Nova rather
than by `geckoOS-boot`, and without `--kiosk` — not the bundle. A shell
respawn is supposed to become a supported operation in alpha 6; this is
waiting there.

Useful control data from the same run: the pre-alpha4 Bloom calls
`nova.List` **zero** times, which is what makes the single call from the new
one meaningful. The ~18 `nova.GetInfo` calls per shell start are also
pre-existing, not introduced here — chatty, but not new.

### Alpha4 A+D — dynamic app registry + first test suite (2026-09-20)

Alpha 4's headline (scope item A) and its contract tests (item D), done
together because A uncovered exactly the bug D exists to catch. **Verified in
the browser preview, not on device** — items B and C of alpha 4 are the
on-device half and are still open.

**The bug A uncovered.** The roadmap said the mock "already handles both"
`nova.List`/`GetInfo`. It did not, in the way that mattered: mock `nova.List`
was implemented by *delegating to `appHost.ListApps`*. Those are two different
projections of two different things — AppHost's is "apps I can supervise"
(`exec`, `icon`, `sessions`, `count`), Nova's is "apps that exist"
(`tier`, `removable`, `singleInstance`, `icons`, `source`). They even collide
on `entry`, which means a component name to AppHost and a webroot-relative
path to Nova. Anything written against the mock would have worked in
`npm run dev` and failed on device — the alpha 2 failure mode, again.

**Shell (`pyxis-experience`)**
- ✅ `useApps()` — Nova's `List` as the app source, refreshed on
  `AppInstalled`/`AppUninstalled`/`RegistryChanged`. Carries no liveness by
  design; running-ness stays `useAppHost`'s question, so a launch doesn't
  read as a registry change.
- ✅ `PYXIS_APPS` demoted to a presentation overlay. Read in two places now.
- ✅ `AppIcon` — glyph → manifest artwork → generic package glyph, with an
  `onError` catch so a missing icon isn't a broken-image box.
- ✅ `entryUrlFor(id, entry)` replaces hardcoded `entryHtml`:
  `pyxis://<id>/<entry>` on device, the package's `devPort` off it.
  `DesktopWindowType.entryHtml` → `entryUrl`.
- ✅ Repointed: `LeftLauncher`, `AppDrawer`, `DesktopSearch`, `BottomDock`,
  and — **not in the roadmap** — `useWindows` and `Desktop`. Desktop's three
  launch sites each did `mockApps.find(...)` and silently dropped the launch
  on a miss, so repointing the launcher alone would have made third-party
  apps visible but unlaunchable.
- ✅ `AppDrawer` empty state distinguishes loading / registry-unreachable /
  no-matches, instead of always claiming "No applications found".
- ✅ Mock `nova.List`/`GetInfo` rewritten as a real `info_json()` projection;
  `installer` added to the mock app set; installer mock now emits
  `RegistryChanged` **on Nova's interface**, matching the device, where it is
  Nova (via `Reload`) and not the installer that announces a new registry.

**Tests — first suite in this repo's UI side**
- ✅ Vitest wired up (`npm test`), with `tsconfig.test.json` so tests can use
  node types the app bundle must never get.
- ✅ `src/hooks/__tests__/novaContract.test.ts`, 10 tests. The shape test
  **parses the field list out of `nova.rs` at test time** rather than
  restating it, so it maintains itself. Confirmed it bites: deleting `tier`
  from the mock fails it with a message naming the field and the fix site.

**Observed working** (browser preview, mock backend): drawer renders Nova's 8
apps including the previously-unreachable Installer; installing `weather`
makes it appear in drawer + desktop search with the fallback glyph; removing
it drops it from both. Lint 0, `tsc -b` clean, `npm run build` ok, tests 10/10.

**Carried into the rest of alpha 4:** B (a real `.ipk` through real `opkg` on
device), C (#10 restore-from-dock, #7 maximize→unmaximize crash).

### Alpha3 — "Nothing lies to the user" (2026-08-16)

First alpha3 pass, working the scope in [`ALPHA_ROADMAP.md`](ALPHA_ROADMAP.md).
**A real build + boot happened.** Everything marked ✅ below was observed on
the booted image via the serial console, not inferred. Two bugs were found by
that boot that no amount of desk-checking would have caught — see
"Found on-device" at the end of this entry.

**A. Rootfs / service backends**
- ✅ **opkg now ships** — built from pinned, checksummed source in a new
  `stage_opkg()`. The roadmap budgeted one apt-list line for this; that turned
  out to be impossible (**Debian packages no `opkg` at all**), so it became a
  chroot build stage instead. **On-device: `opkg version 0.10.0`,
  `opkg list-installed` → rc=0.**
- ✅ **Network status reads the kernel** instead of the absent `nmcli`.
  **On-device: `{"connected": true, "ssid": "enp0s3", "type": "ethernet"}`** —
  previously this reported `connected: false` on every machine forever.
- ✅ **Power actions target BusyBox init**. **On-device: `power.shutdown` over
  Echo powered the guest off.** `/sys/power/state` reports `freeze mem`, so
  `sleep()` has a real backend here.
- ✅ Battery stopped fabricating values. **On-device:
  `{"present": false, "level": null, "charging": false}`** on a machine with no
  battery, instead of the old hardcoded 50% + 40°C.

**B. Correctness**
- ✅ `read_directory` no longer aborts a whole listing over one missing birth
  time. **On-device: `/proc` returns `count: 171` with `"created": null`** —
  the exact call that previously errored and blanked the Files app. `$HOME`
  and `/tmp` list correctly too. This closes #3's root cause.
- ✅ `readFile` honours `encoding` + `maxBytes`, and Files previews images
  inline off the base64 path. **On-device: a PNG read back as
  `"iVBORw0KGgo="`, and a 3000-byte file with `maxBytes:100` correctly
  refused** with the actionable message.
- ✅ **The clock ticks** — verified live in the browser preview (advanced
  02:19→02:24 PM unattended). Aligned to the minute boundary.
- ✅ **Dead quick-settings toggles resolved.** "Notifications",
  "Auto-brightness", "WiFi Enabled" and "Low Power Mode" had no handler and no
  backend to wire one to — removed; WiFi became a read-only status row. "Show
  percentage in top bar" was **wired for real** through the settings bus and
  verified end-to-end: flipping it persists and adds/removes the top bar's
  readout live.
- ✅ **Found and fixed a bug the audit missed:** remochi's `Toggle` declares a
  `checked` prop in its types but the shipped component reads `value`, so
  **every toggle in the shell rendered a permanent "Off"** regardless of state
  — Dark Mode included. Bridged with a correctly-typed `Switch` in both
  `QuickSettingsMenu.tsx` and `SettingsApp.tsx`.
- ✅ **Lint is at zero** (`npx eslint .` → 0 errors, was 2).
- ✅ **Rust tests green: 68 passing, was 58 passing + 1 failing.** The failure
  was pre-existing and deterministic in a full run (`reap_once` calls
  `waitpid(-1)`, which is process-wide, so two parallel reaper tests ate each
  other's children). Merged into one test; 8 new tests added for the
  filesystem and network changes.

**C. Decoration model — decided: Evo owns SSD.** It was already implemented and
verified; only the docs disagreed. See the resolved section above.

**D. Doc drift** — the 8 stale `CLAUDE.md` claims are deleted (not annotated),
`/usr/palm` corrected to `/usr/pyxis`, the boot script's superseded decoration
comment rewritten, and a new "Window decoration model" section added.

**Found on-device — two bugs a build was the only way to catch:**

1. 📋 **`opkg.conf` had an invalid line.** `lists_dir ext <path>` is an old
   OpenWrt-ism; opkg 0.10.0's parser dispatches the first token as a *type*
   (`option`/`dest`/`arch`/`src`/`dist`), so it printed
   `Ignoring invalid line` on **every** invocation. Corrected to
   `option lists_dir <path>` and **verified live** by applying it in the
   running guest: error before, clean after. Also fixed in `create-rootfs.sh`,
   so the next build bakes it in.
2. ✅ **No daemons survive boot — FIXED and verified.** `geckoOS-boot` runs from `/etc/inittab` as
   `::wait:`, so every daemon it backgrounds sits in that script's process
   group sharing `/dev/console`. When the script exits, the group takes SIGHUP
   and they all die. Symptom: `/var/log/{echo,evo,vela}.log` are written
   normally at boot and then stop mid-line with no error, and `ps` shows
   nothing but the two getty bashes — while running the *same script by hand*
   from a serial shell works perfectly (the interactive shell keeps the
   session alive). Fixed by `setsid ... </dev/null` in `start_daemon`.
   **Needs a rebuild to confirm** — this is the one item still unverified.

**Unattended boot now reaches a working system.** On the rebuilt image, with
nothing started by hand: Echo has evo + Rockhopper registered, `readDirectory
/proc` returns 153 entries, `network.getStatus` reports
`connected: true / enp0s3 / ethernet`, `opkg list-installed` is clean (no more
"Ignoring invalid line"), `power.reboot` reboots the machine, and
**`pyxis-appctl list` shows all 10 apps registered — bloom, browser, files,
notes, contacts, tasks, settings, terminal, testapp, and installer.**

That last one matters for alpha 4: Nova's registry already knows about
`installer` and every other app. The gap is purely that Bloom's `PYXIS_APPS`
array never asks it — exactly as the audit said.

**Verification method:** QEMU + pexpect over the serial console
(`/etc/inittab` respawns a root bash on ttyS0). Note `/dev/console` resolves to
tty0 because the kernel cmdline ends `console=ttyS0,115200 console=tty0`, so
boot-script output is *not* visible on serial — that cost real diagnostic time
and is worth knowing next pass. `echo-cli call` needs the **full** destination
(`org.geckoOS.filesystem`, not `filesystem`) and `XDG_RUNTIME_DIR=/run/user/1000`
to find the bus socket.

### Alpha2 — on-device test pass (post-build)

A full build + boot + manual test pass was run against everything below. Result:
**shipping as alpha2** with the confirmed-broken items carried forward as
alpha3 to-dos (see punch-list snapshot above for full detail per item).

**✅ Verified fixed / working on-device:**
- Browser launches straight to vela (#1a).
- Settings persistence, moved to `/var/lib/pyxis/settings.json` (#4).
- Kiosk-first / regular-window-after consistency, via `MOZ_NO_REMOTE` +
  session-restore-off prefs (#5).
- Window layering — shell (Bloom) correctly stays below app windows now
  (`RenderZindex::Background` pin).

**📋 Confirmed still broken — reset to to-do for alpha3, not "in progress":**
- **#7 maximize→unmaximize still crashes the app.** The SSD-size + `size=None`
  fix did not resolve it. Diagnostic `info!` logging is in place
  (`ssd.rs`/`xdg.rs`) — next session should capture a repro log before
  attempting another fix blind.
- **#10 restore-from-dock still doesn't work**, despite the `suspended`
  lifecycle-state fix (which should at least keep the dock icon visible after
  minimize — that sub-symptom needs independent confirmation). The
  click → `nova.Focus` → Evo `FocusWindow` chain needs on-device tracing.
- **#3 Files app** — dev-mock-verified CRUD does not carry over to the real
  device: file entries aren't displaying properly. Root cause (bridge
  round-trip vs. response shape vs. UI) not yet isolated.
- **#2 Terminal QOL** — still needed after rootfs terminfo/bash-completion
  packages were added; **a move to a native (non-web-view) terminal app is
  now under evaluation** rather than continuing to patch xterm.js integration.

**📋 New items found in this test pass:**
- **#11 Window resizing is not implemented** — windows only have two fixed
  sizes (maximized / restored-default); no SSD-driven edge/corner drag-resize
  exists yet. This is new-feature work, not a regression.
- **#12 General visual enhancements still required** — umbrella item; the
  border (#8) and title text (#9) base implementations render, but need a
  follow-up polish pass (scope TBD).

Not re-tested/not reported this pass (rebrand #6, installer e2e) remain
📋 to-do as before — no change in status.

### Prior session — implementation pass (pre-build)

All Rust/rootfs changes in this pass type-checked but were unbuilt/unbooted at
write-time; the Bloom Files work was verified against the dev-server mock only.
(Superseded by the on-device results above — kept here for the historical
record of what was attempted and why.)

- **Bloom (P0):** restore-from-dock (`suspended` state), browser→vela launch,
  Files real ops (open/preview/new-folder/rename/delete) + mutable dev-mock FS.
- **Evo (P1):** maximize-crash fix + diagnostics, rounded window border, title
  text, shell-below-apps layering.
- **Echo/rootfs:** settings → persistent `/var/lib/pyxis`, terminal terminfo +
  bash-completion packages, Nova kiosk-consistency (`MOZ_NO_REMOTE` + session
  prefs).
