# GeckoOS / Pyxis — Project Reference

Consolidated reference for Claude Code sessions in this repo. This is the *index and current-state summary*; the detailed research/design docs it's built from live at the paths listed in [Source documents](#source-documents) — read one of those directly when you need full depth on that subsystem.

**Public product name:** Pyxis. **Internal/dev codename:** GeckoOS. Both names are used interchangeably across the codebase (crate names use `geckoOS-*`, systemd units and docs say `pyxis`/`Pyxis`).

## What this project is

A full desktop Linux OS built from scratch, architecturally descended from two real systems: **Firefox OS/B2G** (web-tech app runtime, IPC-centric service bus) and **webOS's `LunaSysMgr`** (layer-based Wayland compositing, card-style window management). Core system components are Rust; all shell/app UI is web tech (React/Vite) rendered through Gecko/Firefox.

**Central design principle — compositor-first, IPC-driven:** every subsystem talks over one message bus (Echo); all windowing goes through one Wayland compositor (Evo); the desktop shell (Bloom) is web tech; apps — web or native — are ordinary Wayland clients. Nothing renders by being reparented into anyone else's process or DOM; Evo composites every window directly.

## Component map

| Name | Role | Crate/repo path | Status |
|---|---|---|---|
| **Evo** | Wayland compositor & window manager (formerly "Ouranos") | `evo/` (workspace member) | Real, mature (~16.5k lines). Windows bind to Echo sessions by app id (`BindSurface`, sent when a window's id first appears); there is no create/claim contract (the legacy `CreateWindow`, which spawned programs as root, was deleted 2026-11-07). Owns global shortcuts too — see [Global shortcuts](#global-shortcuts-evo). Owns server-side decoration for app windows — see [Window decoration model](#window-decoration-model). Separates apps from the system by how a client connected — see [Wayland trust classes](#wayland-trust-classes-evo). |
| **Echo** | IPC message bus (D-Bus/LS2-style: `destination`+`interface`+`method` routing, service registration) | `echo/` (workspace member) | Real. Routing, registration, forwarding all functional. |
| **Bloom** | Desktop shell UI (wallpaper/status-bar/dock/launcher/overlay layers) | `pyxis-experience/` (sibling repo; `package.json` name is literally `"bloom"`) | Visually complete, richly interactive, **100% simulated** — see [Bloom status](#bloom-pyxis-experience-status) below. |
| **Flow** | React component/design-system library for Bloom's 2000s-adjacent aesthetic | — | Aspirational naming only — Bloom currently uses plain MUI + framer-motion, no separate `Flow` library exists yet. |
| **Synergy** | Native-to-web IPC bridge; equivalent of Firefox OS's `mozApps` / webOS's `window.PalmSystem` | `gecko/bridge/` (native host `geckoOS-native-host.rs` + WebExtension `gaia-bridge.js`/`content.js`) | Real for dev/extension use: genuine Unix-socket client to Echo, full `window.geckoOS` API surface (filesystem, appManager, power, events). Does **not** run on the planned `pyxis://` scheme (WebExtensions don't match custom schemes) — production apps need a compiled-in `JSWindowActor` bridge instead (see scheme plan). |
| **Rockhopper** | Hardware abstraction + legacy (FirefoxOS-era) compat layer (formerly "Persephone", then briefly "Shift" — "Shift" is now the system orchestrator's name instead, see below) | `rockhopper/` (workspace member) | Real, substantial (~2,300 lines): `wayland.rs`, `dbus_session.rs`, `elf_loader.rs`, `compatibility.rs`. |
| **Shift** | System orchestrator: process supervision, boot ordering, power states (formerly "Cronus") | `shift/` (workspace member) | **Real bookkeeping over a simulated process layer** (~1,800 lines). The process table, tree, lifecycle log and limits are real; beneath them `fork_and_exec` spawns nothing, resource sampling returns constants, and the Echo methods (`LaunchProcess`, `TerminateProcess`, …) answer success without acting. `shift-daemon` runs at boot and launches nothing. It used to `kill(2)` its bookkeeping ids as though they were kernel pids; since 2026-09-25 it refuses to signal anything it didn't spawn, which is everything. Wire the Echo stubs to the library **only** after `fork_and_exec` is real. |
| **Vroom** | **Pyxis SDK/PDK packaging toolchain** — builds/packages/installs web (SDK) and native (PDK) apps as `.ipk` via `pyxis`/`pyxis-sdk`/`pyxis-pdk` CLIs (repurposed 2026-07-25; its previous life as a "GPU acceleration / perf monitoring" layer, formerly "Kratos", was audited and confirmed **100% simulated** — hardcoded dashboard numbers, fake GPU structs, zero consumers — and deleted) | `vroom/` (workspace member; spec `vroom/docs/pyxis-sdk-pdk-spec.md`, schemas `vroom/schemas/`, reference script `vroom/tools/`) | Real: `pyxis.json` project config + validation, `appinfo.json`/`CONTROL/control` generation, staging tree, `.ipk` emission (`opkg-build` when present, built-in root-owned tar.gz writer otherwise), web builders (static/vite/webpack via npm), native builders (cmake/meson/cargo/make/qmake), device profiles + scp/ssh `opkg install`, `new` templates, `doctor`/`lint`/`inspect`. **Phase 3 done 2026-07-25** (`vroom/docs/pyxis-phase3-implementation.md`): service packaging (`pyxis.json` `services[]`, policy-gated activation via `/etc/pyxis/allow-packaged-services`), upgrade-aware maintainer scripts (postinst/prerm/postrm + `packaging/` overrides), and the registry/launcher side — which is **Nova + `pyxis-appctl`** (a thin Echo client in `echo/src/bin/appctl.rs`), deliberately *not* the spec's standalone registry daemon. |
| **Installer** | `.ipk`/opkg package installation service (`org.geckoOS.installer`): discovery, metadata-without-install, `opkg install`/`remove`, each followed by a Nova registry refresh. Frontend is the Remochi **App Installer** app (`pyxis-experience/packages/apps/installer`, Wizard-based). | `echo/src/services/installer.rs` (in-process in echo-daemon) | Real (added 2026-07-25). Owns the *package* layer only — opkg owns files + status db, Nova owns identity/launch (hence `Nova::uninstall` refusing ipk apps and pointing at `opkg remove`). Install paths are canonicalized and confined to `/var/lib/pyxis/incoming`, `/media` and `/home`, and since 2026-11-16 a package must carry a valid detached signature unless the dev-image marker allows unsigned (see "Wave 2" under Bus permissions). See `vroom/docs/pyxis-phase3-implementation.md` §7. |
| **Nova** | App runtime host: manifest/origin/install/launch-composition layer above `appHost` (formerly conceptual "pyxis-hostd") | `echo/src/services/nova.rs` (in-process in echo-daemon, **not** a standalone `nova/` crate) | **Real** (~1,900 lines), registered in `echo/src/bin/daemon.rs` (`register_nova_handlers`). Does `manifest.webapp` discovery/validation, `pyxis://` origin mapping (`origins.map`), native-manifest generation, launch composition (delegates to `appHost`), install/uninstall (sha256 integrity + atomic swap), and Evo-visibility-driven suspend/resume. Built in-process rather than as the standalone daemon `nova-runtime-host-plan.md` §8 specifies (deliberate: reuses the one proven request-dispatch path; standalone-crate extraction is a tracked follow-up per the user's "wire in now, extract later" call). At **reload** it pre-generates each app's native `manifest.json` (a `setpriv`-wrapped `vela` launch that drops to geckouser + grants video/render groups) so `appHost.ListApps`/`LaunchApp` see the full app set. **Since 2026-07-25 it is also the ipk-app registry** (SDK/PDK spec §10): scans `/usr/pyxis/applications/*/appinfo.json` (both `web` and `native` runtimes — native apps get a setpriv-wrapped direct-exec manifest, no vela/profile), writes derived manifests to the Nova-owned `/var/lib/pyxis/nova/native/` (AppHost's third scan root; never into the opkg-owned app dir), maps appinfo's §19 permission vocabulary onto the bridge capabilities (deny-by-default for unmapped ones), and refuses `Uninstall` for opkg-owned apps. Driven from devices by `pyxis-appctl` (rescan/list/info/launch/close/focus). **Every app launch is `pyxis-wlctx`, run as root, which makes the app's own Wayland socket (carrying the app id) and drops to the app user `pyxisapp` (uid 1001) itself** — see [Wayland trust classes](#wayland-trust-classes-evo). |
| **gecko** | Gecko/Firefox integration + native bridge host process | `gecko/` (workspace member) | Native-messaging host (`native-host.rs`) is real and working. `gecko/src/gecko.rs`'s libxul-FFI-embedding scaffolding is **obsolete direction** — no maintained desktop libxul embedding API exists (GeckoView is Android-only, EmbedLite is Sailfish-only/stale). Superseded by the from-source custom-Gecko-build plan (see below); `gecko.rs` should be repurposed as launcher/supervisor, not an embedder. |
| **Puppis** | Gecko native-app lifecycle management (launch/suspend/resume/terminate, resource tracking) | `puppis/` (workspace member) | Real (~730 lines), used by Rockhopper's daemon (`GeckoAppLauncher`). Extracted 2026-07-07 from the old `gaia` crate — `gaia` was a full legacy shell (its own actix-web HTTP/WebSocket server serving a static JS UI, long superseded by Bloom, plus several source files that turned out to be entirely orphaned — never even wired into its own module tree). Only `gecko_app_launcher.rs`, the one piece of `gaia` genuinely still in use, was kept; the rest (`ui_server.rs`, static assets, its own now-unused `node_modules/` JS build pipeline) was deleted rather than carried forward under a new name. `suspend_app`/`resume_app` (real webOS-style freeze/thaw logic) already live in this crate — still worth wiring into Nova/appHost per the roadmap below, just not discarded. |
| `pyxis-evo` | — | sibling repo `pyxis-evo/` | **Dead end, not part of the build.** Confirmed 100% unmodified fork of upstream `cosmic-comp` (System76) — zero Pyxis commits, zero Echo/embedding code, never referenced in the Makefile. Not to be confused with the real compositor, `evo/`. Candidate for archival. |
| mini | Prebuilt kernel build (bzImage/init.cpio consumed by `create-rootfs.sh --kernel-prebuilt`) | `mini/` | Build-support only, out of scope of the Pyxis-specific docs. (`gonk/` removed 2026-07-07: a single orphaned `gonk.rs` describing itself as an Android-HAL-replacement — never a workspace member, never compiled, fully superseded by Rockhopper's real hardware-abstraction layer.) |

## Target app-runtime model

Apps — web or native — are **independent Wayland clients that Evo composites directly**, never DOM-embedded surfaces reparented into Bloom. This retires an earlier framing (in `PYXIS_ASSESSMENT.md`) that treated Bloom's `WaylandSurfaceView` placeholder as a gap to *fill in*; per `pyxis-webos-parity-analysis.md` it should instead be **deleted** — Bloom's card view should be chrome/metadata only (title, icon, thumbnail via `wlr-screencopy`, which `evo/src/enhancements/screencopy.rs` provides for real since 2026-09-25: before that it was a stub that nothing compiled), with Evo owning all real compositing.

**App serving is the biggest newly-scoped workstream.** There is no maintained way to embed Gecko in a native host anymore, so the plan is to **build a customized Gecko from source** (`vela`, pinned to an ESR branch) with three compiled-in additions:
1. A native `pyxis://<app-id>/<path>` protocol handler (`nsIProtocolHandler`, backed by `nsIStandardURL` so each app gets a real, isolated, secure-context origin — `pyxis://<app-id>` — mapped to `<apps-root>/<app-id>/webroot/`).
2. A compiled-in `window.geckoOS` bridge via `JSWindowActor` (since WebExtension content scripts can't run on `pyxis://`), matching Synergy's existing API shape exactly.
3. A `vela --app=<id>` chromeless kiosk run mode — one Wayland toplevel per app, `WM_CLASS`/title stamped with `PYXIS_APP_ID` so Evo binds it to its AppHost session by app id deterministically.

Full implementation plan: `pyxis-gecko-embedding-plan.md` (why/approach) and `pyxis-scheme-and-kiosk-plan.md` (protocol handler + kiosk mode, file-by-file).

**Nova** (real, in-process at `echo/src/services/nova.rs` — see the component table above; the standalone `nova/` crate `nova-runtime-host-plan.md` §8 describes was **not** built, in-process was chosen instead) sits above `echo`'s existing `appHost` and owns what `appHost` intentionally leaves out: web-app `manifest.webapp` parsing/validation, `pyxis://` origin mapping, install/uninstall with integrity verification, single-instance policy, and composing the concrete `vela --pyxis-app=<id>` launch command (wrapped in `setpriv` to drop to geckouser). It does **not** duplicate process supervision or surface binding — those stay in `appHost`/`evo`. Full design: `nova-runtime-host-plan.md`.

## Bloom (`pyxis-experience`) status

Visually complete desktop shell (draggable/resizable windows, taskbar, launcher, terminal/files/settings/browser apps) but **entirely simulated client-side**:

- `src/services/EchoClient.ts` checks for `window.__gecko_ipc`, which nothing in the stack ever sets. Synergy's actual bridge exposes **`window.geckoOS`** (via `postMessage`/custom events in the WebExtension form). Every `EchoClient` call silently falls through to an in-memory mock (`setTimeout`-staged fake sessions, `Math.random()` PIDs). **This is the single most impactful fix** — a rename/adapter task, not new architecture — and it would make Bloom talk to Synergy's already-real bridge today, even before `pyxis://`/Nova exist.
- `DesktopWindow.tsx`'s `WaylandSurfaceView` is a placeholder `<div>` — per the target model above, this should be **deleted**, not implemented, and replaced with chrome-only card metadata.
- Known code health issues: the README is still the stock Vite template. Lint is clean (`npx eslint .` → 0) and `npm test` green as of alpha4; keep both there.
- **The app list is dynamic as of alpha 4** (`src/hooks/useApps.ts`): Nova's `List` is the source of truth for the launcher, drawer, search, and dock, refreshed on `AppInstalled`/`AppUninstalled`/`RegistryChanged`. `PYXIS_APPS` survives only as a *presentation overlay* (Lucide glyph, accent, pin state, default window size for built-ins); an app absent from it still appears, drawn via `AppIcon`'s manifest-icon → generic-glyph fallback. `entryUrlFor` resolves `pyxis://<id>/<entry>` on device and the package's dev-server port off it. Verified in the browser: installing `weather` makes it appear in drawer + search, removing it drops it, with no code change.
- **Notifications and sound are real as of alpha 5.** The Bell opens a
  notification centre fed by echo's `org.geckoOS.notification` store and its
  `NotificationShown`/`NotificationClosed` signals, and toasts respect Do Not
  Disturb. The quick-settings volume slider and Settings → Sound drive
  `org.geckoOS.audio` (PipeWire via `wpctl`). Named system sounds go through
  `org.geckoOS.sound`, with the files in `system-sounds/` at the repo root.
  The SDK side of each lives in `packages/app-sdk/src/{notifications,audio,useAudio}.ts`,
  and each has a Vitest contract suite that reads its Rust source. Toasts
  draw in Bloom's overlay, above app windows (see [Shell overlay](#shell-overlay-bloom-above-apps)).
- **The wallpaper is Daylight, and settings are live across apps, as of
  alpha 5** (2026-11-09): see [Daylight wallpaper and live
  settings](#daylight-wallpaper-and-live-settings).
- Git branches: all five remote feature branches are fully merged into `main` or stale pre-merge snapshots — no unmerged work is at risk, though `feature/app-host-client` and `feature/app-surface-embed`'s remote-ref tips are confusingly swapped and worth pruning.

## Wayland trust classes (Evo)

**Built 2026-11-07.** Evo decides what a Wayland client may do from *how it
connected*, never from what it claims (`evo/src/trust.rs`). The bridge's
deny-by-default permissions cover web apps; a native app talks Wayland
directly, and this is the half that covers it.

- **Trusted** = a client on Evo's own socket (`wayland-0`): the shell's vela,
  root-run tools (the serial harness's `grim`), anything geckoOS-boot starts.
  **App** = a client that arrived through a `wp_security_context_v1` socket,
  and XWayland (one client for every X11 window, so never trusted).
- **Two users** (since 2026-11-07): the shell is **geckouser** (1000), every
  Nova-launched app is **pyxisapp** (1001, home `/home/pyxisapp`, groups
  `video,render` granted per launch, no `/etc/group` membership). `wayland-0`
  is `geckouser:geckouser 0600`, so an app can't `connect()` to it: the host
  socket is reachable only by the shell and root. `/run/user/1000` is **0711**
  (apps reach paths they know inside it, Echo's socket and PipeWire's Pulse
  socket, and can't list it). Constants: `nova.rs` (`APP_UID`, `APP_GID`,
  `APP_HOME`, `APP_GROUPS`, `app_owner`), which `create-rootfs.sh` must match
  (a test reads the script).
- Nova starts every app as **`pyxis-wlctx` itself, as root** (`echo/src/bin/wlctx.rs`):
  `pyxis-wlctx --app-id ID --uid 1001 --gid 1001 --groups video,render
  --runtime-dir /run/user/1001 --host-socket …/wayland-0 --hide …/wayland-0
  --link geckoOS=/run/user/1000/geckoOS -- vela …`. As root it (1) connects to
  the host socket, (2) makes the app's socket in `/run/pyxis/wl/<app>-<pid>/`
  (parent root-owned 0755, so nothing the app runs can swap a component for a
  symlink under root; the instance dir and socket are chowned to the app only
  once root is done) and hands it to Evo with sandbox engine `pyxis`, the app
  id and an instance id, (3) makes the app's runtime dir `/run/user/1001`
  (app-owned 0700; root only ever *adds* a missing `geckoOS` link, so Echo's
  socket is at the path the bridge reads, and never touches a planted entry),
  (4) `unshare(CLONE_NEWNS)`s and bind-mounts `/dev/null` over the host socket
  for the app only (best effort and said so: the mode already refuses the app,
  this makes the socket absent too, and is meaningful now that the uid differs:
  `/proc/<shell>/root` isn't reachable from the app), (5) forks, and **in the
  child drops groups, gid, uid, checks the drop took and `setuid(0)` fails,
  and only then arms `PR_SET_PDEATHSIG`**, then execs with `WAYLAND_DISPLAY`
  set to the absolute socket path. It stays as the app's parent (so the
  session's process group is unchanged), forwards TERM/INT/HUP, and removes the
  directory on exit (a SIGKILLed helper's is swept by the next launch).
  appHost's recorded session pid is the *helper's*, so the app gets
  `PR_SET_PDEATHSIG` SIGKILL: a kill aimed at the session pid takes the app too.
  **That is why the drop is the helper's and not `setpriv`'s**: a credential
  change clears the parent-death signal (`commit_creds`). The earlier design ran
  the helper *after* a `setpriv` drop for that reason; with the helper as root
  the drop has to be its own, with the signal armed after it.
  **It fails closed**: no context, no drop, a missing group, or `--uid 0` and
  the app doesn't start, and run as root without `--uid` it refuses (running
  the app on the host socket, or as root, would make it the system). Run as an
  ordinary user with no `--uid` it keeps its old behaviour (socket under
  `$XDG_RUNTIME_DIR/pyxis-wl/`, no drop), which is how `alpha5-wayland-trust`
  starts its probes.
- **Everything that assumed one uid** (done): each app's profile
  `/var/lib/pyxis/profiles/<id>` and data dir `/var/lib/pyxis/data/<id>` are
  pyxisapp's, **except the shell's** (`bloom`, plus the ids in
  `PYXIS_SHELL_APPS`, which geckoOS-boot sets from the boot app): those stay
  geckouser's because the boot script's vela, not Nova, runs them. A dir left
  by an earlier image (geckouser's, with files inside) is taken over, contents
  included, by `chown_tree` (`filesystem/scope.rs`), which never follows a link
  (every level opened `O_NOFOLLOW`, entries changed with `AT_SYMLINK_NOFOLLOW`,
  hard-linked files left alone). Nova writes `user.js` into the app-owned
  profile by unlinking and `create_new`, so a symlink planted there isn't
  followed by root. The filesystem service's `app:<id>` calls run as the app's
  owner (`Owner::of_app`); `user` and `system` stay geckouser's (the Files
  app reads *geckouser's* home). Apps' `HOME` is `/home/pyxisapp`, their
  `XDG_RUNTIME_DIR` `/run/user/1001`, audio is `PULSE_SERVER=unix:/run/pyxis/pulse/native`, a second address PipeWire's
  Pulse server listens on (`10-pyxis-apps.conf`; the socket is 0777, the dir
  geckouser's 755, made by geckoOS-boot). Not the default
  `/run/user/1000/pulse/native`: its dir is 0700 and every libpulse client of
  geckouser's puts it back to 0700 on connect, so it can't be loosened for good.
  PipeWire itself stays geckouser's, `wpctl` is echo's. The GPU needs only the `video`/`render` gids,
  set per launch. The Gecko content sandbox is unchanged in kind: it runs as
  pyxisapp with user namespaces.
- **Every app has its own uid since 2026-11-16** (wave 2, see [Bus
  permissions](#bus-permissions-and-hardening-wave-1)): pyxisapp 1001 is now
  only the fallback (`PYXIS_PER_APP_UID=0`, or a uid table that can't be
  written). What is above about "pyxisapp" holds per app: its own uid and
  private gid, its data dir as `HOME`, `/run/user/<uid>`, and a private `/tmp`.
- Why a security context, not the alternatives: *pid lookup in appHost*
  is asynchronous (a global's `can_view` filter is synchronous), racy against
  pid reuse, and makes the compositor call back into Echo from its hot path.
  *A separate shell socket* gives only a class, not an identity (the app id
  would still be the client's claim). The security context hands Evo the class
  and the app id in the connection itself, in a standard protocol Evo already
  carried; the uid split is what makes the class unforgeable (the app can't
  reach `wayland-0` at all).
- **Globals hidden from apps** (`trust::trusted_only`, used as the global
  filter): wlr-screencopy, data-control, virtual-keyboard, input-method,
  security-context (so an app can't mint contexts or choose its app id), and
  wlr-layer-shell (a layer surface can cover the screen above every window
  and take the keyboard exclusively). A hidden global is also refused when
  its registry name is guessed.
- **Roles and identity.** `bloom` / `bloom-overlay` treatment
  (`WindowElement::is_shell`/`is_overlay`/`is_shell_window`) needs a trusted
  client; an app claiming those ids is an ordinary window. For an app, the
  window's identity (`WindowElement::app_id`) is the context's app id, used
  for BindSurface, `ListWindows` and the title bar's fallback;
  `claimed_app_id` is the raw xdg app_id / X11 class. The origin is recorded
  in the window's `user_data` in `new_toplevel`; a window with none (X11) is
  an app. appHost's bind still waits on the *claimed* id being a real one
  (`is_bindable_app_id`), because a vela window that hasn't run its PYXIS
  block claims `vela-default` and must not bind, but binds under the context's id.
- Not filtered, on purpose: `ext-idle-notify` (a timing side channel only;
  smithay has no filter for it), `keyboard-shortcuts-inhibit` (honoured for the
  focused app, as a game would expect), xdg-activation/foreign (focus- and
  token-bound). A window's *title* is still the client's text, but since
  lane E the bar's primary label is the app's name from `SetAppIdentity`,
  looked up by the trusted app id, with the client's title after it, quieter:
  an app captioning itself "Files" reads "Weather · Files". With no table
  (Bloom on a vela without the bridge entry) the title is the client's own.
- On-device suite: `scripts/serial-tests/alpha5-wayland-trust.txt`, with
  `pyxis-wlprobe` (`echo/src/bin/wlprobe.rs`) as the test client. Both new
  binaries are staged by `create-rootfs.sh`; for `--push` runs they must be
  pushed, or **no app will launch** (Nova's manifests name the helper).

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

**Identity (built 2026-11-11, visual-identity lane E; `evo/src/shell/{identity,busy,ssd}.rs`,
plan `docs/internal/pyxis-visual-identity-plan.md`).**

- **Waterdrop frame**: corners `identity::CORNER_RADII` = 16 4 16 16
  (top-left, top-right, bottom-right, bottom-left), as `identity::Radii`,
  square when maximized/fullscreen. Lane T's contract test in pyxis-experience
  reads `identity.rs` by name: keep `CORNER_RADII` and the `SPINNER_*`
  constants' names and shapes. Every rounded consumer takes all four corners:
  the glass and border arcs (`ssd.rs`), the shadow (`ShadowStyle.radii`; the
  closed form picks each row's left and right corner, so per-quadrant is
  exact), Bloom's open clip (`BloomFx` lands each corner on its own), the
  glint, Lumen's `Params.radii` (Droplet starts from
  `WindowElement::picture_radii`, so a maximized window starts square;
  Recede likewise). The shaders share `radii_glsl!()` (`corner_of`, `rbox4`;
  y down). Evaporate does not clip (its snapshot carries the frame's alpha).
  The client clip (`ClippedSurface`) is unchanged: both bottom corners are 16
  (`depth::CONTENT_RADIUS`, a const assert keeps them equal).
  `cargo test dump_shaders -- --ignored` with `EVO_SHADER_DUMP=<dir>` writes
  every such shader out for a host GLES compile (surfaceless EGL works on the
  dev box, radeonsi and llvmpipe).
- **Controls**: the remochi spinner's shapes and colours (`SPINNER_*` and
  their outlines), 14 px centred in the 32 px targets, rasterized from SDFs
  (`rasterize_control`). Inactive: outlines in the title's quiet ink,
  crossfading with the filled shapes over the focus token. Controls and the
  glass are drawn at the window's alpha; only the label dims when inactive
  (a dimmed glass turned the wallpaper's colour). **Every title-bar buffer is
  premultiplied** in `memory_buffer` (the renderer blends textures as
  premultiplied): rasterized straight, anti-aliased edges showed as a bright
  fringe, and light text on the dark bar smeared.
- **Busy** (`shell/busy.rs`): `xdg_wm_base.ping` when a window takes focus
  (`focus_changed`) or is clicked (`on_pointer_button`), and every 5 s
  (`busy_tick`, in `process_pending_window_actions`); no pong in 2 s = busy
  until the next pong. State is per client, in the `ShellClient`'s data map
  (smithay allows one ping out at a time). Busy slots run the spinner's
  morph (`busy::morph`, 1.1 s, slots a third apart); a hovered slot shows its
  own control, so close stays close. Reduced: each slot blinks its colour in
  place; off: each shape's own dark outline (inactive's are quiet ink).
  `ListWindows` entries gain `busy`. `pyxis-wlprobe window APP SECS
  [--no-pong | --pong-after N]` is a mapped, SSD test window.
- **Title** (`identity.rs`): `org.geckoOS.evo SetAppIdentity {apps:[{app_id,
  name, accent, icon}], dark}` → `{ok, apps, icons}` (bridge `windows:manage`,
  **needs a vela rebuild** for Bloom to send it; echo-cli works now). Icons are
  PNG data URLs decoded once and scaled to 20 px; a bad icon or accent is
  dropped from its entry, never costing the name. The bar shows icon + bold
  name looked up by the trusted `app_id()` (never the claim; X11 windows never
  get one), then the client's title in quiet ink only when it adds anything,
  with vela's `pyxis://<id> - ` prefix and ` — <brand>` suffix stripped. The
  bar's fill is frost of the wallpaper since 2026-11-12 (see [Frosted title
  bar and lock screen](#frosted-title-bar-and-lock-screen)); the accent is
  its bottom seam and the ink is fixed per theme.
  `ListWindows` entries gain `title_label`. Bloom: `useAppIdentity`
  (overlay), which rasterizes each app's waterdrop icon on a canvas.
- On-device suite: `scripts/serial-tests/alpha5-identity-evo.txt` (push the
  two icons in `scripts/serial-tests/identity-icons/`; its header has the
  command line). Passed 2026-11-11 on the 11-09 image with evo, echo-cli and
  pyxis-wlprobe pushed. vela's real title is `pyxis://notes - Notes —
  Nightly`, which the bar shows as just "Notes".

**Depth (built 2026-11-07, lane D; `evo/src/anim/{depth,shader}.rs`).** The
frame reads as one rounded card above whatever is beneath it:

- **Drop shadow** under every app window: a GLES pixel shader (Evan Wallace's
  closed-form blurred rounded rectangle, masked to the *outside* of the frame so
  the translucent title bar isn't darkened from behind). Never under the shell,
  the overlay, popups or a window with no bindable app id yet (the
  `vela-default` pre-window); never under a maximized/fullscreen window, where
  it fades out as the window goes square and back in as it leaves. Focused
  windows get a larger, softer, darker shadow, inactive ones a small one, and a
  move grab lifts it further (`lift`: set on the grab's first motion and cleared
  in `unset`, in `grabs.rs`).
- **Rounded bottom corners** on client content (a texture-shader clip,
  `ClippedSurface`), matching the title bar's 16 px top corners; the border
  gained matching bottom arcs. Maximized/fullscreen: square all round
  (`HeaderBar::square`). The clip assumes a buffer that is not y-inverted or
  rotated; the on-device shots would show the *top* corners rounded if not.
- **Focus crossfade**: header glass/title/buttons dim to 55% when inactive, and
  the shadow shrinks, over the `focus` token (150 ms). State per window in
  `DepthSlot` (user data), driven by `anim::depth::focus_changed` from
  `SeatHandler::focus_changed`. Not counted in `GetAnimationState` (these are
  not `WindowAnim`s).
- **Static styling vs animation**: shadows and corners are drawn in `reduced`
  and `off`; only the focus/lift/appear transitions follow the mode (instant
  when off).
- **Plumbing, and why a trait.** Smithay's stock shader elements are
  `RenderElement<GlesRenderer>` only, and udev draws through a `MultiRenderer`.
  `MultiRenderer`/`MultiFrame` hand out the `GlesRenderer`/`GlesFrame` through
  `AsMut`; `GlesAccess` (anim/shader.rs; impl'd for `GlesRenderer` and the udev
  renderer) names that route, and `ShadowElement`/`ClippedSurface` are
  `RenderElement<R>` for any `R: GlesAccess`. **Generic render code that
  touches `WindowRenderElement<R>` therefore needs `R: GlesAccess`.** Shaders
  compile once per `GlesRenderer` (kept in its EGL context's user data). If
  compiling fails (logged once), or with `EVO_DEPTH=off` /
  `evo.SetDepth {enabled:false}`, Evo draws what it drew before depth: no
  shadow, a square bottom, the old border. `evo.GetDepthState` says whether the
  shaders compiled (echo-cli only; no bridge entry).
- **Damage**: a shadow has a stable id per window and a commit counter that
  moves only when its parameters do, so a window at rest costs no damage; a
  window drawn through a pose (open, close, minimize, a geometry transition)
  takes its shadow with it (`depth::pose_frame`). The shadow's draw skips the
  part of each damage rect inside the frame. A shadow is a render element and
  takes no input, so the resize band is untouched.

## Shell overlay (Bloom above apps)

**Decided and built 2026-09-27; the whole shell moved into it 2026-11-07.** Evo
stacks Bloom, the desktop, beneath every app. Shell UI that must be seen over
apps lives in a second Bloom window, the overlay. The alternative was drawing
that UI in the compositor, and it was rejected so shell UI stays web tech.
Layer-shell was rejected too: Gecko can't create layer surfaces, and Evo hides
that global from apps.

**What is where.** The overlay holds *all* shell UI: top bar, dock, quick
settings, notification centre, app drawer, overview, desktop search and
toasts (`src/components/ShellLayer.tsx`, rendered by `src/overlay/OverlayApp.tsx`
inside the same `AppHostProvider`/`CompositorWindowsProvider` the desktop used
to have). The desktop window (`Desktop.tsx`) is now only the wallpaper. Why the
bar and dock moved too, rather than staying in the desktop with a reserved
strip: panels and their state (`quickSettingsOpen`, the drawer, search) are
driven by the bar and dock, so splitting them across two windows would mean
mirroring every open/close over a BroadcastChannel; in one window it is plain
React state. The cost is that the bar's `backdrop-filter` blurs nothing (the
overlay is transparent), so it is a flat translucent bar now.

- **vela** opens it in the shell's own process when passed `--pyxis-overlay
  overlay.html` (geckoOS-boot does, when the shell ships `overlay.html`). It
  gets the app id `bloom-overlay` and chrome that paints nothing
  (BrowserContentHandler and browser-init.js patches). It is transparent only
  because the shell profile sets `mozilla.widget.use-argb-visuals`, an alpha
  visual that Gecko decides at widget creation, and
  `browser.tabs.allow_transparent_browser`.
- **Evo** (`evo/src/shell/overlay.rs`) treats a *trusted* client's `bloom-overlay` like the shell (an app claiming that id gets nothing, see [Wayland trust classes](#wayland-trust-classes-evo)),
  so it is undecorated, unbound, and fullscreened without being exclusive.
  It sorts it *above* apps (`RenderZindex::Top`, drawn over a fullscreen app
  too) and leaves it out of `ListWindows`. Three things come from Bloom, all
  `windows:manage` at the bridge (`PyxisBridgeParent`'s `BRIDGE_CAPABILITIES`,
  so only the shell's trust-listed ids hold them), all applied on the
  compositor thread:
  - **Input region** (`evo.SetOverlayRegion`): Gecko can't set one on a
    toplevel, so Bloom reports where it draws and only those rectangles take
    input; everywhere else a click falls through. Until Bloom reports, the
    overlay passes every click through. Bloom's pieces (bar, dock, a toast
    stack, a panel's full-screen backdrop) each register what they cover in
    `overlay/useOverlayRegion.ts`, which sends Evo the *union* (Evo holds one
    region). Elements marked `data-shell-region` (and their `data-shell-popup`
    children) are measured; measurements repeat a few times after any layout
    change because the dock springs in and nothing observable says when it
    stops.
  - **Keyboard** (`evo.SetOverlayFocus {focused}`): the overlay **never takes
    the keyboard implicitly** (a click on it, `focus_window`, xdg-activation all
    refuse it, so a toast can't steal what an app is typing). While a panel
    that takes typing is open (any panel, or the search box), Bloom asks
    (`overlay/useOverlayKeyboard.ts`); on close it gives it back and Evo hands
    it to the next app down the stack, else the shell (`refocus_after_loss`).
    Evo tracks the real focus in `focus_changed` and emits
    `OverlayFocusChanged {focused}` on every change, including a loss to
    something else (Alt+Tab, a dock click): Bloom closes its panels on that.
    `evo.GetOverlayState` returns `{keyboard, rects, workArea}` for tests.
  - **Work area** (`evo.SetWorkArea {top, bottom, left, right}`,
    `evo/src/shell/work_area.rs`): the edges apps must leave to the shell.
    Maximize, first placement, restore-from-fullscreen, the unmaximize centre
    fallback, new-window `bounds`, the move clamp (a dragged frame stays
    wholly inside the area, so title bar and resize band are never under the
    bar or dock) and the resize clamp (no edge can be dragged out of the area)
    all use the output minus the insets. **A floating window is shrunk to fit,
    not just shifted** (`EvoState::fit_to_work_area`, run on every commit and
    on a work-area change): Firefox restores at the size it chose for the whole
    screen (1152x752), which would put the bottom edge and resize band off a
    800 px screen under a 44 px bar. It asks the client once per size and
    respects its minimum; X11 windows are only moved. Fitting waits while a
    configure is in flight or a size change hasn't landed yet
    (`WindowElement::begin_settling`): Gecko acks a configure at once but its
    toplevel geometry follows only on the next paint, so a commit in between
    shows the old size, and fitting *that* threw an unmaximize's restored
    place away. **Fullscreen still covers everything.**
    Insets are ignored on an axis where they would leave under half of it. A
    change re-fits open windows (`EvoState::refit_to_work_area`: maximized ones
    resize, floating ones under the bar move out). Insets are zero until Bloom
    reports, and are reset, with the region and keyboard, when the overlay
    window is destroyed, so a dead Bloom can't leave apps squeezed.
- **Bloom** (`src/overlay/`, `src/components/ShellLayer.tsx`):
  - `useWorkArea` reports the bar's height and the dock's height plus its
    margin and a gap, measured by layout (`offsetHeight`, computed `bottom`)
    because a transformed box would report the dock 80 px low while it
    animates in. The dock no longer shrinks when an app is maximized (it
    doesn't float over it any more). A fullscreen app hides the bar and dock
    (not drawn, not clickable, last work area kept).
  - A panel on screen registers the whole screen as its region (its backdrop
    takes every click) and asks for the keyboard. The search box, which is in
    the bar and so has no backdrop of its own, portals one (z 1150) while open.
  - **The minimize-before-panel workaround is gone** (`useGlobalShortcuts`
    callers in `ShellLayer`): launcher, overview and search just open their
    panel over whatever is running. Only Super+D still minimizes windows.
  - **"Just type"**: with no app focused, the *desktop* window has the keyboard
    but the search box is in the overlay. `useForwardJustType` (desktop) sends a
    printable key or Ctrl+Space/Meta+K over the BroadcastChannel; the overlay
    opens the box, asks for the keyboard and starts the query with the key.
  - The desktop draws `ShellLayer` itself (`reporting={false}`, telling Evo
    nothing) only while no overlay is alive: the dev server, or an overlay that
    failed to open within 8 s or went quiet for 5 s (`useOverlayPresence`).
    During the overlay's startup the desktop draws no shell, so it doesn't
    flash up and vanish.
- **Verified** by `scripts/serial-tests/alpha5-shell-layering.txt` with clicks,
  not pixels: bar and dock take clicks over a maximized app, its frame starts
  below the bar and ends above the dock, the drawer and search open without
  minimizing it, typing reaches the overlay (Enter launches the app the word
  names), Escape/Alt+Tab return the keyboard, a window dragged to the top stops
  at the bar. `alpha5-overlay`, `alpha5-windows` and `alpha5-resize` read the
  work area (`GetOverlayState`) instead of assuming a maximized frame is
  `0,0 1280x800`.
- **Needs a vela rebuild.** The bridge table gained `SetWorkArea`,
  `SetOverlayFocus`, `GetOverlayState` and the `OverlayFocusChanged` signal in
  `PyxisBridgeParent.sys.mjs`, which is compiled into vela. On a vela without
  it Bloom's calls are refused (logged once in `/var/log/vela.log`): the bar
  and dock still take clicks (`SetOverlayRegion` was already allowed), but apps
  are not kept out from under them and panels get no keyboard. The suite says
  `BRIDGE-OLD-VELA` and stands in for Bloom on that image. Bloom's built shell
  can be laid over a rootfs without a rebuild: `serial-test.py --push
  ../pyxis-experience/dist:/usr/share/geckoOS/apps/bloom/webroot` (a directory
  pushes its files).
- The lock screen lives here too (built 2026-11-12; see [Frosted title bar
  and lock screen](#frosted-title-bar-and-lock-screen)). Not done: no IPC to put
  an app fullscreen, so the fullscreen-hides-the-shell path is unit-reasoned
  and not covered on device.

## Daylight wallpaper and live settings

**Built 2026-11-09** in four lanes (plan and lane split:
`docs/internal/pyxis-wallpaper-settings-plan.md`). Verified on device by
`alpha5-wallpaper.txt` and `alpha5-settings-live.txt`.

- **Settings reach every page live, through Echo.** The SDK used to carry a
  change on a BroadcastChannel, which is same-origin: `pyxis://settings` and
  `pyxis://bloom` are different origins, so on a device a wallpaper or theme
  picked in Settings showed only after the shell restarted (it worked in the
  dev server, one origin, which hid it). Now `org.geckoOS.settings` emits
  `SettingChanged {key, value, previous, exists}` once per key whose stored
  value actually changed (`value` is what `get` would now answer, so a deleted
  system key reads its default), `BRIDGE_SIGNALS` passes it to any page
  (`null`: every key is readable with `settings.get` anyway), and the SDK's
  `onSettingChange` hears it plus the BroadcastChannel and reports each change
  once (2 s de-dupe). `useSetting(key, fallback, parse?)` (`asBoolean`,
  `asNumber`, `oneOf`) is the one way to read a setting in a component.
- **Echo validates and writes safely** (`echo/src/services/settings/schema.rs`).
  Every `pyxis.*` key has a type, an allowed set or range and a default; a bad
  value is refused with an error naming the key, an unknown `pyxis.*` key is
  refused, other keys are stored as given (apps' own). `get` of an absent key
  answers its default with `exists: false`, so a fresh install reads Daylight
  with no special case. One process-wide lock per read-modify-write, atomic
  write (temp, fsync, rename): before, two concurrent sets lost one and a crash
  mid-write emptied the file. The SDK keeps a TS copy (`settingsSchema.ts`)
  for the dev mock; `settingsContract.test.ts` reads the Rust schema and fails
  when they drift. New keys: `pyxis.wallpaper.motion` (flow|still),
  `pyxis.wallpaper.pinnedTime` (null|0..1439), `pyxis.location`
  (null|`{lat, lon, label, source}`), `pyxis.clock24h`; `pyxis.theme` gained
  `auto` (dark below -6° sun elevation); `pyxis.wallpaper` defaults to
  `daylight`.
- **Time and place** (`org.geckoOS.time`, `echo/src/services/time.rs`, jiff
  over the image's own zone files): `getClock` → `{epoch_ms, timezone,
  utc_offset_minutes, abbreviation, dst, next_transition_ms}`, `listTimezones`
  (`zone1970.tab`, ISO 6709 coordinates), `setTimezone` (`settings:write`;
  validated against the table or `UTC`, relinks `/etc/localtime` atomically,
  writes `/etc/timezone`, emits `TimezoneChanged`), `getLocation` (the
  `pyxis.location` setting, else the zone's city, `source: "none"` for UTC).
  **Pages take the offset from Echo, never from `Intl`**: vela caches the zone
  it started with. SDK `useClock()` (one shared clock per document, ticks on
  Echo's minute, re-syncs on `TimezoneChanged`, every 10 min and after a DST
  transition) and `useLocation()`; the top bar's clock uses it and
  `pyxis.clock24h`. `PYXIS_TIME_ROOT` points the service at a test root.
- **Daylight** (`packages/app-sdk/src/wallpaper/`, `sun.ts`): five gaussian
  colour masses on slow two-sine paths, a domain-warped field, mixed in OKLab,
  with a static full-resolution grain against banding. The palette is
  keyframed on the sun's elevation (night, blue hour, dawn/dusk, golden
  morning/evening, day; morning and evening blended by the hour angle), from
  NOAA's formulas at `useLocation`'s place, or a fixed 06:30/18:30 day when
  there is none (UTC, the image default). Aurora, Sunset, Forest and Mochi are
  fixed palettes on the same engine. The scene is pure (`scene.ts`), drawn by
  a WebGL fragment shader on a canvas at a quarter of the output's resolution,
  CSS-upscaled, at **10 fps** (motion is a few px a second, so 10 fps reads as
  60), or by CSS transforms when there is no WebGL. vela on the VM's llvmpipe
  gets WebGL (Firefox reports it as "Radeon R9 200 Series"), so the device
  draws webgl. `DynamicWallpaper` has a `preview` mode (Settings' thumbnails,
  css; its large preview, webgl).
- **When it moves**: `full` animations flow; `reduced` stands still with the
  palette crossfading each minute; `off` stands still; `pyxis.wallpaper.motion
  = still` and `pinnedTime` likewise. It stops drawing frames while a
  maximized or fullscreen app is up (the flag, at once), while at most 15% of
  the desktop shows (`useWallpaperExposure`: output minus the union of app
  frames; the bar and dock strips are exactly 15% of 800 px), and while the
  document is hidden; paused, the palette still updates each minute. **Evo
  sends no signal on a resize**, so the desktop re-lists frames itself 300 ms
  and 1.5 s after any window signal and every 10 s while a window is up.
- **On-device log** (vela.log, through `dump()`: a page's `console.log` does
  not reach it): `PYXIS-WALLPAPER state=flowing|paused|still backend=webgl|css
  mode=… exposed=… …` once per change of state, and
  `PYXIS-WALLPAPER-SETTING wallpaper=<id> motion=<m> pinned=<min|none>` once
  per change of what the desktop shows (how a suite knows a click in Settings
  reached the shell), and `PYXIS-WALLPAPER-EXPOSURE exposed=… windows=…`
  when the exposure moves by 0.05 or more. Read exposure from that line, not
  the state line: a state change is written when a flag arrives by signal,
  before the frames are re-read, so its `exposed=` is stale.
- **Bloom's window store could lose a window for good** (found by
  `alpha5-wallpaper`, fixed 2026-11-09; `CompositorWindowsProvider`, used by
  the overlay's dock and overview and by the desktop). A list replaced the
  store, and Evo's `ListWindows` is a sample up to 250 ms old: a list that a
  signal about an unknown window asked for could land after `WindowCreated`
  and erase the window, and with no later signal naming it (Files opening
  maximized sends its signals in one burst) it stayed missing. A list now
  keeps windows a signal created, and drops ones a signal closed, since
  shortly before it was asked for (`keep`/`drop` on the `list` action), a
  request made while one is out runs after it, and a list that may have
  missed a signal is followed by another once the sample has caught up
  (`compositorWindowsProvider.test.tsx` replays the race).
  **A second way, found by the identity sweep (fixed 2026-11-11):** Evo sends
  `WindowCreated` at map time, when most windows have no app id yet
  (`"unknown"`), and nothing when the id arrives, and the desktop's first
  `ListWindows` can be a sample from before vela has named a window. About one
  boot in two the store kept a full-screen window as `vela-default` (seen with
  the exposure line's `store=bloom,vela-default`) that no later signal
  touched, so the wallpaper paused for the whole run. Now the first list, and
  any window created with an unresolved id (`unknown`, `vela-default`, empty),
  start up to `ID_RETRIES` relists a second apart until the list gives it an
  id or no longer has it, and the exposure filter trusts the list's app id
  over the store's. `PYXIS-WALLPAPER-EXPOSURE` now ends with `store=<ids>`
  (every window the store holds); 4 of 4 runs passed after, 1 of 3 before.
- **Cost** (4 GB VM, software GL, Evo plus every geckouser process): idle
  desktop +5.0 points of one core flowing against still (budget 8); a floating
  app on screen +7.1-7.6; a maximized app 0 (paused); a Files open 45-47 fps
  over a flowing wallpaper against ~46 over a still one. If a later
  measurement goes over, lower `WALLPAPER_FPS` to 6 before simplifying the look.
- **Settings app** (`packages/apps/settings/src/sections/`, split from one
  845-line file): Appearance (Light/Dark/Auto with the next switch; a live
  thumbnail per wallpaper; a large Daylight preview with a time scrubber that
  previews without moving the desktop; Follow the day / Pin this time;
  Flowing/Still), Date & Time (new: clock, 24-hour, searchable zones, location
  from the zone or by hand, today's sunrise and sunset), About from
  `system.getInfo`. `strictTime.ts` calls the bridge directly on a device so a
  refused `setTimezone` shows as an error instead of the SDK's silent mock
  success. Suite selectors (`data-wallpaper`, `data-theme-choice`,
  `data-settings-section`, `data-timezone-search`, `data-timezone`, …) are
  listed in the lane P hand-over and used by `alpha5-settings-live`.
- **AppShell no longer remounts apps on a theme change.** It keyed remochi's
  `ThemeWrapper` by the theme, which rebuilt every app (losing what was on
  screen) at every change, twice a day with Auto. `ThemeSync` now steers the
  wrapper's own theme from inside (`appShellTheme.test.tsx`).
- Not done: More Daylight families, a GeoIP/Wi-Fi location. (The shell's
  tint from the wallpaper is done: see [Visual identity](#visual-identity-the-shell).)
  remochi's `Button` draws its "( )" caps
  as text, so a bare `Button` reads as "(Set by hand…)" on device; not worth
  working around, since the apps are to be rebased onto Duyo (the user's new
  UI framework, a separate integration path).

## Visual identity (the shell)

**Built 2026-11-11** (the shell read as macOS; plan, lanes and the decisions:
`docs/internal/pyxis-visual-identity-plan.md`; mockups:
`docs/internal/mockups/pyxis-identity.html`). Built from what Pyxis already
had: the waterdrop corner (16 4 16 16, top-right sharp, from the app icons),
the app's accent as light, Daylight's palette, Mochi type, and the remochi
spinner's shapes and colours as the window controls (kept by the user's call;
Evo's half is in [Window decoration model](#window-decoration-model),
"Identity"). All React, each new component self-contained, so the Duyo
port's Phase 3 can wrap it rather than rewrite it.

- **Tokens** (lane T): SDK `packages/app-sdk/src/identity.ts` (`CORNER`,
  `waterdrop(size)`, `SPINNER`, `IDENTITY_FONTS`, `shellTint`, new exports
  only) and Bloom's `src/styles/identity.css` (`--px-corner*`,
  `--px-spinner-*`, `--px-font-*`, `.px-display/.px-ui/.px-accent`).
  `identityContract.test.ts` reads `evo/src/shell/identity.rs` and fails when
  the corner or spinner values drift.
- **Tint** (`useShellTint`, mounted in both Bloom documents; pure maths in
  `shellTint(palette, theme)`): from the wallpaper's own palette (`skyFor`,
  honouring `pinnedTime`), mixed in OKLab. Three families of variables, and
  which to use matters:
  - `--px-ink*`: text drawn **on the wallpaper** (the strip). Follows the
    wallpaper, not the theme (light at night, dark by day), >= 4.5:1 against
    the palette's mean.
  - `--px-material*` / `--px-material-ink*`: **panels** (quick settings, the
    centre, the drawer, cards, search). Follows the theme.
  - `--px-edge-material*` / `--px-edge-ink*`: **the launch bar** and what sits
    on it (Apps/Cards tiles, dashboard chips). Follows the wallpaper, like the
    ink. Added after the first device run: with the image's light theme the
    bar was a pale band across a night wallpaper.
- **Status strip** (lane N, `TopSystemBar.tsx`, `STATUS_STRIP_HEIGHT` 36): no
  background, `--px-ink` with a legibility shadow. Search as an underline
  ("Just type", `DesktopSearch.tsx`), the clock centred in Mochi Light, the
  status cluster (Wi-Fi, volume, battery) opening quick settings. The Bell and
  the Settings gear are gone. Work-area top inset: 36.
- **Launch bar** (lane B, `LaunchBar.tsx`, replacing the deleted
  `BottomDock.tsx`): 64 px, full width, flush with the bottom edge, its
  material fading up from the edge, no blur. Apps and Cards at the left end
  (Apps centre `DX+36`), app icons in the middle as 40 px accent waterdrops,
  52 px apart, centred on the screen; running = a 3 px accent line on the
  screen edge (focused longer, starting pulses, minimized faint, crashed red).
  Every dock contract kept (`data-shell-edge="bottom"` + `data-app` for
  minimize targets, the Lumen ripple). Work-area bottom inset: 64.
- **Dashboard** (`NotificationDashboard.tsx`, at the bar's right end): the
  newest one or two unread notifications as chips, then a count in spinner
  red, which opens the centre (`DX+DW-27`, there only while a notification is
  stored). Toasts rise from above it (`NotificationToasts.tsx`, stack right
  edge 1268, bottom 728); the centre opens upward from it.
- **Cards, drawer, quick settings** (lane C): waterdrop panels in the strong
  material, display-type heads (`CARDS`, `APPLICATIONS`, `QUICK SETTINGS`),
  card title strips tinted with the app's accent and the chosen card ringed
  in it; the overview's flights and thumbnails untouched (the picture's top
  corners are the frame's, scaled). Quick settings' controls moved: volume
  slider line at `PANELT+295`, track `PANELR-318`..`PANELR-62`; Do Not Disturb
  at `PANELR-56, PANELT+88` (`PANELT` = strip + 8, `PANELR` = right - 14).
- **Suites re-aimed** for the layout (`alpha5-bridge-deny`, `-bridge-signals`,
  `-overlay`, `-shell-layering`), and the old vela's refusal of
  `SetAppIdentity` is excluded where they check vela.log, as the earlier
  bridge entries are. New suite `alpha5-identity.txt` (strip and bar rects,
  the 36/64 work area, toasts bottom right, title-bar names, centre, quick
  settings, drawer, cards, shots at four pinned times); on a vela without
  `SetAppIdentity` it stands in with echo-cli (`IDENTITY-STAND-IN`).
  Swept 2026-11-11 on the 11-09 image: `alpha5-identity`, `-identity-evo`,
  `-bridge-deny`, `-bridge-signals`, `-overlay`, `-shell-layering`,
  `-shortcuts-shell`, `-notifications`, `-thumbnails`, `-windows`, `-audio`
  and `-wallpaper` all pass (303 assertions), with evo, echo-cli,
  pyxis-wlprobe, the shell's `dist` and the two test icons pushed (the
  command line is in `alpha5-identity.txt`'s header) until the next image.
- **Left rail** (2026-11-12): `pyxis.launchBar.position` = `bottom` (default)
  | `left`, in Echo's schema and the SDK's copy, chosen in Settings →
  Appearance ("Launch bar", `data-launch-bar-choice`). The rail is 64 px wide
  below the strip (`0,36 64x764`), Apps and Cards at the top, icons 52 px
  apart, running lines on the left edge, the dashboard count alone at the
  bottom (no chips); toasts and the centre anchor `left: 76, bottom: 12`
  (`RAIL_ANCHOR` in `launchBarState.ts`). Work area `{top 36, bottom 0, left
  64}`; a switch re-sends it and Evo re-fits windows off the rail (Evo needed
  no change: `SetWorkArea`'s `left` was already honoured). The edge marker is
  `data-shell-edge="left"`, and `useMinimizeTargets` measures `[data-shell-edge]
  [data-app]`, so Droplet lands on the rail's icons.
- **Dialogs** (2026-11-12, restyled 2026-11-16): `ShellDialog.tsx` is the
  shell's own scrim (`data-shell-region`, the overlay keyboard, Escape cancels
  and Enter confirms, both taken in the capture phase so the panel beneath
  stays open) around **remochi's dialog card** (`mochi-dialog` header, title,
  `Divider`, content, actions; error tone = `mochi-dialog--error`) with
  **remochi caps Buttons** (`ShellPill` wraps `Button`: primary = blue,
  destructive = warning; not the `drop` appearance, by the user's call). The
  privilege prompt is built the same way, with remochi's `Input`. remochi's
  own `Dialog` component can't be used in the overlay: its overlay element
  takes no region marker and its keyboard handling isn't the overlay's, and
  its card is `opacity: 0` outside that overlay, so the shell sets the resting
  pose itself. Used for the Launch Error (`launch-error`) and the Restart /
  Shut Down confirm (`restart`, `shutdown`; cancel is
  `[data-dialog-action="cancel"]`). Buttons carry `aria-label` = their text
  (the caps' brackets are text, so the name was "(OK)").
- `LeftLauncher.tsx` (and `src/styles/pinned-item-overrides.css`, only it
  imports) is imported nowhere, dev server included: dead code, left for the
  user to delete.
- **The apps and Duyo**: the identity stops at Bloom's two documents; what
  remochi, Duyo, the SDK and Phase 3 need for it to reach the apps (corner,
  material and accent tokens; remochi's theme-variable shadowing trap; a
  per-app accent in the manifests) is in `docs/internal/pyxis-identity-upstream.md`.
- **Design calls made 2026-11-12**: remochi's scallop aligns with the waterdrop
  where appropriate, apps keep the Button caps, and what the title bar *says*
  is held for the Gecko port's branding. The frosted title bar and the lock
  screen followed: [Frosted title bar and lock screen](#frosted-title-bar-and-lock-screen).
- **The next image** carries all of it (binaries, Bloom and the eight apps, the
  test icons in `/usr/share/pyxis-test/`; the vela bridge needs no compile,
  its JS is copied): `docs/internal/next-image-checklist.md`.

## Frosted title bar and lock screen

**Built 2026-11-12** (plan, mockups and the binding "Contracts" section:
`docs/internal/pyxis-glass-and-lock-plan.md`,
`docs/internal/mockups/pyxis-glass-and-lock.html`, which has a live lock
prototype). Verified on device by `alpha5-frost.txt` (43 checks) and
`alpha5-lock.txt` (70 checks) on the 11-09 image with evo, echo-daemon,
echo-cli and Bloom's dist pushed.

- **Frost** (`evo/src/shell/frost.rs`, `FrostElement` in `anim/shader.rs`):
  the title bar is real glass, since Evo can see behind it. Tier **F1**
  captures the desktop window (Bloom's wallpaper) when its surface *tree*
  commits, at most 4 times a second, shrinks it to 1/8, blurs and veils it per
  theme (light: 52% toward `#fcfbf8`, OKLab L >= 0.86, chroma x1.25 cap 0.06;
  dark: 50% toward `#101228`, L <= 0.30, cap 0.07; inactive: chroma x0.35, veil
  +0.20, a second texture), and each bar draws the crop under itself with grain,
  a 1 px white rim on top and the app's accent as a 1 px seam at the bottom
  (the accent left the fill). The lightness band keeps the ink fixed per theme
  (`#2a2620` / `#ece9e3`, >= 7:1 over any wallpaper, a unit test sweeps it).
  **F0** is one flat colour (Bloom's `material` from `SetAppIdentity`, veiled)
  for `SetFrost off`, no GL, a failed shader, or a bar not yet drawn once. **F2**
  (frost of other windows under the bar) is not built: over another window a
  bar shows the wallpaper; `frost.rs`'s header names the seam.
  - **Watch the whole surface tree.** vela paints into a subsurface, so the
    desktop's own toplevel hardly commits: watching it alone left the frost
    showing the first palette it saw. `tree_commit` sums every surface's
    counter, reading each from the states `with_surface_tree_downward` hands
    over. **Never call `with_renderer_surface_state` inside that walk**: it
    re-locks the surface the walk holds and hangs the compositor thread (it
    did on device: the IPC thread still answered, so only the screen died).
  - Cost (4 GB VM, software GL): an update 3-7 ms (readback included, budget
    10 ms, `over_budget` counts the rest), ~20 updates in 5 s over a flowing
    wallpaper, 0 with nothing moving; fps 23-26 with frost on against 19 with
    it off in the same run (no cost seen).
  - Switches (echo-cli only): `EVO_FROST=off|wallpaper`, `evo.SetFrost
    {mode}`, `evo.GetFrostState` → `{mode, program, tiers{id: f0|f1}, updates
    {f1}, last_ms, over_budget, skipped}`.
- **Lock: Evo owns what is secret, Echo the decision, Bloom the look.**
  - **Echo** (`echo/src/services/session.rs`, `accounts.rs`): `org.geckoOS.session`
    `lock`/`unlock`/`getState` and `org.geckoOS.accounts` `list`/`setPassword`/
    `setProfile`. Passwords are checked against `/etc/shadow` with libxcrypt's
    `crypt_r` (no PAM: the build host has no libpam headers), set with
    `chpasswd`. Rate limit in the daemon (5 tries, then 30 s doubling to 15
    min). `Message`'s `Debug` prints `<redacted>` for `session.unlock` and
    `accounts.setPassword` (the routing and daemon debug lines print whole
    messages). geckouser has no password on the image (`*`): the lock is then
    press-to-enter. Settings keys `pyxis.lock.after` (null|1..120 min, 10),
    `pyxis.lock.onSleep` (true), `pyxis.lock.notifications` (hidden|count|full).
    `power.sleep` locks first.
  - **Echo tells Evo without bus identity**: echo-daemon writes 32 random bytes
    (hex) to `/run/pyxis/session.key` (root 0600) at start; every
    `LockChanged {locked, seq, mac}` carries an HMAC-SHA256 of
    `pyxis-lock:<0|1>:<seq>`. Evo locks on any lock signal and unlocks only on
    a verified mac with a fresh seq (re-reading the key once on a failure);
    `echo-cli emit` forgeries are refused and counted. Any client can still
    *guess* through `session.unlock`; the rate limit is what holds.
  - **Evo** (`evo/src/shell/lock.rs`, `anim/{veil,clear}.rs`): while locked
    `output_elements` draws only the overlay (and the Veil's ghosts) over black,
    input goes to the overlay or nowhere, the keyboard is forced to it, every
    shortcut and VT switch is off, window actions except `SetWorkArea` are
    refused, thumbnails answer `[]`. The overlay dying leaves it locked and
    black. Lock sources: Super+L, `evo.Lock`, idle (`SetIdleLock {seconds}`,
    from Bloom's `pyxis.lock.after`), `LockChanged {locked:true}`. **Veil**
    (600 ms): snapshot ghosts blurring, greying and fading through Lumen.
    **Clear** (900 ms): live surfaces through Bloom's open shader, blur 14 → 0
    and 1.04 → 1 about `SetLockOrigin` (Bloom's field), while Bloom's opaque
    lock layer opens a hole `r = D(0.5t + 0.5t²)`. After unlock Evo hands the
    keyboard to the front app (not gated on the overlay's keyboard note: a
    lock's focus is forced, and the gate left no window focused on device).
    `GetLockState` (echo-cli) for suites.
  - **Bloom** (`src/lock/`, `ShellLayer`'s lock mode, `useIdleLock`, SDK
    `session.ts`/`accounts.ts`/`useSession.ts` on `EchoClient.callMethodStrict`
    so a refused call never falls back to the mock's success): lens clock
    (`-moz-element` of its own Daylight on Gecko, a lightened gradient
    elsewhere), states rest/awake/verify/wrong/wait/unlocking/ambient/no-password
    (`data-lock-state`), Lock in quick settings. Settings → **Users & Lock**
    (`data-settings-section="users"`): profile, password, lock settings, Lock now.
  - **Needs the rebuilt vela** for Bloom to call `session.*`, `accounts.*` and
    the evo lock methods (all new bridge entries, `regen --check` passes). On
    the 11-09 vela they are refused, so the overlay draws the ordinary shell
    over black while locked, and the suite drives `session.*` with echo-cli.
  - **Found by the suite, not the lock's:** after `pkill -9` of the shell's
    vela, no shell came back within a minute on the 11-09 image, though
    geckoOS-boot runs it under `supervise_daemon`. Unexplained; the lock suite
    prints it instead of asserting it.

## Global shortcuts (Evo)

Bloom's own hotkeys (Ctrl+Space, Meta+K in `DesktopSearch.tsx`) only fire while
Bloom has the keyboard, never while an app does, so shortcuts that must work
anywhere live in Evo (`input_handler.rs`, `process_keyboard_shortcut`):

- **Alt+Tab / Shift+Alt+Tab** is Evo's key and Evo's MRU order, drawn by
  Bloom's overview (`evo/src/shell/switcher.rs`, built 2026-11-07).
  `Switcher<T>` is a pure state machine (injected time, generic windows); the
  `EvoState` methods wire it to the keyboard. A walk freezes `focus_history`'s
  order at the first Tab and **no step raises or restores anything**. Evo
  emits `org.geckoOS.evo` `SwitcherChanged {phase, selected, order}` (window
  ids, MRU first, so `order[0]` is where the walk began; `windows:manage` in
  `BRIDGE_SIGNALS`):
  - `open`: Alt still held 150 ms after the first Tab (`QUICK_TAP`; a
    per-loop `switcher_tick` from `process_pending_window_actions` sends it).
    `step`: another Tab / Shift+Tab, only once shown. `commit`: Alt released:
    Evo has already `focus_window`ed `selected` (restoring a minimized one,
    which animates as lane B's restore). `cancel`: Escape while Alt is held
    (intercepted via `KeyAction::CancelSwitcher`; focus stays).
  - **A quick tap** (Alt up inside 150 ms) switches at once and sends only
    `commit`, never `open`: a shell that was not showing anything ignores it.
  - A `FocusWindow`/`RestoreWindow` request arriving mid-walk (a card clicked
    in the switcher) is the commit (`switcher_chose`), so Alt's release can't
    override the click.
  - **No overlay alive** (no `bloom-overlay` window when the walk starts: dev
    server, dead Bloom): the old direct cycling, each step focuses a window,
    nothing is signalled.
  - `evo.GetSwitcherState` (root, not in the bridge) gives suites
    `{active, shown, selected, order, lastPhase, lastCommit, counts{open,
    step, commit, cancel}}`.
  - **Bloom** (`src/hooks/useSwitcher.ts`, `ShellLayer`, `CardOverview`'s
    `switcher` prop): `open`/`step` show `CardOverview` with cards in Evo's
    order and `selected` highlighted; `commit` zooms the chosen card onto its
    window's real frame while the rest fade (see **Overview transitions and
    thumbnails** below), then takes the overview down after
    `commitMs(mode)` (`overview/flight.ts`: zoom plus fade; a fade in
    `reduced`, nothing in `off`); `cancel` flies the cards back to their
    windows. It covers the screen for clicks but **never asks for the
    keyboard** (Alt is held in the app, Evo has the keys). A card click calls
    the existing activate (Evo's `commit` closes it; a 1.2 s guard does if Evo
    never answers). Super+Tab's overview is the same component.
  - **Needs a vela rebuild** for Bloom to hear it (`SwitcherChanged` is new in
    `BRIDGE_SIGNALS`). On the old vela Evo still switches on release but
    nothing is drawn, and a long Alt+Tab walk gives no feedback.
- **Overview transitions and thumbnails** (built 2026-11-07, `overview/`,
  `shell/thumbnails.rs`). Cards are the windows themselves, flown between the
  grid and each window's frame (`ListWindows` rect = the overlay's CSS px):
  - **Open**: each card starts laid exactly over its window (a minimized one
    over its dock icon) and springs (`TRANSITION.geometry`) into the grid, its
    chrome (glass plate, title bar, icon badge) fading in. **Chosen** (click,
    Enter, Alt released): it grows back onto the window's frame
    (`TRANSITION.open`), the rest fade, then it fades over the last 100 ms to
    reveal the window Evo has already raised; a minimized window lands where a
    window of its size would sit centred (Evo does not report the restore
    place). **Cancel** (Esc, backdrop, switcher cancel): every card returns to
    its window and fades. Only `full` mode flies; `reduced`/`off` fade.
  - How: `useCardFlight` in `CardOverview.tsx` runs on a plain wrapper
    `div` around each card (so it never fights the card's selection lift),
    with framer's `animate()` and `usePresence` for the exit. The maths
    (`flightTransform`, `restRects`, `previewBox`, `commitMs`) is pure in
    `overview/flight.ts` with tests. Rest boxes are measured with the wrapper's
    current transform undone (`restRects`), so a card committed mid-rise (a
    quick Alt+Tab) flies from where it is laid out. **The inner card's
    `layout` prop is off until the card has landed**: framer takes the
    wrapper's flight for a layout change otherwise and the start pose is
    off by tens of px. The scrim, header and cards fade on their own (the
    scrim is a separate element), so cards stay opaque while the overview dims.
  - **`evo.GetWindowThumbnails {window_ids?, max_width, max_height}` →
    `{thumbnails:[{window_id, width, height, data}]}`** (bridge
    `windows:manage`; SDK `getWindowThumbnails` in `packages/app-sdk/src/thumbnails.ts`,
    mock = placeholder SVGs). Evo draws each window offscreen with
    `Backend::snapshot_window` at `thumbnail width / frame width` (never full
    size), crops to the frame (SSD header in, CSD shadow margin out) so the
    aspect is the frame's, and the Echo broker encodes JPEG q80 on a blocking
    worker (`image` crate; its WebP encoder is lossless-only). `data` is a
    `data:image/jpeg;base64,` URL; rounded corners come out black (Bloom
    clips). Bounds default 480x300 and clamp to 480x480. The request is queued
    from the broker and served by the compositor thread's next loop pass
    (`serve_thumbnail_requests`, <= 16 ms).
  - Cache (`Cache` in `thumbnails.rs`): per window, keyed on the surface's
    commit counter and frame size and the requested bounds; a busy window
    (commits every frame) is served as is for 150 ms (`GRACE`) rather than
    redrawn per call; entries for dead windows are dropped. **A minimized
    window gets its last entry however old**, and `minimize_window_after`
    takes one as it minimizes (`thumbnail_at_minimize`). Windows with no
    buffer yet, the shell and the overlay are left out.
  - Bloom side: `overview/thumbnailStore.ts` holds the pictures
    (`useSyncExternalStore`; refreshes coalesce into one follow-up);
    `useThumbnailPrefetch` (ShellLayer) refreshes 350 ms after windows or
    focus change so Alt+Tab, which opens 150 ms after the first Tab, has
    pictures on its first frame; the overview also refreshes on open and
    shows the icon card until a picture loads, then crossfades it in. It
    never blocks the open. Cards are `previewBox` sized: 170 px high, the
    window's aspect, 150-400 px wide.
  - **Needs a vela rebuild**: `GetWindowThumbnails` is a new bridge entry.
    On the old vela the call is refused and every card keeps its icon.
  - On-device: `scripts/serial-tests/alpha5-thumbnails.txt` (count, aspect,
    bounds, JPEG URLs, id filter, minimized, closed, latency).
- **Super+Q** closes the focused app window (xdg close, as the SSD X does;
  never the shell or overlay). Nothing in Evo quits the compositor: Super+Q
  and Ctrl+Alt+Backspace used to, which ended the session, and Super+Enter
  ran `weston-terminal` as root. All gone. VT switching stays; the debug
  toggles (scale/rotate/tint/preview/decorations) exist only with the `debug`
  feature.
- **Shell actions** go out as the `org.geckoOS.evo` signal
  `ShortcutTriggered {action, timestamp}`: `launcher` (Super tapped alone),
  `overview` (Super+Tab), `search` (Super+Space), `show-desktop` (Super+D).
  Gated by `windows:manage` in `BRIDGE_SIGNALS`. Bloom reacts in
  `ShellLayer.tsx` via `useGlobalShortcuts`: the panels are drawn by the
  overlay, above apps, so they open over whatever is running (only
  `show-desktop` minimizes windows; see [Shell overlay](#shell-overlay-bloom-above-apps)).
- A client's `keyboard-shortcuts-inhibit` is honoured, judged by the
  keyboard-focused surface: while inhibited, every key (VT switch included)
  goes to the client.
- `evo.SwitchWorkspace`/`CreateWorkspace` are not implemented and say so.
- On-device suites: `scripts/serial-tests/alpha5-shortcuts.txt` (Evo half,
  Alt+Tab switcher included, fully asserted from `GetSwitcherState`) and
  `alpha5-shortcuts-shell.txt` (Bloom half). The signal half needs a vela
  rebuilt with the patched `PyxisBridgeParent` (`BRIDGE-OLD-VELA` otherwise).
  `serial-test.py` has `#!keydown`/`#!keyup` to hold Alt across steps.

## Window animations (Evo)

**Foundation and open/close built 2026-11-07** (`evo/src/anim/`; design,
motion spec and lane split in `docs/internal/evo-animation-plan.md`; the
module's API for later lanes is the header of `anim/mod.rs`).

- **Animation is a render-time transform, never input.** Hit-testing, focus,
  `ListWindows`, BindSurface and the work area use a window's final geometry
  from the first frame. A ghost (a window that left the Space but is still
  drawn) takes no input. Interactive move/resize are never smoothed.
- **`render::stack_elements` walks the Space itself** (it replaced
  `space_render_elements`, which gives no per-window transform): layer
  surfaces above, windows front to back, layer surfaces below. A window with a
  running `WindowAnim` is drawn through `anim::transform::wrap` (Rescale then
  Relocate, SSD header included; alpha is passed to `render_elements`).
  **Alpha-only changes do produce damage**: smithay's damage tracker compares
  each element's alpha and geometry per frame and the wrappers report the
  wrapped values (tests in `anim/transform.rs` pin this against a
  `DummyRenderer`, so a smithay bump that breaks it fails at the desk). The
  exclusive-fullscreen branch of `output_elements` skips this path, so a window
  entering or leaving fullscreen is drawn through it until its geometry
  transition ends (see **Geometry transitions**).
- **Ghosts** (`anim/ghosts.rs`, thread-local) are drawn directly above their
  `anchor`, the window that was next below when they left; with no anchor
  left they sit above Bloom, beneath every app. Sources: a `Snapshot` (close)
  or a live `WindowElement` (minimize, or its warp, below).
- **Snapshots are CPU pixels, uploaded on first draw.** udev's renderer is a
  `MultiRenderer`, whose `MultiTexture` can't wrap the `GlesTexture` an
  offscreen draw makes, so `anim/snapshot.rs` renders offscreen
  (`render_to_mem`, factored out of screencopy, which now calls it), reads
  back, and `import_memory`s on the first draw. One readback per close (4 MB at
  1280x800). They need the renderer, so they are `Backend::snapshot_window`,
  which only udev implements; winit/x11 close without an animation.
- **Who animates** (`EvoState::animation_eligible`, `shell/animation.rs`):
  an app with a bindable claimed app id. The vela about:blank pre-window
  (`vela-default`) never animates (it would flash a card open and shut on
  every launch); neither do the shell and the overlay. **Open** fires once per
  showing at the first commit where the window is mapped with a bindable id
  (drawn as Lumen's Bloom, below).
  **Close** snapshots in `remove_window_from_workspace` (before anything is
  unmapped) or in a pre-commit hook for a null-buffer unmap (before the buffer
  goes). **Popups** fade in 120 ms on their own clock (state in the popup
  surface's data map); popup-out is not built.
- **Counters are RAII** (`anim/window.rs`): a `WindowAnim` is *started* when
  made and *completed* when dropped, however it ended, so with nothing in
  flight `started == completed` and `active == 0`. Hold one forever and the
  suite fails.
- **Modes**: `EVO_ANIMATIONS=full|reduced|off`, `EVO_ANIMATION_SCALE` (0 =
  off, 10 = slow motion, max 100; the suite uses 60) at start; `org.geckoOS.evo SetAnimationSettings
  {mode?, scale?}` (bridge `settings:write`) and `GetAnimationState` (bridge
  `null`; mode, scale, `active`, `started{}`/`completed{}` per kind) after.
  Reduced is opacity only, capped at 150 ms; the cap applies before the scale.
  New bridge entries need a vela rebuild for *apps* to call them; echo-cli
  needs none.
- **Minimize, restore and show desktop** (lane B, built 2026-11-07;
  `shell/minimize.rs`, poses in `anim/minimize.rs`). Placement stays instant:
  `minimize_window` takes the window out of the Space at once and focus moves
  on. What is *drawn* is a `ghosts::Live` ghost of the same window at its old
  z-slot, springing (`spec` minimize spring, ~320 ms) from its frame into its
  dock icon, alpha going to 0 over the last 30%. **x leads y**: the offset
  spring (horizontal travel) is a bit faster than the scale spring, which also
  carries the vertical travel by scaling about a virtual fixed point chosen so
  the frame's centre lands on the icon's (`anim::minimize::plan`). Restore runs
  on the window itself, from the moment it is back in the Space (it's
  `Restore` in its own animation slot, so it is drawn at its real z-position).
  **Interrupting** is `anim::minimize::retarget`: it samples the running
  animation (open, restore, or the minimize ghost, which `unminimize_window`
  takes with `ghosts::take_live`), re-expresses the pose about the new origin so
  no point jumps, and starts new springs with the old velocity. **A close
  mid-open** now starts from the current pose too (`minimize::close_from`;
  `begin_close_animation` reads the slot before cancelling it).
  A window not yet shown (`has_opened`), not eligible, in exclusive fullscreen
  (restore only) or with animation off just appears/vanishes.
- **Lumen** (the second motion language, the user's call 2026-11-08: the
  genie was "too macOS"; spec and lane split in
  `docs/internal/evo-animation-plan.md`, "Lumen"). Windows behave like light
  through a lens: focus (blur), shape (round off to an orb of the app's
  accent), light (glow, ripples). The genie funnel is retired, drawn in no
  mode (`anim/genie.rs` is a three-name shim over `anim/droplet.rs`).
- **The Lumen program** (`anim/lumen.rs`, lane L1, 2026-11-08): the shared
  snapshot-draw program the effects draw through. One quad of our own
  (`GlesFrame::with_context` behind `GlesAccess`) drawing a `lumen::Source`
  (a `Snapshot` uploaded once, mipmapped) into a rect with a rounded clip of any
  radius (clamped to half the short side: up to a circle), a **blur by mip LOD
  bias** plus four taps (no extra passes), an accent **tint** that keeps some
  of the picture's light and dark (with a lens highlight when round), an outer
  **glow**, alpha, and **squash/stretch** along an angle (applied to the quad's
  corners, so the texture follows; bounded to 1.5). API for other lanes:
  `Source::new(snapshot)`, then per frame `Params::picture(&source, top_left)`
  edited and `source.element(renderer, &params, Effect::X)` →
  `Option<LumenElement>` (`None` = draw your plain fallback); `GhostElement::Warp`
  holds a `LumenElement` (`warp::WarpElement` is an alias). Each element drawn
  counts a frame for its `Effect`; `record_progress` is what suites poll. Shared
  GL helpers: `compile_shader`, `link_program`, `scissor_box`.
  - **The old warp's gotchas still apply and are handled there**: smithay's
    instanced draws leave `VertexAttribDivisor 1` on attribute slots (a program
    of ours must set 0, or every vertex reads the first: the whole window one
    texel of its corner), damage is a per-rect scissor converted through
    `GlesFrame::projection()`, every piece of state touched is put back. A live
    surface can't be sampled by our program (on udev it is a `MultiTexture`),
    hence snapshots.
  - **Accent**: the app's own, from an optional `accent: "#rrggbb"` on each
    `SetMinimizeTargets` entry (Bloom's `PYXIS_APPS` accent; a bad one is
    ignored, never costing the target), else the snapshot's alpha-weighted
    average pushed to saturation >= 0.55 and lightness 0.48-0.64; a colourless
    (white, grey) window borrows Pyxis's default hue so it never makes a grey
    orb (`lumen::accent_from_average`).
  - **Switch and state** (echo-cli only, no bridge): `EVO_LUMEN=off` at start,
    `evo.SetLumen {enabled, mipmaps?}` after; off, every effect draws its plain
    fallback (genie-lite for minimize), logged. `evo.GetLumenState` →
    `{enabled, program: untried|compiled|failed, frames{bloom, droplet,
    evaporate, recede, glint}, last_progress{…}, landed{land, rise}}` (other
    lanes add their own fields). A failed build, no GLES, a failed upload or a
    backend with no `snapshot_window` (winit/x11) fall back, logged once.
- **Droplet (minimize) and Rise (restore)** (lane L1, 2026-11-08;
  `anim/droplet.rs` the pose maths, `anim/warp.rs` the payload and draw). Over
  the first 60% the window's size goes geometrically to its icon's short side
  and its corner radius to half of it (a circle); blur grows to 6 px, accent
  tint and glow to 1 by ~50%; the centre travels a quadratic arc (control 40%
  across, 70 px above the higher end, smoothstep-eased); once orb-like it
  squashes and stretches along its velocity (up to 1.3 : 1/1.3, by how orb-like
  it is); the last 18% shrinks it by 55% and the last 14% fades it. The window's
  shadow (drawn at the frame) fades over the first 15%. Rise is the same with
  `p` running back.
  - **Driven by lane B's spring, not a new animation**: `p` is read off the
    minimize `TransformAnim`'s `scale.y` (`droplet::progress_for`), so
    counters, stagger and **retargeting** are unchanged: a restore that
    interrupts a minimize turns the same spring round, and the pose (a function
    of `p` and of the spring's rate, both continuous) cannot jump. The rate is
    measured across frames on the payload (samples under 4 ms apart, the same
    frame drawn for another output, don't count). The spring's envelope is
    `minimize::DROPLET` 720 ms / `RISE` 680 ms when Lumen will draw
    (genie-lite keeps 320/300): the spring is front-loaded, so this *reads*
    ~480 ms (p >= 0.99 at 481 ms, a unit test). Slow-motion shots therefore use
    scale 13, since a spring's run is capped at 10 s.
  - Tests: `p=0` is exactly the frame (size, centre, 16 px corners, sharp,
    untinted, opaque, unstretched), `p=1` is centred on the icon, no bigger,
    invisible; radius <= half the short side and stretch in bounds for any
    frame, icon, `p` and speed; every quantity continuous in `p`; the arc lifts.
  - **`MinimizeLanded {app_id, window_id, phase: "land"|"rise", timestamp}`**
    on `org.geckoOS.evo` (bridge `BRIDGE_SIGNALS` `windows:manage`; needs a
    vela rebuild for Bloom to hear it): `land` at 90% of a minimize's run,
    found by scanning the spring when it starts (in reduced, at 90% faded; with
    animation off, at once; show desktop's Recede sends none), `rise` as a
    restore starts. Queued per window on the compositor thread (a restore
    drops a pending land) and sent from `EvoState::minimize_landed_tick` in
    `process_pending_window_actions`, so it fires the same whether or not Lumen
    draws. Bloom ripples the icon on both (lane L4).
  - **Cost** (4 GB VM, software GL): four windows minimizing at once,
    `GetRenderingStatus` fps **40 / 49 / 50 with Lumen** (three samples a
    second apart, scale 10) against **28 / 37 / 37 genie-lite** (`SetLumen
    false`), which draws four live windows' elements: no loss (a second run:
    43 / 44 / 50 against 38 / 37 / 37). Snapshots took
    6-32 ms each (the first, 31.6 ms, is the cold one). One snapshot per window per minimize, as before
    (`lumen droplet: snapshot ... took` in evo.log).
  - **Ghost z-order**: ghosts are kept in stack order (`ghosts::add`). A ghost
    anchored to a window that is itself leaving is re-anchored to what that
    window was anchored to, and the leaving window's own ghost goes in just
    *below* it, so two separate minimizes keep the windows' order
    (`Ghost::owner`; shot in `droplet-zorder`).
  - `echo-cli watch [--signal S] [--interface I] [--seconds N] [--count K]`
    prints signals as JSON lines (every connection hears every signal); the
    suite reads `MinimizeLanded` with it, so it needs a new echo-cli pushed
    (`--push target/release/echo-cli:/usr/bin/echo-cli`) until the next image.
- **Evaporate (close)** (Lumen, lane L2, 2026-11-08; `anim/evaporate.rs`).
  The close snapshot burns away from its close button: a threshold dissolve
  over a field `0.5 × value noise (16 × 11 cells over the frame, plus 12% at
  64 × 44) + 0.5 × distance from the SSD close button` (normalised so the far
  corner is 1), threshold −0.10 → 1.02 linear in `p`, so nothing has gone at
  `p = 0` and everything has at `p = 1`. Pixels within 0.06 of the threshold
  glow, mixed toward `accent·0.45 + white·0.55` by `k²`. The picture swells
  1 → 1.04 (cubic out), blurs to 2.5 px (mip LOD bias), loses its shadow by
  30% and fades over the last 20%; ~380 ms. Accent: Bloom's from
  `SetMinimizeTargets`, else `lumen::accent_of` on the snapshot.
  - **Its own program**, not Lumen's shared one (that one clips, blurs and
    tints; it has no field or threshold): one quad, the noise computed in the
    fragment shader (Dave Hoskins' `hash12`, no `sin`), one texture fetch per
    pixel. `field()` in Rust is the same sum line for line, so the unit tests
    check what the shader draws (and an offscreen render of the shader on the
    host matched the model's fraction gone to within 1%). It uses Lumen's
    `link_program`/`scissor_box` and the same GL rules (divisor 0, a scissor
    per damage rect, state put back).
  - `GhostSource::Evaporate` carries the `Evaporate` (snapshot + accent); the
    pose is an ordinary `TransformAnim` (`evaporate::transform`), so **a close
    mid-open starts from the open's scale, place and opacity**, and progress is
    read off its scale animation. `evaporate::choose` picks Evaporate (full
    mode, `evaporate::usable()`: Lumen on and the program not failed), else the
    foundation's scale/fade (reduced: a plain fade ≤ 150 ms; off: nothing). A
    program that fails on the first frame of a running Evaporate draws the
    plain snapshot fading evenly for the rest of it; a failure is logged once.
  - Counters: `GetLumenState` `frames.evaporate`, `last_progress.evaporate`,
    and `evaporate_program` (untried|compiled|failed). The close kind's
    `started`/`completed` balance as before.
  - **Polling gotcha (cost a debugging round):** a suite loop that polls
    echo-cli with no `sleep` burns its count in a second or two (each call
    takes a few ms) and falls through *silently*, so the 25/50/75% shots land
    early and look like a shader that under-dissolves. `wait_ep` sleeps 0.1 s
    per poll and prints `EP-REACHED`/`EP-MISSED`, which the suite asserts.
  - **Cost** (4 GB VM, software GL): three windows closing at once at scale
    10, `GetRenderingStatus` fps **41 / 39** with Evaporate against **43 / 43**
    with Lumen off (other runs: 39 / 37 against 41 / 41, 38 / 37 against
    46 / 46), so a few fps under the plain close. The first sample
    after the three closes reads 0-1 in both: that is the three snapshot
    readbacks, not the shader. One snapshot per close, as before.
- **`evo.SetMinimizeTargets {targets:[{app_id,x,y,w,h}]}` → `{ok}`**
  (bridge `windows:manage`; needs a vela rebuild for Bloom to call it, echo-cli
  needs none): the whole table of dock icon rects, output-logical, relative to
  the output's origin; it replaces the last one. Parsed in `shell/minimize.rs`
  and held in a process-wide table (read only when a window minimizes, so it is
  set on the Echo thread like the overlay region, not queued). An app with no
  entry minimizes to a 48 px stand-in at the output's bottom centre
  (`resolve_target`). Reset in `toplevel_destroyed` when the overlay dies, with
  the work area. Bloom: `src/overlay/useMinimizeTargets.ts` measures every
  `[data-shell-edge="bottom"] [data-app]` button by layout (`offsetParent`
  chain, not the transformed box) and sends it through
  `setMinimizeTargets` (`packages/app-sdk/src/overlay.ts`).
- **Bloom (open)** (Lumen, lane L3, 2026-11-08; `anim/bloom.rs` the look and
  its maths, `anim/shader.rs` the two programs). It replaces the plain open
  (scale 0.92 + fade, `TransformAnim::open`), which is now drawn only with
  Lumen off (`EVO_LUMEN=off`, `SetLumen {enabled:false}`) or when Bloom's shader
  failed to compile (logged once). ~420 ms: `TransformAnim::bloom` is scale
  0.90 -> 1 on a ζ 0.86 spring about the frame's centre, a 16 px rise (expo
  out) and alpha done by 35%, so counters, a close mid-open
  (`minimize::close_from`) and a minimize mid-open treat it as any open.
  `WindowAnim::bloom` (`BloomFx`) carries the rest, as a function of time
  progress `p` (`bloom::look_at`, tested): blur 10 px -> 0 as `(1-p)^3` (the
  focus pull; under 0.3 px it samples once), corner radius 42 -> the frame's
  own (0 when maximized), accent tint 0.45 -> 0 by half-way, and a radial glow
  of the accent beneath the window, `0.33 sin(πp)`, out to 0.75 x its long
  side. Accent: `shell::minimize::accent_for` (Bloom's `SetMinimizeTargets`
  `accent`), else a neutral cool white (`bloom::NEUTRAL_ACCENT`). A window that
  is only appearing has no snapshot to average.
  - **On the live surface.** There is no snapshot to draw, so
    `render::stack_elements` hands the window's elements to `bloom::apply`,
    which swaps each textured one (client surfaces, lane D's `ClippedSurface`
    unwrapped, the title bar's memory buffers) for a `shader::BloomSurface`.
    That is a smithay texture-shader override like `ClippedSurface`: a 13-tap
    two-ring disc turned per pixel by a hash (fine grain, not doubled
    images), the tint, and a rounded clip on all four corners with an edge
    softened by the blur, so the silhouette goes out of focus too. The title
    bar's solid border edges can't be shaded, so they fade in once the
    corners are within ~2 px of landing (`bloom::border_alpha`). The glow is
    `shader::GlowElement` (a pixel shader, `OutputRenderElements::Glow`),
    drawn below the window and above its shadow. Both carry a fresh `Id`
    every frame, which is what makes the damage tracker repaint a surface
    whose commit hasn't moved, and repaint the plain element that replaces
    it when Bloom ends. Popups are drawn plain.
  - Programs: compiled on the first opening window, per `GlesRenderer`, own
    status (independent of `EVO_DEPTH`). No program means the same
    `TransformAnim` drawn plain (Bloom's scale, rise and fade). Reduced is the
    150 ms fade; off is nothing. Blur alone can be switched off with
    `EVO_BLOOM_BLUR=off` or `SetLumen {enabled, bloom_blur}`.
  - State: `GetLumenState` `frames.bloom`, `last_progress.bloom` (both
    counted in `anim::lumen`), plus `bloom_program`, `bloom_blur` and
    `bloom_blurred_frames` at the top level.
  - **Cost** (4 GB VM, software GL, three apps opening 2 s apart at scale 5,
    `GetRenderingStatus` fps while opens run, four runs): with blur
    28 49 49 46 46 46 / 27 47 47 44 42 42 / 47 49 49 45 45 47 /
    26 46 48 36 44, without 27 47 40 40 40 45 / 27 48 48 41 41 48 /
    27 47 47 39 39 44 / 45 45 44 41: no measurable cost,
    so there is no lighter blur mode. (Lumen off read 16-19, two samples
    each, with the launches still loading the VM.)
  - Seen in slow-motion shots of a fresh Files launch: soft, washed toward
    the accent, wide corners, sharp by ~75%. Files' first buffer already has
    content, so the blank-first-buffer case wasn't seen; a blank one would show
    as a soft tinted card, not a white flash. The shadow keeps the frame's
    16 px corners while Bloom's are wider.
  - On-device: the "bloom" section of `alpha5-animations.txt` (frames
    through Bloom and blurred, none in reduced/off or with Lumen off, counters
    balanced, `bloom-08/25/50/75` and `bloom-noblur-25` shots at Bloom's own
    progress, fps with and without blur). Its launches wait for the last vela
    of the app to exit (`bgone`) and for the window (`bmapped`). **The
    "flake" in about half the full-length runs** (the Files launch after the
    off-mode one never mapping) was Gecko's automatic safe mode: the suite
    closes several vela before their start-up finishes, each counts as a
    start-up crash, and past three a chromeless vela exited at once on every
    launch, for good. Fixed in Nova's `user.js` (see "On-device verification").
    `bmapped` alone can't see such a failure, since `ListWindows` lists the
    about:blank pre-window under the app's id too.
- **Show desktop (Super+D) is Evo's, drawn as Lumen's Recede** (lane L4,
  2026-11-08; `shell/recede.rs` the state and stagger, `anim/recede.rs` the
  poses and draw). `EvoState::show_desktop` (called from the
  `KeyAction::Shell("show-desktop")` handler, which still emits the
  `ShortcutTriggered` signal) minimizes every app window on screen, front-most
  first, 45 ms x scale apart (`spec::SHOW_DESKTOP_STAGGER`), each drawn as a
  **Recede** (~460 ms: scale 1 -> 0.88 about the centre, 14 px down, blur
  0 -> 10 px, alpha on a power-1.6 curve, a bezier fit) from a snapshot through
  the Lumen program (`Effect::Recede`). It goes through
  `minimize_window_as(.., MinimizeStyle::Recede(delay))`, so placement, focus
  and `Suspended` are a minimize's; it sends **no `MinimizeLanded`** (nothing
  lands in an icon). Ghosts are made back to front so the stack's order holds.
  - **State model: the desktop is shown.** Evo remembers the set it sent away
    (thread-local, front-most first). Super+D **emerges exactly that set** when
    no app window is on screen and one of them is still minimized
    (`recede::toggle`): back-most first, same stagger, ~440 ms each (**Emerge**,
    the Recede run back on mirrored curves; from the pose of a Recede still
    running, on its picture), through `unminimize_window_as(..,
    RestoreStyle::Emerge(delay))`, mapping back to front so the stack returns as
    it was, and the front-most is focused. Otherwise it recedes what is on
    screen. So a **dock restore** after a show desktop is a Rise of that one
    window (as any restore) and ends the shown state by putting a window on
    screen; the next Super+D sends *it* away rather than bringing the rest back.
    No hook in the restore or open paths: "shown" is judged at the keypress.
  - Counters: a Recede is a `minimize`, an Emerge a `restore` (no new kinds, so
    `GetAnimationState`'s shape is unchanged). `GetLumenState.recede` =
    `{shown, recedes, emerges, recede_order, emerge_order}` (window ids, in the
    order their animations start) for the suites; `frames.recede` counts frames
    of both. Reduced: an opacity fade in place (<= 150 ms, no stagger, no
    snapshot); off: instant; Lumen off: the live window under the same motion,
    unblurred. Bloom's `show-desktop` handler no longer moves windows.
  - **Cost** (4 GB VM, software GL): three windows receding at scale 10,
    `GetRenderingStatus` fps 45/37/38 through Lumen against 50/48/45 plain
    (`SetLumen false`); one snapshot per window at the keypress, 5-6 ms each at
    1280x680 (`lumen recede: snapshot ... took` in evo.log). Shots
    `recede-*`/`emerge-*` in `alpha5-animations.txt`.
- **Bloom's side of Lumen** (lane L4, 2026-11-08; `pyxis-experience`):
  - **Dock ripple and dip** on `MinimizeLanded` (`land` and `rise`): the icon
    rings in its app's hue, the ring spreading 14 px past the icon over 620 ms
    (expo out) while its light fades evenly, and the glyph dips a damped 5 px
    (`components/DockLanding.tsx`, state per app in `hooks/useDockLandings.ts`,
    SDK `onMinimizeLanded` / `MINIMIZE_LANDED_SIGNAL` in
    `packages/app-sdk/src/overlay.ts`). Reduced: the ring alone, fading in
    place (<= 150 ms); off: nothing. The dev server's mock emits `land` 430 ms
    after `MinimizeWindow` and `rise` on `RestoreWindow`. **Needs the vela
    rebuild** on device (the bridge entry is new).
  - **Accent** on each `setMinimizeTargets` entry: `accent: "#rrggbb"` from
    `APP_ACCENTS[id].hue` (`apps/registry.ts`, `appHue`), left out for an app
    Bloom has no accent for (`minimizeTargetPayload` drops an invalid one).
  - **Wallpaper drift** (`Desktop.tsx`, `hooks/useWallpaperDrift.ts`): while
    the desktop is shown the wallpaper layer scales 1 -> 1.035 over 700 ms
    (ease in-out), and back. Bloom mirrors Evo's rule from the signals:
    `ShortcutTriggered show-desktop` toggles, `WindowMinimized {minimized:
    false}` or `WindowCreated` ends it. Full mode only. Tokens: `LUMEN` in
    `src/motion.ts`. Tests: `src/overlay/__tests__/lumen.test.tsx`.
- Counters: `minimize`/`restore` kinds. Reduced is an opacity fade in place
  (<= 150 ms), off is instant. A spring's run is capped at 10 s
  (`REST_SCAN_LIMIT`), so slow-motion shots of a minimize use scale 30, not 60.
- **Geometry transitions** (lane G, built 2026-11-07, `anim/geometry.rs`,
  `shell/geometry_anim.rs`): maximize, unmaximize, fullscreen in and out, a
  fit to the work area and a work-area change (`refit_to_work_area`) move the
  window's *visible rect* from the old frame to the new with a ζ = 1 spring
  (about 280 ms, `spec::GEOMETRY`).
  - **Technique.** Every request handler calls
    `EvoState::begin_geometry_transition(window, expected_size)` **first,
    before the configure and before `place_window`** (and before fullscreen
    suppresses the header): it snapshots the window, SSD included, and the
    transition **waits**, drawing the snapshot at the old frame with the live
    window hidden (the live window is already at its final place with its old
    size, which would look like a jump; it still gets frame callbacks, which
    go to every Space window, so it can paint). The spring starts when the
    client's new size **lands** (`geometry().size` differs from the stale one,
    seen at draw time), or after `PENDING_LIMIT` (750 ms, inside the Settling
    marker's 2 s) if the client never resizes (a refusal, a minimum), and then
    settles onto whatever frame the window has. From then both the snapshot
    (opaque, underneath) and the live window (fading in over the first 40%)
    are drawn *scaled* to the moving rect, then the snapshot is freed. Only
    scaled textures are drawn, so the header text is never re-rasterized per
    frame (the live header was drawn once, at its final width).
  - **Placement and input stay instant and final.** The transition is a
    render-time pose (`GeoMotion::pose` takes the live frame; nothing reads
    the animated rect back). `GeoMotion` is a pure state machine with
    injected time; `GeoTransition` (snapshot + RAII counter, kind `geometry`)
    sits in the window's user_data and `render::stack_elements` draws it when
    the window has no `WindowAnim` (which wins, and cancels it).
  - **Interruptible.** The rect is two springs (location, size, in px), so a
    retarget keeps value and velocity. A live frame that changes under a
    running transition (a re-fit, a late size) retargets it by itself at the
    next frame. A new request that changes the size (maximize then unmaximize)
    takes a fresh snapshot of the live window and goes back to waiting while
    the springs carry on: no jump, one transition, one count.
  - **Only when there is a frame to leave.** Skipped (instant) for a window
    that has not opened yet, has `placement_pending`, is not mapped, or holds
    another animation, and when the backend can't snapshot. Unmaximize and
    unfullscreen only begin if the layout says the window *is* maximized or
    fullscreen: a client letting go of a flag it never had must not freeze its
    picture waiting for a size that will not change. A request whose expected
    size equals the current one (a pure move) needs no snapshot and no
    waiting: the live window slides.
  - **Fullscreen.** `output_elements` takes the exclusive branch only when the
    fullscreen window has no geometry transition, so entering and leaving draw
    through the normal stack (overlay above) until the transition ends, then
    hand over. `geometry::sweep`, run at the top of `output_elements`, ends
    transitions nobody draws (a window minimized or closed mid-transition) by
    the clock, so they can't hold the hand-over or a counter open.
  - **Modes.** Reduced: no scaling; the snapshot stays at the old frame and
    crossfades symmetrically with the live window at the new one, at most 150
    ms (instant when there is no snapshot). Off, or scale 0: nothing starts.
    Interactive move and resize start no transition (`DRAG-NO-TRANSITION`).
  - **Cost.** One GPU readback per transition (4 MB at 1280x800; another on an
    interrupt). `GetRenderingStatus` fps read 47-49 in the 4 GB VM (software
    GL) with two full-size layers on screen. The slow-motion shots at scale 60
    outlast a spring's 10 s run cap (`REST_SCAN_LIMIT`), so a spring that slow
    ends a few pixels early; only slow motion is affected.
  - **The shadow follows the moving rect.** `geometry::Drawn::shadow_pose`
    (live frame → the rect being drawn, alpha 1; the snapshot's rect while
    waiting) is what `stack_elements` hands `window_shadow` in place of an
    animation pose. A maximized layout casts none, so a maximize shows no
    shadow on the way and an unmaximize has it from the start.
  - X11 windows take the same hook (`x11.rs` maximize/fullscreen); that path
    has no on-device test.
  - **Settle glint** (Lumen, lane L4, 2026-11-08; `anim/glint.rs`): from 42%
    of the transition's nominal duration to its end, a soft diagonal band of
    white light (along `x + 0.6y`, a gaussian core with a faint tail, peak
    alpha `0.32 x sin(pi g)`) crosses the window from its top-left corner to
    its bottom-right, clipped to the frame drawn now, with the frame's corners
    as they are at that moment (16 px times the shadow's `appear`, which fades
    as a window goes square). A pixel shader (`GlintElement`,
    `OutputRenderElements::Glint`, in front of the live window), its own
    program in the EGL context's user data beside lane D's, through
    `GlesAccess`. The transition is held until the band is across
    (`GeoMotion::glint_pending`), so it never stops half way when the springs
    rest early. Full motion and Lumen on only; a failed compile draws none.
    Counted under `GetLumenState` `frames.glint` / `last_progress.glint`, with
    `glint_program` beside them. **On a white window it barely reads** (white
    over near-white is a couple of levels: measured, the band is there and
    moving, at x+0.6y ~900 at 50% and ~1500 at 75%); it shows on dark ones
    (`glint-term-*` shots, the Terminal).
- **Depth (lane D)**: shadows, rounded bottom corners, the focus crossfade and
  the grab lift, all following the window's pose through open, close, minimize
  and geometry transitions. Design and plumbing in
  [Window decoration model](#window-decoration-model).
- **Not done**: the open does not wait for a blank
  first buffer. A window minimized within ~260 ms of opening jumps from its
  open pose to the Droplet's start (the open's own pose and alpha are not
  folded into the Droplet's). The Droplet's picture is a snapshot, so a window that repaints while
  it flies (a video) freezes. On a multi-GPU setup the snapshot texture is
  uploaded to the render node's GLES context, which is the frame's only when
  both are one device (true of the VM and a single GPU). Minimize targets are
  relative to the output of the window being minimized, which is right for the
  one output Bloom's overlay covers.
- On-device suite: `scripts/serial-tests/alpha5-animations.txt` (counters,
  `off`, `reduced`, and slow-motion shots of a window mid-open and mid-close to
  look at; minimize/restore, the two interruptions, show desktop with three
  apps, and a close mid-open; targets set with echo-cli). Its geometry
  sections assert the counters, the final frames against the work-area maths,
  the interrupt, a work-area change and F11 fullscreen, and take the `geo-*`
  shots caught part-way at scale 60. Its Droplet section takes `droplet-*` /
  `rise-*` shots when `GetLumenState.last_progress.droplet` reaches 25/50/75%
  (icons below, left and right of the window, a restore, both interruptions,
  two windows' z-order, the lite fallback), asserts the program compiled,
  frames went through it (none with Lumen off, or in reduced/off),
  `MinimizeLanded` seen with `echo-cli watch` and landing between 75% and 95%,
  and prints the fps of four simultaneous minimizes with and without Lumen.

## Known code-level issues to track

- `echo/src/services/appmanager.rs:102` — no signature verification before tarball extraction (Nova's install plan formalizes the fix; not yet implemented). `appmanager` is superseded by Nova and should probably just be deleted.
- `evo` TODOs: HiDPI scale not read from render surface (`udev.rs`), X11 override-redirect ordering (`shell/x11.rs`). (`GetRenderingStatus` answers with real fps/memory since alpha 5.)
- `evo` resize (#11) works since alpha 5: an invisible 8 px band **outside** the SSD frame starts the grab (`WindowElement::ssd_resize_band`), because vela never sends `xdg_toplevel.resize` itself. Resize grabs work in **frame** size (header included) and convert to client size only in the configure they send (`grabs.rs::client_size`). Sending the frame size grew every SSD window by 32 px the moment a resize began. `cargo test --workspace` now has no expected failures.
- `evo`'s Echo broker matches replies by `reply_to`, and it re-registers `org.geckoOS.evo` after any reconnect (alpha 5). It used to take the next frame as a request's reply: a signal, or a call forwarded to Evo, could be eaten and never answered. It also never re-registered, so a dropped connection lost the service for the rest of the session. Its signals are fire-and-forget: a signal never waits on the compositor thread, and a request waits at most 3 s.
- `evo.ListWindows` lists the live toplevels (`EvoState::sync_toplevels`), with frame geometry. It used to read a map that held only windows Evo created itself (the old create/claim path, since deleted), so it was empty on every device (the same two-registries trap the window-action handlers once fell into).
- **A window's place, layout and keyboard focus each have one path** (`evo/src/shell/placement.rs`, since 2026-11-07). Move grabs, maximize, fullscreen, resize and X11 configure all go through `EvoState::place_window` and `set_window_layout`, which write the workspace record and map the element only if the window should be on screen; before, they wrote the Space directly and the record held only a window's birth location, so minimize/restore put windows back where they were born. Nothing may call `space.map_element` for a managed window. `focus_window` is the only way an app takes the keyboard (new windows on their first mapped commit, the dock, xdg-activation, click); closing or minimizing the focused window hands focus to the next app down the stack, else the shell. A minimized window is out of the Space but stays in the workspace and `windows_by_id`: look windows up with `tracked_window_for_surface`/`_toplevel`/`_x11`, not `space.elements()`, or a window closed while minimized leaks. `WindowElement::is_ssd()` is a `Cell` read, never `decoration_state()`, so geometry/hit-testing can't hit the RefCell re-entrancy panic. X11 windows are registered like xdg ones. BindSurface/UnbindSurface are fire-and-forget through the broker (sent once per window, when its app id first appears or changes, with the client's pid); a bind lost to an Echo reconnect is not retried. Minimize sets `xdg_toplevel` `Suspended`: that, not SIGSTOP, is the app's background hint. On-device suite: `scripts/serial-tests/alpha5-windows.txt`.
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
- **Permissions ARE enforced at the bridge, deny by default** (since
  2026-09-27). `PyxisBridgeParent.sys.mjs`'s `BRIDGE_CAPABILITIES` lists
  every (service, method) the bridge forwards with the capability it needs
  (`audio.setVolume` → `audio`, `evo.SetOverlayRegion` → `windows:manage`,
  …), or `null` for the few that need none (`system.getInfo`,
  `settings.get`, power/deviceInfo readings). An unlisted pair is refused
  (`Pyxis capability denied: app '<id>' called <svc>.<m>, which no app may
  call`), including `process.*`, `Echo.*`, Rockhopper/Shift and the device
  identifiers. Before that, an unlisted pair was *allowed*: any app could
  open a root shell (`terminal`), `process.spawn` as root, and drive evo,
  nova, appHost, installer, audio, sound and settings. The vocabulary lives
  in the table, `nova.rs` and vroom (config.rs + both schemas); two nova
  tests fail when they drift. Privileged: `notifications:manage`,
  `appManager:*`, `settings:write`, `windows:manage`, `terminal`, and
  `filesystem:user`/`system`/`apps` (trusted ids: bloom, installer,
  settings, terminal, files). Each refusal is logged once
  (shell: /var/log/vela.log; launched apps: /var/log/echo.log) — look
  there, since EchoClient falls back to its mock and a refused app looks
  fine. On-device suite: `scripts/serial-tests/alpha5-bridge-deny.txt`.
  **Signals are gated the same way** (since 2026-09-28). Echo sends every
  signal to every connection, so the bridge alone decides what an app
  hears: `BRIDGE_SIGNALS` lists each (service, event) with its capability
  (`notification.*` needs `notifications:manage`, `terminal.*` `terminal`,
  `appHost.*` and `nova.*` `appManager:manage`, …), checked when an app
  subscribes and again at delivery. Before, any app heard every app's
  notification text and the root shell's output. A refused subscription
  is logged like a refused call, and the app is told nothing: its listener
  never fires. Subscriptions belong to the document that made them (one
  actor each), not to its browsing context. On-device suite:
  `scripts/serial-tests/alpha5-bridge-signals.txt`.
  **Filesystem calls are confined** (since 2026-10-06). echo-daemon is
  root, and its filesystem service used to do whatever path a call named:
  any app with `filesystem:write` could rewrite /etc/shadow or delete
  /usr/bin. Echo can't tell apps apart (no peer credentials; `sender` is
  whatever the client writes), so the bridge stamps every filesystem call
  with a `scope` over what the page sent, and echo enforces it
  (`echo/src/services/filesystem/scope.rs`): every path resolves with
  `openat2(RESOLVE_BENEATH | NO_XDEV | NO_MAGICLINKS)` beneath the scope's
  root, on a thread that has dropped to the scope's user (no groups, no caps):
  pyxisapp for `app:<id>`, geckouser (uid/gid 1000) for `user`/`system`. Scopes: `app:<id>`, the app's own `/var/lib/pyxis/data/<id>`
  (made on first use, 0700, which the app sees as `~`; deleted, with the
  app's Gecko profile, by `installer.Remove`/`nova.Uninstall`, which wait
  for a running app to exit first); `user`, the home
  (`filesystem:user`, Files); `system`, the root filesystem read-only, by
  absolute path (`filesystem:system`, Files; /proc, /sys, /dev, /run and
  /tmp are other mounts and out of reach); and another app's own data,
  named by the call's `app` field (`filesystem:apps`, Bloom's desktop
  search over Notes/Contacts/Tasks). The bridge lists these in
  `FILESYSTEM_SCOPES`. Paths in a scope are `~`-rooted both ways, `..` and
  (outside `system`) absolute paths are refused, and a call with no
  `scope` is the old unconfined service, reachable only by clients that
  hold the socket. `deviceInfo.getStorageInfo` now answers for `/`
  whatever path it is given. Writing outside the home is planned as a
  sudo-backed "privileged access" unlock
  (`docs/internal/pyxis-privileged-access-plan.md`). On-device suite:
  `scripts/serial-tests/alpha5-fs-confine.txt`; hello-pyxis's proof file
  moved with it, to `/var/lib/pyxis/data/com.pyxis.hello/proof-<ms>.txt`.
  Still open: the visible half (prompt, per-app view, revoke).
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
- **App windows get the browser's shortcuts** (2026-11-16): the
  browser-init.js patch disables every `<key>` in `mainKeyset` for a
  `pyxisapp` window, so Ctrl+N/T/W/L/F/Q reach the page. Many are `reserved`
  (a page can't preventDefault them), and Ctrl+N in Notes/Contacts/Tasks used
  to open a full Firefox window inside the app. Copy/paste (`editMenuKeys`)
  and the external browser are untouched. The dev vela loads browser-init.js
  from source through a symlink, so on an image it can be pushed to
  `/usr/lib/pyxis-gecko/browser/chrome/browser/content/browser/browser-init.js`
  with no rebuild.
- **Vela's start page** (2026-11-16): a static page the browser app ships
  (`pyxis-experience/packages/apps/browser/public/start/`, the dashboard's
  pinned sites, a DuckDuckGo box, a clock and a sky that follows the hour),
  opened as `file:///usr/share/geckoOS/apps/browser/webroot/start/index.html`
  so the browser window gives it no bridge. Nova writes it into the browser
  profile's user.js only (`BROWSER_START_URL`, `browser_start_prefs`) as
  `browser.startup.homepage` and `pyxis.newtab.url`, which the
  AboutNewTab.sys.mjs patch reads (Firefox has had no new-tab pref since 41).
- **`CONFIG_USER_NS` and sound are on from alpha 5** (`scripts/build-kernel.sh`'s
  `ensure_y` list; the real kernel config is `scripts/.config` plus that list,
  **not** `mini/linux/.config`, which is an older kernel the image does not
  boot). Before, vela's content sandbox ran genuinely degraded, and Firefox's
  `sandbox-unprivileged-namespaces` warning was telling the truth. The pref
  that hides that warning in app windows stays, as belt and braces. Verified
  with `--kernel`: namespaces work, and apps launch and render under the
  sandbox. The image picks the kernel up at its next rebuild.
- **echo-daemon must never `waitpid(-1)`.** AppHost's SIGCHLD reaper used to,
  and it collected the exit status of every child, including `wpctl`, `opkg`
  and `pw-play`, before the code running them could. That code then failed
  with ECHILD ("No child processes"), even though the command had worked.
  The reaper now waits only on the pids it launched
  (`AppHost::reap_once`). Anything else that spawns must wait on its own
  child (`process.rs` and `terminal.rs` do).
- **remochi is 0.5.1 with `@duyojs/duyo` 1.1.3 as of alpha 5** (0.4.0 /
  1.1.0 came in 2026-11-09; port plan `docs/internal/pyxis-duyo-port-plan.md`;
  both repos are in `~/Development/`). 0.4 renamed `Button`'s `variant`
  to `type` (`htmlType` is the native one). It also fixed the two traps
  below, whose workarounds are still in the code until the Duyo port removes
  them:
  - `Slider` was uncontrolled in 0.3 (copied `value` at mount); 0.4 re-syncs
    `value` when not dragging. The volume sliders' `revision` key is now
    redundant.
  - `Toggle`'s types said `checked` while 0.3 read only `value`; 0.4 takes
    both. The locally-typed `Switch` cast in `QuickSettingsMenu.tsx` is now
    redundant (Settings' went with its Duyo port).
- **The eight apps are Duyo since 2026-11-09** (Phase 2; Bloom is still React,
  blocked on Duyo's whole-tree re-render). Each app is `*.view.json` +
  code-behind over `@pyxis/app-sdk/duyo` (`mountDuyoApp`, `pyxis.*` kinds,
  binders); React islands go in the app's own pack (`mountDuyoApp(kind,
  { packs })`), never into `pyxisPack`. Read `docs/internal/duyo-idioms.md`
  before touching one, and `duyo-upstream-gaps.md` for Duyo's limits (a
  published `path` silently has no setter; a remochi Dialog's `onClose`
  bubbles; hidden components stay in the DOM). Dev servers need
  `optimizeDeps.exclude: ['@duyojs/duyo']` (every config has it).

### Corrected 2026-08-16 (were stale, verified fixed — do not re-investigate)

`dbus.rs`'s `todo!()` test, the `ouranos.service` unit, Bloom's dead components/orphan `.scss`/`tsconfig-json.json`, the "12 `no-explicit-any` errors", `AppDrawer`'s phantom launcher entries, `useEcho.ts`'s react-hooks/refs violation, "Evo minimize not implemented", `apphost.rs`'s doc comment, and the claim that Nova scans `/usr/palm/applications` (it scans `/usr/pyxis/applications`) were all carried here long after the code stopped matching them.

## The one thing webOS got right, and Pyxis now has

webOS's LS2 bus enforced a **JSON role-file allow-list per identity** *at the bus itself*. Pyxis now does both halves: the **bridge** (`PyxisBridgeParent`'s `BRIDGE_CAPABILITIES`, deny-by-default since 2026-09-27) for `pyxis://` pages, and **Echo itself** for every socket client (2026-11-16, see [Bus permissions](#bus-permissions-and-hardening-wave-1)). What is left of this item is the user-visible half (install-time prompt, per-app view, revoke) in roadmap item 6.

**The Wayland half has the same shape, and is narrower since 2026-11-07**: Evo
separates apps from the system by connection class (see [Wayland trust
classes](#wayland-trust-classes-evo)), an app launched by Nova can't bind
screencopy, data-control, virtual-keyboard, input-method or the security-context
manager and can't claim a shell role, **and (since the app-uid lane) it can't
dial the host socket to escape that**: apps run as `pyxisapp`, `wayland-0` is
`geckouser:geckouser 0600`, and `pyxis-wlctx` hides it in the app's mount
namespace besides. The same split closes the shared-uid holes that no Wayland
filter could: an app can no longer `ptrace` the shell, read its `/proc/<pid>`
or its profile (`/var/lib/pyxis/profiles/bloom` is geckouser 0700). What remains
open:

- Apps have separate uids (since 2026-11-16), so the kernel keeps one app out
  of another's processes, data and profile; nothing yet confines an app's
  syscalls (no seccomp filter or LSM profile for native apps).
- **Echo's socket is mode 0666**, as it must be (apps reach it), but since
  2026-11-16 Echo checks every call and signal by the peer's kernel identity
  (`SO_PEERCRED`): see [Bus permissions](#bus-permissions-and-hardening-wave-1).
- **Anything root or geckoOS-boot starts that draws a window is trusted**, by
  design. Check new launch paths. The one that looked like a gap, Puppis's
  `GeckoAppLauncher` (reached from Rockhopper), isn't a launch path: it **starts
  no process at all** (it records an app and allocates its own ids; nothing
  in Rockhopper's daemon even calls it, the service it registers has no
  handlers), so there was nothing to route through `pyxis-wlctx` and nothing to
  remove for it. It was kept because roadmap item 3 builds on its suspend/resume
  bookkeeping, and `launcher_spawns_nothing` (puppis) fails if it ever gains an
  exec, so that can't happen unwrapped by accident. Every launch Nova makes
  (vela apps, native ipk apps, the external browser) goes through
  `contained_launch`; a nova test counts the call sites.
- A **window's title** is still the client's text, though the bar leads with
  the app's trusted name once Bloom sends `SetAppIdentity`; and apps can still use
  `/tmp` (sticky, shared with the shell's uid for reading world-readable files).
- `PR_SET_NO_NEW_PRIVS` is set on every app by `pyxis-wlctx` (wlctx.rs, after
  the uid drop). No LSM profile or seccomp filter confines native apps yet.

## Bus permissions and hardening (wave 1)

**Built 2026-11-16** (audit and lanes: `docs/internal/pyxis-hardening-plan.md`;
wave 2 is planned there). Verified on device: the shell's uid is refused
`process.spawn` and `Echo.EmitSignal`, an app's is refused `process.spawn`,
root still works, and an app connection hears none of a terminal's output while
root hears all of it; nine overlay/Echo suites pass.

- **Tiers** (`echo/src/security.rs`, from `SO_PEERCRED`, never from the
  message): `Root` (uid 0, the daemon's, system accounts below 1000) passes
  everything; `Shell` (uid 1000, the shell's vela) passes every rule but
  `RootOnly` (all of `org.geckoOS.process.*`, `Echo.EmitSignal`, and
  filesystem calls with no `scope`); `User` (1002-9999) and `App` (1001,
  10000+, and any other unplanned uid, `nobody` included) get only what
  `access_rule` grants by capability, checked against the caller's app,
  resolved from its pid (`AppHost::app_id_for_pid`, walking parents to the
  `pyxis-wlctx` helper) and Nova's manifest grants. Unlisted `org.geckoOS.*`
  methods are Root/Shell only. `Tier::is_privileged()` is Root or Shell.
  `org.geckoOS.*` names can be registered by Root only.
- **The bus and the bridge must agree.** A method the bridge forwards to an app
  must be allowed in `access_rule` too, or the app is refused on a device while
  the dev mock answers. When adding a bridge entry, add the rule.
  (`novaBridgeContract.test.ts` pins the SDK's nova calls to the bridge table.)
- **Signals** (`echo/src/signal_policy.rs`): only Root may emit on an
  `org.geckoOS.*` interface (a refusal is logged once per connection), and each
  connection hears what `signal_rule` allows: Root and Shell everything (the
  bridge then filters per page), an app only public signals and those its
  capabilities cover, mirroring `BRIDGE_SIGNALS`. Signals not in that table
  (`Echo.Shutdown`, `Echo.Pong`) are Root/Shell only.
- **Passwords** (`session.rs`, `privilege.rs`, `accounts.rs`): an account with
  no password still unlocks the screen press-to-enter, but privileged access
  refuses it with `reason: "no-password"` (the prompt sends the user to
  Settings → Users & Lock). The rate limit is per user and persists in
  `/run/pyxis/auth-limits.json` (`PYXIS_AUTH_LIMITS` overrides), so an Echo
  restart no longer resets it. `privilege.Authenticate` must name a
  `requestId`; the prompt's app name comes from Nova, never the caller.
- **Terminal**: the user's shell runs as the session user (not root) with a
  cleared environment. `process.kill` takes only known signal names or 1-64.
- **Image** (needs the rootfs and kernel rebuilt, not yet verified on device):
  `PYXIS_DEV_IMAGE` (default 1) keeps the root shells on tty1/ttyS0 that the
  harness needs; 0 gives getty logins. `/etc/sysctl.d/90-pyxis-hardening.conf`
  (kptr/dmesg restrict, Yama ptrace 1, protected links/fifos/regular, BPF and
  redirect hardening) is applied by rcS; `/tmp` is `nosuid,nodev`. The kernel
  gains Yama, Landlock, lockdown (integrity, forced at build time), usercopy,
  fortify, slab and init-on-alloc hardening, and loses kexec
  (`scripts/build-kernel.sh`).

### Wave 2 (2026-11-16)

Verified on device with the binaries, bridge, Bloom, Settings, Installer and
manifests pushed (the image predates it), and the dev marker pushed too.

- **Signed packages** (`echo/src/services/package_sig.rs`, used by both the
  installer and `nova.Install`): a package installs only with a detached
  `<pkg>.sig` (raw `openssl dgst -sha256 -sign` bytes; optional
  `<pkg>.sig.keyid`, else every key is tried) that verifies against
  `/etc/pyxis/keys/<id>.pub`. **Dev images allow unsigned** packages: the marker
  `/etc/pyxis/allow-unsigned-packages` is created by create-rootfs.sh when
  `PYXIS_DEV_IMAGE=1` (the suites' acceptance .ipk is unsigned; an image
  without the marker refuses it, as `nova.Install`'s and the installer's
  `"unsigned"` errors say). Both paths verify and install a root-owned private
  copy, never the user-writable original; Nova refuses tar members that aren't
  regular files or directories. Install sources: `/var/lib/pyxis/incoming`,
  `/media`, `/home` (not `/tmp` or `/root`). vroom signs: `pyxis keygen <id>`,
  `pyxis sign <ipk> --key <pem> [--key-id <id>]`, or `signing` in pyxis.json.
- **Privileged ids are the image's**: only System-tier (image) apps may hold a
  `PRIVILEGED_APP_IDS` id's privileged capabilities; an .ipk named `terminal`
  gets none, and nor does a built-in re-shipped as an .ipk update.
- **A uid per app** (`nova.rs` `app_uid`): from `/var/lib/pyxis/uids.json`
  (`{"high_water", "apps": {id: uid}}`), allocated from 10000 up, never reused;
  the shell's apps (bloom, bloom-overlay, `PYXIS_SHELL_APPS`) never get an
  entry. `pyxis-wlctx` gives each app a private `/tmp` (tmpfs, the host's
  `/tmp/.X11-unix` bound in).
- **`network` split**: `network:read` (status, addresses, scan) and privileged
  `network:manage` (connect, DNS, hostname, interfaces); a plain `network` in a
  manifest means read.
- **Firewall** (needs the rootfs and kernel rebuilt): nftables, default-drop
  inbound (loopback, established, DHCP, ICMP/ND rate-limited), applied by rcS;
  netfilter is in the kernel's ensure list.
- **Still open**: no update mechanism, the Wi-Fi password is on nmcli's argv,
  no seccomp/LSM confinement of native apps.

### Wave 3: one root-access dialog (2026-11-16)

Every root action asks the user, as polkit does on Ubuntu, through the shell's
`PrivilegePromptDialog` (contract and lanes: `pyxis-hardening-plan.md`,
"Wave 3"). Verified on device by `alpha5-root-dialog.txt`.

- **What asks** (`echo/src/actions.rs`): `package.install`
  (`installer.Install`, `nova.Install`) and `package.remove`
  (`installer.Remove`, `nova.Uninstall`) every time; `network.manage`
  (connectWifi, disconnectWifi, setInterfaceEnabled, setDNS, setHostname) once,
  then 300 s per app; `accounts.manage` (create, delete) every time; and the
  older `filesystem.system-write` flow. Not prompted (GNOME's defaults):
  timezone, power, `accounts.setPassword`, session, settings, windows. **Root
  callers (Evo, echo-cli as root) never ask; the shell does** (as `bloom`).
- **How** (`daemon.rs` `hold_decision`, `privilege.rs` `authorize`): after the
  access check, a call whose action has no live grant is held in a spawned task
  (the connection keeps reading, so the shell's own answer can arrive on it);
  Echo emits `PrivilegeRequested` with `action` and `details` (taken from the
  call by Echo, never the app's text) and the call completes on
  `privilege.Authenticate {requestId, password}`, or fails with error text
  `{"reason":"denied"|"timeout"|"no-password",…}` on `Dismiss`, after 120 s, or
  for an account with no password. Grants are per (app, action), cleared on
  lock. An app whose id doesn't resolve is refused outright.
- **Clients**: held calls get 150 s in the bridge (`HELD_CALL_TIMEOUT_MS`); the
  SDK's `callMethod` rethrows Echo's refusals (`isPolicyRefusal`) instead of
  falling back to the mock, which used to turn a refused install into a fake
  success; `installErrorText` words them. echo-cli (`EchoClient::call_method`)
  now skips signals and frames not answering its call: with a held call the
  connection stays free, so signals arrive before the reply.
- **Dialog**: title and sentence per action from `details` and Nova's name
  ("Installer wants to install **Weather** 1.2"); `data-privilege-request-action`
  marks the action for suites (`data-privilege-action` is the buttons').

## Consolidated roadmap (dependency order)

0. **Decide + build the app-serving strategy** — `pyxis://` scheme + from-source `vela` Gecko build. Blocking prerequisite for real web-app launching; the biggest unplanned workstream. (`pyxis-gecko-embedding-plan.md`, `pyxis-scheme-and-kiosk-plan.md`)
1. **Unify the JS bridge contract** — ✅ done: Bloom talks to the real `window.geckoOS` bridge (now an `@pyxis/app-sdk` monorepo package with `EchoClient`/`useEcho`/`useAppHost`).
2. **Build Nova** — ✅ done (in-process at `echo/src/services/nova.rs`, not the standalone crate `nova-runtime-host-plan.md` §8 describes). Registry/discovery, launch composition (`pyxis-wlctx`-wrapped `vela` run as the app user, delegates to `appHost`), install/uninstall, suspend/resume, origins map, permissions vocabulary all real. Bloom launches via `nova.Launch` and reads its app list from `nova.List` (alpha 4, `useApps.ts`). Standalone-crate extraction is a deferred follow-up.
3. **Wire Puppis's card lifecycle** (`suspend_app`/`resume_app`, already real code, not yet called by anything) behind `appHost`/Nova, driven by Evo visibility/focus events, wired to Gecko's own background-tab throttling. (Nova no longer suspends on minimize, since 2026-11-07: SIGSTOP silenced minimized music and left a minimized app unable to handle SIGTERM. `appHost.suspend`/`resume` stay as explicit calls on the process group, and `close`/`kill` SIGCONT a suspended group first. Evo's xdg `suspended` hint is the background signal.)
4. **Wire real status/notifications** — replace Bloom's static mock battery/network values with real Rockhopper Echo signals.
5. **Delete `WaylandSurfaceView`** — replace with chrome-only card metadata + `wlr-screencopy` thumbnails.
6. **Build the security/capability model** — per-app permission allow-list at the bridge boundary, tarball signature verification, sandboxing/cgroups for spawned apps.
7. **Finish Evo's rough edges** — minimize, HiDPI scale, real fps/memory stats, X11 ordering.
8. **Housekeeping** — update the architecture recap for Vroom's new SDK/PDK role; decide `pyxis-evo`'s fate (archive recommended); extend Bloom test coverage beyond alpha 4's Nova contract suite; replace Bloom's stock-Vite README. (Resolved: `gaia/` retired 2026-07-07; the `dbus.rs` test, `ouranos.service`, `apphost.rs`'s doc comment, and Bloom's dead code/lint errors/phantom entries are all done — see "Corrected 2026-08-16" above.)

## On-device verification

`scripts/serial-test.py` boots the image headless and drives it over the
serial console. Since alpha 5 it can also **see, click and hear**:

- `#!shot <name> [grim args]` saves a screenshot to `target/serial-shots/`.
  grim asks Evo over wlr-screencopy. QMP `screendump` still reports "no
  surface" on the GL scanout, so this is the only way to get pixels. Look at
  the PNG: a capture succeeding says nothing about whether the screen is right.
- `#!pointer move|click|drag X Y …` drives a virtio tablet over QMP. A
  coordinate can be `$VAR`, read from the guest shell.
- `#!type TEXT` and `#!key QCODE…` drive the virtio keyboard the same way
  (click into a field first). `alpha5-fs-confine.txt` types into Bloom's
  desktop search.
- `#!listen <name>` saves what the guest's HDA card played since the last
  listen, and reports `silent` when nothing did.
- `--push LOCAL:/path` files are laid over the rootfs by geckoOS-boot
  **before any daemon starts** (a read-only virtio disk, serial `pyxisin`), so
  `--push target/release/evo:/usr/bin/evo` tests a new Evo with no sudo
  rebuild. `--pull` brings files out, and `--kernel` boots a new kernel with
  the image's own root UUID.

```bash
scripts/serial-test.py --script scripts/serial-tests/alpha4-ipk-e2e.txt
scripts/serial-test.py -c 'opkg list-installed' --keep-running
scripts/serial-test.py --memory 4096 --script scripts/serial-tests/alpha4-exit-criteria.txt
```

Suites assert: a `#!must <text>` / `#!must-not <text>` line applies to the
next command and fails the run. Without them a suite only *prints*, and a
run in which every step errored still exits 0 — which has happened, twice.
The suites live in `scripts/serial-tests/`, including `alpha5-*.txt` for
screenshots, resize, notifications, audio and the overlay.
`alpha5-app-uid.txt` checks the app-user split (see [Wayland trust
classes](#wayland-trust-classes-evo)): the app uid can't dial `wayland-0`, an app
runs as `pyxisapp` under the root `pyxis-wlctx`, killing the helper kills the
app, sound and data work as `pyxisapp`. **It needs a rootfs rebuild for the
user/group, `/home/pyxisapp`, and geckoOS-boot's runtime-dir modes**; on an old
image its first step does those by hand and prints `ROOTFS=stand-in` (a rebuilt
image prints `ROOTFS=rebuilt`). Any other suite run on an old image must do the
same stand-in or apps can't reach Echo/PulseAudio (two lines at the top of that
suite). `alpha5-fs-confine` still asserts data dirs are `1000 1000`: they are
`1001 1001` now.

Every suite passes, with no known failures: all 26, swept 2026-11-09 against
the rebuilt image (09:01, Daylight and live settings included) with no
`--push`, except `alpha5-wallpaper`, which needs Bloom's window-store fix
(found by that sweep; see [Daylight wallpaper and live
settings](#daylight-wallpaper-and-live-settings)) pushed until the next image:
`--push ../pyxis-experience/dist:/usr/share/geckoOS/apps/bloom/webroot`.
With it, `alpha5-wallpaper` passed three runs of three, and the overlay suites
(`shell-layering`, `shortcuts-shell`, `thumbnails`, `overlay`, `windows`,
`settings-live`, `bridge-deny`) pass too. The next image also sets the kernel
hostname from `/etc/hostname` (About read "(none)"). The sweep of 2026-11-08
(16:24 image) found three bugs, fixed since. First, **an app closed during start-up four times could never be
launched again** (Gecko counts each as a start-up crash, and past
`toolkit.startup.max_resumed_crashes` a chromeless vela exits at once; nothing
reset the count). Nova's `user.js` and the shell's profile set it to -1.
Second, **windows closed while restoring left their restore
animation active for good** (an animation completes only when drawn; a killed
client has no buffer, so no close was made and nothing cancelled it):
`remove_window_from_workspace` now ends a window's own animations, and
`alpha5-close-mid-restore` checks it. Third, **a large frame could cost a
connection**: echo-daemon's per-connection loop and Evo's broker each read a
frame as one `select!` branch, which is not cancel-safe, so a signal arriving
while a frame was half read (a thumbnail reply is hundreds of KB) dropped the
half that was read and put the stream out of step; echo-daemon closed Evo's
connection, the call in flight failed with "Connection task dropped the
request", and `org.geckoOS.evo` was unregistered until the broker came back.
Both now read frames in a task of their own and wait on its channel
(`UnixSocketTransport::recv_message` says so; test
`broker_keeps_a_frame_split_by_its_own_signal`). `alpha5-app-uid` reports `ROOTFS=rebuilt`, `alpha5-echo-restart`
`SUPERVISOR-OK`, and `alpha5-shell-layering` and `alpha5-shortcuts-shell` have
their `BRIDGE-OK` lines (the rebuilt vela). `system-service` prints a
`✗ Rockhopper` line in a service listing; that is its expected output (a
registered-but-unreachable entry), not an assertion. If one of these fails on a
fresh image, it is a regression. Check first that the image is current: an
older one needs `--push` of the binaries, and an app can only reach Echo while
`geckoOS-boot` leaves `/run/user/1000` at 711 (it once reset it to 700).
`alpha5-shortcuts-shell` covers Super, Super+Space and Super+Tab opening the
launcher, search and overview over a running app.

How the suites find the shell: Bloom's bar and dock are the overlay's input
rects (`evo.GetOverlayState`), so `alpha5-bridge-deny` reads them from the
guest and places its clicks relative to them (the Bell, the Settings gear, the
dock's Files button). The quick-settings panel is one rectangle to Evo, so the
volume slider and Do Not Disturb switch inside it are placed from its anchors
(14 px from the right edge, 8 px under the bar); if a panel layout change
breaks them, look at `bridge-volume.png` and `bridge-dnd.png`. The notification
centre re-reads `notification.list` each time it opens, since signals are lost
while the overlay restarts. `kiosk-relaunch` counts app windows (`ListWindows`
minus Bloom) rather than the workspace's "total windows", which also counts the
shell and each launch's short-lived `about:blank` vela window.

A suite that reads hello-pyxis's proof file waits for it, since the page
writes it only once every probe has answered.

It runs on a qcow2 overlay, so the image is never mutated and each run
starts clean. Note that `make run` does NOT use an overlay: a hand-booted image
keeps whatever you did in it, such as an installed package. `make run` also
keeps its UEFI NVRAM (`target/build/images/OVMF_VARS.fd`), and the harness
never does. The harness therefore can't catch boot-order bugs: adding the
sound card moved the disk's PCI slot, and the saved NVRAM left `make run` at
"Start PXE over IPv4". Both now pin the disk with `bootindex=0`. The script's
docstring carries the serial gotchas that have each cost hours.

A full rebuild + verify is one command, `bash scripts/rebuild-and-verify.sh`
— it does the no-root work first, keeps the two root steps together, stages
the acceptance package into the rootfs *before* the image is built from it
(getting that order wrong yields an image that looks fine and has nothing to
install), and then runs all four suites.

The rootfs stages need root, so a full image build is still the user's to run
(`SKIP_TESTS=1 bash scripts/rebuild-and-verify.sh`). Between builds, `--push`
and `--kernel` cover most iteration.

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
- `docs/internal/pyxis-visual-identity-plan.md` (and `mockups/`) — the shell's visual identity; `pyxis-identity-upstream.md` — what remochi, Duyo and the apps need for it; `next-image-checklist.md` — what the next rebuild picks up and how to verify it. `pyxis-glass-and-lock-plan.md` — the frosted title bar and the lock screen, with the lanes' binding contracts.
