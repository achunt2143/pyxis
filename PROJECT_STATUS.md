# Pyxis / GeckoOS — Project Status & Module Overview

Living status board for the whole system: what each module is, where it lives,
how it works, and what's done / in progress / to-do / backlog per module.

- **Companion docs:** [`CLAUDE.md`](CLAUDE.md) is the architectural index (read it
  for the deep component map + design rationale). This file is the *status/roadmap*
  layer on top of it. Deep-dive design docs are listed in CLAUDE.md's
  "Source documents" section.
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
| 3 | Files app — real file ops | 📋 **to-do** — dev-mock only; **on real device, files are not yet displaying properly** | `FilesApp.tsx`, `filesystem.rs` |
| 4 | Settings persistence | ✅ verified on-device | `settings.rs` |
| 5 | Kiosk-first / regular-window-after | ✅ verified on-device | `nova.rs` |
| 6 | Rebrand Firefox → Vela | 📋 to-do (needs vela rebuild) — not attempted this cycle | `gecko/` |
| 7 | Maximize closes app on unmaximize | 📋 **to-do — still crashes.** The SSD-size + `size=None` fix from this cycle did NOT resolve it; needs re-diagnosis with the added `info!` logging (see Evo section) | `evo/shell/xdg.rs`, `ssd.rs` |
| 8 | Thin window border (+ follows rounding) | 🚧 base implementation shipped; **general visual polish still required** (see #12) | `evo/shell/ssd.rs` |
| 9 | Title bar shows app name | 🚧 base implementation shipped; **general visual polish still required** (see #12) | `evo/shell/ssd.rs`, `element.rs` |
| 10 | Restore from dock | 📋 **to-do — still does not work.** The `suspended`-state dock fix didn't resolve it; needs re-diagnosis (see Evo/Bloom sections) | Bloom `BottomDock.tsx`, `useEcho.ts` |
| 11 | **Window resizing** (new, found in testing) | 📋 **to-do** — not implemented at all; windows currently only have two fixed sizes (maximized / restored-default). No user-driven resize (edge/corner drag) exists yet | `evo/shell/grabs.rs`, `xdg.rs` |
| 12 | **General visual enhancements** (new, found in testing) | 📋 to-do — umbrella item, scope TBD next pass (title bar/border legibility, spacing, etc.) | Evo SSD / Bloom |
| — | Window layering (shell below apps) | ✅ verified on-device | `evo/shell/element.rs` |
| — | Installer present / install flow | 📋 to-do (build + e2e test) | `installer` app + service |

---

## Modules

### Evo — Wayland compositor & window manager
- **Path:** `evo/` (crate `geckoOS-evo`) · ~17.5k lines · mature
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
  | appmanager | `services/appmanager.rs` | ⚰️ superseded by Nova; ⚠️ `:102` extracts tarballs with no sig check |
- **Known issues:** ⚠️ `dbus.rs:637-640` unit test uses `todo!()` (panics if run).
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
- **Path:** `rockhopper/` · ~2,900 lines · real (`wayland.rs`, `dbus_session.rs`,
  `elf_loader.rs`, `compatibility.rs`). Daemon uses Puppis's `GeckoAppLauncher`.
- **Status:** 🚧 substantial; role vs Nova/appHost worth clarifying.

### Shift — system orchestrator
- **Path:** `shift/` · ~2,250 lines · real. Process supervision, boot ordering,
  power states; own `echo_ipc.rs`. **Status:** 🚧 real.

### Puppis — Gecko native-app lifecycle
- **Path:** `puppis/` · ~700 lines · real `suspend_app`/`resume_app` freeze/thaw.
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
- Bloom dead-code/lint cleanup + test coverage.

### Nice-to-have 💡
- Window snapping/tiling; minimize/restore animations.
- `pyxis://` headless background apps (`PYXIS_BACKGROUND`).
- Screencopy-based live dock thumbnails.
- Real fps/memory in the `GetRenderingStatus` IPC method (signal path is real).

---

## Change log

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
