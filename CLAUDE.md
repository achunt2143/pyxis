# GeckoOS / Pyxis — Project Reference

Consolidated reference for Claude Code sessions in this repo. This is the *index and current-state summary*; the detailed research/design docs it's built from live at the paths listed in [Source documents](#source-documents) — read one of those directly when you need full depth on that subsystem.

**Public product name:** Pyxis. **Internal/dev codename:** GeckoOS. Both names are used interchangeably across the codebase (crate names use `geckoOS-*`, systemd units and docs say `pyxis`/`Pyxis`).

## What this project is

A full desktop Linux OS built from scratch, architecturally descended from two real systems: **Firefox OS/B2G** (web-tech app runtime, IPC-centric service bus) and **webOS's `LunaSysMgr`** (layer-based Wayland compositing, card-style window management). Core system components are Rust; all shell/app UI is web tech (React/Vite) rendered through Gecko/Firefox.

**Central design principle — compositor-first, IPC-driven:** every subsystem talks over one message bus (Echo); all windowing goes through one Wayland compositor (Evo); the desktop shell (Bloom) is web tech; apps — web or native — are ordinary Wayland clients. Nothing renders by being reparented into anyone else's process or DOM; Evo composites every window directly.

## Component map

| Name | Role | Crate/repo path | Status |
|---|---|---|---|
| **Evo** | Wayland compositor & window manager (formerly "Ouranos") | `evo/` (workspace member) | Real, mature (~16.5k lines). Real two-step window-create/claim contract bound to Echo sessions. Owns server-side decoration for app windows — see [Window decoration model](#window-decoration-model). |
| **Echo** | IPC message bus (D-Bus/LS2-style: `destination`+`interface`+`method` routing, service registration) | `echo/` (workspace member) | Real. Routing, registration, forwarding all functional. |
| **Bloom** | Desktop shell UI (wallpaper/status-bar/dock/launcher/overlay layers) | `pyxis-experience/` (sibling repo; `package.json` name is literally `"bloom"`) | Visually complete, richly interactive, **100% simulated** — see [Bloom status](#bloom-pyxis-experience-status) below. |
| **Flow** | React component/design-system library for Bloom's 2000s-adjacent aesthetic | — | Aspirational naming only — Bloom currently uses plain MUI + framer-motion, no separate `Flow` library exists yet. |
| **Synergy** | Native-to-web IPC bridge; equivalent of Firefox OS's `mozApps` / webOS's `window.PalmSystem` | `gecko/bridge/` (native host `geckoOS-native-host.rs` + WebExtension `gaia-bridge.js`/`content.js`) | Real for dev/extension use: genuine Unix-socket client to Echo, full `window.geckoOS` API surface (filesystem, appManager, power, events). Does **not** run on the planned `pyxis://` scheme (WebExtensions don't match custom schemes) — production apps need a compiled-in `JSWindowActor` bridge instead (see scheme plan). |
| **Rockhopper** | Hardware abstraction + legacy (FirefoxOS-era) compat layer (formerly "Persephone", then briefly "Shift" — "Shift" is now the system orchestrator's name instead, see below) | `rockhopper/` (workspace member) | Real, substantial (~2,300 lines): `wayland.rs`, `dbus_session.rs`, `elf_loader.rs`, `compatibility.rs`. |
| **Shift** | System orchestrator: process supervision, boot ordering, power states (formerly "Cronus") | `shift/` (workspace member) | **Real bookkeeping over a simulated process layer** (~1,800 lines). The process table, tree, lifecycle log and limits are real; beneath them `fork_and_exec` spawns nothing, resource sampling returns constants, and the Echo methods (`LaunchProcess`, `TerminateProcess`, …) answer success without acting. `shift-daemon` runs at boot and launches nothing. It used to `kill(2)` its bookkeeping ids as though they were kernel pids; since 2026-09-25 it refuses to signal anything it didn't spawn, which is everything. Wire the Echo stubs to the library **only** after `fork_and_exec` is real. |
| **Vroom** | **Pyxis SDK/PDK packaging toolchain** — builds/packages/installs web (SDK) and native (PDK) apps as `.ipk` via `pyxis`/`pyxis-sdk`/`pyxis-pdk` CLIs (repurposed 2026-07-25; its previous life as a "GPU acceleration / perf monitoring" layer, formerly "Kratos", was audited and confirmed **100% simulated** — hardcoded dashboard numbers, fake GPU structs, zero consumers — and deleted) | `vroom/` (workspace member; spec `vroom/docs/pyxis-sdk-pdk-spec.md`, schemas `vroom/schemas/`, reference script `vroom/tools/`) | Real: `pyxis.json` project config + validation, `appinfo.json`/`CONTROL/control` generation, staging tree, `.ipk` emission (`opkg-build` when present, built-in root-owned tar.gz writer otherwise), web builders (static/vite/webpack via npm), native builders (cmake/meson/cargo/make/qmake), device profiles + scp/ssh `opkg install`, `new` templates, `doctor`/`lint`/`inspect`. **Phase 3 done 2026-07-25** (`vroom/docs/pyxis-phase3-implementation.md`): service packaging (`pyxis.json` `services[]`, policy-gated activation via `/etc/pyxis/allow-packaged-services`), upgrade-aware maintainer scripts (postinst/prerm/postrm + `packaging/` overrides), and the registry/launcher side — which is **Nova + `pyxis-appctl`** (a thin Echo client in `echo/src/bin/appctl.rs`), deliberately *not* the spec's standalone registry daemon. |
| **Installer** | `.ipk`/opkg package installation service (`org.geckoOS.installer`): discovery, metadata-without-install, `opkg install`/`remove`, each followed by a Nova registry refresh. Frontend is the Remochi **App Installer** app (`pyxis-experience/packages/apps/installer`, Wizard-based). | `echo/src/services/installer.rs` (in-process in echo-daemon) | Real (added 2026-07-25). Owns the *package* layer only — opkg owns files + status db, Nova owns identity/launch (hence `Nova::uninstall` refusing ipk apps and pointing at `opkg remove`). Install paths are canonicalized and confined to `/var/lib/pyxis/incoming`, `/media`, `/tmp`, `/home`, `/root`. See `vroom/docs/pyxis-phase3-implementation.md` §7. |
| **Nova** | App runtime host: manifest/origin/install/launch-composition layer above `appHost` (formerly conceptual "pyxis-hostd") | `echo/src/services/nova.rs` (in-process in echo-daemon, **not** a standalone `nova/` crate) | **Real** (~1,900 lines), registered in `echo/src/bin/daemon.rs` (`register_nova_handlers`). Does `manifest.webapp` discovery/validation, `pyxis://` origin mapping (`origins.map`), native-manifest generation, launch composition (delegates to `appHost`), install/uninstall (sha256 integrity + atomic swap), and Evo-visibility-driven suspend/resume. Built in-process rather than as the standalone daemon `nova-runtime-host-plan.md` §8 specifies (deliberate: reuses the one proven request-dispatch path; standalone-crate extraction is a tracked follow-up per the user's "wire in now, extract later" call). At **reload** it pre-generates each app's native `manifest.json` (a `setpriv`-wrapped `vela` launch that drops to geckouser + grants video/render groups) so `appHost.ListApps`/`LaunchApp` see the full app set. **Since 2026-07-25 it is also the ipk-app registry** (SDK/PDK spec §10): scans `/usr/pyxis/applications/*/appinfo.json` (both `web` and `native` runtimes — native apps get a setpriv-wrapped direct-exec manifest, no vela/profile), writes derived manifests to the Nova-owned `/var/lib/pyxis/nova/native/` (AppHost's third scan root; never into the opkg-owned app dir), maps appinfo's §19 permission vocabulary onto the bridge capabilities (deny-by-default for unmapped ones), and refuses `Uninstall` for opkg-owned apps. Driven from devices by `pyxis-appctl` (rescan/list/info/launch/close/focus). |
| **gecko** | Gecko/Firefox integration + native bridge host process | `gecko/` (workspace member) | Native-messaging host (`native-host.rs`) is real and working. `gecko/src/gecko.rs`'s libxul-FFI-embedding scaffolding is **obsolete direction** — no maintained desktop libxul embedding API exists (GeckoView is Android-only, EmbedLite is Sailfish-only/stale). Superseded by the from-source custom-Gecko-build plan (see below); `gecko.rs` should be repurposed as launcher/supervisor, not an embedder. |
| **Puppis** | Gecko native-app lifecycle management (launch/suspend/resume/terminate, resource tracking) | `puppis/` (workspace member) | Real (~730 lines), used by Rockhopper's daemon (`GeckoAppLauncher`). Extracted 2026-07-07 from the old `gaia` crate — `gaia` was a full legacy shell (its own actix-web HTTP/WebSocket server serving a static JS UI, long superseded by Bloom, plus several source files that turned out to be entirely orphaned — never even wired into its own module tree). Only `gecko_app_launcher.rs`, the one piece of `gaia` genuinely still in use, was kept; the rest (`ui_server.rs`, static assets, its own now-unused `node_modules/` JS build pipeline) was deleted rather than carried forward under a new name. `suspend_app`/`resume_app` (real webOS-style freeze/thaw logic) already live in this crate — still worth wiring into Nova/appHost per the roadmap below, just not discarded. |
| `pyxis-evo` | — | sibling repo `pyxis-evo/` | **Dead end, not part of the build.** Confirmed 100% unmodified fork of upstream `cosmic-comp` (System76) — zero Pyxis commits, zero Echo/embedding code, never referenced in the Makefile. Not to be confused with the real compositor, `evo/`. Candidate for archival. |
| mini | Prebuilt kernel build (bzImage/init.cpio consumed by `create-rootfs.sh --kernel-prebuilt`) | `mini/` | Build-support only, out of scope of the Pyxis-specific docs. (`gonk/` removed 2026-07-07: a single orphaned `gonk.rs` describing itself as an Android-HAL-replacement — never a workspace member, never compiled, fully superseded by Rockhopper's real hardware-abstraction layer.) |

## Target app-runtime model

Apps — web or native — are **independent Wayland clients that Evo composites directly**, never DOM-embedded surfaces reparented into Bloom. This retires an earlier framing (in `PYXIS_ASSESSMENT.md`) that treated Bloom's `WaylandSurfaceView` placeholder as a gap to *fill in*; per `pyxis-webos-parity-analysis.md` it should instead be **deleted** — Bloom's card view should be chrome/metadata only (title, icon, thumbnail via `wlr-screencopy`, which `evo/src/enhancements/screencopy.rs` already provides), with Evo owning all real compositing.

**App serving is the biggest newly-scoped workstream.** There is no maintained way to embed Gecko in a native host anymore, so the plan is to **build a customized Gecko from source** (`vela`, pinned to an ESR branch) with three compiled-in additions:
1. A native `pyxis://<app-id>/<path>` protocol handler (`nsIProtocolHandler`, backed by `nsIStandardURL` so each app gets a real, isolated, secure-context origin — `pyxis://<app-id>` — mapped to `<apps-root>/<app-id>/webroot/`).
2. A compiled-in `window.geckoOS` bridge via `JSWindowActor` (since WebExtension content scripts can't run on `pyxis://`), matching Synergy's existing API shape exactly.
3. A `vela --app=<id>` chromeless kiosk run mode — one Wayland toplevel per app, `WM_CLASS`/title stamped with `PYXIS_APP_ID` so Evo's existing two-step window-create/claim contract binds it deterministically.

Full implementation plan: `pyxis-gecko-embedding-plan.md` (why/approach) and `pyxis-scheme-and-kiosk-plan.md` (protocol handler + kiosk mode, file-by-file).

**Nova** (real, in-process at `echo/src/services/nova.rs` — see the component table above; the standalone `nova/` crate `nova-runtime-host-plan.md` §8 describes was **not** built, in-process was chosen instead) sits above `echo`'s existing `appHost` and owns what `appHost` intentionally leaves out: web-app `manifest.webapp` parsing/validation, `pyxis://` origin mapping, install/uninstall with integrity verification, single-instance policy, and composing the concrete `vela --pyxis-app=<id>` launch command (wrapped in `setpriv` to drop to geckouser). It does **not** duplicate process supervision or surface binding — those stay in `appHost`/`evo`. Full design: `nova-runtime-host-plan.md`.

## Bloom (`pyxis-experience`) status

Visually complete desktop shell (draggable/resizable windows, taskbar, launcher, terminal/files/settings/browser apps) but **entirely simulated client-side**:

- `src/services/EchoClient.ts` checks for `window.__gecko_ipc`, which nothing in the stack ever sets. Synergy's actual bridge exposes **`window.geckoOS`** (via `postMessage`/custom events in the WebExtension form). Every `EchoClient` call silently falls through to an in-memory mock (`setTimeout`-staged fake sessions, `Math.random()` PIDs). **This is the single most impactful fix** — a rename/adapter task, not new architecture — and it would make Bloom talk to Synergy's already-real bridge today, even before `pyxis://`/Nova exist.
- `DesktopWindow.tsx`'s `WaylandSurfaceView` is a placeholder `<div>` — per the target model above, this should be **deleted**, not implemented, and replaced with chrome-only card metadata.
- Known code health issues: the README is still the stock Vite template. Lint is clean (`npx eslint .` → 0) and `npm test` green as of alpha4; keep both there.
- **The app list is dynamic as of alpha 4** (`src/hooks/useApps.ts`): Nova's `List` is the source of truth for the launcher, drawer, search, and dock, refreshed on `AppInstalled`/`AppUninstalled`/`RegistryChanged`. `PYXIS_APPS` survives only as a *presentation overlay* (Lucide glyph, accent, pin state, default window size for built-ins); an app absent from it still appears, drawn via `AppIcon`'s manifest-icon → generic-glyph fallback. `entryUrlFor` resolves `pyxis://<id>/<entry>` on device and the package's dev-server port off it. Verified in the browser: installing `weather` makes it appear in drawer + search, removing it drops it, with no code change.
- Git branches: all five remote feature branches are fully merged into `main` or stale pre-merge snapshots — no unmerged work is at risk, though `feature/app-host-client` and `feature/app-surface-embed`'s remote-ref tips are confusingly swapped and worth pruning.

## Window decoration model

**Decided, and implemented: Evo owns server-side decoration for app windows.**
One compositor-drawn frame for every client, so decoration stays consistent
instead of varying with whatever theme each toolkit applies.

How it actually works — this is non-obvious and cost a full debugging cycle to
establish, so don't re-derive it:

- **GTK3, and therefore vela, never binds `zxdg_decoration_manager_v1`.** Evo
  advertises it; the client ignores it. Any fix attempted in Evo's
  `XdgDecorationHandler` will change nothing for a vela window.
- The protocol GTK3 *does* speak is **`org_kde_kwin_server_decoration`**. Evo
  offers it with default mode `Server` (`state.rs`), and
  `xdg.rs::ack_configure` derives `is_ssd` from whichever protocol the client
  actually negotiated.
- `MOZ_GTK_TITLEBAR_DECORATION=none` (set in `create-rootfs.sh`) is the
  **client-side half and is load-bearing**: it stops Firefox forcing its own
  CSD, which is what makes GDK request `Server`. Without it vela requests
  `Client` and draws its own titlebar. `GTK_CSD=0` is a no-op on Wayland.
- **Bloom is explicitly exempt** (`is_shell` check in `ack_configure`). The
  shell is the desktop; a titlebar on it is always wrong. The guard is
  load-bearing rather than defensive — bloom negotiates `Server` like every
  other app, and only `--kiosk` (passed only in fullscreen boot mode) makes it
  request `Client`.

Consequence for the open bugs: #7 (maximize→unmaximize crash), #8, #9 and #11
(resize) are all **Evo SSD work**, not GTK's. They are not blocked on a
decision — the decision is above.

## Known code-level issues to track

- `echo/src/services/appmanager.rs:102` — no signature verification before tarball extraction (Nova's install plan formalizes the fix; not yet implemented). `appmanager` is superseded by Nova and should probably just be deleted.
- `evo` TODOs: HiDPI scale not read from render surface (`udev.rs`), hardcoded fps/memory in the `GetRenderingStatus` **IPC method** (the signal path is real), X11 override-redirect ordering (`shell/x11.rs`).
- `evo` open bug: no user-driven window resize (#11, alpha 5). Its first concrete defect is known: `enhancements/decorations.rs`'s corner resize handles can never fire, because the side-border regions exclude the corners that `get_resize_handle` looks for. **`test_resize_handle_detection` is red on purpose** — it's correct, and it's the one expected failure in `cargo test --workspace`. (#7 maximize→unmaximize and #10 restore-from-dock were fixed and confirmed in alpha 4.)
- `echo`'s `org.geckoOS.Echo` `ListServices` reads only `ServiceRegistry`, which
  is written solely by the over-the-wire `RegisterService` call — never by the
  in-process services `daemon.rs` installs via `register_handler`, so it answers
  "(no services registered)" on a booted device. **It is kept that way on
  purpose** (external clients may depend on its `service_name`/`service_id`/
  `registered_at` shape), which makes it a trap for new code rather than a bug
  to fix: to find out what is actually on the bus, call
  `org.geckoOS.system.listServices` (`echo/src/services/system.rs`, added
  2026-09-20), which merges the router's handler table with the registry and
  resolves `reachable` per entry against the live connection table. `echo-cli
  services`/`status` were repointed at it 2026-09-20 and now print
  `kind`/`reachable`/method counts; `echo-cli ping` no longer times a service
  listing at all, it calls the no-op `system.ping`. Still open: the registry is
  never pruned on disconnect, so a registered-then-crashed client keeps its
  entry forever — the listing now shows it as unreachable instead of implying
  it is running.
- **Permissions ARE enforced at the bridge** — this file said the opposite for months. `PyxisBridgeParent.sys.mjs`'s `_capabilityFor(service, method)` maps each call to a capability (`filesystem.writeFile` → `filesystem:write`, `readFile`/`readDirectory`/`exists`/`stat` → `filesystem:read`, …) and throws `Pyxis capability denied: app '<id>' did not declare '<cap>'` when the app's declared list lacks it. Verified on device 2026-09-21 the hard way: a probe page dropped into the `files` app wrote nothing at all, because `files`' `manifest.webapp` declares only `filesystem:read`. What is still missing is the *visible* half — an install-time prompt, a per-app view, and revocation (roadmap item 6) — not the check itself.
- `pyxis-experience`'s README is still the stock Vite template. Test coverage is no longer zero — Vitest is wired up (`npm test`), with `src/hooks/__tests__/novaContract.test.ts` asserting the mock's `nova.List` shape against fields **parsed out of `nova.rs` itself**, so adding a field to `to_info_json` without teaching the mock fails at the desk. That suite is the pattern to extend; it is still the only one.
- **App windows must stay out of SessionStore, and chrome must be measured
  *settled*.** `SessionStore.restoreWindowFeatures()` reasserts every
  `WINDOW_HIDEABLE_FEATURES` entry as `.visible = true` when the restore state
  carries no `hidden` field, which rewrites a window's chrome flags and
  regenerates `chromehidden` — turning a correctly-opened chromeless app
  window into a normal Firefox window (40px of `#nav-bar`) about two seconds
  after load. Fixed 2026-09-25 by giving `pyxisapp` windows the same guard
  upstream gives its own Taskbar Tabs windows
  (`gecko/patches/pyxis/browser/components/sessionstore/SessionStore.sys.mjs.PYXIS_DIFF`
  — read its header before touching app-window chrome). The reusable lesson:
  **a chrome check made at window open passes on a broken build.** Four
  diagnoses were wrong because every probe fired at or before load;
  `PYXIS-DIAG chrome-late:` logs the settled state 5s in and is what
  `scripts/serial-tests/kiosk-relaunch.txt` asserts on.
- **The kernel has no `CONFIG_USER_NS`** (`# CONFIG_USER_NS is not set`), so
  vela's content sandbox is genuinely degraded — unprivileged user namespaces
  are unavailable. Firefox's own `sandbox-unprivileged-namespaces` warning was
  telling the truth; it is suppressed in app windows
  (`security.sandbox.warn_unprivileged_namespaces=false` in nova.rs's user.js
  template) because a notification bar in an app window is noise, **not**
  because the problem is solved. Enabling it is a kernel rebuild and belongs
  with roadmap item 6.
- **remochi's `Toggle` types don't match its runtime**: `index.d.ts` declares a `checked` prop, but the exported component reads `value` and calls `onChange({ value })`. Passing `checked` type-checks and is silently ignored. Bloom bridges this with a locally-typed `Switch` cast in `QuickSettingsMenu.tsx` and `SettingsApp.tsx` — check for that trap before adding a new `Toggle`.

### Corrected 2026-08-16 (were stale, verified fixed — do not re-investigate)

`dbus.rs`'s `todo!()` test, the `ouranos.service` unit, Bloom's dead components/orphan `.scss`/`tsconfig-json.json`, the "12 `no-explicit-any` errors", `AppDrawer`'s phantom launcher entries, `useEcho.ts`'s react-hooks/refs violation, "Evo minimize not implemented", `apphost.rs`'s doc comment, and the claim that Nova scans `/usr/palm/applications` (it scans `/usr/pyxis/applications`) were all carried here long after the code stopped matching them.

## The one thing webOS got right that Pyxis hasn't built

webOS's LS2 bus enforced a **JSON role-file allow-list per identity** *at the bus itself*, before any message was delivered. Pyxis enforces at the **bridge** instead (`PyxisBridgeParent._capabilityFor`, checked against the app's declared `permissions[]` — confirmed working on device 2026-09-21), which covers every `pyxis://` app but **not** a native client that opens the Echo socket directly: `echo-cli` can still call anything. So the gap is narrower than this section used to claim — it is bus-level enforcement for non-bridge clients, plus the user-visible half (install-time prompt, per-app view, revoke) in roadmap item 6. See `nova-runtime-host-plan.md` §5/§7 and `pyxis-webos-parity-analysis.md` §4 item 6.

## Consolidated roadmap (dependency order)

0. **Decide + build the app-serving strategy** — `pyxis://` scheme + from-source `vela` Gecko build. Blocking prerequisite for real web-app launching; the biggest unplanned workstream. (`pyxis-gecko-embedding-plan.md`, `pyxis-scheme-and-kiosk-plan.md`)
1. **Unify the JS bridge contract** — ✅ done: Bloom talks to the real `window.geckoOS` bridge (now an `@pyxis/app-sdk` monorepo package with `EchoClient`/`useEcho`/`useAppHost`).
2. **Build Nova** — ✅ done (in-process at `echo/src/services/nova.rs`, not the standalone crate `nova-runtime-host-plan.md` §8 describes). Registry/discovery, launch composition (setpriv-wrapped `vela`, delegates to `appHost`), install/uninstall, suspend/resume, origins map, permissions vocabulary all real. Bloom launches via `nova.Launch` and reads its app list from `nova.List` (alpha 4, `useApps.ts`). Standalone-crate extraction is a deferred follow-up.
3. **Wire Puppis's card lifecycle** (`suspend_app`/`resume_app`, already real code, not yet called by anything) behind `appHost`/Nova, driven by Evo visibility/focus events, wired to Gecko's own background-tab throttling. (Nova's own `handle_window_minimized`/`handle_window_restored` already drive `appHost.suspend`/`resume` off Evo's `WindowMinimized` signal — this item now covers wiring Puppis's freeze/thaw specifics on top.)
4. **Wire real status/notifications** — replace Bloom's static mock battery/network values with real Rockhopper Echo signals.
5. **Delete `WaylandSurfaceView`** — replace with chrome-only card metadata + `wlr-screencopy` thumbnails.
6. **Build the security/capability model** — per-app permission allow-list at the bridge boundary, tarball signature verification, sandboxing/cgroups for spawned apps.
7. **Finish Evo's rough edges** — minimize, HiDPI scale, real fps/memory stats, X11 ordering.
8. **Housekeeping** — update the architecture recap for Vroom's new SDK/PDK role; decide `pyxis-evo`'s fate (archive recommended); extend Bloom test coverage beyond alpha 4's Nova contract suite; replace Bloom's stock-Vite README. (Resolved: `gaia/` retired 2026-07-07; the `dbus.rs` test, `ouranos.service`, `apphost.rs`'s doc comment, and Bloom's dead code/lint errors/phantom entries are all done — see "Corrected 2026-08-16" above.)

## On-device verification

`scripts/serial-test.py` boots the image headless and drives it over the
serial console — the project's only verification channel, since Evo needs
virgl and QMP `screendump` reports "no surface" on a GL scanout (re-checked
on QEMU 10.2.1 with `device`/`head` variants; it is not stale tooling).

```bash
scripts/serial-test.py --script scripts/serial-tests/alpha4-ipk-e2e.txt
scripts/serial-test.py -c 'opkg list-installed' --keep-running
scripts/serial-test.py --memory 4096 --script scripts/serial-tests/alpha4-exit-criteria.txt
```

Suites assert: a `#!must <text>` / `#!must-not <text>` line applies to the
next command and fails the run. Without them a suite only *prints*, and a
run in which every step errored still exits 0 — which has happened, twice.
The four suites live in `scripts/serial-tests/`.

It runs on a qcow2 overlay, so the image is never mutated and each run
starts clean, and it can `--push` a file in by chunked base64 (sha256
verified) when there is no other way to get one into the guest. The script's
docstring carries the serial gotchas that have each cost hours.

A full rebuild + verify is one command, `bash scripts/rebuild-and-verify.sh`
— it does the no-root work first, keeps the two root steps together, stages
the acceptance package into the rootfs *before* the image is built from it
(getting that order wrong yields an image that looks fine and has nothing to
install), and then runs all four suites.

**What it cannot do is look at the screen, or move the mouse.** Anything
whose acceptance is visual — or that is *about* pointer input, like bug #7's
press/release double-dispatch — needs `make build-release && make run` and a
human. Note the rootfs stages need root, so a build cannot be fully
automated either.

**The 2 GB reference VM will not run all 8 built-in apps at once.** At the
`emulate.sh` default the guest saturates (1929/1987 MB) and the last apps to
launch stay alive but never map a surface, with no OOM kill to explain it —
which reads exactly like a compositor bug and is not one. Pass
`--memory 4096` when a suite runs more than about five apps.

## Source documents

Read these directly for full depth — this file is a summary, not a replacement:

- `PROJECT_STATUS.md` — **living status board**: per-module overview (what/where/how) plus done / in-progress / to-do / backlog / nice-to-have status, the alpha2 punch-list snapshot, and a per-session change log. Start here for "what's the current state and what's next."
- `PYXIS_ASSESSMENT.md` — original code-grounded audit distinguishing the dead `pyxis-evo` fork from the real, mocked-but-mature `pyxis-experience`/Bloom shell. Its "surface embedding gap" framing is superseded by `pyxis-webos-parity-analysis.md`'s recommendation to delete rather than fill the placeholder.
- `vroom/docs/pyxis-architecture-recap.md` — canonical component-name map and system diagram (Evo/Echo/Bloom/Synergy/Rockhopper/Shift/pyxis-hostd); note it predates Nova's naming and describes Vroom's retired GPU-layer role, not its current SDK/PDK role.
- `vroom/docs/pyxis-sdk-pdk-spec.md` — the Pyxis packaging-system spec (`.ipk`/opkg, `pyxis.json`, `appinfo.json`, SDK/PDK CLI model) that `vroom/` implements; JSON Schemas in `vroom/schemas/`, reference shell packager in `vroom/tools/`.
- `pyxis-webos-parity-analysis.md` — grounds the architecture against real webOS/`LunaSysMgr` internals component-by-component; identifies the app-serving gap; the source of the "delete `WaylandSurfaceView`" recommendation.
- `webos-lunasysmgr-architecture.md` — background research: how webOS 3.0.5 actually booted and ran apps (LS2 bus, `LunaSysMgr`'s three hats, app packaging/launch flow), pulled from the real TouchPad rootfs/binaries.
- `webos-webkit-upgrade-plan.md` — background research: a hypothetical WPE-WebKit replacement plan for webOS's system WebKit; established that no maintained desktop embedding API exists for modern engines, which fed directly into the Gecko approach below.
- `pyxis-gecko-embedding-plan.md` — commits to the `pyxis://` custom-scheme + from-source Gecko build ("Approach A") over libxul FFI embedding ("Approach B", rejected).
- `pyxis-scheme-and-kiosk-plan.md` — file-by-file implementation plan for the `pyxis://` protocol handler and `vela` kiosk run mode.
- `nova-runtime-host-plan.md` — full crate design for `nova/` (app manifest/install/launch-composition layer).
