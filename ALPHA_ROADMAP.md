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
- [ ] **Needs a human at the screen:** confirm the app *appears* in the
      launcher and launches from a click. Headless screenshots are
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

### Exit criteria

Status as of 2026-09-20, after a rebuild and a full on-device pass.

- [x] **A third-party `.ipk`, built with `pyxis-sdk` and installed through
      the Installer app, appears in the launcher and launches.** Confirmed
      by the user on a real desktop, and by
      `scripts/serial-tests/alpha4-ipk-e2e.txt` (33 assertions) headlessly.
      *Caveat on "with its own icon":* the acceptance package shipped a 1×1
      transparent placeholder from `pyxis-sdk new`, which loads without
      error and draws as nothing — so the icon path could not be told apart
      from the generic fallback. It now carries a real 128×128 icon, but
      that has not been looked at on screen yet.
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
- [x] **Test suite green.** Bloom: 17 vitest, `tsc -b` and eslint clean.
      Rust: 75. On-device: `alpha4-ipk-e2e.txt` 33/33,
      `system-service.txt` and `alpha4-exit-criteria.txt` green at 4 GB.

## Alpha 5 — "A desktop, not a shell"

**Theme:** the system-level surfaces a desktop is expected to have.

### Scope

**A. Layer-shell compositing** (audit §4.5) — *unblocks the rest of this alpha*

- [ ] Render `wlr-layer-shell` layers in Evo's `output_elements`. Layers are
      already accepted, just never composited.
- [ ] Move the wallpaper to a real background-layer client, so it sits below
      Bloom rather than inside it. Enables shell transparency, "show desktop",
      and a wallpaper that survives a shell restart.

**B. Session control** (audit §4.1)

- [ ] Power menu in quick-settings: Shut down / Restart / Lock / Log out,
      calling `org.geckoOS.power` (fixed in alpha 3).
- [ ] Lock screen as an `overlay`-layer client — input-exclusive, unlocked by
      a credential check against the geckouser account.
- [ ] A confirm-before-shutdown flow that gives running apps a chance to close.

**C. Notifications** (audit §3.6, §4.2)

- [ ] Rewrite `notification.rs` as Echo-native: store the notification, emit an
      Echo signal. Drop the `notify-send`/`dbus-send` shell-outs entirely —
      there is no notification daemon or session bus in this image, and there
      shouldn't need to be.
- [ ] Notification centre in Bloom, on the Bell button (which currently opens
      quick-settings like every other icon). Toast + history + dismiss.
- [ ] Expose `notification.Show` through `@pyxis/app-sdk` so apps can use it.

**D. Window management completion**

- [ ] Resize, scoped per alpha 3's decoration decision. If GTK owns app
      decoration this is mostly already handled and the work is confined to
      non-GTK/XWayland clients.
- [ ] Real values in Evo's `GetRenderingStatus` IPC method — the signal path
      (`record_frame`, `current_process_rss_mb`) is already real, only the
      method returns hardcoded fps/memory.
- [ ] HiDPI scale read from the render surface (`udev.rs:1460`).

**E. Audio** (audit §1.4)

- [ ] Decide the stack (PipeWire is the current-generation answer; ALSA-direct
      is the minimal one) and add it to the rootfs.
- [ ] Volume control in quick-settings; output device selection in Settings.
- [ ] Verify vela plays audio.

**F. Display settings** (audit §4.4)

- [ ] Settings → Display: resolution, scale, and multi-monitor arrangement.

### Exit criteria

- Lock, unlock, shut down, and reboot all work from the UI.
- An app-raised notification appears as a toast and lands in the centre.
- Audio plays from a web page in vela; volume control works.
- Wallpaper survives a Bloom restart.
- Every control in quick-settings and Settings does something real.

---

## Alpha 6 — "Trustworthy and durable"

**Theme:** the security model, resource behaviour, and robustness that separate
a demo from something you'd leave running.

### Scope

**A. Permission model made visible** (audit §4.6)

- [ ] Install-time permission prompt in the Installer, driven by Nova's
      declared-permission list (`nova.rs:194-232`).
- [ ] Settings → Apps: per-app permission view with revoke.
- [ ] Enforce the capability allow-list at the bridge boundary — the
      `JSWindowActor` should refuse an ungranted service call, not just filter
      the API surface.

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
