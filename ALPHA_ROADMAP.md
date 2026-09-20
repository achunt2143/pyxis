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

**A. Dynamic app registry** (audit §2.1) — *the headline*

- [ ] Add `listApps()` / `getAppInfo(id)` to `@pyxis/app-sdk`'s `useEcho`,
      calling `org.geckoOS.nova` `List`/`GetInfo`. The mock path already
      handles both (`EchoClient.ts:689-698`) — extend it to return the same
      shape as the real service.
- [ ] Add a `useApps()` hook: Nova's list as the source of truth, refreshed on
      Nova's install/uninstall signals, with `PYXIS_APPS` demoted to a
      *presentation overlay* (Lucide icon + pin state + default size for
      built-ins only).
- [ ] Extend `PyxisAppDef` with an `iconUrl` so third-party apps can supply
      `pyxis://<id>/icon.png`; keep `LucideIcon` as the built-in fallback and
      a generic glyph as the last resort.
- [ ] Repoint `LeftLauncher`, `AppDrawer`, `DesktopSearch`, `BottomDock` at
      `useApps()`.
- [ ] Add `installer` to the registry so the App Installer is reachable from
      the desktop. *(It is currently built, packaged, staged, and invisible.)*
- [ ] Change `registry.ts`'s `entryHtml` to `pyxis://<id>/index.html`, keeping
      the `localhost:518x` ports behind a dev-only branch.

**B. Installer end-to-end** (audit §1.1)

- [ ] Build a `.ipk` with `pyxis-sdk`, drop it in `/var/lib/pyxis/incoming`,
      install it from the Installer app, and launch it from the launcher.
      This single flow is the alpha 4 acceptance test.
- [ ] Verify the uninstall split holds: `installer.Remove` → `opkg remove` →
      Nova refresh; `nova.Uninstall` correctly refuses opkg-owned apps.

**C. Fix the two carried-over window bugs**

- [ ] **#10 restore-from-dock.** Discriminator first (audit §6): does a failed
      dock click produce the "Couldn't reach …" snackbar? If yes → the IPC
      chain is fine and it's identity resolution; check `appHost.ListRunning`
      for **two sessions per app** (the `bind_surface` synth path,
      `apphost.rs:724-746`), which means vela's Wayland `app_id` isn't matching
      Nova's. If no snackbar → the click never reaches `handleOrphanSelect`;
      look at the dock's orphan branch, not the bus.
- [ ] **#7 maximize→unmaximize crash.** Re-diagnose *after* alpha 3's
      decoration decision lands. Capture the existing `info!` diagnostics
      (`ssd.rs::clicked`, `ssd.rs::handle_maximize`, `xdg.rs::maximize_request`
      / `unmaximize_request`) around a repro before changing any code. Answer
      first: does Evo itself die (all windows go) or just the client?

**D. Bridge contract tests** (audit §5.4)

- [ ] Add Vitest to `pyxis-experience`. First suite: assert `EchoClient`'s mock
      responses match the real services' JSON shapes, generated from or checked
      against the Rust side. This is the test that would have caught the Files
      class of bug at the desk instead of on device.

### Exit criteria

- A third-party `.ipk`, built with `pyxis-sdk` and installed through the
  Installer app, appears in the launcher with its own icon and launches.
- Uninstalling it removes it from the launcher.
- All 8 built-in apps launch, focus from the dock, minimize, restore, and close.
- Maximize→unmaximize does not crash.
- Vitest suite green in CI.

---

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
