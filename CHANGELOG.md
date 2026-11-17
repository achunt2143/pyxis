# Changelog

All notable changes to the Pyxis (codename GeckoOS) system repo: the Evo
compositor, the Echo bus and its in-process services (including Nova), the vela
Gecko patch series, the Vroom SDK, and the image, kernel and on-device test
tooling. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

The shell, SDK and apps live in the sibling repo; see
[`../pyxis-experience/CHANGELOG.md`](../pyxis-experience/CHANGELOG.md).
Design rationale for most entries below is in `CLAUDE.md` and `docs/internal/`.

## [0.1.0-alpha.5] - 2026-11-16

Alpha 5 turns the simulated-to-real shell into a trust-separated desktop: apps
are untrusted Wayland and bus clients, the compositor has its own motion and
visual language, and the shell gains an overlay, a lock screen and live
settings.

### Evo

#### Added
- Wayland trust classes (`evo/src/trust.rs`): clients are Trusted (Evo's own socket) or App (via `wp_security_context_v1`, and XWayland), decided by how they connected, never by what they claim.
- Globals hidden from apps (`trust::trusted_only`): wlr-screencopy, data-control, virtual-keyboard, input-method, security-context, wlr-layer-shell.
- Shell overlay (`evo/src/shell/overlay.rs`): the trusted `bloom-overlay` window is stacked above apps, undecorated, unbound and left out of `ListWindows`.
- Overlay IPC: `evo.SetOverlayRegion` (input region), `evo.SetOverlayFocus`, `evo.GetOverlayState`, and the `OverlayFocusChanged` signal.
- Work area (`evo/src/shell/work_area.rs`, `evo.SetWorkArea`): maximize, placement, move/resize clamps and a shrink-to-fit of floating windows respect the shell's insets; reset when the overlay dies.
- Global shortcuts in Evo (`input_handler.rs`): Super (launcher), Super+Tab (overview), Super+Space (search), Super+D (show desktop), Super+Q (close focused app), emitted as `ShortcutTriggered`.
- Alt+Tab switcher (`evo/src/shell/switcher.rs`): Evo-owned MRU state machine, `SwitcherChanged` signal, `evo.GetSwitcherState`.
- Window thumbnails (`evo/src/shell/thumbnails.rs`): `evo.GetWindowThumbnails` renders windows offscreen, caches per commit counter, and the broker encodes JPEG q80.
- Real wlr-screencopy (`evo/src/enhancements/screencopy.rs`); it was an uncompiled stub before.
- Animation foundation (`evo/src/anim/`): render-time transforms with RAII counters, ghosts, snapshots, modes (`EVO_ANIMATIONS=full|reduced|off`, `EVO_ANIMATION_SCALE`), `SetAnimationSettings` and `GetAnimationState`.
- Lumen motion language: shared snapshot program (`anim/lumen.rs`), Bloom open (`anim/bloom.rs`), Droplet minimize and Rise restore (`anim/droplet.rs`, `anim/warp.rs`), Evaporate close (`anim/evaporate.rs`), Recede/Emerge for show desktop (`anim/recede.rs`), settle glint (`anim/glint.rs`).
- Minimize, restore and show desktop animations with retargeting; `evo.SetMinimizeTargets` (dock icon rects and accents) and the `MinimizeLanded` signal.
- Geometry transitions (`anim/geometry.rs`, `shell/geometry_anim.rs`): maximize, unmaximize, fullscreen, work-area refits animate on a spring from a snapshot.
- Depth (`anim/depth.rs`, `anim/shader.rs`): drop shadows, rounded bottom corners on client content, focus crossfade and grab lift; `GlesAccess` trait, `evo.SetDepth` / `GetDepthState`.
- Visual identity (`shell/identity.rs`, `shell/ssd.rs`): waterdrop frame (corners 16 4 16 16), window controls drawn as the remochi spinner shapes, `evo.SetAppIdentity` (app name, accent, icon in the title bar), `title_label` in `ListWindows`.
- Busy detection (`shell/busy.rs`): `xdg_wm_base.ping` on focus, click and every 5 s; busy windows show the spinner morph; `ListWindows` gains `busy`.
- Frosted title bar (`shell/frost.rs`, `FrostElement`): blurred, veiled wallpaper crop behind each bar, F0 flat fallback, `evo.SetFrost` / `GetFrostState`, `EVO_FROST`.
- Session lock (`shell/lock.rs`, `anim/veil.rs`, `anim/clear.rs`): fail-closed drawing and input, Veil and Clear transitions, Super+L, idle lock (`SetIdleLock`), `evo.Lock`, `GetLockState`; unlock only on a signal with a verified HMAC.
- `MaximizeWindow` / `UnmaximizeWindow` IPC, window resize band (`WindowElement::ssd_resize_band`), `echo-cli watch`.

#### Changed
- Server-side decoration is Evo's for all app windows; the decision is recorded and negotiated over `org_kde_kwin_server_decoration`, because GTK3 never binds xdg-decoration. Bloom is exempt.
- Window place, layout and focus each have one path (`shell/placement.rs`: `place_window`, `set_window_layout`, `focus_window`); nothing maps a managed window into the Space directly.
- Minimize sets `xdg_toplevel` `Suspended` as the background hint instead of SIGSTOP (Nova no longer suspends on minimize).
- `GetRenderingStatus` returns real fps and memory.
- Windows bind to Echo sessions by app id (`BindSurface`); the `is_shell`/`is_overlay` roles require a trusted client.

#### Fixed
- `toplevel_destroyed` was unimplemented, so closed windows were never removed.
- `CloseWindow` was a stub that never told the client; error replies said "Unknown error" and lost the cause.
- Resize (#11) never started because vela never sends `xdg_toplevel.resize`; the 8 px band outside the frame now starts the grab, and grabs work in frame size (they grew SSD windows by 32 px).
- Windows closed while restoring left their restore animation active forever.
- The Echo broker took the next frame as a request's reply and never re-registered `org.geckoOS.evo` after a reconnect; it now matches by `reply_to` and re-registers.
- A large frame split by a signal dropped half of itself and put the stream out of step (non-cancel-safe `select!` read); frames are now read in a task of their own.
- Frost updates hung the compositor thread when `with_renderer_surface_state` was called inside a surface-tree walk.
- After unlock, the front app now gets the keyboard back (the overlay-keyboard gate left no window focused).

#### Removed
- `CreateWindow`, which spawned programs as root.
- Super+Q and Ctrl+Alt+Backspace quitting the compositor, and Super+Enter running `weston-terminal` as root; debug toggles remain only under the `debug` feature.
- The genie minimize funnel (retired for Lumen; `anim/genie.rs` is now a shim).

### Echo

#### Added
- Notifications, sound and audio as real services (`org.geckoOS.notification`, `org.geckoOS.sound`, `org.geckoOS.audio` via PipeWire `wpctl`); system sounds in `system-sounds/`.
- Time service (`echo/src/services/time.rs`, jiff): `getClock`, `listTimezones`, `setTimezone`, `getLocation`, `TimezoneChanged`.
- Settings: schema validation for every `pyxis.*` key (`settings/schema.rs`), `SettingChanged {key, value, previous, exists}` signal, atomic writes under one lock; new keys `pyxis.wallpaper.motion`, `pyxis.wallpaper.pinnedTime`, `pyxis.location`, `pyxis.clock24h`, `pyxis.launchBar.position`, `pyxis.lock.after`, `pyxis.lock.onSleep`, `pyxis.lock.notifications`; `pyxis.theme` gains `auto`.
- Session and accounts services (`services/session.rs`, `services/accounts.rs`): `lock`/`unlock`/`getState`, `list`/`setPassword`/`setProfile`; passwords checked with libxcrypt `crypt_r`, rate limited, and `power.sleep` locks first.
- `LockChanged {locked, seq, mac}`: HMAC-SHA256 with a key in `/run/pyxis/session.key` (root 0600), so Evo can tell Echo's word from an `echo-cli emit` forgery.
- `pyxis-appctl` (`echo/src/bin/appctl.rs`) and the Installer service (`services/installer.rs`), see Nova.
- `pyxis-wlctx` launch helper (`echo/src/bin/wlctx.rs`) and `pyxis-wlprobe` test client (`echo/src/bin/wlprobe.rs`).

#### Changed
- Filesystem service confined (`filesystem/scope.rs`): every path resolves with `openat2(RESOLVE_BENEATH | NO_XDEV | NO_MAGICLINKS)` on a thread that dropped to the scope's user; scopes `app:<id>`, `user`, `system` (read-only), and another app's data via `filesystem:apps`.
- `deviceInfo.getStorageInfo` answers for `/` whatever path it is given.
- `echo-cli` skips signals and unrelated frames while waiting for its reply.
- `org.geckoOS.system.listServices` replaces the empty-by-design `Echo.ListServices` as the way to see what is on the bus; `echo-cli services`/`status` repointed.
- Per-connection frame reads moved to a task of their own (see Evo fix above).

#### Fixed
- AppHost's SIGCHLD reaper called `waitpid(-1)` and stole exit statuses from `wpctl`, `opkg` and `pw-play` (ECHILD); it now waits only on pids it launched.
- Two concurrent settings writes could lose one, and a crash mid-write could empty the file.

### Nova

#### Added
- Nova is the `.ipk` app registry (scans `/usr/pyxis/applications/*/appinfo.json`, writes derived manifests to `/var/lib/pyxis/nova/native/`) and refuses `Uninstall` for opkg-owned apps.
- Every launch goes through `pyxis-wlctx` as root, which creates the app's own Wayland socket carrying the app id, drops to the app user and sets `PR_SET_NO_NEW_PRIVS`; it fails closed.
- Per-app uids (wave 2, `nova.rs` `app_uid`, `/var/lib/pyxis/uids.json`, allocated from 10000, never reused), private gid, private `/tmp`, `/run/user/<uid>`; `pyxisapp` (1001) is the fallback (`PYXIS_PER_APP_UID=0`).
- `network` capability split into `network:read` and privileged `network:manage`.
- `PRIVILEGED_APP_IDS` can be held only by image (System-tier) apps; an `.ipk` named `terminal` gets nothing.
- Vela start page: Nova writes `pyxis.newtab.url` and the homepage into the browser app's profile only.

#### Changed
- App data dirs and Gecko profiles are owned by the app user (`chown_tree` takes over a prior image's dirs without following links); the shell's stay geckouser's.
- `user.js` is written by unlink-and-`create_new` so a planted symlink is not followed by root.
- Apps use `PULSE_SERVER=unix:/run/pyxis/pulse/native` (a second PipeWire Pulse address), not the 0700 default.

#### Fixed
- An app closed four times during start-up could never be launched again (Gecko counted each as a crash and went into safe mode); `user.js` and the shell profile now set `toolkit.startup.max_resumed_crashes` to -1.

### Security

- Bridge is deny-by-default (`PyxisBridgeParent` `BRIDGE_CAPABILITIES`): an unlisted service/method pair is refused; before, any app could call `terminal`, `process.spawn` as root, and drive evo, nova, installer and settings.
- Bridge signals are gated per capability (`BRIDGE_SIGNALS`); apps no longer hear other apps' notifications or the root shell's output.
- Apps run as a separate uid from the shell; `wayland-0` is `geckouser 0600` and hidden in the app's mount namespace, so an app cannot `ptrace` the shell or read its profile.
- Bus permission tiers (wave 1, `echo/src/security.rs`, `signal_policy.rs`): Root, Shell, User and App tiers from `SO_PEERCRED`; `org.geckoOS.process.*`, `Echo.EmitSignal` and unscoped filesystem calls are Root-only; only Root may emit or register `org.geckoOS.*`; per-connection signal filtering.
- Passwords (wave 1): no-password accounts refuse privileged access (`no-password`), auth rate limit persists in `/run/pyxis/auth-limits.json`, `privilege.Authenticate` requires a `requestId`, and `Message`'s `Debug` redacts `session.unlock` and `accounts.setPassword`.
- Terminal runs the user's shell as the session user with a cleared environment; `process.kill` takes only known signals or 1-64.
- Signed packages (wave 2, `echo/src/services/package_sig.rs`): detached `<pkg>.sig` verified against `/etc/pyxis/keys/<id>.pub`; unsigned allowed only with the dev-image marker; install from a root-owned private copy; Nova refuses non-regular tar members; sources limited to `/var/lib/pyxis/incoming`, `/media`, `/home`.
- Root-access dialog (wave 3, `echo/src/actions.rs`, `services/privilege.rs`): `package.install`/`remove`, `network.manage`, `accounts.manage` and system filesystem writes ask for the user's password through the shell; grants are per (app, action) and cleared on lock; held calls survive on the connection.
- Firewall: nftables default-drop inbound, applied by rcS.

### vela patches (`gecko/patches/pyxis/`)

#### Added
- `PyxisBridgeParent.sys.mjs`: capability table, signal table, filesystem scopes, shell-overlay and lock bridge entries (`SetWorkArea`, `SetOverlayFocus`, `GetOverlayState`, `GetWindowThumbnails`, `SetMinimizeTargets`, `SetAppIdentity`, `session.*`, `accounts.*`, `time.*`), 150 s timeout for held calls (`HELD_CALL_TIMEOUT_MS`).
- Shell overlay window (`--pyxis-overlay`): app id `bloom-overlay`, chrome that paints nothing, ARGB visual (`BrowserContentHandler`, `browser-init.js`).
- Vela branding (`browser/branding/unofficial`, `browser/locales`, `moz.configure`: `MOZ_APP_VENDOR=Pyxis`, `MOZ_APP_BASENAME=Vela`).
- `AboutNewTab.sys.mjs`: vela's new tab is the `pyxis.newtab.url` pref when set.

#### Changed
- App windows disable every `<key>` in `mainKeyset` so Ctrl+N/T/W/L/F/Q reach the app's page; before, Ctrl+N in Notes opened a full Firefox window inside the app. `editMenuKeys` and the external browser window are unchanged.
- `PYXIS-DIAG chrome-late:` is a permanent settled-chrome probe.

#### Fixed
- App windows lost kiosk chrome about 2 s after load because `SessionStore.restoreWindowFeatures()` rewrote chrome flags (guard added to `SessionStore.sys.mjs`).
- Lock screen: `setCompositorVisible` no longer throttles Bloom (`bloom`, `bloom-overlay` and `system` exempt), so the overlay is not blank after a session lock.

### Vroom

#### Added
- `pyxis keygen <id>` and `pyxis sign <ipk> --key <pem> [--key-id <id>]`, and a `signing` block in `pyxis.json` (`vroom/src/signing.rs`).
- `appinfo.json` / `pyxis.json` schema and spec updates for the `network:read`/`network:manage` split and signing.

### Image & kernel

#### Added
- Kernel: `CONFIG_USER_NS` and sound on; hardening (Yama, Landlock, lockdown integrity, hardened usercopy, fortify, slab and init-on-alloc, BPF unpriv off, dmesg restrict); netfilter/nftables; kexec off (`scripts/build-kernel.sh`).
- Rootfs (`scripts/create-rootfs.sh`): PipeWire, `/dev/shm`, system sounds, `pyxisapp` user, `/etc/sysctl.d/90-pyxis-hardening.conf`, nftables ruleset, `/tmp` `nosuid,nodev`, hostname from `/etc/hostname`.
- `PYXIS_DEV_IMAGE` (default 1) keeps root shells on tty1/ttyS0 and the unsigned-package marker; 0 gives getty logins.

#### Changed
- `make build-rootfs` builds gecko before the Pyxis web build; `PYXIS_NPM_CI` reuses existing `node_modules`.
- `rebuild-and-verify.sh` checks binaries, Bloom and app dists and Gecko patch freshness before the root steps.
- `stage_binaries` reports missing binaries with `[ERROR]`; the disk is pinned with `bootindex=0` so `make run` keeps booting.

### Tests / tooling

#### Added
- `scripts/serial-test.py`: `#!shot`, `#!pointer`, `#!type`/`#!key`, `#!listen`, `#!keydown`/`#!keyup`, `--push`, `--pull`, `--kernel`.
- New on-device suites (`scripts/serial-tests/`): alpha5-wayland-trust, app-uid, bridge-deny, bridge-signals, fs-confine, overlay, shell-layering, shortcuts, shortcuts-shell, thumbnails, windows, resize, animations, close-mid-restore, notifications, audio, wallpaper, settings-live, identity, identity-evo, frost, lock, root-dialog, echo-restart, launch-raise, startup-lockout, eyes, plus `tour-screenshots`; `kiosk-relaunch` now part of `rebuild-and-verify.sh`.
- Contract tests that read Rust source (settings schema, identity corner/spinner values).

#### Fixed
- `cargo test --workspace` has no expected failures (the resize test is fixed, not ignored).

## [0.1.0-alpha.4] - 2026-09-25

Released 2026-09-25 (tag `alpha4`). The shell reads Nova's registry instead of
a hardcoded app list, and a `.ipk` built with Vroom installs through opkg and
launches from the desktop. Also added `org.geckoOS.system`, window-action
argument validation, a 4 GB reference VM, the first headless serial test
harness, and fixes for the kiosk-on-relaunch bug and Shift signalling the
wrong processes. See `PROJECT_STATUS.md` ("Alpha 4 released") and
`git log alpha3..alpha4` for details; earlier releases are in git history.
