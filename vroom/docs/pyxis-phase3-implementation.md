# Phase 3 Implementation Notes

Status: Implemented 2026-07-25

Phase 3 of `pyxis-sdk-pdk-spec.md` §22 ("launcher registry daemon and
`pyxis-appctl`, permissions model, service packaging helpers, upgrade-aware
maintainer script templates") is implemented. The spec was drafted before the
project's architecture direction was settled, so two of its assumptions were
deliberately adapted to fit the now-defined direction (one bus, one registry,
compositor-first). This file records what shipped and where it diverges.

## 1. Registry "daemon" = Nova, not a new process

The spec's §10/§22 "launcher registry daemon" is **not** a standalone daemon.
The registry role was already owned by Nova (`echo/src/services/nova.rs`,
in-process in echo-daemon) — duplicating it would have created the second
registry the architecture explicitly forbids. Instead:

- Nova's `reload()` now scans **three** roots, later wins on id collision:
  1. `/usr/share/geckoOS/apps` — system `manifest.webapp` apps
  2. `/usr/pyxis/applications` — **opkg/ipk-installed `appinfo.json` apps** (new)
  3. `/var/lib/pyxis/apps` — Nova-installed `manifest.webapp` apps
- `appinfo.json` parsing follows the launcher contract (spec §10): validate
  before indexing (id↔dir match, web `main` relative/no traversal, native
  `main` absolute, §19 permission vocabulary), skip+log invalid manifests.
- Both runtimes are supported:
  - **web**: the app dir itself is the webroot (ipk stages the payload flat,
    §14.5); the app gets a `pyxis://<id>` origin via Nova's origins map and
    launches through the normal setpriv→vela composition.
  - **native**: Nova composes setpriv→`main` (+ `entryArgs`) directly — no
    vela, no Gecko profile — with the same Wayland session env and
    `PYXIS_APP_ID` stamp so Evo's window-claim contract binds native clients
    identically.
- Derived `manifest.json` files for ipk apps are written to the Nova-owned
  root `/var/lib/pyxis/nova/native/<id>/` (AppHost's third scan root), never
  into `/usr/pyxis/applications` — that tree is opkg-owned, and a web payload
  may carry its own PWA `manifest.json`. Stale entries are garbage-collected
  on every reload.
- `nova.Uninstall` refuses ipk apps with a pointer to `opkg remove` — Nova
  must not delete files behind opkg's status database.

## 2. `pyxis-appctl` — thin Echo client (`echo/src/bin/appctl.rs`)

The device-side CLI the generated maintainer scripts call. Maps directly
onto the existing `org.geckoOS.nova` interface:

| command | Nova method |
|---|---|
| `pyxis-appctl rescan` | `Reload` |
| `pyxis-appctl list` / `info <id>` | `List` / `GetInfo` |
| `pyxis-appctl launch <id> [--params json]` | `Launch` (single-instance aware) |
| `pyxis-appctl close <id>` / `focus <id>` | `Close` / `Focus` |
| `pyxis-appctl uninstall <id> [--purge]` | `Uninstall` (Nova-managed apps only) |

Fails fast with nonzero exit when echo-daemon is unreachable — maintainer
scripts call it under `|| true`, so it must never hang an `opkg install`.
Staged into the image by `create-rootfs.sh` alongside `pyxis`.

## 3. Permissions model

Two vocabularies exist by design, with an explicit mapping at the boundary:

- **Declaration** (`appinfo.json` `requiredPermissions[]`): the spec §19 dot
  vocabulary (`network`, `camera`, `filesystem.read`, …) — validated by both
  the toolchain (`vroom` at package time) and Nova (at index time). Vroom's
  `config::PERMISSIONS`, `schemas/*.json`, and Nova's `APPINFO_PERMISSIONS`
  are kept in lockstep.
- **Enforcement** (bridge capabilities): Nova's `KNOWN_CAPABILITIES` colon
  vocabulary, kept in lockstep with `PyxisBridgeParent.sys.mjs`. The mapping
  (`nova::appinfo_permission_to_capability`) currently covers
  `filesystem.read→filesystem:read`, `filesystem.write→filesystem:write`,
  `notifications→notifications`; everything else is recorded (visible in
  `GetInfo`/`registry.json` as `requiredPermissions`) but grants **nothing**
  — deny-by-default at the bridge, per §19.2 ("installation does not imply
  runtime authorization"). New capabilities get enforcement points at the
  bridge first, then a mapping entry here.

## 4. Service packaging helpers (vroom)

`pyxis.json` gains an optional `services[]` (name = reverse-DNS id, unit =
project-relative systemd unit file). The toolchain stages units to:

- `/opt/pyxis/apps/<id>/services/` for bundled-app native packages
  (enabled by absolute path — systemd supports this — keeping the payload
  self-contained for clean uninstall)
- `/usr/lib/systemd/system/` for system-bin and web packages

and lists the names in `appinfo.json` `services[]` (spec §11).

**Policy gate**: generated `postinst` only enables/starts units when
`/etc/pyxis/allow-packaged-services` exists on the image (spec §11.2:
activation is platform policy, not a package right). Images opt in by
creating that flag file.

## 5. Upgrade-aware maintainer scripts (vroom)

Generated per spec §9/§18, idempotent so install and upgrade share one path:

- `postinst`: `pyxis-appctl rescan`, icon cache refresh, policy-gated
  `systemctl enable --now` for shipped units
- `prerm`: `pyxis-appctl close <id>` (stop the running app before its files
  vanish — also the upgrade path), `systemctl disable --now` shipped units
- `postrm`: remove `/var/cache/pyxis/<id>` only (§18.2 — never user
  data/profiles), `daemon-reload`, final `rescan` so the launcher entry
  disappears

Developer overrides: any of `packaging/{preinst,postinst,prerm,postrm}` in
the project replaces the generated script verbatim (`preinst` is only
emitted when provided). Scripts must start with `#!/bin/sh` (§9.1) —
enforced at package time.

## 6. Validated against the real first-party apps

Phase 3 was exercised end-to-end by packaging the existing PIM apps
(`pyxis-experience/packages/apps/{notes,contacts,tasks}`) as `.ipk`. Each
gained a `pyxis.json` + generated `assets/icon.png`; `pyxis build` drove
their real `tsc -b && vite build`, and `pyxis package` produced installable
packages (~230 KB each). Three things this surfaced:

1. **App ids are not always reverse-DNS.** The first-party apps use single
   segment ids (`notes`), and those ids are load-bearing platform identity —
   the `pyxis://<id>` origin authority, the profile dir name, and the
   `PYXIS_APP_ID` Evo binds windows by. The id pattern was relaxed to accept
   both rather than renaming apps across the stack.
2. **The two permission vocabularies met in practice.** These apps declare
   `filesystem:read` (colon) in `manifest.webapp`; their `pyxis.json`
   declares `filesystem.read` (dot), and Nova translates back at index time.
   The generated manifest is pinned by a regression test
   (`appinfo_accepts_first_party_pim_app`).
3. **ipk installs deliberately shadow ROM apps.** These apps already ship in
   the image under `/usr/share/geckoOS/apps/<id>` (`manifest.webapp` +
   `webroot/`), so an ipk of the same id under `/usr/pyxis/applications/<id>`
   collides — and wins, by scan order. That makes `opkg install
   notes_0.2.0_all.ipk` a supported way to update a shipped app without
   rebuilding the image. Each root keeps its own payload layout, so the swap
   is clean.

Also fixed here: staged trees are now permission-normalized (dirs 0755,
executables 0755, data 0644). Previously the payload inherited the
developer's umask, which shipped group-writable files (0664/0775) into a
root-owned system directory.

## 7. App Installer (frontend + installer service)

Added 2026-07-25, completing the "install a package on device" story that
Phase 3 otherwise only supported from a developer host via `pyxis install`.

**Service** (`echo/src/services/installer.rs`, `org.geckoOS.installer`) owns
the package layer — and only that layer:

| method | behaviour |
|---|---|
| `ListAvailable(dir?)` | scan `/var/lib/pyxis/incoming`, `/media`, `/tmp` for `.ipk` |
| `Inspect(packagePath)` | read control + `appinfo.json` **without** installing |
| `Install(packagePath, force?)` | `opkg install`, then `Nova::reload()` |
| `Remove(package, purge?)` | `opkg remove`, then `Nova::reload()` |
| `ListInstalled()` | `opkg list-installed`, annotated with registry state |

The Nova split is deliberate and matches the guard already in place: opkg
owns the files and its status database (which is why `Nova::uninstall`
refuses ipk apps and points at `opkg remove`), Nova owns identity, origins,
and launch. Every mutation ends by refreshing Nova's registry, so a
successful install means the app is immediately launchable — `launchable`
in the response reflects whether Nova actually picked it up.

Install paths are validated before use: absolute, canonicalized (so
`/tmp/../etc/x.ipk` cannot slip through), `.ipk`-suffixed, and confined to
the same drop roots the UI offers. Installing runs maintainer scripts as
root, so a caller does not get to name an arbitrary path.

**Frontend** (`pyxis-experience/packages/apps/installer`) is an ordinary
sandboxed Remochi web app built on the `Wizard` component — Choose → Review
→ Install → Done. It never shells out; it has no way to. Its only channel
to the system is Echo. The Review step surfaces version deltas
(upgrade/downgrade/reinstall, which also decides whether opkg needs
`--force-reinstall`) and flags requested permissions that have **no
enforcement point yet**, so the UI does not imply guarantees the platform
does not make.

Two implementation notes worth keeping:

- The install is started when the Install step *mounts*, not in the
  previous step's `onNext`. Remochi's Wizard renders only the active step's
  content, so mounting is the "user arrived" signal; doing it in `onNext`
  would block the Next button on the whole install and show a frozen Review
  screen instead of live progress. A ref guards against StrictMode's
  double-invoked mount effect firing two concurrent opkg runs.
- **The awaited response is the authority; signals only narrate.** The
  `Install` response and the `InstallProgress` signals are independent
  deliveries (the service spawns each signal onto the runtime), so a late
  progress signal can land *after* the response. Without a guard that
  overwrites the terminal phase and strands any UI gated on it — which is
  exactly what happened the first time the wizard was driven end to end.
  `useInstaller` latches a terminal flag on the response and ignores
  subsequent progress signals for that run. The dev mock deliberately
  resolves only after its phases play out, so it cannot re-introduce the
  inverted ordering the UI must not be written against.

**Known gap:** the appinfo dot vocabulary has no equivalent for
`appManager:install`/`appManager:manage`, so the installer can only declare
those via `manifest.webapp` (in-image staging). An ipk-installed installer
would get no app-management capability. Whether to extend the vocabulary or
reserve app management for in-image system apps is an open policy question.

## Naming: no Palm namespace

The app root is `/usr/pyxis/applications`, not webOS's
`/usr/palm/applications`. Pyxis is not a webOS OSE fork; the structure is
borrowed, the vendor namespace is not. There are no `palm` references left
in code, schemas, or the packaging spec — only in the webOS *research* docs
(`webos-lunasysmgr-architecture.md`, `pyxis-webos-parity-analysis.md`),
where they correctly describe real webOS paths.

## Not in scope (unchanged from spec §25)

Package signatures, sandboxing implementation, store feed format, delta
updates, rollback. The bridge-boundary enforcement rollout for the wider
permission vocabulary (camera, location, …) tracks the security-model
workstream (CLAUDE.md roadmap item 6), not this phase.
