# Pyxis Packaging System Spec

Version: Draft 1
Status: Proposed

## 1. Goals

Pyxis defines a single application packaging and installation model built around `.ipk` archives and an `opkg`-compatible installer. Two developer-facing toolchains target that package format:

- **Pyxis SDK**: builds, packages, installs, and debugs web applications.
- **Pyxis PDK**: builds, packages, installs, and debugs native Linux applications.

The goal is to give developers one app model, one manifest model, one package model, and one installation story across both runtimes.

## 2. Core principles

1. **One package format**: all installable apps ship as `.ipk`.
2. **One package manager**: the platform installs packages through `opkg` or an `opkg`-compatible layer.
3. **One app manifest**: all GUI apps must contain `appinfo.json` in the app root.
4. **Two runtimes**: `web` and `native`.
5. **Separate package metadata from app metadata**: `CONTROL/control` describes the package; `appinfo.json` describes the app.
6. **Launcher discovery is manifest-driven**: the launcher indexes apps from installed `appinfo.json` files.
7. **Maintainer scripts are optional but standardized**: packages may include `preinst`, `postinst`, `prerm`, and `postrm`.

## 3. Terminology

- **SDK**: the web application toolchain.
- **PDK**: the native application toolchain.
- **Package**: an installable `.ipk` archive.
- **App root**: installed directory containing `appinfo.json`.
- **Runtime**: execution class of an app, currently `web` or `native`.
- **Launcher**: Pyxis shell component that discovers and launches apps.
- **Registry**: platform cache/index of installed applications.

## 4. Package format

Pyxis uses the standard IPK-style package structure produced by `opkg-build`-compatible tooling.

A package contains:

- a package metadata directory: `CONTROL/`
- a staged root filesystem tree containing files to install

### 4.1 Required package metadata files

- `CONTROL/control`

### 4.2 Optional package metadata files

- `CONTROL/preinst`
- `CONTROL/postinst`
- `CONTROL/prerm`
- `CONTROL/postrm`
- `CONTROL/conffiles`

## 5. Install locations

Pyxis separates application identity from executable payload.

> The app root is `/usr/pyxis/applications`. Earlier drafts of this spec used
> `/usr/palm/applications`, inherited from webOS — Pyxis is not a webOS OSE
> fork and does not use the Palm namespace. The directory *structure* (app
> metadata separate from payload) is kept; only the vendor name changed.

### 5.1 Web apps

Web apps install under:

```text
/usr/pyxis/applications/<app-id>/
```

This directory must contain `appinfo.json` and all app assets.

### 5.2 Native apps

Native apps install app metadata under:

```text
/usr/pyxis/applications/<app-id>/
```

Executable payload may be installed in one of two supported modes:

#### Mode A: system-bin mode

```text
/usr/bin/<command>
/usr/lib/<package>/...
/usr/share/<package>/...
```

Use this mode for apps that expose reusable CLI tools, services, or traditional Linux desktop binaries.

#### Mode B: bundled-app mode

```text
/opt/pyxis/apps/<app-id>/bin/<executable>
/opt/pyxis/apps/<app-id>/lib/...
/opt/pyxis/apps/<app-id>/resources/...
```

Use this mode for self-contained applications that should remain isolated from the main root filesystem.

### 5.3 Recommendation

- Use **Mode A** for system utilities and apps with shared binaries.
- Use **Mode B** for consumer-facing GUI applications to simplify uninstallation and sandboxing.

## 6. Required files for all GUI apps

Every GUI app, regardless of runtime, must ship:

```text
/usr/pyxis/applications/<app-id>/appinfo.json
/usr/pyxis/applications/<app-id>/icon.png
```

Recommended additional files:

```text
/usr/pyxis/applications/<app-id>/README.md
/usr/pyxis/applications/<app-id>/assets/...
```

## 7. appinfo.json specification

`appinfo.json` is the canonical app manifest. The launcher and registry use it to identify, display, and launch installed apps.

### 7.1 Required fields

```json
{
  "id": "com.example.app.clock",
  "version": "1.0.0",
  "vendor": "Example Corp",
  "title": "Clock",
  "type": "web",
  "main": "index.html",
  "icon": "icon.png"
}
```

Required keys:

- `id`: unique identifier, string. Reverse-DNS (`com.example.app.clock`) for
  third-party apps; Pyxis's own first-party apps use a single lowercase
  segment (`notes`, `contacts`) because for those the id is platform-fixed
  identity — it is the `pyxis://<id>` origin authority, the profile directory
  name, and the `PYXIS_APP_ID` Evo binds windows by
- `version`: semantic version string
- `vendor`: publisher/author string
- `title`: human-readable display name
- `type`: `web` or `native`
- `main`: runtime entrypoint
- `icon`: path relative to app root unless absolute for special cases

### 7.2 Optional fields

```json
{
  "largeIcon": "largeIcon.png",
  "bgImage": "launch.png",
  "splashBackground": "splash.png",
  "appDescription": "Simple clock app",
  "keywords": ["clock", "time", "alarm"],
  "uiRevision": 1,
  "category": "Utilities",
  "fullscreen": false,
  "requiredPermissions": ["network"],
  "window": {
    "width": 960,
    "height": 600,
    "resizable": true,
    "frame": true
  },
  "entryArgs": [],
  "services": [
    "com.example.app.clock.service"
  ]
}
```

### 7.3 Runtime-specific meaning of `main`

- If `type` is `web`, `main` must be a relative path inside the app root, typically `index.html`.
- If `type` is `native`, `main` must be an absolute path to an executable installed by the package, such as `/opt/pyxis/apps/com.example.app.notes/bin/notes` or `/usr/bin/pyxis-notes`.

### 7.4 Proposed JSON schema

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "Pyxis appinfo.json",
  "type": "object",
  "required": ["id", "version", "vendor", "title", "type", "main", "icon"],
  "properties": {
    "id": {
      "type": "string",
      "pattern": "^[a-z0-9][a-z0-9_-]*(\\.[a-z0-9_-]+)*$"
    },
    "version": {
      "type": "string"
    },
    "vendor": {
      "type": "string"
    },
    "title": {
      "type": "string"
    },
    "type": {
      "type": "string",
      "enum": ["web", "native"]
    },
    "main": {
      "type": "string"
    },
    "icon": {
      "type": "string"
    },
    "largeIcon": {
      "type": "string"
    },
    "bgImage": {
      "type": "string"
    },
    "splashBackground": {
      "type": "string"
    },
    "appDescription": {
      "type": "string"
    },
    "keywords": {
      "type": "array",
      "items": { "type": "string" }
    },
    "uiRevision": {
      "type": "integer",
      "minimum": 1
    },
    "category": {
      "type": "string"
    },
    "fullscreen": {
      "type": "boolean"
    },
    "requiredPermissions": {
      "type": "array",
      "items": { "type": "string" }
    },
    "window": {
      "type": "object",
      "properties": {
        "width": { "type": "integer", "minimum": 1 },
        "height": { "type": "integer", "minimum": 1 },
        "resizable": { "type": "boolean" },
        "frame": { "type": "boolean" }
      },
      "additionalProperties": false
    },
    "entryArgs": {
      "type": "array",
      "items": { "type": "string" }
    },
    "services": {
      "type": "array",
      "items": {
        "type": "string",
        "pattern": "^[a-z0-9][a-z0-9_-]*(\\.[a-z0-9_-]+)*$"
      }
    }
  },
  "additionalProperties": false
}
```

## 8. Package metadata: CONTROL/control

`CONTROL/control` describes the package to the package manager.

### 8.1 Required fields

```control
Package: com.example.app.clock
Version: 1.0.0
Architecture: all
Section: applications
Priority: optional
Maintainer: Example Corp <dev@example.com>
Description: Clock application for Pyxis
```

### 8.2 Required semantics

- `Package` must equal `appinfo.json.id`
- `Version` must equal `appinfo.json.version`
- `Architecture` must be:
  - `all` for web apps
  - explicit target architecture for native apps, such as `x86_64`, `aarch64`, or `armv7`
- `Section` should default to `applications`
- `Priority` should default to `optional`

### 8.3 Optional fields

- `Depends`
- `Provides`
- `Conflicts`
- `Replaces`
- `Source`
- `Homepage`
- `License`

## 9. Maintainer scripts

Pyxis supports the standard maintainer script set:

- `preinst`
- `postinst`
- `prerm`
- `postrm`

### 9.1 Rules

- Scripts must be executable.
- Scripts must begin with `#!/bin/sh`.
- Scripts should use `set -e`.
- Scripts must be idempotent.
- Scripts must exit `0` on success.

### 9.2 Recommended behavior

- `postinst`: refresh launcher registry, icon cache, MIME database, or service activation state.
- `prerm`: stop services and unregister temporary runtime state before removal.
- `postrm`: remove caches owned only by the package.

### 9.3 Default generated `postinst`

```sh
#!/bin/sh
set -e
if command -v pyxis-appctl >/dev/null 2>&1; then
  pyxis-appctl rescan || true
fi
if command -v pyxis-cachectl >/dev/null 2>&1; then
  pyxis-cachectl refresh-icons || true
fi
exit 0
```

## 10. Launcher and registry contract

The Pyxis shell must follow these rules:

1. Scan installed applications from `/usr/pyxis/applications/*/appinfo.json`.
2. Validate each manifest before indexing.
3. Ignore invalid manifests but log them.
4. Cache a registry database for fast lookup.
5. Reload the registry when:
   - the shell starts
   - `pyxis-appctl rescan` is called
   - install/remove operations complete

### 10.1 Launch behavior

- If `type=web`, launch the system web runtime against the installed app root and `main`.
- If `type=native`, execute the binary given by `main` with any `entryArgs`.

## 11. Services model

Apps may ship companion services.

### 11.1 Web service helpers

A web app may install service helper binaries or scripts outside its app root and reference them in the `services` array.

### 11.2 Native services

A native package may install system services under:

```text
/usr/lib/systemd/system/
/usr/libexec/
/opt/pyxis/apps/<app-id>/services/
```

The package should use maintainer scripts to enable or reload services only if Pyxis policy permits packaged service activation.

## 12. Project configuration file

Each app project should define a top-level project file for the toolchain.

Filename:

```text
pyxis.json
```

### 12.1 Web example

```json
{
  "id": "com.example.app.clock",
  "title": "Clock",
  "vendor": "Example Corp",
  "version": "1.0.0",
  "runtime": "web",
  "sourceDir": "src",
  "publicDir": "public",
  "buildDir": "build",
  "entry": "index.html",
  "icon": "assets/icon.png",
  "sdk": {
    "builder": "vite"
  },
  "package": {
    "section": "applications",
    "priority": "optional",
    "architecture": "all"
  }
}
```

### 12.2 Native example

```json
{
  "id": "com.example.app.notes",
  "title": "Notes",
  "vendor": "Example Corp",
  "version": "1.0.0",
  "runtime": "native",
  "sourceDir": "src",
  "buildDir": "build",
  "icon": "assets/icon.png",
  "pdk": {
    "builder": "cmake",
    "binaryName": "pyxis-notes",
    "installMode": "bundled-app"
  },
  "package": {
    "section": "applications",
    "priority": "optional",
    "architecture": "aarch64",
    "depends": ["libc6", "libstdcpp"]
  }
}
```

## 13. SDK and PDK CLI model

Provide one umbrella CLI and two specialized frontends.

### 13.1 Umbrella CLI

```bash
pyxis new
pyxis build
pyxis package
pyxis install
pyxis run
pyxis doctor
pyxis lint
pyxis emulator
```

### 13.2 Specialized CLIs

```bash
pyxis-sdk new web
pyxis-sdk build
pyxis-sdk package
pyxis-sdk install
pyxis-sdk serve
pyxis-sdk inspect

pyxis-pdk new native
pyxis-pdk build
pyxis-pdk package
pyxis-pdk install
pyxis-pdk gdb
pyxis-pdk symbols
```

### 13.3 Behavior

- `pyxis build` dispatches based on `pyxis.json.runtime`
- `pyxis package` always emits `.ipk`
- `pyxis install` copies the package to a target and runs `opkg install`

## 14. SDK: fully defined web app system

### 14.1 Purpose

The SDK targets HTML/CSS/JavaScript/TypeScript applications rendered in a Pyxis-managed web runtime.

### 14.2 Supported builders

- Vite
- esbuild
- webpack
- static/no-build

### 14.3 Source layout

```text
my-web-app/
├── pyxis.json
├── package.json
├── src/
├── public/
└── assets/
```

### 14.4 Build contract

The SDK must:

1. Read `pyxis.json`
2. Run the configured web builder
3. Produce a static output directory
4. Generate `appinfo.json`
5. Stage an IPK filesystem tree
6. Generate `CONTROL/control`
7. Optionally generate default `postinst`
8. Build the `.ipk`

### 14.5 Generated package tree

```text
dist/pkg/
├── CONTROL/
│   ├── control
│   └── postinst
└── usr/
    └── pyxis/
        └── applications/
            └── com.example.app.clock/
                ├── appinfo.json
                ├── index.html
                ├── assets/...
                └── icon.png
```

### 14.6 Runtime rules

- Web apps must not rely on a writable app root.
- App data should go to per-user data directories defined by the runtime.
- `main` must be present and resolve inside the installed app root.
- `architecture` is always `all`.

### 14.7 Generated files

#### appinfo.json

```json
{
  "id": "com.example.app.clock",
  "version": "1.0.0",
  "vendor": "Example Corp",
  "title": "Clock",
  "type": "web",
  "main": "index.html",
  "icon": "icon.png",
  "uiRevision": 1
}
```

#### control

```control
Package: com.example.app.clock
Version: 1.0.0
Architecture: all
Section: applications
Priority: optional
Maintainer: Example Corp <dev@example.com>
Description: Clock application for Pyxis
```

### 14.8 Web install workflow

```bash
pyxis-sdk build
pyxis-sdk package
scp dist/com.example.app.clock_1.0.0_all.ipk device:/tmp/
ssh device 'opkg install /tmp/com.example.app.clock_1.0.0_all.ipk'
```

## 15. PDK: fully defined native app system

### 15.1 Purpose

The PDK targets compiled Linux applications such as C, C++, Rust, Go, or other native binaries.

### 15.2 Supported builders

- CMake
- Meson
- qmake
- Cargo
- Make

### 15.3 Source layout

```text
my-native-app/
├── pyxis.json
├── CMakeLists.txt
├── src/
├── assets/
└── packaging/
```

### 15.4 Build contract

The PDK must:

1. Read `pyxis.json`
2. Configure the native build toolchain
3. Build the executable payload
4. Collect runtime libraries if bundling is enabled
5. Generate `appinfo.json`
6. Stage the install tree
7. Generate `CONTROL/control`
8. Optionally generate maintainer scripts
9. Build the `.ipk`

### 15.5 Native install modes

#### bundled-app mode

```text
dist/pkg/
├── CONTROL/
│   ├── control
│   └── postinst
├── opt/
│   └── pyxis/
│       └── apps/
│           └── com.example.app.notes/
│               ├── bin/notes
│               ├── lib/...
│               └── resources/...
└── usr/
    └── pyxis/
        └── applications/
            └── com.example.app.notes/
                ├── appinfo.json
                └── icon.png
```

#### system-bin mode

```text
dist/pkg/
├── CONTROL/
│   ├── control
│   └── postinst
├── usr/
│   ├── bin/
│   │   └── pyxis-notes
│   ├── lib/
│   │   └── pyxis-notes/...
│   └── pyxis/
│       └── applications/
│           └── com.example.app.notes/
│               ├── appinfo.json
│               └── icon.png
```

### 15.6 Native runtime rules

- `main` must be an absolute executable path.
- `architecture` must not be `all`.
- Declared dependencies should reflect runtime libraries unless the app is fully bundled.
- If the app installs a desktop file or services, maintainer scripts may refresh shell caches.

### 15.7 Generated files

#### appinfo.json

```json
{
  "id": "com.example.app.notes",
  "version": "1.0.0",
  "vendor": "Example Corp",
  "title": "Notes",
  "type": "native",
  "main": "/opt/pyxis/apps/com.example.app.notes/bin/notes",
  "icon": "icon.png",
  "uiRevision": 1
}
```

#### control

```control
Package: com.example.app.notes
Version: 1.0.0
Architecture: aarch64
Section: applications
Priority: optional
Maintainer: Example Corp <dev@example.com>
Depends: libc6, libstdcpp
Description: Native Notes application for Pyxis
```

### 15.8 Native install workflow

```bash
pyxis-pdk build
pyxis-pdk package
scp dist/com.example.app.notes_1.0.0_aarch64.ipk device:/tmp/
ssh device 'opkg install /tmp/com.example.app.notes_1.0.0_aarch64.ipk'
```

## 16. Unified packaging pipeline

Both SDKs must converge on the same packaging API.

### 16.1 Internal stages

1. Validate project config
2. Validate identifier and version
3. Build payload
4. Generate app manifest
5. Generate package metadata
6. Stage filesystem tree
7. Generate maintainer scripts
8. Invoke `opkg-build`
9. Emit artifact metadata

### 16.2 Internal data model

```json
{
  "id": "com.example.app.notes",
  "version": "1.0.0",
  "runtime": "native",
  "architecture": "aarch64",
  "appRoot": "/usr/pyxis/applications/com.example.app.notes",
  "entrypoint": "/opt/pyxis/apps/com.example.app.notes/bin/notes",
  "packageFile": "dist/com.example.app.notes_1.0.0_aarch64.ipk"
}
```

## 17. Device install protocol

The CLIs should support direct installation to a target device.

### 17.1 Device profiles

Store profiles in developer host config:

```text
~/.config/pyxis/devices.json
```

Example:

```json
{
  "default": "living-room-box",
  "devices": {
    "living-room-box": {
      "host": "192.168.1.50",
      "user": "root",
      "port": 22,
      "installPath": "/tmp"
    }
  }
}
```

### 17.2 Install sequence

1. copy `.ipk` to target
2. run `opkg install`
3. run `pyxis-appctl rescan`
4. optionally launch the app

## 18. Upgrade and removal semantics

### 18.1 Upgrade

An upgrade is a package install with a higher version for the same `Package` ID.

Rules:

- `Package` identity is immutable.
- `id` must never change for an in-place upgrade path.
- `postinst` must tolerate upgrades.
- registry refresh is required after upgrades.

### 18.2 Removal

Rules:

- `prerm` stops any running services.
- `postrm` removes package-owned caches only.
- launcher entries are removed after the next registry refresh.

## 19. Permissions and trust model

Pyxis should define permissions at the app layer even if package installation is root-managed.

### 19.1 Permission names

The packaging vocabulary (`pyxis.json` `permissions[]`, copied verbatim into
`appinfo.json` `requiredPermissions[]`) uses dotted names:

| Permission | Bridge capability | Privileged |
| --- | --- | --- |
| `network` | `network` | no |
| `notifications` | `notifications` | no |
| `camera` | — | no |
| `microphone` | — | no |
| `location` | — | no |
| `bluetooth` | — | no |
| `filesystem.read` | `filesystem:read` | no |
| `filesystem.write` | `filesystem:write` | no |
| `ipc` | — | no |
| `background` | — | no |
| `power` | `power` | no |
| `audio` | `audio` | no |
| `browser` | `browser` | no |
| `notifications.manage` | `notifications:manage` | **yes** |
| `appmanager.install` | `appManager:install` | **yes** |
| `appmanager.manage` | `appManager:manage` | **yes** |
| `settings.write` | `settings:write` | **yes** |
| `windows.manage` | `windows:manage` | **yes** |
| `terminal` | `terminal` | **yes** |
| `filesystem.user` | `filesystem:user` | **yes** |
| `filesystem.system` | `filesystem:system` | **yes** |
| `filesystem.apps` | `filesystem:apps` | **yes** |

Two vocabularies exist and must stay in correspondence. The dotted names above
are the *packaging* vocabulary. The runtime bridge uses colon-separated
*capability* names (`filesystem:read`, `appManager:install`, …), which is also
what an in-image app's `manifest.webapp` `permissions[]` declares directly.
Nova translates dotted → colon at registry load
(`appinfo_permission_to_capability`).

Entries with no bridge capability are recorded but grant nothing today; they
are declarations of intent awaiting an enforcement point. Every colon
capability must be reachable from some dotted permission, so that an
ipk-packaged app is never second-class next to an in-image one — this is
enforced by a test in `echo/src/services/nova.rs`.

What each capability covers is decided by the bridge
(`PyxisBridgeParent.sys.mjs`'s `BRIDGE_CAPABILITIES`), which lists every
(service, method) a `pyxis://` app can reach and the capability it needs:

| Capability | Covers |
| --- | --- |
| `filesystem:read` / `filesystem:write` | `filesystem.*` reads / writes, in the app's own data (see *Where filesystem calls reach*, below) |
| `power` | shutdown, reboot, sleep, wake, screen brightness and timeout |
| `notifications` | raising a notification (`notification.show`) |
| `audio` | `audio.*` (volume, mute, outputs) and `sound.*` (system sounds) |
| `network` | `network.*`, whose status includes the Wi-Fi SSID |
| `browser` | opening an http(s) URL in a browser window (`nova.OpenExternalBrowser`) |
| `notifications:manage` | reading and dismissing every app's notifications |
| `appManager:manage` | launching, closing, raising and listing other apps (`nova`, `appHost`, legacy `appManager`) |
| `appManager:install` | installing and removing apps (`installer.*`, `nova.Install`/`Uninstall`/`Reload`) |
| `settings:write` | `settings.set`/`delete`/`clear`/`reset` |
| `windows:manage` | Evo's control of any app's windows, and the shell overlay's input region |
| `terminal` | the terminal service, a root shell |
| `filesystem:user` | `filesystem.*` reaching the user's home instead of the app's own data |
| `filesystem:system` | `filesystem.*` reads of the whole root filesystem, by absolute path |
| `filesystem:apps` | `filesystem.*` reaching another app's own data, named by the call's `app` |

A few calls need no capability: `system.getInfo`/`ping`/`listServices`,
`settings.get`/`getAll` (every app reads its theme), and readings such as the
battery level, the OS version and the root filesystem's free space
(`deviceInfo.getStorageInfo`). A call the table does not list is refused
whatever the app declares; `process.*` and the bus's own `org.geckoOS.Echo`
methods are left out on purpose. Two more tests in `nova.rs` keep the three
copies of this vocabulary in step: the bridge's table, Nova's, and vroom's
(`src/config.rs` and both schemas).

#### Where filesystem calls reach

An app's `filesystem.*` calls reach its own data and nothing else, unless it
holds one of the two privileged filesystem capabilities. The bridge stamps
every call with a *scope*, and Echo confines the call to it: the kernel
resolves each path beneath the scope's root (`openat2` with
`RESOLVE_BENEATH`), so neither `..` nor a symlink can leave it, and the call
runs as the user, never as root.

| Scope | Root | Who | Paths |
| --- | --- | --- | --- |
| the app's own | `/var/lib/pyxis/data/<id>` | every app | `~`, `~/notes.json`, `notes.json` |
| the user's | `/home/geckouser` | `filesystem:user` | `~`, `~/Documents/a.txt` |
| the system's, read-only | `/` (the root filesystem only) | `filesystem:system` | `/etc/os-release` |
| another app's own | `/var/lib/pyxis/data/<other id>` | `filesystem:apps`, with `app: "<other id>"` | `~/notes.json` |

- `~` is the root of the app's scope, and replies give paths back the same
  way, so an app never depends on where its data lives. An absolute path
  needs `filesystem:system`, and can only be read; `..` is refused in every
  scope.
- An app's own directory is made on first use, owned by the user, mode
  0700. Removing the app (`installer.Remove`, `nova.Uninstall`) deletes it,
  and the app's Gecko profile (its localStorage, IndexedDB and cookies), so
  a package that later takes the same id starts empty. A package removed
  with `opkg remove` directly keeps both.
- The system scope stops at the root filesystem: `/proc`, `/sys`, `/dev`,
  `/run` and `/tmp` are other mounts and stay out of reach.
- Writing outside the home has no capability yet. It is to come with a
  user-approved "privileged access" unlock.

### 19.2 Policy

- `requiredPermissions` in `appinfo.json` declares requested capabilities.
- The launcher or runtime enforces user-facing permissions.
- `opkg` installation alone does not imply runtime authorization.
- The bridge is deny-by-default: it forwards only the (service, method)
  pairs it lists, and refuses the rest with the same `Pyxis capability
  denied` error as an undeclared capability. A new service is out of apps'
  reach until the bridge's table names it.

#### App tiers

Every registered app has a **tier**, determined by which root Nova discovered
it in. The tier records *who put the app there and what authority that
implies*, and is reported as `tier` in `nova.List`/`nova.GetInfo`.

| Tier | Root | Installed by | Manifest |
| --- | --- | --- | --- |
| `system` | `/usr/share/geckoOS/apps` | the image build | `manifest.webapp` |
| `opkg` | `/usr/pyxis/applications` | `opkg install` (root) | `appinfo.json` |
| `user` | `/var/lib/pyxis/apps` | `nova.Install` (tarball) | `manifest.webapp` |

`system` and `opkg` are **root-mediated**: something with root privilege made
a deliberate decision to place the app. `user` is not — it is an unsigned
tarball admitted at runtime.

This is a different axis from the manifest *format*. Conflating the two is
what originally allowed the escalation below: `system` and `user` apps both
carry `manifest.webapp`, so nothing downstream could tell an app shipped in
the image apart from a tarball dropped into the writable root.

Tier governs three things:

| | `system` | `opkg` | `user` |
| --- | --- | --- | --- |
| May hold privileged permissions | yes¹ | yes¹ | **never** |
| Owns its app id | yes | yes | no |
| Removable by `nova.Uninstall` | no | no | yes |

¹ subject to the trust list, below.

Nova removes only what Nova installed. `opkg` apps are removed with `opkg
remove` (whose status database owns the files); `system` apps cannot be
uninstalled at all, since their payload is part of the image and no rescan
could restore it.

#### Privileged permissions and app management

`appmanager.install` and `appmanager.manage` confer the right to install and
remove *other* apps. They are **privileged**: declaration and grant are
separate steps. So are the other rights over other apps or the whole system:
`notifications.manage` (every app's notifications), `windows.manage` (every
app's windows), `settings.write` (settings every app reads), `terminal` (a
root shell), and `filesystem.user`, `filesystem.system` and `filesystem.apps`
(the user's files, the whole root filesystem, and other apps' data).
Everything below applies to all nine.

- **Any app may declare them.** Declaring is not an error at package time, at
  schema validation, or at `appinfo.json` parse time. An installer-type app
  must be packageable as a normal `.ipk` by a normal SDK build; making the
  declaration fatal would mean the package fails to load entirely rather than
  running with reduced rights.
- **A grant requires both a trusted id and a root-mediated tier.** The app id
  must be on Nova's platform trust list *and* its tier must be `system` or
  `opkg`. Nova withholds the capability from everything else and logs the
  refusal; the declaration survives in the app's recorded permissions for
  display and tooling, but grants nothing.
- **Neither half is sufficient alone.** Keying on tier alone would grant app
  management to every ipk on the system — `/usr/pyxis/applications` is
  root-owned, but *every* ipk lands there, so that path gates nothing. Keying
  on id alone would let a user-installed app claim a trusted id and inherit
  its authority.
- **Being installed by root is not evidence of trust.** Root-installability is
  a property of the package manager, not a statement that an app should be
  able to manage other apps.

This answers the question of whether app management should be reserved for
in-image system apps: **it is reserved by trust, not by delivery mechanism.**
A privileged app may ship as an `.ipk` — the packaging format is not the
security boundary — but it reaches app management only by being on the trust
list at a root-mediated tier. This keeps the first-party installer buildable
and updatable through the normal SDK/opkg path without letting an arbitrary
sideloaded package self-elevate merely by asking.

The gate applies to both manifest paths. `manifest.webapp` apps are filtered
against the same rule on the colon vocabulary, because `USER_APPS_ROOT` is
writable and accepts plain tarballs; gating only the `appinfo.json` path would
leave the identical escalation reachable by shipping the same app as a
`.tar.gz`.

The trust list is currently a compile-time set of app ids in Nova
(`PRIVILEGED_APP_IDS`), baked into the image so that no installed package can
extend it. This is an interim stand-in. Once package signature verification
lands, the criterion should become "the package signature chains to a platform
trust root" and the id list should be retired.

#### Id ownership and shadowing

Id collisions across roots resolve by scan order, so a later root shadows an
earlier one. Shadowing is deliberate and load-bearing for updates: `opkg
install notes_0.2.0_all.ipk` replaces the ROM copy of `notes` without
rebuilding the image, the same model webOS and Android use.

Shadowing is **not symmetric**, however. Only root-mediated tiers may claim an
id:

- an `opkg` app may shadow a `system` app — the documented update path;
- a `user` app may **not** take an id held by a `system` or `opkg` app. It is
  refused at `nova.Install` and skipped at rescan.

Without this, a tarball named `installer` would both impersonate the real
installer in the launcher and — before tier was tracked — inherit its
app-management authority. The two checks are independent on purpose: a
user-tier app is denied privileged capabilities even if the id reservation is
somehow bypassed.

`user`-over-`user` replacement stays allowed, so reinstalling or upgrading a
user app works normally.

## 20. Validation rules

### 20.1 Common validation

- `id` must match the id pattern (reverse-DNS, or a single lowercase segment
  for first-party apps).
- `Package == id`
- `Version` values must match.
- `icon` must exist.
- `appinfo.json` must exist in app root.

### 20.2 Web validation

- `type` must equal `web`.
- `main` must be relative.
- `Architecture` must equal `all`.
- output must contain referenced entry file.

### 20.3 Native validation

- `type` must equal `native`.
- `main` must be absolute.
- `Architecture` must not equal `all`.
- binary at `main` must exist in staged tree and be executable.

## 21. Error model

Standard exit classes:

- `E_CONFIG`: invalid `pyxis.json`
- `E_BUILD`: builder failure
- `E_MANIFEST`: invalid `appinfo.json`
- `E_PACKAGE`: invalid package metadata or package build failure
- `E_DEVICE`: copy/install/launch failure on target

## 22. Recommended initial implementation roadmap

### Phase 1

- Implement `pyxis.json`
- Implement manifest generator
- Implement package metadata generator
- Implement staging tree generator
- Wrap `opkg-build`

### Phase 2

- Add `pyxis-sdk` support for Vite
- Add `pyxis-pdk` support for CMake
- Add device profiles and remote install

### Phase 3

- Add launcher registry daemon and `pyxis-appctl`
- Add permissions model
- Add service packaging helpers
- Add upgrade-aware maintainer script templates

## 23. Opinionated defaults

These defaults are recommended for Pyxis v1:

- app root: `/usr/pyxis/applications/<app-id>`
- native GUI payload root: `/opt/pyxis/apps/<app-id>`
- package tool: `opkg-build`
- install tool: `opkg`
- launcher refresh command: `pyxis-appctl rescan`
- web architecture: `all`
- native default architecture: target triple mapped to `aarch64` or `x86_64`
- package identity equals app identity

## 24. Reference examples

### 24.1 Web app package example

```text
com.example.app.clock_1.0.0_all.ipk
```

Contains:

```text
/usr/pyxis/applications/com.example.app.clock/appinfo.json
/usr/pyxis/applications/com.example.app.clock/index.html
/usr/pyxis/applications/com.example.app.clock/assets/app.js
/usr/pyxis/applications/com.example.app.clock/icon.png
```

### 24.2 Native app package example

```text
com.example.app.notes_1.0.0_aarch64.ipk
```

Contains:

```text
/opt/pyxis/apps/com.example.app.notes/bin/notes
/opt/pyxis/apps/com.example.app.notes/lib/libnotescore.so
/usr/pyxis/applications/com.example.app.notes/appinfo.json
/usr/pyxis/applications/com.example.app.notes/icon.png
```

## 25. Non-goals for v1

- package signatures
- app sandboxing implementation details
- store/repository feed format
- delta updates
- rollback snapshots

These can be layered on later without changing the core SDK/PDK package contract.
