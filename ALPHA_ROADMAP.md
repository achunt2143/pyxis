# Pyxis — Alpha Roadmap to Pre-Beta

Release plan from alpha2 (shipped) to **pre-beta**. Derived from
[`AUDIT_2026-08.md`](AUDIT_2026-08.md); read that first for the evidence behind
each item. Status tracking stays in [`PROJECT_STATUS.md`](PROJECT_STATUS.md) —
this file is the *plan*, that file is the *state*.

## Definition of pre-beta

Pre-beta means: **a person who has never seen Pyxis can boot the image, use it
as their desktop for an hour, install a third-party app, and not hit a wall.**

Concretely, all of the following must be true:

1. Every shipped app opens, does its job, and closes cleanly.
2. Every app installed via `.ipk` appears in the launcher and launches.
3. Windows can be moved, resized, minimized, maximized, restored, and closed
   without crashing anything.
4. The user can shut down, reboot, and lock the machine from the UI.
5. Notifications appear.
6. The status bar tells the truth (clock ticks, network reflects reality).
7. No dead controls in the shell — every switch and button does something.
8. A crash in one app doesn't take down the shell or the compositor.

Everything below is sequenced to reach that, in dependency order.

---

## Sequencing principle

Each alpha has **one theme** and ships only when its exit criteria pass **on a
real boot**, not on a type-check. Alpha2's lesson is written into this plan:
a fix that hasn't been observed working on-device is not done, and re-guessing
without a repro log is how a cycle gets burned. Every alpha below therefore
ends with a mandatory on-device pass, and every "fix" item names the specific
observation that proves it.

Rough calendar assumes solo work; adjust freely, the *order* is the part that
matters.

---

## Alpha 3 — "Nothing lies to the user"

**Theme:** eliminate every place the system reports something false or fails
because a binary isn't in the image. Almost all of it is small, and it clears
the noise that makes the harder bugs hard to see.

**Why first:** you cannot debug #7/#10 reliably while the status bar is
frozen, the installer is impossible, and half the shell's controls are inert.
This pass is cheap and makes every later diagnosis trustworthy.

### Scope

**A. Rootfs — binaries services depend on** (audit §1)

- [x] ~~Add `opkg` to `create-rootfs.sh`'s apt list~~; write `/etc/opkg/opkg.conf`
      (`dest root /`, arch list matching Vroom's `CONTROL/control` output).
      **Corrected while doing it:** there is no `opkg` (or `opkg-utils`) package
      in Debian, in any suite — the one-line apt fix assumed here does not
      exist, and adding it would have failed the whole `apt install`. Done
      instead as `stage_opkg()`, building pinned + sha256-checked opkg 0.10.0
      from the Yocto release tarball inside the chroot (`-DWITH_CURL=OFF
      -DWITH_GPGME=OFF -DUSE_SOLVER_LIBSOLV=OFF` — Pyxis only installs local
      `.ipk` files), with build deps purged afterwards. Arch list is mapped from
      `DEBIAN_ARCH` to Vroom's naming rather than hardcoded.
- [x] Replace `NetworkService::get_status()`/`get_type()`'s `nmcli` dependency
      with direct kernel reads (`/sys/class/net/*/operstate`, `device` symlink
      filter, `/proc/net/route` for default route). Reuse the synthetic-interface
      filter already proven at `create-rootfs.sh:690-700`.
- [x] Repoint `PowerService::shutdown`/`reboot` at BusyBox
      (`busybox poweroff` / `busybox reboot`). Make `sleep()` either use
      `/sys/power/state` or return a structured `unsupported` error instead of
      a false success.

**B. Correctness bugs** (audit §3)

- [x] `filesystem.rs::read_directory` — make `created`/`modified` non-fatal
      (`.ok()` → `null`), and make per-entry `metadata()` failures skip the
      entry rather than abort the listing. **This is the primary suspect for
      the Files bug (#3); do it before any on-device tracing.**
- [x] `filesystem.rs::read_file` — honour the `encoding` parameter; add
      `base64` for binary reads plus a `maxBytes` guard. Unblocks Files image
      preview.
- [x] `TopSystemBar.tsx` — tick the clock (`setInterval`, aligned to the
      minute).
- [x] `QuickSettingsMenu.tsx` — wire or remove the dead toggles
      (Notifications / Auto-brightness / WiFi Enabled / Low Power Mode /
      Battery Percentage). Removing is an acceptable answer; leaving them is
      not.
- [x] `DesktopSearch.tsx:34, :52` — fix the two real lint errors
      (`react-hooks/set-state-in-effect`). Get lint to zero and keep it there.

**C. Decoration-model decision** (audit §5.1) — *blocking for alpha 4*

- [x] Write down the answer: does Evo draw SSD for app windows, or does GTK?
      The boot script (`create-rootfs.sh:960-990`) says GTK; `PROJECT_STATUS.md`
      items #8/#9/#11 assume Evo. Pick one, update both, and re-scope #8/#9/#11
      against the decision.
      *Recommendation: GTK owns decoration for GTK/vela clients; Evo SSD scopes
      down to non-GTK and XWayland clients only.* Note: the dev (me) recommends
      to prioritize Evo SSD to keep themeing consistent.

**D. Doc drift** (audit §7)

- [x] Correct the 8 stale claims in `CLAUDE.md`/`INTEGRATION_NOTES.md`. Delete
      rather than annotate where the item is simply done.
- [x] Update `apphost.rs:20`'s "Intentionally NOT in this MVP" comment — surface
      registration is real now (`state.rs:1701`).

### Exit criteria (all observed on a real boot)

Verified 2026-08-16 on the built image over the serial console.

- [x] Network reflects actual DHCP state — `network.getStatus` returns
      `connected: true / enp0s3 / ethernet` (was permanently `false`).
      Clock ticks verified in the browser preview; the Bloom-rendered status
      bar still wants an eyeball on-device.
- [x] Files lists rather than blanking — `readDirectory` on `/proc` returns
      153 entries with `created: null`, and `$HOME`/`/tmp` list correctly.
      This was the exact call that used to error out wholesale.
- [x] `opkg list-installed` runs without error (rc=0, no parse warnings).
- [x] Shutdown **and** reboot work on the device (`power.shutdown` powered the
      guest off; `power.reboot` rebooted it).
- [x] `npx eslint .` → 0 errors. `cargo clippy` → 32 → 26 warnings, none new.
- [x] `CLAUDE.md` contains no claim contradicted by the code (stale line
      counts were the last ones; corrected).

**Found and fixed by the boot itself** (neither was findable at the desk):

- [x] `opkg.conf` used the OpenWrt-ism `lists_dir ext PATH`; opkg 0.10.0 wants
      `option lists_dir PATH` and warned on every invocation.
- [x] **No daemon survived boot.** `geckoOS-boot` runs from inittab as
      `::wait:`, so its backgrounded daemons shared its process group and
      controlling terminal, and all took SIGHUP when it exited — logs written
      normally, then stopping mid-line, with the same script working by hand.
      Fixed with `setsid ... </dev/null`; an unattended boot now comes up
      complete.

---

## Alpha 4 — "All my apps working"

**Theme:** the app pipeline runs end to end — package, install, discover,
launch, focus, close — with no hardcoded list anywhere.

**Why second:** this is the release that satisfies the actual goal. It depends
on alpha 3's opkg fix, and its debugging depends on alpha 3's honesty fixes.

### Scope

**A. Dynamic app registry** (audit §2.1) — *the headline* — ✅ **done**

- [x] Add `listApps()` / `getAppInfo(id)` to `@pyxis/app-sdk`'s `useEcho`,
      calling `org.geckoOS.nova` `List`/`GetInfo`.
      **Corrected while doing it:** the mock did not "already handle both" in
      any usable sense — `nova.List` was implemented by *delegating to
      `appHost.ListApps`*, a different projection of a different thing. It
      returned `exec`/`icon`/`count` and omitted `tier`/`removable`/
      `singleInstance`/`icons`, so anything built against it would have
      worked in dev and broken on device. Replaced with `mockNovaRegistry()`,
      a real `info_json()`-shaped projection, and pinned by item D.
- [x] Add a `useApps()` hook (`src/hooks/useApps.ts`): Nova's list as the
      source of truth, refreshed on `AppInstalled`/`AppUninstalled`/
      `RegistryChanged`, with `PYXIS_APPS` demoted to a presentation overlay.
      `RegistryChanged` turned out to be the load-bearing one: it is what
      `nova.Reload` emits, and the Installer's `opkg` path goes through
      Reload — so an `.ipk` app arrives by that signal and no other.
      The hook deliberately carries no liveness; running-ness stays
      `useAppHost`'s question.
- [x] Extend `PyxisAppDef` with `iconUrl`; the three-tier fallback (Lucide
      glyph → manifest artwork → generic package glyph, plus an `onError`
      catch so a 404 doesn't leave a broken-image box) lives in the new
      `AppIcon` component, shared by all four consumers.
- [x] Repoint `LeftLauncher`, `AppDrawer`, `DesktopSearch`, `BottomDock` at
      `useApps()` — plus `useWindows` and `Desktop.tsx`, which the roadmap
      missed: `Desktop`'s three launch sites each did
      `mockApps.find(...)` and **silently dropped the launch** when the id
      wasn't in the hardcoded list, so repointing the launcher alone would
      have made third-party apps visible but unlaunchable. `AppDrawer`'s
      empty state now distinguishes loading / unreachable-registry / no
      matches instead of always claiming "No applications found".
- [x] Add `installer` to the registry so the App Installer is reachable from
      the desktop. *(Done in the alpha3 Bloom pass; also added to the mock
      app set, which was still missing it.)*
- [x] Replace `registry.ts`'s hardcoded `entryHtml` with `entryUrlFor(id,
      entry)`: `pyxis://<id>/<entry>` on device, the package's `devPort` off
      it. The discriminator is `location.protocol === 'pyxis:'` rather than
      `import.meta.env.DEV` — the latter is a build-time flag and would be
      wrong for a dev build run on device. `DesktopWindowType.entryHtml` is
      renamed `entryUrl` to match.

**B. Installer end-to-end** (audit §1.1) — *service layer done on device;
one step needs a human*

- [x] Build a `.ipk` with `pyxis-sdk`, drop it in `/var/lib/pyxis/incoming`,
      install it, and launch it. **Verified on a real boot 2026-09-20** with
      `scripts/serial-test.py --script scripts/serial-tests/alpha4-ipk-e2e.txt`:
      `pyxis-sdk new/package` → `installer.ListAvailable` discovers it →
      `installer.Inspect` reads metadata without installing →
      `installer.Install` → `opkg install` → `registry reloaded: 11 apps` →
      files at `/usr/pyxis/applications/<id>/` → Nova indexes it
      (tier `opkg`, source `appinfo.json`, `removable: false`) → derived
      native manifest written to `/var/lib/pyxis/nova/native/<id>/` →
      `origins.map` updated → `appHost.ListApps` and `pyxis-appctl list` see
      it → `nova.Launch` spawns vela → **AppHost reports a live session with
      a mapped compositor surface**, and Evo negotiates SSD for it
      (`app_id=Some("com.pyxis.hello") kde_decoration=Some(Server)`).
- [x] The app really runs, not just opens a window: the test app writes a
      probe file through the bridge, and on device it reported
      `origin=pyxis://com.pyxis.hello`, `secureContext=true`,
      `bridge=present`, `initialized=true` — and the write itself succeeded,
      which proves Nova's appinfo→capability mapping
      (`filesystem.write` → `filesystem:write`) grants a *third-party* opkg
      app a real capability.
- [x] Verify the uninstall split holds. `nova.Uninstall` refuses with
      "app '<id>' was installed via opkg; use `opkg remove <id>` instead";
      `installer.Remove` succeeds, files go, Nova drops the app and cleans
      up its derived manifest.
- [x] The shell's half, on device: Bloom drives its launcher from the real
      bus. echo.log shows exactly one `nova.List` per shell start, rising on
      each install and each removal — so `useApps`'s `RegistryChanged`
      subscription works against real Echo, not just the mock.
- [x] **Needs a human at the screen:** confirm the app *appears* in the
      launcher and launches from a click. **Confirmed by the user**, and on
      2026-09-25 with the real 128×128 icon drawn, which closes the icon
      caveat under the exit criteria below. Headless screenshots are
      impossible here — QMP `screendump` reports "no surface" on a GL
      scanout, re-confirmed on QEMU 10.2.1, so this is not stale tooling.
      Everything behind the pixels is verified; what is unverified is the
      pixels. Run `make build-release && make run` (the rootfs stages need
      root) and look.

**C. Fix the two carried-over window bugs** — *#10 verified; #7 needs a mouse*

- [x] **#10 restore-from-dock.** The three fixes made 2026-08-17 (bind over
      Echo rather than into Evo's own process-global AppHost; bind at
      `ack_configure` where a real app id exists rather than at map time;
      `IPCBus::call_method` forwarding in-process→external calls) had sat at
      "awaiting on-device confirmation" ever since. **Confirmed 2026-09-20**
      by `scripts/serial-tests/alpha4-exit-criteria.txt`: no session has
      `appId: "unknown"` or `pid: 0`, `nova.Focus` returns a real `windowId`
      instead of "No handler registered for org.geckoOS.evo.FocusWindow",
      and the whole dock chain round-trips — `MinimizeWindow(window_id)` →
      the app reads `suspended` → `nova.Focus` → back to `running`.
- [x] **#7 maximize→unmaximize crash.** Root-caused and fixed 2026-08-17 (a
      single physical click dispatched on both press *and* release, with the
      header bar's button bands moving between the two, so the release
      landed on Close). The fix — ignore anything but `ButtonState::Pressed`
      in `element.rs`'s `PointerTarget::button`.
      **Confirmed by the user on the rebuilt image, 2026-09-20:** maximize
      then restore-down, no crash. It could not be checked any other way —
      the bug is a pointer press/release pair, so driving maximize over IPC
      would not exercise the broken path, and there is no synthetic-input or
      screenshot channel (QMP `screendump` → "no surface" on a GL scanout,
      re-confirmed on QEMU 10.2.1).

**D. Bridge contract tests** (audit §5.4) — *done, shipped with A*

- [x] Add Vitest to `pyxis-experience` (`npm test`; `tsconfig.test.json`
      typechecks tests separately so the app bundle never gets node types).
      First suite, `src/hooks/__tests__/novaContract.test.ts` (10 tests):
      asserts the mock's `nova.List` carries every field **parsed out of
      `nova.rs::to_info_json`/`info_json` at test time**, so the check
      maintains itself; asserts it leaks none of appHost's; and drives the
      full install → appears → remove → gone flow through `EchoClient`.
      Confirmed it bites by deleting `tier` from the mock and watching it
      fail with an actionable message. **Done as part of A rather than last**
      — it is the test that catches exactly the mismatch A uncovered.

**Also fixed, outside the plan**

- [x] **Apps came back as Firefox windows on relaunch** (reported since
      alpha 2, never in scope). SessionStore restored the previous session's
      window features into the new app window and un-hid the URL bar about
      two seconds after load. Fixed 2026-09-25 by giving `pyxisapp` windows
      the SessionStore guard upstream gives Taskbar Tabs. Verified on device
      against a control. See
      `gecko/patches/pyxis/browser/components/sessionstore/SessionStore.sys.mjs.PYXIS_DIFF`.
- [x] Evo never removed destroyed windows (`toplevel_destroyed`
      unimplemented); `evo.CloseWindow` was a stub; `evo.Maximize`/
      `UnmaximizeWindow` didn't exist; every Evo error arrived as
      "Unknown error".
- [x] Shift sent signals to its bookkeeping ids as though they were kernel
      pids. It was latent in a normal boot, but live under `cargo test`.
- [x] Three on-device suites had assertions that could not pass.

### Exit criteria

Status as of 2026-09-20, after a rebuild and a full on-device pass;
re-verified at release 2026-09-25.

- [x] **A third-party `.ipk`, built with `pyxis-sdk` and installed through
      the Installer app, appears in the launcher and launches.** Confirmed
      by the user on a real desktop, and by
      `scripts/serial-tests/alpha4-ipk-e2e.txt` (33 assertions) headlessly.
      *Caveat on "with its own icon":* the acceptance package shipped a 1×1
      transparent placeholder from `pyxis-sdk new`, which loads without
      error and draws as nothing — so the icon path could not be told apart
      from the generic fallback. It now carries a real 128×128 icon, **seen
      on screen by the user 2026-09-25** — the manifest-icon path works.
- [x] **Uninstalling it removes it from the launcher.** Verified both ways.
- [x] **All 8 built-in apps launch, focus from the dock, minimize, restore,
      and close.** All eight at once did not fit the old 2 GB reference VM:
      at 2048 MB the guest reached 1929/1987 MB and the last three to launch
      never mapped a surface — alive, not OOM-killed (no oom-kill in
      `dmesg` at all), just unable to allocate enough to paint, which
      presents as a compositor fault and is not one. Measured at 4096 MB all
      eight map, using ~2793 MB, so **`emulate.sh`'s `QEMU_MEMORY` default
      is now 4096** and `serial-test.py` matches it. Giving a backgrounded
      app its memory back is still alpha 6's job (cgroups, Puppis
      freeze/thaw); this is headroom, not a fix for the underlying
      appetite.
- [x] **Maximize→unmaximize does not crash.** Confirmed by the user on the
      rebuilt image. See item C for why this one could not be automated.
- [x] **Test suite green** — with one known, deliberate exception.
      Re-verified at release, 2026-09-25, on the rebuilt image. Bloom: 17
      vitest, `tsc -b` and eslint clean. On device: `kiosk-relaunch` 16,
      `alpha4-ipk-e2e` 34, `system-service` 21, `alpha4-exit-criteria` 33.
      Rust workspace: **171 of 172**.
      **Correction:** this line used to say "Rust: 75", which was echo's count
      alone. Nobody had run the workspace, which had two red tests. One was
      catching a real bug (Shift passing its bookkeeping id to `kill(2)` as a
      kernel pid), now fixed. The other,
      `evo::test_resize_handle_detection`, is correct and is left failing:
      the corner resize handles can never fire. It moves to alpha 5 D with
      resize. Two on-device suites had also stopped being able to pass (see
      "Also fixed" above).

## Alpha 5 — "A desktop, not a shell"

**Theme:** the system-level surfaces a desktop is expected to have.

### Scope

**0. Verification that can see, hear and click** *(added 2026-09-25, done)*

Almost every exit criterion below is visual or audible, and the harness could
only read the serial console. `scripts/serial-test.py` now has:

- [x] **Screenshots.** `#!shot <name>` runs `grim` in the guest, which asks
      Evo over wlr-screencopy. Evo's screencopy had been a stub that nothing
      compiled. It is now real (`evo/src/enhancements/screencopy.rs`) and
      verified on device, full output and regions. It also needed `/dev/shm`,
      which rcS never mounted.
- [x] **Pointer.** `#!pointer move|click|drag` drives a virtio tablet over QMP.
- [x] **Sound.** `#!listen <name>` records the guest's HDA card to a WAV and
      reports its level (`silent` when nothing played).
- [x] **Fast iteration.** `--push` files go in on a transfer disk that
      geckoOS-boot lays over the rootfs before any daemon starts, so a pushed
      `evo` is what the whole boot runs. `--pull` brings files out, and
      `--kernel` boots a new kernel with the image's own command line. No sudo
      rebuild per iteration.

**A. Layer-shell compositing** (audit §4.5) — *unblocks the rest of this alpha*

- [ ] Render `wlr-layer-shell` layers in Evo's `output_elements`. **Probably
      already true:** the udev path goes through smithay's
      `space_render_elements`, which draws background/bottom layers below
      windows and top/overlay above. Unverified, because no layer client
      exists yet. Note that the fullscreen branch of `output_elements` skips
      layers entirely.
- [ ] Move the wallpaper to a real background-layer client, so it sits below
      Bloom rather than inside it. Enables shell transparency, "show desktop",
      and a wallpaper that survives a shell restart.
- [x] **Shell UI above apps: Bloom's overlay** *(decided and built
      2026-09-27)*. Evo stacks Bloom beneath every app, so shell UI that must
      be seen over apps had nowhere to go. The decision: a transparent Bloom
      overlay window, not compositor-drawn UI, so shell UI stays web tech.
      vela opens it in the shell's own process (`--pyxis-overlay
      overlay.html`, from geckoOS-boot) with the app id `bloom-overlay`.
      Evo stacks that above apps, never decorates or focuses it, and gives it
      input only where Bloom says it draws (`evo.SetOverlayRegion`).
      Everywhere else, clicks pass through. Toasts live there now. Verified on
      device by `alpha5-overlay.txt`: a toast's X, clicked over a maximized
      app, dismisses it, and clicks beside the toast reach the app.
      *Takes effect in the image at the next rebuild.* The lock screen goes
      here next (B). The top bar, dock and panels could follow: today a
      maximized app covers them, and nothing reserves their space.

**B. Session control** (audit §4.1)

- [ ] Power menu in quick-settings: Shut down / Restart / Lock / Log out,
      calling `org.geckoOS.power` (fixed in alpha 3). *Shut down, Restart
      and Sleep exist and are real. Lock and Log out do not.* Shut down and
      restart now play the shutdown sound first.
- [ ] Lock screen in Bloom's overlay (A), input-exclusive, unlocked by a
      credential check against the geckouser account. Evo must enforce the
      lock itself, not trust the overlay to cover the screen: while locked it
      should draw and route input to nothing but the overlay. smithay's
      `ext-session-lock` is the protocol-level version of that guarantee.
- [ ] A confirm-before-shutdown flow that gives running apps a chance to close.

**C. Notifications** (audit §3.6, §4.2)

- [x] Rewrite `notification.rs` as Echo-native: store the notification, emit an
      Echo signal. The `notify-send`/`dbus-send` shell-outs are gone.
      `show`/`list`/`close`/`closeAll` are real, with `NotificationShown`
      and `NotificationClosed` signals. The daemon had registered no `list` at
      all, so Bloom's centre could never have shown anything.
- [x] Notification centre in Bloom, on the Bell button, with a badge count;
      toasts; dismiss. Do Not Disturb holds toasts (not critical ones) and the
      notification sound. Toasts draw in Bloom's overlay, above app windows
      (see A).
- [x] Expose notifications through `@pyxis/app-sdk` (`showNotification` and
      friends). The bridge stamps the caller's app id, so an app cannot post as
      another. Reading and clearing the centre needs the new privileged
      `notifications:manage`, which only the shell holds.

**D. Window management completion**

- [x] Resize (#11): drag an invisible 8 px band outside Evo's frame, corners
      included. Verified on device by `alpha5-resize.txt`: a shrink from the
      bottom-right edge and a grow from the top-left, exact to the pixel, plus
      a control drag inside the frame that must not resize. The corner bug is
      fixed (`test_resize_handle_detection` is green, and `cargo test
      --workspace` has no expected failures now). Found and fixed on the way:
      maximized SSD windows overhung the screen by the header's 32 px;
      windows were placed blind, before their size was known, often partly
      off-screen; and Evo's Echo broker read the next frame as a request's
      reply, so it could lose its bus registration mid-session.
- [x] Real values in Evo's `GetRenderingStatus` IPC method, and `ListWindows`
      now lists the live toplevels. It used to list only contract-created
      windows, so it was empty on every device.
- [ ] HiDPI scale read from the render surface (`udev.rs:1460`).

**E. Audio** (audit §1.4)

- [x] Decide the stack: **PipeWire** (with its Pulse server and WirePlumber).
      This was effectively forced: vela's build defines `MOZ_PULSEAUDIO` and
      not `MOZ_ALSA`, so it can only play through a Pulse server. The kernel
      had `# CONFIG_SOUND is not set`. `build-kernel.sh` now builds HDA and
      virtio-snd in. The 2026-09-25 image has the whole stack, verified by
      `alpha5-audio.txt`.
- [x] Volume control in quick-settings (the slider used to write a setting
      nothing read) and output device selection in Settings → Sound, over the
      new `org.geckoOS.audio`. System sounds (`system-sounds/` at the repo
      root) play through `org.geckoOS.sound`: boot, shutdown, notification,
      error. Setting the volume changed it and then reported "No child
      processes", because AppHost's reaper collected every child of
      echo-daemon, `wpctl` included. It now waits only on the apps it
      launched. The quick-settings knob sat at 0 beside a label reading 75%:
      remochi's Slider reads `value` only when it mounts.
- [x] Verify vela plays audio: `alpha5-audio.txt` passes all 19 checks on
      device (2026-09-27), and its recording of hello-pyxis holds a 440 Hz
      tone lasting 1.00 s. The boot and notification sounds are recorded too,
      and Do Not Disturb is silent. *Not yet confirmed by ear* through
      `make run`, which went to PXE until 2026-09-27 (the sound card moved
      the disk's PCI slot, and the saved NVRAM buried it; emulate.sh pins it
      now with bootindex).

**F. Display settings** (audit §4.4)

- [ ] Settings → Display: resolution, scale, and multi-monitor arrangement.

**G. Window animations** *(added 2026-11-07; plan:
`docs/internal/evo-animation-plan.md`)*

- [x] Evo animation core (`evo/src/anim/`) + open / close / popup animations
      (lane F, 2026-11-07; verified in `alpha5-animations.txt`).
- [x] Bloom motion tokens shared with Evo, and Settings → Animations
      (Full / Reduced / Off) (lane M, 2026-11-07; verified in Vitest and the
      dev server; the setting reaches Evo only after the vela rebuild, which
      adds `SetAnimationSettings` to the bridge).
- [x] Minimize / restore toward the dock icon, and show-desktop (lane B,
      2026-11-07: ghosts springing into `SetMinimizeTargets` rects, interruptible,
      Super+D staggered by Evo; verified in `alpha5-animations.txt`).
- [x] Maximize / unmaximize / fullscreen / fit-to-work-area transitions.
- [x] Depth: drop shadows, rounded corners, focus crossfade, grab lift (lane D,
      2026-11-07: GLES shaders through `GlesAccess`, following every pose;
      verified in `alpha5-animations.txt`'s depth section).
- [x] Alt+Tab through the existing overview (`CardOverview` switcher mode,
      driven by Evo's `SwitcherChanged`; lane S, 2026-11-07). Evo half fully
      verified on device (`alpha5-shortcuts.txt`); Bloom's half is asserted in
      `alpha5-shortcuts-shell.txt` and **needs the vela rebuild** to be heard
      (`BRIDGE-OLD-VELA` on the current image).
- [x] Genie mesh warp for minimize and restore (lane G2, 2026-11-08: a
      textured mesh through our own GL program, driven by the minimize spring;
      falls back to genie-lite; verified in `alpha5-animations.txt`'s genie
      section). **Superseded 2026-11-08 by Lumen's Droplet** (the user found the
      funnel too macOS); its GL plumbing is what Lumen draws through.
- [x] Lumen: Droplet and Rise (lane L1, 2026-11-08: minimize rounds off into
      an orb of the app's accent and lands in its dock icon, restore rises out
      of it; the shared Lumen program in `anim/lumen.rs`, `MinimizeLanded`;
      the genie funnel retired; verified on device in `alpha5-animations.txt`'s
      Droplet section, with `alpha5-windows` and `alpha5-shortcuts` green).
- [x] Lumen: Bloom (lane L3, 2026-11-08: the open comes into focus: the live
      window blurred 10 px -> 0, tinted toward its accent, corners 42 -> 16,
      over a radial glow, growing from 0.90 on a spring and rising 16 px; a
      texture shader on the live surface beside lane D's; the plain open with
      Lumen off; verified in `alpha5-animations.txt`'s bloom section, no
      measurable fps cost for the blur).
- [x] Lumen: Evaporate (lane L2, 2026-11-08: the close snapshot burns away
      from its close button in a noisy front with a glowing accent rim,
      swelling to 1.04 and going soft; its own program in
      `anim/evaporate.rs`, the plain close with Lumen off; a close mid-open
      starts from the open's pose; verified on device in
      `alpha5-animations.txt`'s Evaporate section, the whole suite 143/143 and
      `alpha5-windows` green; fps with three windows closing 38-41 against
      41-46 plain).
- [x] Lumen: Recede and glint (lane L4, 2026-11-08: show desktop recedes each
      window through the Lumen program, front-most first, 45 ms apart, and a
      second Super+D emerges exactly that set, back-most first; the Settle
      glint crosses a window as maximize/unmaximize/fit end; verified in
      `alpha5-animations.txt`'s show-desktop and glint sections, fps with three
      windows receding 45/37/38 Lumen against 50/48/45 plain).
- [x] Lumen: Bloom ripple, accent, wallpaper (lane L4, 2026-11-08: the dock
      icon rings and dips on `MinimizeLanded`, minimize targets carry each
      app's accent, the wallpaper eases in while the desktop is shown;
      verified in Vitest and the dev server; the ripple **needs the vela
      rebuild** on device).
- [x] Overview polish: live window thumbnails (`evo.GetWindowThumbnails`, JPEG
      from `snapshot_window`, cached per commit) and transitions matched to
      Evo's: cards rise from their windows' frames, the chosen one zooms onto
      its window, a cancel returns them (2026-11-07; Evo half verified in
      `alpha5-thumbnails.txt`, Bloom's half in the dev server; **needs the vela
      rebuild** for the bridge entry).
- [x] `alpha5-animations.txt`, and all suites green with animations on. Swept
  2026-11-08 on the 20:04 image with no `--push`: 24 suites, all green. The
  sweep before it (16:24 image) found three bugs, fixed in the 20:04 image: an app
  closed during start-up four times could never launch again (Gecko's
  automatic safe mode; `toolkit.startup.max_resumed_crashes` -1,
  `alpha5-startup-lockout`); windows closed mid-restore kept their animation
  active (`alpha5-close-mid-restore`); and echo-daemon and Evo's broker read
  frames inside `select!`, which is not cancel-safe, so a signal arriving
  mid-frame cost Evo its Echo connection.

- [x] Daylight wallpaper and live settings (2026-11-09, four lanes; plan
  `docs/internal/pyxis-wallpaper-settings-plan.md`): a flowing gradient that
  follows the sun (WebGL at 10 fps on the VM, +5.0 points of one core idle,
  paused under a maximized app); Echo's settings validated, written atomically
  and signalled (`SettingChanged`), so a change in Settings reaches the shell
  live across origins; `org.geckoOS.time` (time zone, clock, location);
  Settings' Appearance, Date & Time and About pages. Verified in
  `alpha5-wallpaper.txt` and `alpha5-settings-live.txt`; all 26 suites green
  on the 09:01 image. The sweep found Bloom's window store losing a freshly
  opened window to a stale list (fixed; push the shell until the next image).

**Pulled forward from alpha 6:** unprivileged user namespaces
(`CONFIG_USER_NS`), in the same kernel rebuild as sound. vela's content sandbox
has run degraded without them. Verified on device: the namespace works, and
apps still launch and render under the sandbox.

### Exit criteria

- Lock, unlock, shut down, and reboot all work from the UI.
- An app-raised notification appears as a toast and lands in the centre.
- Audio plays from a web page in vela; volume control works.
- Wallpaper survives a Bloom restart.
- Every control in quick-settings and Settings does something real.
- Windows open, close, minimize, restore and maximize with animation, and
  Alt+Tab walks windows in the overview; Reduced and Off modes are honoured.

---

## Alpha 6 — "Trustworthy and durable"

**Theme:** the security model, resource behaviour, and robustness that separate
a demo from something you'd leave running.

### Scope

**A. Permission model made visible** (audit §4.6)

- [ ] Install-time permission prompt in the Installer, driven by Nova's
      declared-permission list (`nova.rs:194-232`).
- [ ] Settings → Apps: per-app permission view with revoke.
- [x] Enforce the capability allow-list at the bridge boundary. **Already
      done and confirmed on device 2026-09-21** — this was listed as
      outstanding for months while `PyxisBridgeParent._capabilityFor` was
      already refusing ungranted calls with
      `Pyxis capability denied: app '<id>' did not declare '<cap>'`. Found
      by accident: a probe dropped into the `files` app could not write a
      file, because its `manifest.webapp` declares only `filesystem:read`.
      **Still open**, and the real content of this item: Echo's *socket*
      clients bypass it entirely (`echo-cli` can call anything), so
      enforcement is per-bridge, not per-bus the way webOS's LS2 role files
      were.
- [x] Confine each app's filesystem calls to its own data (2026-10-06).
      echo-daemon is root, and its filesystem service took any path, so
      `filesystem:write`, which any .ipk may hold, was root over the whole
      disk. The bridge now stamps a scope on each call, and echo resolves
      the call's paths beneath it with `openat2(RESOLVE_BENEATH)` as
      geckouser. Apps get `/var/lib/pyxis/data/<id>` (deleted with the
      app); Files gets the home and a read-only view of `/`; Bloom gets the
      PIM apps' data for desktop search. Verified on device by
      `scripts/serial-tests/alpha5-fs-confine.txt`. See CLAUDE.md.
- [ ] "Privileged access": a sudo-backed unlock that lets an app change
      files outside the home for a few minutes, with the prompt drawn by
      the shell. Plan: `docs/internal/pyxis-privileged-access-plan.md`.

**B. Package integrity**

- [ ] Real signature verification for `nova.Install` (sha256 is an integrity
      check, not an authenticity one).
- [ ] Fix or remove `appmanager.rs:102`'s unverified tarball extraction. It's
      superseded by Nova and should probably just go.

**C. Resource isolation**

- [ ] Wire Puppis's `suspend_app`/`resume_app` freeze/thaw behind Nova/AppHost
      so suspension reaches Gecko's *content* processes, not just the parent
      (audit §3.8).
- [ ] cgroup limits for spawned apps.
- [ ] Drive Gecko's own background-tab throttling from Evo visibility events.

**D. Shift: real or gone** (audit §3.7)

- [ ] Wire `shift/src/echo_ipc.rs`'s handlers to the real implementations in
      `process.rs`/`resources.rs`/`tree.rs`, **or** stop registering the
      interface. Fabricated process data on the bus is worse than no data.

**E. Robustness**

- [ ] Shell restart without losing session: Bloom crash → respawn → dock
      repopulates from live AppHost state.
- [ ] Compositor crash recovery, or at minimum a clean, diagnosable failure.
- [ ] App crash containment — verify a crashing app never takes the shell with
      it.

**F. Cleanup**

- [ ] Rebrand Firefox → Vela (`browser/branding/vela/` + `--with-branding`).
      Permitted and required under MPL + the Firefox trademark policy. Needs a
      rebuild, so schedule it with another Gecko rebuild rather than alone.
- [ ] Resolve Rockhopper's role (audit §5.2) — define it or stop starting it.
- [ ] Archive `pyxis-evo/`.
- [ ] Replace `pyxis-experience/README.md` (still the stock Vite template).
- [ ] Migrate Terminal to `@xterm/xterm` / `@xterm/addon-fit` (audit §5.3).
- [ ] Repurpose `gecko/src/gecko.rs` as launcher/supervisor; delete the
      obsolete libxul-embedding scaffolding.

### Exit criteria

- Installing an app shows what it can access; denying a permission is honoured
  at the bridge.
- Killing Bloom and letting it respawn restores a correct dock.
- Killing an app leaves the shell and compositor healthy.
- A suspended app's content processes are actually stopped.
- No component on the bus returns fabricated data.

---

## Pre-beta gate

Ship pre-beta when alpha 6's exit criteria hold **and**:

- [ ] A clean install boots to a usable desktop with no manual steps.
- [ ] One hour of ordinary use — browse, edit a note, manage files, install an
      app, lock, unlock — produces no crash and no dead end.
- [ ] `PROJECT_STATUS.md` has zero 📋 items in the "confirmed broken" category.
- [ ] Rust tests + the new UI suite green; lint clean both sides.
- [ ] `CLAUDE.md` / `PROJECT_STATUS.md` accurate as of the release commit
      (verified, not assumed — that's what this audit had to reconstruct).
- [ ] An external tester who has never used Pyxis completes the alpha 4
      acceptance flow unaided.

---

## Cross-cutting: keep the docs honest

The largest avoidable cost this audit surfaced wasn't a bug — it was that
`CLAUDE.md` carried eight claims that stopped being true. Each is a future
session's wasted hour.

Two cheap habits fix it permanently:

1. **Fix an item → delete its doc entry in the same commit.** Not "mark
   resolved." Delete. `PROJECT_STATUS.md`'s change log is the historical
   record; the status board should only ever describe *now*.
2. **A release isn't cut until the status board matches the code.** Make it a
   checklist line in the release process, alongside the tests.

---

## At-a-glance

| Release | Theme | Headline outcome |
|---|---|---|
| **Alpha 3** | Nothing lies to the user | opkg present, clock ticks, Files lists, network truthful, decoration model decided |
| **Alpha 4** | All my apps working | Dynamic Nova-driven registry; install an `.ipk` and launch it from the desktop |
| **Alpha 5** | A desktop, not a shell | Lock screen, power menu, notifications, audio, layer-shell wallpaper, resize |
| **Alpha 6** | Trustworthy and durable | Permission UI + enforcement, package signing, cgroups, crash containment |
| **Pre-beta** | — | An outsider can use it for an hour and not hit a wall |
