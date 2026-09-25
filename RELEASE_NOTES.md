# GeckoOS / Pyxis — Alpha 4 Release Notes

**Theme: "All my apps working."**

**Headline:** the app pipeline runs end to end. A third-party application
packaged with `pyxis-sdk` installs through the Installer, appears in the
launcher without anyone editing a source file, launches into its own
Evo-composited window on its own `pyxis://` origin, reaches the system
bridge with the capabilities it declared, and uninstalls cleanly. The
hardcoded app list is gone.

Alpha 3 made the system stop lying. Alpha 4 makes it useful.

It also closes the longest-running bug in the app-window work: **launching an
app a second time no longer comes up looking like a regular Firefox window.**

---

## The headline change: the launcher reads a registry

`PYXIS_APPS` in Bloom was a hardcoded array and the *only* source of apps
for the launcher, drawer, search and dock. No `.ipk` could ever appear on
the desktop, however cleanly it installed — and the Installer app itself,
built and staged since July, was unreachable from the UI.

The shell now reads `nova.List` through `useApps()`, refreshed on Nova's
`AppInstalled` / `AppUninstalled` / `RegistryChanged` signals. `PYXIS_APPS`
survives only as a *presentation overlay*: which Lucide glyph a built-in
draws with, its accent colour, whether it is pinned, its preferred window
size. An app absent from it still appears, drawn from its own manifest icon
with a generic glyph as the last resort.

**The load-bearing detail, easy to get wrong:** `RegistryChanged` is the
signal that matters, not `AppInstalled`. `nova.Install` is the user-tier
tarball path; an `.ipk` goes Installer → `opkg` → `nova.Reload`, which emits
`RegistryChanged` alone. Subscribing only to `AppInstalled` reads as
obviously correct and never fires for the case the feature exists for.

## Packaging, install and launch, verified on a real boot

`scripts/serial-tests/alpha4-ipk-e2e.txt` drives the whole flow with 33
assertions: `pyxis-sdk package` → `installer.ListAvailable` discovers it →
`Inspect` reads metadata without installing → `Install` → `opkg install` →
Nova indexes it as tier `opkg`, source `appinfo.json`, `removable: false` →
derived `setpriv` manifest and `origins.map` entry → visible to
`appHost.ListApps` and `pyxis-appctl` → `nova.Launch` → a live session with
a mapped compositor surface and server-side decoration.

The test app proves it *ran*, not merely that a window opened: it probes the
bridge on load and writes the result to a file. On device it reports
`origin=pyxis://com.pyxis.hello`, `secureContext=true`, `bridge=present` —
and the write itself succeeding proves Nova's appinfo→capability mapping
grants a **third-party** app a real capability, not just the built-ins.

The tier split holds: `nova.Uninstall` refuses an opkg-owned app and says to
use `opkg remove`; `installer.Remove` succeeds and Nova drops it.

## Two long-standing window bugs closed

- **#7, maximize → unmaximize closed the app.** A single physical click was
  dispatched on both press *and* release, and the header bar's button bands
  move between the two — so the release landed on Close. Fixed by ignoring
  anything but `ButtonState::Pressed`. Confirmed by hand; it cannot be
  automated, since driving maximize over IPC never touches the pointer path.
- **#10, restore from the dock.** Three separate faults, all fixed in August
  and none confirmed until now: Evo bound surfaces into its own
  process-global `AppHost` instead of the daemon's; it bound at map time,
  before a real `app_id` exists, so everything landed under `"unknown"`; and
  `IPCBus::call_method` never forwarded in-process→external calls, so
  `nova.Focus` could not reach Evo at all. The whole chain now round-trips.

## Apps no longer come back as Firefox windows

Reported five times since August: the first launch of an app is chromeless,
every launch after it has a URL bar. Four diagnoses were made; three were
wrong, and two of those were written up as fixes before a re-test disproved
them.

The kiosk window features were always applied correctly. The window is born
chromeless and then has its chrome flags rewritten about two seconds later —
by **SessionStore**, restoring the previous session's window features into
the new app window. `restoreWindowFeatures()` sets `.visible = true` on every
hideable feature the stored state doesn't list as hidden, and the app
window's state lists none, so the toolbar, location bar and menu bar all come
back and `#nav-bar` reappears. Only from the second launch, because the
restore needs a previous session to restore.

Firefox has the same problem with its own single-purpose app windows (Taskbar
Tabs) and guards SessionStore against it with one root-element attribute. A
Pyxis app window now gets the same guard. It's a JavaScript-only patch to the
vela fork, with no Gecko rebuild needed.

Verified on the rebuilt image with a control: three launches with the fix
keep the full kiosk `chromehidden` set, and the same three launches with
upstream's SessionStore restore the URL bar every time.

Why it took four attempts is the reusable part: **every probe fired at or
before window load, where the window is still correct**, so a broken build
measured clean. `PYXIS-DIAG chrome-late:` now logs the settled state five
seconds in, and `kiosk-relaunch.txt` asserts on it.

Two of the fixes made while chasing it were real, just not this bug:

- **Evo never removed a destroyed window.** `toplevel_destroyed` wasn't
  implemented, and smithay's no-op default meant it compiled and silently did
  nothing — the workspace count climbed 1→6 over three launches with zero
  removals. Dead windows now leave.
- **The sandbox-warning notification bar** stacked one per launch. It's
  suppressed in app windows now. The warning itself was correct, though —
  see Known limits.

## `org.geckoOS.system` — the bridge's last two dead methods

`window.geckoOS.getSystemInfo()` and `.listServices()` were in the API
surface every app sees, pointing at a service registered nowhere. Built
rather than deleted, since `listServices` is genuinely useful.

It exposed a real trap while being built: `org.geckoOS.Echo`'s own
`ListServices` reads `ServiceRegistry`, which **only** the over-the-wire
`RegisterService` call writes to — so on a booted device it reports 3
services while a dozen are live. The new `system.listServices` merges the
router's handler table with the registry and reports 16, resolving
`reachable` per entry against the live connection table. The old handler is
left alone deliberately (external clients may depend on its shape) and is
now documented as a trap rather than fixed.

## The reference VM needs 4 GB

Every app is its own vela — a full Gecko. Eight of them plus the shell
saturate a 2 GB guest at ~1929/1987 MB, and the apps that lose the race stay
**alive, with no oom-kill**, simply unable to allocate enough to map a
surface. That presents as a compositor fault and is not one. `QEMU_MEMORY`
now defaults to 4096, where all eight map using ~2793 MB. Giving a
backgrounded app its memory back is alpha 6's job (cgroups, Puppis
freeze/thaw); this is headroom, not a cure for the appetite.

## Testing became real

Bloom had zero tests. It now has Vitest and a Nova contract suite whose
shape assertions are **parsed out of `nova.rs` at test time** rather than
restated, so adding a field to the Rust projection without teaching the mock
fails at the desk instead of on device three weeks later. That suite
immediately earned itself: the mock implemented `nova.List` by delegating to
`appHost.ListApps` — a different projection of a different thing, colliding
on the field name `entry` — so anything built on it worked in `npm run dev`
and would have broken on device.

On the systems side, `scripts/serial-test.py` boots the image headless and
scripts the serial console, which is this project's only verification
channel. Suites assert with `#!must` / `#!must-not` directives. That was not
the first design: the suites originally only *printed*, and a run in which
every step failed still exited 0 — which happened, twice, and is why the
assertions exist.

At release, on the rebuilt image:

| | |
|---|---|
| On device | `kiosk-relaunch` 16 · `alpha4-ipk-e2e` 34 · `system-service` 21 · `alpha4-exit-criteria` 33 — all green |
| Bloom | 17 Vitest tests; `tsc -b` and eslint clean |
| Rust workspace | 171 of 172 — the one failure is the known corner-resize bug (see Known limits) |

Earlier drafts reported "Rust: 75", which was echo's count alone. The
workspace-wide run is what found the Shift signalling bug.

## Fixed along the way

- Nova handed web clients an absolute *filesystem* path for app icons, which
  a `pyxis://` client resolves against its origin root and 404s. The native
  manifest still needs the absolute form, so the two projections are now
  separate.
- `useApps()` fetched once per consumer — six components, five `nova.List`
  calls per shell start, and a fan-out on every signal. One shared store now.
- `evo`'s `FocusWindow`/`MinimizeWindow`/`RestoreWindow` answered
  `{"success": true, "window_id": 0}` for any argument, including the
  natural mistake of passing `appId`. They now reject a missing or
  non-numeric id.
- `evo.CloseWindow` was a stub that answered success and never told the
  client anything. It now sends a real `xdg_toplevel.close`.
- `evo.MaximizeWindow`/`UnmaximizeWindow` didn't exist ("Method not
  implemented"). With no IPC for maximize, neither the shell nor a test could
  maximize a window — only a human with a pointer — which is part of why #7
  and the kiosk bug both resisted headless reproduction.
- Every Evo error reached its caller as **"Unknown error"**: Evo wrote
  `body["error"]`, the bus read `body["message"]`. Both sides fixed, and a
  suite asserts the reason survives the trip.
- `echo-daemon` discarded the cause of nearly every error (`e.to_string()`
  drops anyhow's context chain) and logged every routed message at `info`,
  burying what was left. Causes kept (`{e:#}`); per-message logging demoted.
- **Shift signalled the wrong processes.** `send_signal` passed Shift's
  bookkeeping id — a counter starting at 1000 — to `kill(2)` as though it were
  a kernel pid, so terminate/suspend/resume hit whatever unrelated process
  held pid 1000, 1001, and so on, as root on device. It wasn't reachable in a
  normal boot (Shift's Echo methods are stubs and the daemon launches
  nothing), but `cargo test` itself sent SIGTERM to pid 1000 on whatever
  machine ran it. It now refuses to signal a process that has no kernel pid,
  which today is all of them.
- Three on-device suites carried assertions that **could not pass**: two read
  a proof file the acceptance app no longer writes, and three sentinels long
  enough to be eaten by the harness's command-echo stripper. One of those also
  demanded the wrong number. All fixed, and the stripper's length rule is now
  documented where suite authors will read it.

## Known limits

- A window action's reply is sent when the action is **queued**; existence is
  resolved later in the compositor loop, which has no way back to that reply.
  So a well-formed id for a window that does not exist still returns success.
  Fixing it means plumbing a response channel through the queue.
- The service registry is never pruned on disconnect, so a registered-then-
  crashed client keeps its entry. `listServices` marks it unreachable rather
  than hiding it.
- Permissions **are** enforced at the bridge — an app that didn't declare a
  capability gets `Pyxis capability denied`. (An earlier draft of these notes
  said the opposite; it was wrong.) What's missing is the visible half, for
  alpha 6: an install-time prompt, a per-app view, revocation, and enforcement
  for native clients that open the Echo socket directly, which the bridge
  never sees.
- **The kernel is built without `CONFIG_USER_NS`**, so vela's content sandbox
  is genuinely weaker than it should be. Firefox's warning about it was
  correct. It's suppressed in app windows because a warning bar in an app is
  noise, not because the problem is solved. Enabling it means a kernel
  rebuild, and it belongs with alpha 6's security work.
- **One Rust test fails, on purpose:** `evo`'s `test_resize_handle_detection`.
  It's correct: the left and right border regions exclude the corners, so the
  four corner resize handles can never fire. That belongs to #11 (window
  resize, alpha 5), and it's left red rather than `#[ignore]`d so it can't be
  forgotten.
- Shift's process layer is simulated beneath real bookkeeping: `fork_and_exec`
  spawns nothing, resource sampling returns constants, and its Echo methods
  (`LaunchProcess` and the rest) answer success without doing anything.
  Nothing in a normal boot depends on it.
- Headless screenshots remain impossible (QMP `screendump` → "no surface" on
  a GL scanout, re-confirmed on QEMU 10.2.1), so anything whose acceptance is
  visual still needs a human.

## Overall

Alpha 4 set out to make the app pipeline work end to end, and it does. The
unplanned cost was in the tests rather than the product: two suites that
passed while proving nothing, three that could never pass, a "Rust: 75" that
covered one crate out of seven, and one fix of mine that broke `nova.Focus`
outright and was caught only by running it on a device. Both are the same
lesson alpha 3 learned about the system — a green result you have not
watched fail is not evidence — now applied to the test tooling itself.

The kiosk bug adds a corollary for anything visual: **a check made at the
wrong moment is as bad as no check.** The window really was chromeless at
the moment every probe looked at it.

---

# GeckoOS / Pyxis — Alpha 3 Release Notes

**Theme: "Nothing lies to the user."**

**Headline:** every place the system reported something false, or failed because
a binary simply wasn't in the image, is fixed — and a boot-level bug that had
been silently killing *every daemon* moments after startup is fixed with it.
The status bar, the network indicator, the battery readout, the Files listing,
the package manager and the power actions now all tell the truth, verified on a
real boot rather than on a type-check.

This was intended as a cheap "clear the noise" release before the harder work.
It mostly was — but booting it turned up two defects that no amount of desk
review would have found, one of them significant.

---

## The boot fix (the big one)

**No daemon survived boot.** `geckoOS-boot` runs from `/etc/inittab` as a
`::wait:` entry, so every daemon it backgrounded shared that script's process
group and controlling terminal. When the script exited, the whole group took
SIGHUP and died together.

The symptom was almost designed to mislead: `/var/log/{echo,evo,vela}.log` were
each written normally at boot and then simply stopped mid-line with no error,
and by the time you had a shell, `ps` showed nothing but the two getty bashes —
while running *the same script by hand* from that shell worked perfectly, because
an interactive shell keeps the session alive and no SIGHUP is ever sent.

Fixed with `setsid ... </dev/null` in `start_daemon`, so each daemon is its own
session leader with no controlling terminal. Verified: an unattended boot now
brings up the full stack (`echo-daemon` pid 211/sid 211, `vela` pid 252/sid 252).

This is very likely implicated in some of alpha2's "IPC doesn't work" symptoms —
a shell talking to a compositor that has already been killed produces exactly
that class of failure.

## Rootfs / packaging

- **opkg now ships.** The Installer service gates every operation on
  `require_opkg()`, and opkg had never been in the image — which is why the
  install flow had never once been runnable. It cannot be apt-installed:
  **Debian packages no `opkg` (and no `opkg-utils`) in any suite.** It is now
  built from pinned, sha256-checked source (Yocto 0.10.0) in a new chroot build
  stage, configured for local `.ipk` installs only (no curl/gpgme/libsolv), with
  build dependencies purged afterwards.
- `/etc/opkg/opkg.conf` is generated with an architecture list mapped from
  `DEBIAN_ARCH` to Vroom's naming (`amd64` → `x86_64`), so a native `.ipk` built
  by `pyxis-pdk` is accepted on the machine that built it.
- Verified on-device: `opkg version 0.10.0`, `opkg list-installed` → rc 0.

## Echo services

- **Network status reads the kernel.** `get_status()`/`get_type()` used `nmcli`,
  which is not in the image (Pyxis uses BusyBox `udhcpc`), so the shell reported
  `connected: false` on every machine forever. Now reads `/proc/net/route` for
  the default route, filters synthetic interfaces by the `/sys/class/net/*/device`
  symlink, and checks `operstate`. Verified on-device:
  `{"connected": true, "ssid": "enp0s3", "type": "ethernet"}`.
- **Power actions target BusyBox init**, not the absent `shutdown`/`systemctl`.
  Verified on-device: shutdown powers the machine off, reboot reboots it.
  `sleep()` now uses `/sys/power/state` and returns a structured
  `supported: false` instead of a spurious success.
- **Battery stops inventing numbers.** It previously fell back to a hardcoded
  50% and a constant 40°C on machines with no battery; it now reports
  `present: false` with a null level, and scans for the real battery rather than
  assuming `BAT0`.
- **`set_screen_brightness` actually sets brightness.** It was piping nothing
  into `tee` and reporting success regardless — it now writes the sysfs backlight
  directly, and finds the device instead of hardcoding `intel_backlight`.
- **Files is fixed at the root cause.** `read_directory` propagated with `?` on
  `created()`, which returns `Unsupported` on tmpfs/overlayfs/procfs/sysfs and
  older ext4 inodes — so **one bad entry aborted the entire listing** and the app
  rendered nothing. Timestamps are now non-fatal (serialized as `null`),
  unreadable entries are skipped rather than fatal, and dangling symlinks fall
  back to `symlink_metadata`. Verified on-device: `/proc` returns 171 entries
  with `created: null`, the exact call that used to fail outright.
- **`readFile` honours its `encoding` parameter** (it was `read_to_string`
  regardless, so every binary read errored). `base64` is supported, with a
  `maxBytes` cap checked against the file's real length before reading, since the
  whole body crosses the bus as one JSON message.

## Bloom (desktop shell)

- **The clock ticks.** It was computed once per render with no timer, so it
  showed boot time forever. Now aligned to the minute boundary.
- **Files previews images inline**, using the new base64 read path.
- **Dead quick-settings controls are gone.** "Notifications",
  "Auto-brightness", "WiFi Enabled" and "Low Power Mode" had no handler and no
  backend to wire one to; they were removed, and WiFi became an honest read-only
  status row. "Show percentage in top bar" was wired for real through the
  settings bus and verified end to end.
- **Every toggle in the shell was rendering a permanent "Off"** — including Dark
  Mode. remochi's published types declare a `checked` prop, but the component it
  actually exports reads `value` and calls `onChange({value})`, so `checked`
  type-checked cleanly and was silently ignored. Bridged with a correctly-typed
  wrapper in both the shell and Settings.
- The SDK stops fabricating fallbacks: a failed battery or network call returns
  `null` instead of `level: 87` / `connected: true, ssid: "GeckoNet"`. The
  latter was the worst of them — an unreachable network service was
  indistinguishable from a healthy connection.

## Evo (compositor)

- **The decoration model is settled and written down: Evo owns server-side
  decoration for app windows.** There was no live contradiction, only stale
  text — the boot script still carried a superseded 2026-07-08 comment. The code
  has implemented Evo-SSD since 2026-07-25, negotiated over
  `org_kde_kwin_server_decoration` (GTK3 never binds `xdg-decoration` at all).
  Documented in `CLAUDE.md`; items #8/#9/#11 keep their existing Evo-SSD scoping,
  and #7 is an SSD bug rather than a two-parties-disagree bug.

## Testing and verification

- **Rust: 69 tests passing, up from 58 passing + 1 permanently failing.** That
  failure was deterministic in a full run: `reap_once` calls process-wide
  `waitpid(-1)`, and two parallel reaper tests consumed each other's children.
  Merged into one test; 9 new tests added for the filesystem and network changes,
  including a base64 encoder pinned against an independent implementation.
- **Lint is at zero** (`npx eslint .`, down from 2 errors). `cargo clippy` went
  from 32 warnings to 26, none new.
- On-device verification is now a repeatable process: QEMU + pexpect over the
  serial console, which `/etc/inittab` gives a root shell. Documented, including
  the traps — boot-script output goes to tty0 rather than serial, and `echo-cli`
  needs both the full destination name and `XDG_RUNTIME_DIR`.

## Documentation

- Eight stale claims in `CLAUDE.md` were deleted rather than annotated (the
  `todo!()` test, `ouranos.service`, Bloom "dead code", the "12 lint errors",
  phantom launcher entries, "minimize not implemented", and more — all long since
  fixed), along with stale module line counts and `/usr/palm` → `/usr/pyxis`.
- The audit that drove this release is `AUDIT_2026-08.md`; the plan is
  `ALPHA_ROADMAP.md`. Both are current as of this release.

## Known issues carried into alpha 4

- **#7 narrowed this cycle: maximize now works — it is *unmaximize* that
  crashes the app.** Restoring a maximized window down kills the client; the
  compositor survives. That is a meaningful narrowing: the item has been recorded
  since alpha2 as "maximize closes the app", and the maximize path is fine. Both
  handlers are defensively written with no panics, so the fault is in what the
  client is told on the way *down* — `unmaximize_request` sends
  `state.size = None` and re-maps at the saved location. Next step is the
  client-side log at the moment of the crash;
  `scripts/capture-maximize-repro.sh` collects exactly that window.
- **#10 restore-from-dock: re-diagnosed, and the previous theory was wrong.** The
  audit predicted two appHost sessions per app from an app_id mismatch; there is
  only one. Apps launch correctly and their bridge works (a launched Files app
  makes real `readDirectory` calls). The actual fault is narrower: **the
  compositor surface never binds to the appHost session** — `surfaces: []`,
  `ListSurfaceSessions` count 0 — so `nova.Focus` correctly reports "no mapped
  compositor window yet". Surface binding is where to look, not identity.
- #2 Terminal QOL, #11 window resize, and #12 visual polish are unchanged.
  #12 remains an undefined umbrella and needs scoping before it can be scheduled.
- The shell's app list is still the hardcoded `PYXIS_APPS` array — Nova's
  registry already returns all 10 apps on-device, including the Installer, but
  nothing in the shell asks it. That is alpha 4's headline.

---

## Overall Alpha 3 summary

Alpha 3 was meant to be the cheap release that makes later debugging
trustworthy, and it delivered that: the status bar, network, battery, Files,
packaging and power actions are now all honest, and each was confirmed on a real
boot rather than assumed. The unplanned wins were larger than the planned ones —
opkg turned out to need building from source rather than a one-line package add,
and the discovery that no daemon survived boot reframes several long-standing
"IPC is broken" symptoms as downstream of a dead compositor. The app pipeline
itself is in better shape than the audit suggested: apps launch, the bridge
works, and Nova's registry is complete. What remains between here and "all my
apps working" is surface binding, the maximize crash, and pointing the shell at
the registry that already exists.

---

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
