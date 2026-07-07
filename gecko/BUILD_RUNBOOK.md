# Pyxis Gecko Build Runbook

Status as of 2026-07-05 (updated: **the parent-brokered content-process file
read is now implemented** — see "Parent-brokered channel (IPDL) implemented"
below, closing the single highest-risk gap tracked since the original
scaffold; before that, **the `--app=<id>` kiosk run mode is now implemented**
— see "Kiosk run mode implemented" below; before that, **the bridge's Echo
transport is now real** — see "Echo transport implemented" below; before
that, **the build works now** — see further below). This is the precise,
ordered set of steps for "build `vela` from source." It assumes
you've read `pyxis-gecko-embedding-plan.md` and
`pyxis-scheme-and-kiosk-plan.md` (the design), and `gecko/GECKO_PIN` (the
version pin + reasoning — **note the pin changed since this doc was first
written**: ESR 140/`release` was abandoned for mozilla-central/`master` after
real bootstrap failures showed Taskcluster doesn't index toolchain artifacts
for release/ESR branches; see `GECKO_PIN`'s "Why the pivot away from ESR 140"
section for the full story).

## Parent-brokered channel (IPDL) implemented as of 2026-07-05

The single highest-risk, explicitly-not-yet-solved gap called out in every
previous section of this doc — `PyxisChannel::OpenContentStream` doing a
direct, unconditional `NS_NewLocalFileInputStream` open, which is unsafe
under a real e10s/Fission content-process sandbox — is now closed. This is
the literal next C++ task flagged at the end of "What's NOT done" below, and
in `PyxisChannel.cpp`'s own "STILL NOT SOLVED HERE" comment.

**Design**: a new, Pyxis-owned IPDL protocol, `PPyxisChannel`
(`netwerk/protocol/pyxis/PPyxisChannel.ipdl`), `manager PBackground` (NOT
`manager PNecko`). Two real precedents were read in full before choosing this
shape:
- `netwerk/protocol/gio/PGIOChannel.ipdl` — `manager PNecko`, a full
  remoted-`nsIChildChannel` actor (`OnStartRequest`/`OnDataAvailable`/
  `OnStopRequest` all forwarded over IPC, listener-diversion machinery). Too
  much machinery for what Pyxis needs — a single stream handed back for one
  already-fully-specified request, not a fully remoted redirect/upload/
  suspend-capable channel.
- `netwerk/protocol/res/ExtensionProtocolHandler.{h,cpp}`'s
  `SubstituteRemoteFileChannel`/`NewFD`, and the `PNecko`-level
  `GetExtensionStream`/`GetExtensionFD` request/reply methods
  (`netwerk/ipc/PNecko.ipdl`, `NeckoParent::RecvGetExtensionStream`/
  `RecvGetExtensionFD`) — the actually-matching shape (a single async
  "resolve+open, hand back a stream" request/reply pair). The one thing NOT
  copied verbatim: moz-extension: added its methods directly to shared
  `netwerk/ipc/PNecko.ipdl` + `NeckoParent`/`NeckoChild`, which would have
  meant editing those shared files for Pyxis too — explicitly out of scope
  per this task's constraints (kept to `PyxisChannel.{h,cpp}`, new IPDL/actor
  files, and `moz.build`). `manager PBackground` (matching
  `dom/locks/PLockManager.ipdl` and, notably, `netwerk/protocol/http/
  PHttpBackgroundChannel.ipdl` — a `mozilla::net`-namespaced protocol already
  using this exact manager) gives the same "content process asks parent for
  a self-contained async service" capability without touching
  `NeckoParent`/`NeckoChild` at all. Adding any new `PBackground` managee
  still requires three small, additive lines in the shared
  `ipc/glue/PBackground.ipdl` plus one `Alloc` override in
  `ipc/glue/BackgroundParentImpl.h/.cpp` — unavoidable (there's no way to add
  a reachable protocol without registering it with *some* manager), but far
  smaller than `PNecko`'s channel-actor boilerplate. See the mirrored
  `.PYXIS_DIFF` notes at `gecko/patches/pyxis/ipc/glue/` for the exact three
  files/lines touched.

**Actors**: `PyxisChannelChild`/`PyxisChannelParent`
(`netwerk/protocol/pyxis/PyxisChannelChild.{h,cpp}`,
`PyxisChannelParent.{h,cpp}`), named after `GIOChannelChild`/
`GIOChannelParent`'s convention even though the lifecycle model is closer to
`dom/locks`' hand-written-final-actor style. `PyxisChannelChild` is
deliberately NOT itself an `nsIChannel` (unlike `GIOChannelChild`, which
doubles as both actor and channel) — `PyxisChannel` stays the one and only
`nsIChannel` implementation in both processes; `PyxisChannelChild` is a
short-lived `nsICancelable` helper (mirroring `ExtensionStreamGetter`'s role
in `ExtensionProtocolHandler.cpp` exactly) that owns one in-flight
`GetContent` request and feeds the eventual stream (or failure) to the
channel's listener. `PyxisChannelParent` does the real
`PyxisOriginMap::Resolve` + path confinement + open, but does NOT duplicate
that logic — it's been factored out of `PyxisChannel.cpp` into a new shared
helper, `PyxisChannelUtils` (`netwerk/protocol/pyxis/PyxisChannelUtils.{h,cpp}`),
that both `PyxisChannel::OpenContentStream` (parent-direct path) and
`PyxisChannelParent::RecvGetContent` (brokered path) call identically. The
path-traversal-defense logic itself is byte-for-byte unchanged from the
original scaffold, just moved.

**Where the branch actually lives — a deliberate deviation from the
originally-sketched shape**: the task's own commented-out sketch suggested
branching on `XRE_IsContentProcess()` directly inside `OpenContentStream`.
Reading `nsBaseChannel.cpp`'s `BeginPumpingData` (not assumed, read line by
line) showed this is NOT where the branch belongs: `BeginPumpingData` tries
`BeginAsyncRead()` FIRST and only falls back to `OpenContentStream()` if
`BeginAsyncRead()` returns `NS_ERROR_NOT_IMPLEMENTED`. `BeginAsyncRead` is
the override `nsBaseChannel.h`'s own doc comment describes as being for
exactly "the stream isn't available synchronously, async work is needed to
retrieve it" — and it's the same override `SimpleChannel`/
`ExtensionStreamGetter` actually use for moz-extension:'s equivalent
content-process-proxy case, confirmed by reading `netwerk/base/SimpleChannel.
{h,cpp}` directly. So: `PyxisChannel::OpenContentStream` keeps doing the
direct, parent-process-only open completely unchanged (now via
`PyxisChannelUtils`, but byte-for-byte the same behavior); a NEW
`PyxisChannel::BeginAsyncRead` override does the `XRE_IsContentProcess()`
check and, when true, delegates to `PyxisChannelChild`. `OpenContentStream`
also gained a `MOZ_DIAGNOSTIC_ASSERT(!XRE_IsContentProcess())` at its top —
defense-in-depth, since it should now be structurally unreachable from a
content process for any `AsyncOpen()`-driven load.

**IPDL type-mapping detail worth flagging** (verified against
`ipc/ipdl/ipdl/lower.py`'s `_cxxInType`, not assumed): `GetContent`'s
multi-value `returns(nsresult, nsIInputStream, nsCString, int64_t)` resolver
does NOT take those as plain value types — the generated resolver's
`std::function` signature is `std::tuple<const nsresult&, nsIInputStream*,
const nsACString&, const int64_t&>` (raw pointer for the refcounted
`nsIInputStream`, since it has no `[MoveOnly]` attribute; `const nsACString&`
for `nsCString` specifically, a special-cased name substitution in the same
codegen function). Confirmed directly against a real generated single-value
case already in this tree (`GetExtensionStreamResolver` = `std::function<void
(nsIInputStream*)>`) rather than guessed. `PyxisChannelParent::RecvGetContent`
constructs a named `Type` tuple alias matching this exactly and passes it to
`aResolve`, following the same real pattern used by
`BackgroundParentImpl::RecvEnsureRDDProcessAndCreateBridge`.

**Not independently compiler-verified** (no `./mach build` run for this
piece) — every API signature (`nsBaseChannel::BeginAsyncRead`'s exact
contract, `PBackgroundChild::GetOrCreateForCurrentThread`, the
`SendGetContent`/`ResolveOrRejectValue` promise consumption pattern, the
resolver tuple element types) was verified against either a real header in
this checkout or a real generated header under `obj-pyxis-webapp/ipc/ipdl/`,
not assumed from memory of older Firefox versions. See the task's final
report for the exact rebuild command and a concrete test plan (re-enabling
multiprocess and reloading `pyxis://testapp/index.html`).

## Kiosk run mode implemented as of 2026-07-05

`pyxis-scheme-and-kiosk-plan.md` Part B ("`vela` kiosk entry") is now
implemented in the live checkout and mirrored at
`gecko/patches/pyxis/browser/{components,base/content}/*.PYXIS_DIFF` (real
`git diff` output against the three upstream files touched, not prose —
verified to `git apply --check` cleanly against a clean `central-work` tree
this session). **Not yet compiled** (no `./mach build` run for this piece;
see "Not independently compiler-verified" below) — this section documents
what was implemented and, more importantly, the real mechanisms it was
verified against, not assumed to exist.

**Files touched** (all pre-existing upstream files — no new directory, since
this reuses the already-registered `nsBrowserContentHandler`
`command-line-handler` component rather than adding a new XPCOM class):

- `browser/components/BrowserContentHandler.sys.mjs` — recognizes
  `--app=<id>` / `-app <id>` (and falls back to `PYXIS_APP_ID` env var),
  reads `PYXIS_DISPLAY_MODE`/`PYXIS_BACKGROUND`, and opens a chromeless
  kiosk window via a new `openPyxisAppWindow()` function.
- `browser/components/nsIBrowserHandler.idl` — adds a read-only
  `pyxisAppId` attribute, mirroring the existing `kiosk` attribute on the
  same interface.
- `browser/base/content/browser-init.js` — in `onBeforeInitialXULLayout()`,
  stamps `pyxisapp`/`windowclass`/`windowname` attributes onto the window's
  root element from a `pyxisApp` key on the `extraOptions` property bag,
  mirroring the existing (real, shipping) `taskbartab` attribute-stamping
  block immediately above it.

**Command-line registration mechanism (verified, not assumed):**
`nsBrowserContentHandler` is already registered in the `command-line-handler`
category as `m-browser` (`browser/components/components.conf:32`) — no new
`nsICommandLineHandler` component was needed; `--app` is handled inside its
existing `handle()` method (`bch_handle`), right next to the existing
`--kiosk`/`--kiosk-monitor` block. Confirmed from
`toolkit/components/commandlines/nsICommandLine.idl`'s own doc comment that
on Linux/Mac, `--flag`, `--flag=param`, and `--flag param` all normalize to
the same `-flag`/`handleFlagWithParam("flag", ...)` call before any handler
sees them — so there is no separate single- vs. double-dash code path to
write; `--app=<id>`, `--app <id>`, and `-app <id>` are all handled
identically by one `cmdLine.handleFlagWithParam("app", false)` call.

**Chrome stripping — reused a real existing mechanism, did not invent one:**
searched for `toolkit/components/pwa/`-style PWA/SSB support (none exists in
this tree) and for Firefox's existing generic `--kiosk` flag (does exist,
`BrowserContentHandler.sys.mjs`'s `gKiosk`/`BrowserHandler.kiosk`) — but
`--kiosk` alone only fullscreens the window and autohides the toolbox
(`browser-fullScreenAndPointerLock.js`); it does **not** strip the tab strip,
URL bar, or menus, so it isn't sufficient by itself for an app-content-only
window. The real precedent that IS sufficient, found by searching for
"taskbartab": **`browser/components/taskbartabs/`**, a genuine, already-
shipping (currently Windows-focused for the *pinning* parts, but the window-
chrome-hiding parts are platform-generic) site-specific-browser-style
feature. `TaskbarTabsWindowManager.sys.mjs`'s `kTaskbarTabsWindowFeatures`
window-features string (`titlebar,close,toolbar,location,personalbar=no,
status,menubar=no,resizable,minimizable,scrollbars`) is the direct model for
this patch's `kPyxisAppWindowFeatures` — vela goes further and also
turns off `toolbar`/`location` (TaskbarTabs keeps a read-only URL bar for
anti-spoofing trust; a kiosk app has no URL bar at all).

Traced the *exact* mechanism, not just the feature string, end to end:
- `nsWindowWatcher::CalculateChromeFlagsForSystem`
  (`toolkit/components/windowwatcher/nsWindowWatcher.cpp:1928-1949`) turns
  `toolbar=no`/`location=no`/`menubar=no`/`personalbar=no`/`status=no` into
  unset `CHROME_TOOLBAR`/`CHROME_LOCATIONBAR`/`CHROME_MENUBAR`/
  `CHROME_PERSONAL_TOOLBAR`/`CHROME_STATUSBAR` bits.
- `AppWindow::ApplyChromeFlags` (`xpfe/appshell/AppWindow.cpp:2317-2361`)
  turns unset chrome bits into a `chromehidden="menubar toolbar location
  directories status extrachrome"` attribute on `browser.xhtml`'s root
  element.
- `toolkit/themes/shared/global-shared.css:180-188` (verified real CSS, not
  guessed) hides every element carrying a matching `.chromeclass-*` class:
  `:root[chromehidden~="location"] .chromeclass-location` (hides `#nav-bar`,
  which carries `class="browser-toolbar chromeclass-location"` per
  `browser/base/content/navigator-toolbox.inc.xhtml:107-108`),
  `:root[chromehidden~="directories"] .chromeclass-directories` (hides
  `#PersonalToolbar`), `:root[chromehidden~="menubar"] .chromeclass-menubar`
  (hides `#toolbar-menubar`).
- The tab strip (`#TabsToolbar`) carries **no** `chromeclass-*` class, so
  `chromehidden` alone does not hide it — confirmed by reading
  `navigator-toolbox.inc.xhtml:23-24`. It's hidden by a separate, but
  equally real, mechanism: `toolbar=no` makes `window.toolbar.visible ===
  false`, which makes `TabBarVisibility.update()`
  (`browser/components/tabbrowser/content/tabbrowser.js:8868-8899`) treat
  the window as popup-like (`isSingleTabWindow = isPopup || isTaskbarTab`)
  and toggle `#navigator-toolbox`'s `tabs-hidden` attribute — the same
  mechanism TaskbarTabs' own windows rely on for their single-tab styling.

Net effect, verified against real CSS/JS across three files rather than
assumed from the plan doc's generic description: a `vela --app=<id>`
window shows only page content — no tabs, no URL bar, no menu bar, no
bookmarks toolbar.

**WM_CLASS / Wayland `app_id` stamping — verified end to end, not assumed:**
searched for "WM_CLASS" in `widget/gtk/nsWindow.cpp` and found the real,
current mechanism: `nsWindow::SetWindowClass(xulWinType, xulWinClass,
xulWinName)` (`widget/gtk/nsWindow.cpp:6587-6639`) copies `xulWinClass`
verbatim (no transformation — confirmed by reading the function; only
`xulWinType`, which this patch does NOT touch, gets uppercased/sanitized)
into `mGtkWindowAppClass`, and `RefreshWindowClass()`
(`nsWindow.cpp:6534-6585`) uses it for **both** the X11 path
(`XSetClassHint`) and the Wayland path
(`gdk_wayland_window_set_application_id(gdkWindow,
mGtkWindowAppClass.get())`, `nsWindow.cpp:6580-6583`) — i.e. this one
attribute drives the real `xdg_toplevel.set_app_id` request Evo's
`smithay`-based `new_toplevel` handler (`evo/src/shell/xdg.rs:46`) reads as
`app_id`, which is exactly what Evo's two-step create/claim contract
(`evo/src/enhancements/echo_ipc.rs`) correlates windows on.

`SetWindowClass`'s `xulWinClass`/`xulWinName` params are read from plain
`windowclass`/`windowname` **attributes** (not interned `nsGkAtoms` — a
`GetAttribute(u"windowclass"_ns, ...)` string lookup, confirmed in
`xpfe/appshell/AppWindow.cpp:1546-1551` — so no atom-table change was needed
for this patch) on the root element of `browser.xhtml`, read once in
`AppWindow::SyncAttributesToWidget()`, called from
`AppWindow::BeforeStartLayout()`. Verified (not assumed) that
`browser-init.js`'s `onBeforeInitialXULLayout()` genuinely runs before that
C++ read: it's wired to the `MozBeforeInitialXULLayout` DOM event
(`browser/base/content/browser-main.js:38-42`), which
`dom/prototype/PrototypeDocumentContentSink.cpp:656-658` dispatches
synchronously immediately before calling `StartLayout()` (line 666), and
`StartLayout()` is what eventually calls `AppWindow::BeforeStartLayout()`
(via `Document::SetMayStartLayout()`, `dom/base/Document.cpp:9821-9829`,
which calls `win->BeforeStartLayout()` directly). So setting
`windowclass`/`windowname` attributes in the JS event handler is guaranteed
to happen in time for the C++ to pick them up on the same window's first
layout pass — not a race, verified via the actual call chain.

Window title was deliberately **not** overridden — Evo's correlation
contract keys purely on `app_id` (grepped `evo/src/enhancements/echo_ipc.rs`
for "title"; it only ever reads/uses `app_id`), and the page's own title
showing in the (chromeless, so barely visible anyway) window title is
standard, desirable kiosk-app behavior, not a gap.

**Single toplevel per app instance**: falls out naturally from
`openPyxisAppWindow()` opening exactly one `BrowserWindowTracker` window
per `--app` invocation with one URL loaded into it — **not independently
verified live** (would need a real build; see below), but nothing in the
traced call path opens a second window or tab for this case, and popup/
`window.open` calls from kiosk-window content are a separate, pre-existing
Firefox concern (popup blocker / whether `window.open` targets are
constrained) not modified by this patch. **Explicitly flagged as open**:
whether DevTools or other popups can still spawn extra (non-kiosk-styled)
windows from inside a `--app` session was not tested and is not blocked by
anything this patch adds.

**What this patch does NOT do** (explicitly out of scope or genuinely not
implemented — see the task's final report for the complete list):
- `PYXIS_BACKGROUND=1` (headless/no-window apps, webOS "noWindow"
  equivalent, `pyxis-scheme-and-kiosk-plan.md` §B1 point 5) is detected
  (env var is read) but **not implemented** — falls back to opening a
  normal kiosk window and logs a `console.error` explaining the gap, rather
  than silently doing the wrong thing or guessing at a headless mechanism
  (`HeadlessShell.sys.mjs`, already imported in this file for `--headless`,
  targets automation/testing, not a real app runtime mode, and was not
  repurposed without being able to verify its behavior against a real
  build).
- Navigation lock-down (same-origin `pyxis://` stays in-window, off-origin
  handled per manifest — `pyxis-scheme-and-kiosk-plan.md` §B1 point 4) is
  **not implemented** — this patch only opens the initial window/URL.
- The `entry` path (`pyxis://<id>/<entry>`) defaults to `index.html` or an
  optional `--app-entry <path>` flag; real manifest-driven `entry`
  resolution is Nova's job once it exists, per
  `gecko/src/gecko.rs`'s own doc comments.
- Multi-app / shared-parent process model (`pyxis-scheme-and-kiosk-plan.md`
  §B2) — out of scope for this pass; each `--app` invocation is a normal,
  independent `vela` process today (fine for a first real
  end-to-end milestone, per Part C phase 2's gate: "card appears via the
  real launch path (driven manually, then via Nova)").

**Not independently compiler-verified** (no `./mach build` run this
session) — correctness was established by reading the actual C++/JS source
end to end (`nsWindowWatcher.cpp`, `AppWindow.cpp`, `nsWindow.cpp`,
`tabbrowser.js`, `global-shared.css`, `navigator-toolbox.inc.xhtml`,
`nsICommandLine.idl`, `PrototypeDocumentContentSink.cpp`, `Document.cpp`)
and confirmed with `node --check` (syntax only) on both modified `.mjs`/`.js`
files, rather than by compiling. See the task's final report for the exact
rebuild and test commands.

## Echo transport implemented as of 2026-07-05

`PyxisBridgeParent.sys.mjs`'s `_echoRequest` — the last stub blocking real
`window.geckoOS.callService(...)` calls from reaching Echo — is now a real
implementation, not a placeholder. Both the file-level "two real options"
comment and the "What's NOT done" bullet below describing it as a stub are
now stale in that one respect; this section is the update.

**Decision: option (a) — a privileged JS Unix-domain-socket client**, using
`nsISocketTransportService.createUnixDomainTransport()`, NOT option (b) (a
Rust/XPCOM wrapper crate around `geckoOS-echo`'s existing client). Both
options turned out to be real and buildable in this exact tree, but (a) wins
decisively once actually checked against the real code, for the opposite
reason the old comment guessed:

- `nsISocketTransportService.createUnixDomainTransport(nsIFile)` is a real,
  `scriptable` method already in `netwerk/base/nsISocketTransportService.idl`
  — zero IDL to write, zero `components.conf` entries, zero libxul relink.
  `devtools/shared/security/socket.js` and
  `devtools/shared/transport/transport.js` already exercise the sibling TCP
  path (`createTransport` + `openInputStream`/`openOutputStream` +
  `asyncWait`/`onInputStreamReady`/`onOutputStreamReady`) from plain chrome
  JS with no C++ glue at all — the Unix-domain variant is the identical
  stream API, just a different transport constructor.
- Option (b) has genuine precedent in this tree too
  (`toolkit/components/kvstore/src/lib.rs`,
  `dom/webauthn/authrs_bridge/src/lib.rs` — real `#[xpcom(implement(...))]`
  structs), but **every** real example requires a new `.idl`
  (`XPIDL_SOURCES`), a `components.conf` CID registration, a C++ constructor
  shim header, and adding the crate as a dependency of
  `toolkit/library/rust/shared/Cargo.toml` so it statically links into
  `gkrust-shared`/libxul. That's real Gecko-build-system surgery this
  session cannot compile or verify — and it is *more* total ceremony than
  (a), not less, contrary to what the old comment guessed before either
  option had actually been checked against the real tree.

**Wire protocol**, verified directly against `echo/src/{transport,message,
lib}.rs` and `echo/src/bin/daemon.rs` (not guessed): 4-byte little-endian
length prefix (Echo's own code calls this "CRITICAL" — do not use
`nsIBinaryOutputStream`/`nsIBinaryInputStream`'s `write32`/`read32`, which
are big-endian/"network order" per their own IDL doc comments; the prefix is
hand-assembled/parsed byte-by-byte instead), then that many bytes of UTF-8
JSON encoding Echo's `Message{header,body}` shape verbatim, with
`message_type` values in `SCREAMING_SNAKE_CASE` (`"METHOD_CALL"`,
`"METHOD_RETURN"`, `"ERROR"`, `"HANDSHAKE"`, `"SIGNAL"` — from
`message.rs`'s `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`), and
`destination`/`interface` both set to `"org.geckoOS.<service>"` (matching
`geckoOS-native-host.rs`'s existing `call_echo_service`, the closest real
Rust-client precedent for this same call shape). Socket path is
`$XDG_RUNTIME_DIR/geckoOS/echo.sock`, falling back to `/tmp/geckoOS/echo.sock`
— same convention as `echo/src/lib.rs`'s `get_socket_path`.

**One real bug caught and fixed during implementation, not by inspection
alone but by reading the IDL doc comments closely**:
`nsIScriptableInputStream.read()` silently truncates at the first NUL byte
(documented in its own IDL: "If the data contains a null byte, then this
method will return a truncated string"). Echo's 4-byte length-prefix header
routinely contains NUL bytes (any message body under 256 bytes has a zero
high byte), which would have silently corrupted framing on roughly 7 out of
8 messages. Fixed by using `readBytes()` instead, the NUL-safe counterpart
on the same interface. Also verified (by reading `js/xpconnect/src/
XPCConvert.cpp`'s actual `JSData2Native`/`NativeData2JS` conversion code,
not assumed) that `nsIOutputStream.write()`'s `string` in-param and
`nsIScriptableInputStream.readBytes()`'s `ACString` out-param both round-trip
raw bytes 0-255 losslessly as a JS "binary string" (one JS char code per
byte, Latin1) with no NUL-truncation or re-encoding — the same binary-string
convention `devtools/shared/transport/stream-utils.js` already relies on in
this tree, so the write path did not need a similar fix.

**Implementation**: `toolkit/actors/PyxisBridgeParent.sys.mjs` (mirrored at
`gecko/patches/pyxis/toolkit/actors/PyxisBridgeParent.sys.mjs`). A single
process-wide `EchoConnection` class owns the socket, multiplexes concurrent
method calls by `message_id`/`reply_to` correlation (same correlation model
`daemon.rs`'s `handle_connection` and `geckoOS-native-host.rs`'s
`echo_broker_loop` already use), and resolves/rejects a `_sendAndAwait`
promise per call with a 25s timeout. `_handleCallService` (the existing
capability-gate method) now calls through to `_echoRequest`, which just
forwards to the shared connection's `callMethod(service, method, params)`.
Signal fan-out (`dispatchSignalToSubscribers`) is also wired as a stretch
goal: any `SIGNAL`-typed message arriving on the shared connection is
checked against every live actor's per-browsingContext subscription table
(`gSubscriptions`, already existed) and forwarded via the existing
`forwardEventToContent` method. `_getDeclaredPermissions` was **not**
touched — still returns `[]` unconditionally, per Nova not existing yet.

**Not independently compiler-verified** (no `./mach build` available this
session, and this is plain-JS `.sys.mjs` with no C++/Rust build-system
changes, so there is no separate crate to `cargo check` either) — correctness
was established by reading the actual XPConnect marshalling code
(`XPCConvert.cpp`), the actual IDL files
(`nsISocketTransportService.idl`, `nsIScriptableInputStream.idl`,
`nsIOutputStream.idl`, `nsIEnvironment.idl`), and tracing every field name
against Echo's real Rust structs, rather than by running anything. See the
task's final report for a concrete test plan.

## Build status: FULL PATCH SERIES COMPILES as of 2026-07-05

`./mach build` with the `pyxis://` protocol handler + `PyxisBridge` actor
wired into `DIRS`/`ActorManagerParent.sys.mjs` now compiles cleanly. Two more
real compile errors surfaced on the first actual compiler run (inspection
alone hadn't caught these) and are now fixed in both the live checkout and
`gecko/patches/pyxis/`:

4. **Unified-build anonymous-namespace collision**: Gecko's build system
   concatenates multiple `.cpp` files into one `Unified_cpp_*.cpp`
   translation unit. `PyxisOriginMap.cpp` and `PyxisProtocolHandler.cpp` each
   had `namespace { StaticRefPtr<T> sSingleton; }` — fine per-file in
   isolation, but a hard redefinition error once unified together into the
   same scope. Renamed to `sOriginMapSingleton`/`sProtocolHandlerSingleton`.
5. **`kScheme` was a pointer, not an array**: `static constexpr auto kScheme
   = "pyxis"` deduces `auto` to `const char*`. `nsACString::AssignLiteral`/
   `EqualsLiteral` only have overloads for an actual `const char (&)[N]`
   array (to deduce the length) — a pointer silently matches none of them.
   Fixed in `PyxisProtocolHandler.h` to `static constexpr char kScheme[] =
   "pyxis"`.

Both are documented inline at the fix site. See the original "Build status"
note below for the Phase-0 (vanilla, unmodified Gecko) milestone this built
on top of.

## Post-build finding: every load was silently blocked (found live, not by inspection)

Once the build succeeded, live testing (typing `pyxis://testapp/index.html`
in the address bar, then directly exercising
`Services.io.newChannelFromURI(...).asyncOpen(...)` from the Browser Console
to bypass UI/command-line heuristics) surfaced a real security-posture bug
that no amount of source reading had caught:

6. **Missing mandatory protocol security flag.**
   `nsIProtocolHandler.idl` states plainly: "ALL PROTOCOL HANDLERS MUST SET
   ONE OF THE FOLLOWING SIX FLAGS" (`URI_LOADABLE_BY_ANYONE`,
   `URI_DANGEROUS_TO_LOAD`, `URI_IS_UI_RESOURCE`, `URI_IS_LOCAL_FILE`,
   `URI_LOADABLE_BY_SUBSUMERS`, `URI_IS_WEBEXTENSION_RESOURCE`) — and
   `nsContentSecurityManager::CheckChannelHasProtocolSecurityFlag`
   (`dom/security/nsContentSecurityManager.cpp`) enforces this on literally
   every channel open, hard-failing with `NS_ERROR_CONTENT_BLOCKED` if none
   is set. `components.conf`'s flag list had none of the six — confirmed via
   a real `asyncOpen()` call throwing that exact error synchronously.
   Checked the real `moz-extension:` registration
   (`netwerk/build/components.conf`) to see which one it uses: it sets
   `URI_IS_WEBEXTENSION_RESOURCE`, which is semantically extension-specific
   and doesn't apply to Pyxis. `URI_LOADABLE_BY_SUBSUMERS` ("loadable only by
   callers with a principal that subsumes this uri... privileged code and
   websites that are same origin") is the one that actually matches
   `PyxisProtocolHandler.h`'s own already-written design intent ("only
   privileged/chrome code and same-origin pyxis:// documents may load it") —
   added to `components.conf`.

This is why the very first live tests silently failed with no visible error:
the command-line/address-bar paths swallow `resolveURIInternal`/navigation
exceptions without a user-visible dialog, so "nothing happens, no errors"
was consistent with this the whole time — the Browser Console's direct
`asyncOpen()` call was what finally surfaced the real `NS_ERROR_CONTENT_BLOCKED`.
**This needed a rebuild** (`components.conf` changes regenerate
`StaticComponents.cpp`) before retesting.

## Post-build finding: isSecureContext false / null principal on pyxis:// (2026-07-05, found live, not by inspection)

With the `URI_LOADABLE_BY_SUBSUMERS` fix above landed and rebuilt,
`pyxis://testapp/index.html` finally loaded real content end-to-end in a
real running build (fresh tab, never-navigated-before, to rule out stale
browsing-context state) — registration, origin-map resolution, path
confinement, and content serving all genuinely work. But
`window.isSecureContext` read `false`, which should be `true` (we explicitly
registered `URI_IS_POTENTIALLY_TRUSTWORTHY` for exactly this reason — SW,
`crypto.subtle`, and other modern APIs the app runtime needs). Confirmed via
the Browser Console that the flag itself genuinely is registered and
readable (`URIChainHasFlags(uri, URI_IS_POTENTIALLY_TRUSTWORTHY)` → `true`),
so the flag registration was never the problem. The real signal:
`gBrowser.selectedBrowser.contentPrincipal.originNoSuffix` read
`moz-nullprincipal:{uuid}`, not `pyxis://testapp` — the document's actual
security principal was an opaque null principal, and `isSecureContext:false`
is the *correct* value for a null principal. So the actual bug was "why is
the principal null," not anything about `isSecureContext` itself.

7. **`NS_NewURI` doesn't key URI-class selection off registered protocol
   flags — that was a wrong assumption baked into `PyxisProtocolHandler.h`'s
   own scaffold notes.** Traced the principal-computation path
   (`Document::Reset` → `nsScriptSecurityManager::GetChannelResultPrincipals`
   → `GetChannelResultPrincipal` → `GetChannelURIPrincipal` →
   `BasePrincipal::CreateContentPrincipal(nsIURI*, ...)` →
   `ContentPrincipal::GenerateOriginNoSuffixFromURI`). That last function
   (`caps/ContentPrincipal.cpp`) does a `do_QueryInterface` of the URI to
   `nsIStandardURL` and returns `NS_ERROR_FAILURE` if that fails — no
   warning, no assertion in release builds. `BasePrincipal::
   CreateContentPrincipal` (`caps/BasePrincipal.cpp`) silently treats that
   failure as "must be an opaque-origin scheme" and falls back to
   `NullPrincipal::Create()`. Checked what `NS_NewURI`
   (`netwerk/base/nsNetUtil.cpp`) actually constructs for `pyxis:` — it is a
   hardcoded, scheme-by-scheme `if (scheme.EqualsLiteral("..."))` chain, not
   a lookup keyed off `components.conf`'s registered flags.
   `URI_STD` is literally `= 0` in `nsIProtocolHandler.idl` — a documentary
   value only, consumed solely by `URIChainHasFlags`-style flag queries, not
   by any URI-class-selection logic. Since `pyxis` wasn't one of the
   hardcoded cases, every `pyxis://` URI fell through to the
   `network.url.useDefaultURI` branch (on by default) and got constructed as
   a `DefaultURI` (`netwerk/base/DefaultURI.h`) — whose interface table is
   only `nsIURI` + `nsISerializable` + `nsISizeOf`. No `nsIStandardURL`,
   ever, no matter what flags components.conf registers. `moz-extension:`
   never hits this because it has its own hardcoded case in the same chain
   that calls `ExtensionProtocolHandler::NewURI`, whose URI type
   (`SubstitutingURL`, `netwerk/protocol/res/SubstitutingProtocolHandler.cpp`)
   literally subclasses `nsStandardURL`.

   Fix: added a new hardcoded case to `NS_NewURI` for `scheme.EqualsLiteral
   ("pyxis")`, right next to the existing `indexeddb`/`uuid` case (same
   `nsStandardURL` + `URLTYPE_AUTHORITY` construction, default port `-1` to
   match `components.conf`'s `default_port: -1`). This is a genuine
   stock-file change (`netwerk/base/nsNetUtil.cpp` is not under
   `netwerk/protocol/pyxis/`) — no components.conf-only fix exists, because
   `URI_STD` was never actually wired to anything that picks a concrete
   nsIURI implementation. Mirrored as a diff-note (not a full-file copy,
   since this is a ~4200-line stock file) at
   `gecko/patches/pyxis/netwerk/base/nsNetUtil.cpp.PYXIS_DIFF`, matching the
   existing convention from `toolkit/actors/
   ActorManagerParent.sys.mjs.PYXIS_DIFF`. Also corrected
   `PyxisProtocolHandler.h`'s point-5 scaffold note, which had asserted
   (incorrectly, and without ever being tested against a real build until
   now) that `URI_STD` handlers get "generic nsIStandardURL construction for
   free" from `NS_NewURI` — that claim is what let this bug go unnoticed
   through the whole compile-and-first-load cycle.

   Not yet re-verified against a real compiler/browser run (next build not
   done as of this writing) — see "Exact next commands" below. Verification
   plan: reload `pyxis://testapp/index.html` in a fresh tab and check both
   `window.isSecureContext` (expect `true`) and
   `gBrowser.selectedBrowser.contentPrincipal.originNoSuffix` (expect
   `"pyxis://testapp"`, not `moz-nullprincipal:...`).

## Build status: WORKING as of 2026-07-05 (Phase 0, vanilla Gecko)

`MOZCONFIG=/home/andrew/geckos/geckos/gecko/mozconfig-vela ./mach build`
completes cleanly on branch `central-work` (mozilla-central @
`5836a062726f715fda621338a17b51aff30d0a8c`).
`obj-pyxis-webapp/dist/bin/vela --version` reports
`Mozilla Firefox 142.0a1` — confirming the app-rename (`--with-app-name`)
took effect. This was Phase 0 (vanilla/unmodified Gecko, no pyxis patches
wired into `DIRS`/`ActorManagerParent.sys.mjs` yet) — see "Where things stand
now" below for what's been added since.

Two mozconfig flags had to be removed to get here (both are Linux/modern-tree
incompatibilities, not Pyxis-specific bugs — see `mozconfig-vela`'s
inline comments): `--disable-maintenance-service` (Windows-only, not a
registered option on Linux) and `--enable-webrender` (fully removed from the
tree; WebRender is unconditional now).

## Where things stand now (patch series wired in, not yet rebuilt)

Since the Phase-0 build succeeded, the full `pyxis://` protocol handler +
bridge actor patch series has been copied into the real checkout and
**reviewed line-by-line against real mozilla-central headers/IDL** (not just
the ESR-140-era reading the scaffold was originally written against). Real
bugs were found and fixed in this pass — see "Bugs found during real-tree
review" below. The next step is simply to rebuild
(`./mach build` again — should be a fast incremental build) and see whether
it compiles; the C++ has never actually been through a compiler yet.

## What's already done (original scaffolding session)

- **Source cloned**: shallow, single-branch clone at
  `target/gecko-src/gecko-dev/` (gitignored via the existing `/target/`
  rule — verified), later widened to `master`/mozilla-central and unshallowed
  (see `GECKO_PIN`). Checked out on local branch `central-work`.
- **mozconfig written**: `gecko/mozconfig-vela`, validated flag names
  against the real tree where practical (kept a few items as explicit
  "verify at build-config time" notes where the real answer depends on
  `./mach configure` succeeding, which needs the full bootstrap).
- **`pyxis://` protocol handler scaffold**: `gecko/patches/pyxis/netwerk/protocol/pyxis/`
  — `PyxisProtocolHandler.h/.cpp`, `PyxisChannel.h/.cpp`,
  `PyxisOriginMap.h/.cpp`, `components.conf`, `moz.build`. Written against
  REAL interfaces read directly from the pinned tree (not guessed) — see
  "Key findings" below for what changed versus the plan docs' generic
  sketches.
- **`window.geckoOS` bridge scaffold**: `gecko/patches/pyxis/toolkit/actors/`
  — `PyxisBridgeChild.sys.mjs`, `PyxisBridgeParent.sys.mjs`. Written as
  plain-JS `JSWindowActor` subclasses (the REAL mechanism, confirmed by
  reading `toolkit/actors/RemotePageChild.sys.mjs` and
  `toolkit/actors/WebChannelChild.sys.mjs`/`WebChannelParent.sys.mjs` in the
  pinned tree) — NOT compiled C++, which an earlier draft of this scaffold
  incorrectly assumed before the real tree was available to check against.
- **`gecko/src/gecko.rs` repurposed**: the old libxul-FFI scaffolding is
  gone. It's now `vela` launch-composition logic
  (`compose_launch`/`WebAppLaunchSpec`/`LaunchEnvironment`/`ComposedLaunch`),
  matching `echo/src/services/apphost.rs`'s real `AppManifest` shape
  (`exec`/`args`/`env`) field-for-field. Wired into `gecko/src/lib.rs`
  (previously `gecko.rs` wasn't even in the module tree — dead code).
  `gecko/bin/gecko-shell.rs` is now a working dev CLI
  (`gecko-shell compose --app-id <id> [--entry <path>] [--exec-only]`).
  **This part is real and verified**: `cargo check --workspace` and
  `cargo test -p geckoOS-gecko-integration --lib` both pass (6/6 tests,
  including 3 new ones); the CLI was run end-to-end and produces a correct
  composed command.

## Key findings that corrected the plan docs' generic sketches

Reading the real pinned tree (not guessing) surfaced several concrete
corrections worth knowing before you touch the C++/JS further — each is
also called out inline as a "SCAFFOLD NOTICE" comment in the relevant file:

1. **`nsIProtocolHandler` at this ESR has only 3 members**: `scheme`,
   `newChannel(uri, loadinfo)`, `allowPort(port, scheme)`. No `NewURI`, no
   `GetProtocolFlags`, no `GetDefaultPort` on the interface — all of
   flags/default-port are `components.conf`'s `protocol_config` table only.
   (`netwerk/base/nsIProtocolHandler.idl` in the checkout.)
2. **No `URI_SANDBOXED` flag exists** in this ESR's flag set at all — drop
   it from any mental model; the equivalent posture comes from simply not
   setting `URI_LOADABLE_BY_ANYONE`/`URI_LOADABLE_BY_SUBSUMERS`.
3. **The real closest precedent is `moz-extension:`**
   (`netwerk/protocol/res/ExtensionProtocolHandler.{h,cpp}` +
   `SubstitutingProtocolHandler.{h,cpp}`), registered with
   `URI_STD | URI_IS_LOCAL_RESOURCE | URI_IS_POTENTIALLY_TRUSTWORTHY | URI_HAS_WEB_EXPOSED_ORIGIN`
   — almost the exact flag set Pyxis wants, and it already solves the
   sandboxed-content-process file-read problem via `NeckoParent` IPDL
   methods (`NewFD`/`SubstituteRemoteFileChannel`). **This is the real
   mechanism `PyxisChannel` still needs to copy for the content-process
   case — not yet implemented in this pass, see "What's NOT done" below.**
4. **`nsIStandardURLMutator::Init(urlType, defaultPort, spec, charset, baseURI)`**
   is the real mutator call, not a fluent `.SetSpec()` chain.
5. ~~Plain `URI_STD` handlers don't need a custom `NewURI` — the netwerk core
   builds the `nsIStandardURL` generically from the registered flags.~~
   **CORRECTED, 2026-07-05 — this was wrong** (found live: see the
   "isSecureContext false / null principal" section above, item 7).
   `NS_NewURI` (`netwerk/base/nsNetUtil.cpp`) does not consult registered
   protocol flags at all when picking a concrete `nsIURI` class — it's a
   hardcoded scheme-by-scheme dispatch, and `pyxis` wasn't in it, so every
   `pyxis://` URI silently became a `DefaultURI` (no `nsIStandardURL`) no
   matter what `components.conf` registered. Fixed by adding an explicit
   `pyxis` case to that dispatch (mirrored at
   `gecko/patches/pyxis/netwerk/base/nsNetUtil.cpp.PYXIS_DIFF`). Pyxis still
   doesn't need a *custom* `NewURI` override on `PyxisProtocolHandler`
   itself — app-id resolution still correctly defers to channel-open time
   (`PyxisChannel`) — the fix lives in the shared dispatch function, not in
   a per-handler override.
6. **`nsBaseChannel` (`netwerk/base/nsBaseChannel.h`) is the real base class**
   for "stream bytes for a URI" channels — subclasses implement one method,
   `OpenContentStream`. `netwerk/protocol/file/nsFileChannel.h/.cpp` is the
   closest real local-file-backed example and was used directly as the
   pattern for `PyxisChannel`.
7. **JSWindowActors are plain JS `.sys.mjs` ES modules**, not compiled C++,
   registered declaratively in `toolkit/modules/ActorManagerParent.sys.mjs`'s
   `JSWINDOWACTORS` object (see the real `WebChannel`/`ViewSourcePage`
   entries there). `Cu.exportFunction` + `Cu.cloneInto` inside `actorCreated()`
   is the real mechanism for injecting a privileged API object into content
   — see `toolkit/actors/RemotePageChild.sys.mjs`.
8. **`moz-extension:` is registered from the top-level
   `netwerk/build/components.conf`**, while `moz-gio` has its own
   directory-local `components.conf` (`netwerk/protocol/gio/`) — both
   patterns are real; Pyxis follows the self-contained `gio`-style pattern
   (own `components.conf` in `netwerk/protocol/pyxis/`) since it keeps the
   whole patch series confined to one new directory + a one-line `DIRS`
   edit, matching the plan's "patch-series discipline" instruction.

## Bugs found during real-tree review (2026-07-05, before first compile attempt)

The original scaffold was written by reading docs/headers, never compiled.
Re-checking it against the actual `central-work` tree (not just reasoning
about the ESR-140 tree from memory) surfaced three real, would-not-compile
bugs, all now fixed in both the live checkout and the `gecko/patches/pyxis/`
source-of-truth copies:

1. **`components.conf`'s CID was a literal placeholder**
   (`{00000000-0000-0000-0000-000000000pyx}` — not even valid hex). Replaced
   with a real generated UUID: `{08e06cca-c7c0-48f4-aad8-0421abe2e026}`.
2. **`NS_LITERAL_CSTRING_FROM_CSTRING` does not exist anywhere in this tree**
   (`PyxisChannel.cpp`'s traversal-defense check used it to wrap a `".."`
   literal for `nsACString::Find`). `Find()` takes a `string_view` and a bare
   string literal converts directly — fixed to `path.Find("..")`.
3. **`NS_NewLocalFile` takes UTF-16 (`nsAString`), not UTF-8/native
   (`nsACString`)** — three call sites (`PyxisChannel.cpp` x2,
   `PyxisOriginMap.cpp` x1) passed `nsCString`/`nsDependentCString` values
   straight to it, which doesn't compile. Fixed by switching to
   `NS_NewUTF8LocalFile` (the UTF-8-native counterpart that exists
   specifically for this), no `NS_ConvertUTF8toUTF16` detour needed.

Everything else in the scaffold (the `nsIProtocolHandler`/`nsBaseChannel`
API shape, the `components.conf` flag set, the `ActorManagerParent`
registration shape, `toolkit/actors/moz.build`'s file-list convention) was
checked against the real `central-work` tree and found unchanged from the
ESR-140-era reading — no drift there.

## What's NOT done (real, non-trivial follow-up — not just formatting)

- ~~**The parent-brokered file-read for sandboxed content processes**
  (`pyxis-scheme-and-kiosk-plan.md` §A4, "the single highest-risk item").
  `PyxisChannel::OpenContentStream` currently only implements the
  parent-process-safe direct-file-open path.~~ — **implemented 2026-07-05**,
  see "Parent-brokered channel (IPDL) implemented" above. New `PPyxisChannel`
  protocol (`manager PBackground`) + `PyxisChannelChild`/`PyxisChannelParent`
  actors + shared `PyxisChannelUtils` helper. Not independently
  compiler-verified this session (no `./mach build` run for this piece) —
  see that section for the exact rebuild command and test plan, which
  specifically calls for re-enabling multiprocess (i.e. NOT forcing e10s off
  the way earlier single-process-mode testing did specifically to route
  around this exact gap) before retesting `pyxis://testapp/index.html`.
- **`PyxisOriginMap`'s inotify watch** (`StartWatching()`) is stubbed —
  `ReloadNow()` works and is called at construction, but nothing currently
  triggers a re-read when Nova updates `/run/pyxis/nova/origins.map` after
  this session's initial load. Cheapest real fix candidate noted inline:
  have the bridge parent actor call `ReloadNow()` on a `nova.AppInstalled`
  Echo signal instead of implementing raw inotify — the Echo transport this
  depends on now exists (see next bullet and "Echo transport implemented"
  above), so this is now blocked only on Nova itself existing and emitting
  that signal.
- ~~**`PyxisBridgeParent`'s Echo transport is a stub that throws.**~~ —
  **implemented 2026-07-05**, see "Echo transport implemented" above. Chose
  option (a) (privileged JS Unix-socket client via
  `nsISocketTransportService`), not option (b) (XPCOM wrapper crate) — not
  independently compiler-verified this session (no `./mach build` run), but
  grounded directly in the real XPConnect marshalling code and IDL, not
  guessed.
- **The capability allow-list (`_getDeclaredPermissions`) always returns
  `[]`** (deny-by-default for anything gated) because Nova — the thing that
  would own the real permissions registry — doesn't exist yet
  (`nova-runtime-host-plan.md` is design-only). The gate itself
  (`_capabilityFor` + the refusal check in `_handleCallService`) is real and
  wired; only the data source is a placeholder.
- ~~**The kiosk run mode itself** (`vela --app=<id>`, chrome
  stripping, `WM_CLASS`/title stamping) — not scaffolded~~ — **implemented
  2026-07-05**, see "Kiosk run mode implemented" above. Still open within
  it: `PYXIS_BACKGROUND` headless apps, navigation lock-down, and — like
  everything else in this list — not yet compiled/run against a real build.
- ~~UUIDs/CIDs in `components.conf` are placeholders~~ — **fixed 2026-07-05**,
  see "Bugs found during real-tree review" above.

## Exact next commands, in order

### Steps 1-4 (unshallow, bootstrap, copy patch series, Phase-0 vanilla build) — DONE

Superseded by the "Build status" and "Where things stand now" sections above.
The one difference from the original plan: `DIRS`/`ActorManagerParent`
registration has now ALSO been wired in (not held back for a separate
second build as originally planned) — since the review pass already found
and fixed the compile bugs by inspection, there's no remaining value in a
separate unregistered-files build first.

### 5. Rebuild with the patch series wired in — the actual next step

```
cd /home/andrew/geckos/geckos/target/gecko-src/gecko-dev
MOZCONFIG=/home/andrew/geckos/geckos/gecko/mozconfig-vela ./mach build
```

This is the **first time this C++ has ever been through a compiler.** The
review pass fixed everything found by inspection, but inspection isn't
compilation — expect this to surface at least a few more real errors
(missing includes, const-correctness, signature mismatches on methods not
checked line-by-line). Should be a fast incremental build off the
already-built Phase-0 objdir, not a full rebuild.

Once it compiles, work through `pyxis-scheme-and-kiosk-plan.md` §A6
(gtest/xpcshell/mochitest for the protocol handler: origin correctness,
secure context, SW registration, per-app storage isolation,
traversal-defense battery) and §B5 (kiosk mode acceptance — implemented but
unverified, see "Kiosk run mode implemented" above) in that order.

### 6. Manual kiosk-mode test, once step 5 builds

Generate a real composed command with the dev CLI (see "Where the dev-CLI
fits right now" below for what this does), then run it:

```
cd /home/andrew/geckos/geckos
cargo run -p geckoOS-gecko-integration --bin gecko-shell -- \
    compose --app-id testapp --entry index.html --exec-only
```

This prints a copy-pasteable `KEY=VAL ... /path/to/vela --pyxis-app testapp`
line (NOT `--app testapp` — see the dated "OS integration" section below
for why; requires `WAYLAND_DISPLAY` set and `VELA_BIN` pointed at
`obj-pyxis-webapp/dist/bin/vela` if not installed to
`DEFAULT_VELA_PATH`). Run that line as-is under a live Evo session.
Expected, per this section's design above: exactly one chromeless window
opens showing a load error for `pyxis://testapp/index.html` (a real
`testapp` origin/webroot won't exist yet unless Part A's origin map has one
registered — that's fine, the point of this test is chrome/WM_CLASS, not
content) with no tab strip, no URL bar, no menu bar, no bookmarks toolbar.

Verify WM_CLASS/app_id from outside the process — `xprop WM_CLASS` under
XWayland, or, more relevantly for Evo, check what Evo itself logs for the
`app_id` on `new_toplevel` (`evo/src/enhancements/echo_ipc.rs`'s tracked
`Window creation` log line, per the "Bugs found"-style logging already in
that file) — it should read exactly `testapp`, not a transformed/uppercased
variant.

## Where the dev-CLI fits right now

`cargo run -p geckoOS-gecko-integration --bin gecko-shell -- compose --app-id <id> --entry <file> [--exec-only]`
composes and prints the exact env+argv a real Nova would eventually hand to
`appHost.LaunchApp`. Useful today for sanity-checking the launch contract
independent of whether `vela` exists yet — point
`VELA_BIN` at your `obj-pyxis-webapp/dist/bin/vela` once
step 4 produces one, and `--exec-only` gives you a copy-pasteable shell
command to run it by hand under Evo.

## OS integration: wiring vela into the real boot image (2026-07-05/06)

Once the Gecko build itself worked, the next ask was "wire it into the
actual OS build so we can run a full test" — i.e. make `vela`
part of `scripts/create-rootfs.sh`'s image and have `geckoOS-boot` launch
it for real, rather than testing only on the host desktop. Three real bugs
surfaced doing this, none catchable without an actual boot:

1. **`stage_gecko()` was still the ABANDONED libxul-FFI-embedding stage**
   (downloading a stock Firefox ESR tarball and cherry-picking `.so`
   files) — stale since the pivot to the from-source `vela`
   kiosk-mode approach. Rewritten to install the real
   `obj-pyxis-webapp/dist/bin/` tree to `/usr/lib/pyxis-gecko/` (the exact
   path `gecko/src/gecko.rs`'s `DEFAULT_VELA_PATH` expects), with
   in-place `strip --strip-unneeded` on the copied ELF files (a dev objdir
   build embeds full debug symbols — `libxul.so` alone is ~3.4GB
   unstripped). Added a new `stage_pyxis_smoketest_app` stage installing
   the already-host-validated test app
   (`target/pyxis-test-app/webroot/index.html`), and rewired
   `geckoOS-boot`'s `gecko` service entry (previously a no-op bare
   `gecko-shell`, which only PRINTS a composed command and never spawns
   anything) to actually write a stand-in `/run/pyxis/nova/origins.map`
   and launch `vela` with the real `compose_launch` env-var
   contract. Also bumped `scripts/package.sh`'s default `IMAGE_SIZE` from
   2048MB to 6144MB — the old default had no realistic headroom once a
   real Gecko runtime is in the image.

2. **"Couldn't load XPCOM" — two distinct real causes, found in sequence
   across two rebuild-and-boot cycles:**
   - **Missing GTK3/desktop-stack system libraries.** The rootfs's package
     list was written for Evo alone (which never touches GTK), but
     `vela` uses `--enable-default-toolkit=cairo-gtk3-wayland` and
     dynamically loads `libmozgtk.so`/`libxul.so`, which need the full
     GTK3 stack at runtime. Confirmed via `ldd` directly against the real
     build output (not guessed): GTK3, ATK/AT-SPI (accessibility), epoxy
     (GL dispatch), seccomp (Gecko's own sandboxing), lcms2 (color
     management), ALSA, glycin, systemd libs, and a long tail of X11
     client libraries were all absent. Added `libgtk-3-0`,
     `libatk-bridge2.0-0`, `libatspi2.0-0`, `libepoxy0`, `libseccomp2`,
     `liblcms2-2`, `libsystemd0`, ALSA/glycin (probed for the right
     package name across Debian's t64 transition, same pattern as the
     existing `DISPLAY_INFO_PKG` probe), `libwayland-egl1`,
     `libwayland-cursor0`, and explicit X11 client libs to
     `stage_install_base_packages()`.
   - **`rsync -a` preserves symlinks as symlinks — several in `dist/bin`
     point OUTSIDE the directory.** Even after the packages above were
     fixed, "Couldn't load XPCOM" persisted. `dist/bin` has real symlinks
     to sibling objdir/source-tree paths (`application.ini ->
     ../../build/application.ini`, `dependentlibs.list ->
     ../../toolkit/library/build/dependentlibs.list`) and even absolute
     host paths (`libonnxruntime.so ->
     /home/.../onnxruntime-x86_64-linux-gnu/libonnxruntime.so`,
     `fix_stacks.py`, `dmd.py`) — none of which exist inside the rootfs,
     since only `dist/bin/`'s own contents get copied. Copied verbatim via
     plain `rsync -a`, every one of these becomes a dangling symlink once
     moved to a different root filesystem. `application.ini` specifically
     is what XPCOM reads at startup to locate the Gecko Runtime
     Environment — a dangling symlink there is exactly what "Couldn't
     load XPCOM" means, and it explains why the error persisted even after
     every real shared-library dependency was satisfied. Fixed by changing
     `stage_gecko()`'s copy to `rsync -aL` (dereference symlinks into real
     file copies). Verified via `find . -xtype l` in the source tree that
     zero symlinks are actually broken on the host (all ~6300, at any
     depth, resolve successfully at copy time) — `-L` alone was sufficient,
     no manual per-file fixup needed.

3. **`--app` collides with a pre-existing NATIVE Gecko flag, unrelated to
   this JS-level kiosk-mode work.** After both fixes above, the app still
   failed immediately with `Error: unrecognized path: testapp` — traced
   directly to `browser/app/nsBrowserApp.cpp:167`:
   `argc > 1 && IsArg(argv[1], "app")`. If `argv[1]` is `-app`/`--app`,
   Gecko's native startup code (BEFORE XPCOM/JS ever initializes) treats
   `argv[2]` as a path to an ALTERNATE `application.ini` file — an
   existing embedding/XULRunner-style feature, completely unrelated to
   Pyxis. `XRE_GetFileFromPath("testapp", ...)` fails (not a valid path),
   and `toolkit/xre/nsAppRunner.cpp:5975` prints exactly
   `Error: unrecognized path: %s` and exits — the kiosk-mode JS handler in
   `BrowserContentHandler.sys.mjs` never even gets a chance to run. This
   was a real design bug in the original kiosk-mode pass (an agent that,
   per its own report, could not compile-test or run the binary, so this
   kind of native-argv-parsing collision was unreachable by inspection
   alone). Fixed by renaming the flag to `--pyxis-app` everywhere it
   appears: `BrowserContentHandler.sys.mjs`'s `handleFlagWithParam` call,
   its `--help` text, its doc comments, `nsIBrowserHandler.idl`'s
   `pyxisAppId` doc comment, `gecko/src/gecko.rs`'s `compose_launch`
   (including its own unit test asserting on the composed args),
   `scripts/create-rootfs.sh`'s boot invocation, and `apps/browser/launch`'s
   forward-looking comment. `--app-entry` (a separate flag) does not
   collide and was left unchanged. Re-ran `cargo test -p
   geckoOS-gecko-integration --lib` after the rename — all 6 tests pass.

4. **`geckoOS-boot` launched `vela` as root, and Gecko refuses to
   run as root.** After the `--pyxis-app` rename, the next real boot
   failed with `Running Nightly as root in a regular user's session is
   not supported` — `geckoOS-boot` itself runs as root (started by
   BusyBox init), so everything it `start_daemon`s, including
   `vela`, inherited that. `$XDG_RUNTIME_DIR=/run/user/1000` is
   (correctly) owned by `geckouser`, not root, which is exactly what
   Gecko's own root-detection check flags as unsupported. Fixed by
   dropping privileges specifically for the `vela` service via
   `setpriv --reuid=1000 --regid=1000 --clear-groups`, NOT `su`: this is a
   minimal debootstrapped rootfs with no `/etc/pam.d/` configuration
   anywhere, and `su` goes through PAM for session setup — a second,
   different failure mode that would have been just as opaque. `setpriv`
   (util-linux, already installed) drops privileges via direct
   `setuid()`/`setgid()` syscalls with no PAM/login-session machinery at
   all, which is the correct tool for a non-interactive boot script.
   `/var/lib/pyxis/profiles/testapp` is now also `chown 1000:1000` (it was
   root:root before, which would have blocked geckouser from writing into
   its own profile dir once running as a non-root user).

5. **`vela` raced ahead of Evo's own compositor startup.** After
   fix 4, the next real boot failed with `Failed to connect to Wayland
   display '/run/user/1000/wayland-0' ... No such file or directory`.
   Evo (`evo/src/main.rs`) does correctly read `$WAYLAND_DISPLAY` and
   default to `wayland-0` — confirmed by reading the source, ruling out a
   naming mismatch — so this was pure timing: `geckoOS-boot` only put a
   fixed `sleep 1` after `start_daemon evo`, unlike the `wait_for_socket`
   check it already uses for echo's socket a few lines above. Evo's own
   init (DRM/GPU probing, seat enumeration) can take longer than a flat
   1-second sleep, especially cold under QEMU/virtio-gpu. Fixed by
   applying the exact same `wait_for_socket` pattern to Evo's Wayland
   socket (`${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}`, 40 tries / ~20s) that
   was already sitting right there for echo — this should have been
   there from the start.

6. **The `wait_for_socket` fix for #5 didn't help — because Evo was never
   going to create the socket at all.** The exact same Wayland-connect
   error persisted even with the longer wait, which ruled out timing and
   meant Evo was failing before reaching that point. Evo's `--tty-udev`
   backend (`evo/src/udev.rs`) uses `LibSeatSession::new()` (the `libseat`
   crate) for VT/device access mediation — this needs EITHER a running
   `seatd` daemon OR systemd-logind. This image has neither: `seatd`
   (the package) was already in the apt install list, but nothing ever
   actually STARTED the daemon, and there's no systemd-logind at all
   (BusyBox is PID 1). Confirmed by downloading and inspecting the real
   `seatd` `.deb` (not guessed): the daemon binary is `/usr/sbin/seatd`,
   its own upstream systemd unit invokes it as `seatd -g video`, and
   `strings` against the binary confirms its default socket path is
   `/run/seatd.sock`. Fixed by starting `seatd -g video` before Evo (with
   the same `wait_for_socket` pattern, on `/run/seatd.sock`), and setting
   `LIBSEAT_BACKEND=seatd` explicitly on Evo's environment so it doesn't
   spend any time on logind auto-detection first (there's also no
   dbus-daemon running anywhere in this image, confirmed by grepping this
   whole script for it — that backend attempt would fail immediately
   rather than hang, but being explicit removes the ambiguity entirely).

7. **Evo binds `wayland-1` but advertises `wayland-0` — the actual root
   cause of the whole "no wayland socket" saga.** With seatd fixed, the
   real `evo.log` finally showed Evo starting up COMPLETELY successfully —
   seat acquired, `/dev/dri/card0` opened, EGL/virgl renderer initialized
   (`GL Renderer: virgl`), DRM output configured at 640x480, XWayland
   spawned. Everything worked. The one line that mattered:
   `Created new socket name=Some("wayland-1")`. Evo binds **wayland-1**,
   but `geckoOS-boot` tells `vela` to connect to **wayland-0**.
   Root cause is a real Evo bug: `evo/src/main.rs` reads
   `WAYLAND_DISPLAY` (defaulting to `wayland-0`), logs "Wayland socket:
   wayland-0", and sets it in its own env — but `evo/src/state.rs`'s
   `init()` then called `ListeningSocketSource::new_auto()`, which IGNORES
   that name and auto-picks. Smithay's `new_auto()` is
   `bind_auto("wayland", 1..33)` — the range STARTS AT 1, deliberately
   (its own source comment: "We don't try wayland-0 since clients may
   connect to the wrong compositor... clients should be connecting based
   off the WAYLAND_DISPLAY... environment variables"). So Evo could NEVER
   bind wayland-0 via that path; it always got wayland-1, while every
   client was told wayland-0. This is also why the earlier "Evo worked"
   claims were misleading — Evo's compositor always came up fine; the
   socket-name desync just meant nothing could connect to it. Fixed in
   `evo/src/state.rs` by binding the exact advertised name via
   `ListeningSocketSource::with_name(&desired)` (reading `WAYLAND_DISPLAY`,
   which main.rs already set), with a fallback to `new_auto()` +
   re-syncing `WAYLAND_DISPLAY` to the actually-bound name if the desired
   name is somehow taken. This is a real source change to Evo (not the
   boot script), so it required `cargo build --release -p geckoOS-evo`
   (done, compiles clean) and a re-run of `stage_binaries` in
   create-rootfs.sh to pick up the new `target/release/evo`.

8. **Root-owned Wayland socket, unprivileged client → "Permission
   denied".** With fix #7, Evo bound `wayland-0` and the client error
   changed from "No such file or directory" to `Permission denied` — the
   socket now existed, but couldn't be connected to. Evo runs as **root**
   (needs it for the compositor / device path), so it creates
   `/run/user/1000/wayland-0` owned `root:root` at mode ~0755 (root's
   umask). `vela` runs as **geckouser** (uid 1000) because Gecko
   refuses to run as root (fix #4) — and connecting to a Unix socket
   requires *write* permission on the socket file, which "other" doesn't
   get from 0755. Fixed in `geckoOS-boot` by `chown 1000:1000` on the
   Wayland socket right after `wait_for_socket` confirms it exists (plus
   Echo's socket + dir, so the `window.geckoOS` bridge can reach Echo on
   the first `callService` without hitting the identical problem), and by
   setting `HOME=/home/geckouser` in vela's env (setpriv doesn't
   change HOME, so it would otherwise inherit root's `/root` and every
   GTK/Gecko `$HOME/.cache`/dconf write would be denied — the same
   permission class, one step later, headed off proactively). Noted inline
   as the pragmatic bring-up fix, not the final architecture: the proper
   model runs the whole session (evo, echo) as geckouser with only
   seatd/udevd as root, so every session socket is geckouser-owned from
   creation — deferred until the smoke test passes to avoid a larger
   refactor (seatd video-group membership, echo/shift root-need audit).
   This is boot-script-only (no rebuild of Evo or Gecko).

9. **The window OPENED — "Profile Missing" + runtime-env polish.** With
   fix #8, vela connected to Wayland and Evo composited a real
   window (visible under QEMU). The one remaining blocker was a modal
   dialog: "Your Firefox profile cannot be loaded. It may be missing or
   inaccessible." Root cause: `PYXIS_PROFILE_DIR` is only an ENV var —
   nothing in Gecko natively consumes it to SELECT the profile — so
   Firefox fell back to default `$HOME/.mozilla/firefox/profiles.ini`
   resolution and failed. Fixed by passing Gecko's native `-profile
   <path>` flag (parsed by nsAppRunner before the JS command-line handler,
   so it composes fine with `--pyxis-app`), pointed at the same per-app
   dir `PYXIS_PROFILE_DIR` names (already pre-created + chowned to
   geckouser 700) — which is also the correct per-app storage isolation
   the pyxis:// origin model wants, not just a workaround. Same pass, the
   non-fatal noise the real boot's log surfaced was cleaned up too:
   pre-create `/home/geckouser/.cache/fontconfig` + `.config` owned
   geckouser (the "Fontconfig error: No writable cache directories"), add
   `libpci3` (the `glxtest: libpci missing` GraphicsCriticalError), add
   `fontconfig` + `fonts-dejavu-core` + a chroot-time `fc-cache -f -s` (so
   the system cache is pre-built world-readable AND there's at least one
   real font family so the test page's text actually renders), and set
   `HOME=/home/geckouser` (already done in #8). Boot-script + package-list
   only, no Evo/Gecko rebuild.

None of these nine were catchable by inspection or unit tests — each one
only surfaced on a real boot, and each one was root-caused by reading the
actual failing code path (Gecko's real `ldd` output, the real symlinks in
`dist/bin`, the real `nsBrowserApp.cpp` argv-parsing, Evo's real
`LibSeatSession` dependency, Evo's own `evo.log` showing the
`wayland-1`-vs-`wayland-0` socket-name desync, the root-vs-geckouser
socket-ownership mismatch, and Firefox's env-var-vs-`-profile`-flag profile
selection) rather than guessing from the generic error text alone.

## In-progress: "Profile Missing" persists even with `-profile` (2026-07-06)

Fix #9's `-profile <dir>` did NOT resolve the dialog — `/var/lib/pyxis/
profiles/testapp` still has nothing in it after a boot but our own
manually-placed `user.js` (no `prefs.js`, `.parentlock`, `times.json` --
proof Firefox never actually initialized this directory as a profile).
Traced the real call chain in `toolkit/xre/nsAppRunner.cpp`/`toolkit/
profile/nsToolkitProfileService.cpp`: `ProfileMissingDialog` fires from
TWO different call sites -- (A) if `NS_GetToolkitProfileService()`'s
`Init()` fails (which unconditionally needs to create/read `$HOME/.mozilla/
<appname>/{profiles,installs}.ini`, regardless of `-profile`), or (B) if
`SelectStartupProfile()` (the function that actually reads `-profile`)
fails for some other reason. Neither of Firefox's own two hardcoded
`PR_fprintf(PR_STDERR, ...)` error strings for the `-profile`-specific
failure modes appeared in the log, which doesn't fit either fully-expected
path -- and `NS_ERROR()` (used to report `Init()` failure) is compiled out
in this release build, so a path-A failure would be entirely silent.

Added TEMPORARY, unconditional `fprintf(stderr, "[PYXIS-DIAG] ...")`
diagnostic instrumentation (marked for removal once root-caused, NOT a
permanent patch -- no `.PYXIS_DIFF` mirror created for it on purpose) at
every real decision point: `nsToolkitProfileService::Init()`'s directory
resolution steps and success/failure return, `NS_GetToolkitProfileService`'s
own failure branch, `XRE_main`'s two `ProfileMissingDialog` call sites, and
-- most importantly -- `SelectStartupProfile`'s `CheckArg(*aArgc, aArgv,
"profile", &arg)` result and the full argv array as Gecko itself sees it,
to check the leading hypothesis: that `-profile` is somehow not even being
recognized in argv (`ar == ARG_NONE`), not that it's failing after being
recognized. Rebuilt cleanly (`./mach build`, 43s incremental -- only these
two files changed). Next step: get a fresh `/var/log/vela.log` with
this build and read the `[PYXIS-DIAG]` lines top to bottom; they will show
exactly which of the two ProfileMissingDialog call sites fired and why.

## Faster iteration: scoped passwordless sudo + rootfs reuse (2026-07-06)

Two changes to close the "edit -> rebuild -> boot -> paste logs back"
round-trip loop, which had been the entire bottleneck all session:

1. **Scoped `NOPASSWD` sudoers entry** for exactly `scripts/create-rootfs.sh`
   and `scripts/package.sh` (not a blanket grant) -- set up by the user,
   e.g. via `sudo visudo -f /etc/sudoers.d/pyxis-build`:
   `andrew ALL=(root) NOPASSWD: /home/andrew/geckos/geckos/scripts/create-rootfs.sh, /home/andrew/geckos/geckos/scripts/package.sh`.
   Important mechanical detail, confirmed via `sudo -l -n`: this only
   covers invoking the SCRIPTS THEMSELVES via `sudo <script>` -- it does
   NOT separately whitelist the many internal `sudo mkdir`/`sudo mount`/
   `sudo chroot`/etc. calls each script makes. Those work anyway because
   once the whole script is already running as EUID 0 (via the whitelisted
   outer `sudo` invocation), Debian/Ubuntu's default `pam_rootok.so` PAM
   config lets root's own `sudo` calls through without any additional
   authentication -- confirmed empirically (`sudo scripts/create-rootfs.sh`
   ran cleanly through many internal `sudo` calls with zero prompts), not
   assumed from the sudoers entry alone.

2. **`stage_prepare_rootfs` now reuses an existing rootfs by default**,
   only wiping it with `--force` (`FORCE_REBUILD=1`). This flag was already
   declared and CLI-settable (`--force`) but never actually consumed
   anywhere -- meaning every single prior run, including plain `make
   build-release` (which calls `create-rootfs.sh --kernel-prebuilt`, no
   `--force`), always re-debootstrapped a fresh Debian base from the
   network regardless of whether anything actually needed it. Every other
   `stage_*` function was already idempotent (`write_rootfs_file`
   overwrites, `rsync -a --delete`/`cp -a` overwrite, `apt install` of
   already-present packages is a fast no-op) -- debootstrap was the one
   genuinely expensive, safely-skippable step. Also made
   `stage_mount_chroot_support`'s bind/proc/sysfs mounts idempotent
   (`mountpoint -q` guards) so re-running against a reused rootfs can't
   hit a hard "already mounted" failure if a prior run's `cleanup_mounts`
   EXIT trap didn't get to fire. Real measured effect: a full
   `create-rootfs.sh --kernel-prebuilt` run against an already-debootstrapped
   base went from many minutes (full network debootstrap + package fetch)
   to well under a minute.

Also found and fixed, while actually exercising a non-interactive run for
real (not by inspection): `package.sh`'s "image already exists" prompt used
`read -p ... ; if [[ ! $REPLY =~ ... ]]` expecting to fall through to a
"skip" branch on a declined/empty answer -- but `read` on a closed/non-tty
stdin (any backgrounded or agent-driven invocation) returns NON-ZERO on
EOF immediately, and this script's `set -e` aborted the whole script right
there, before ever reaching the intended fallback ("Script failed with
exit code 1", no clear message why). Fixed by checking `[ -t 0 ]` first:
keep the interactive confirmation for a real terminal, but default to
overwriting when there's no tty to prompt on, since the disk image is a
fully regenerable build artifact (deterministically rebuilt from the
rootfs/kernel/initramfs every time), not user data worth guarding against
accidental loss.

With both fixes, the full loop -- edit source, `sudo create-rootfs.sh
--kernel-prebuilt`, `sudo package.sh --x86_64-efi`, boot, inspect logs --
can now run without a human in the middle for the rootfs/packaging half.
Booting + reading logs back out of the image (e.g. via `virt-cat`/libguestfs,
already used earlier this session to read files out of a qcow2 image
without mounting) is the remaining piece to close the loop end-to-end.

## Closing the loop: headless boot + the real "Profile Missing" root cause (2026-07-06)

**Headless QEMU boot.** `emulate.sh` always passes `-device virtio-vga-gl`
(GPU device requiring an OpenGL-capable display backend) alongside whatever
`-display "${QEMU_DISPLAY}"` is given. `QEMU_DISPLAY=none` fails outright
("OpenGL is not supported by display backend 'none'") because `none`
provides no GL context at all. `QEMU_DISPLAY=egl-headless` is QEMU's actual
mechanism for exactly this case -- headless (no visible window) but still
GL-capable, so virgl/GPU accel keeps working -- and boots cleanly. This is
what let the rest of this investigation run unattended: boot with a bounded
`timeout`, let it run, then pull `/var/log/*.log` back out of the disk image
via `virt-cat -a <img> -m /dev/sda2 <path>` after the process exits (the
image file itself is untouched by killing QEMU; ext4 tolerates the abrupt
stop fine for read-mostly log files).

One console-visibility gotcha, not a bug: booting headlessly showed kernel
dmesg over serial fine, but NONE of `geckoOS-boot`'s own `echo`/`[boot] ...`
lines ever appeared, and a root shell prompt appeared almost immediately
after the rootfs mounted. This is because `/etc/inittab`'s `::sysinit`,
`::wait`, `::ctrlaltdel`, `::shutdown` entries have empty `id` fields, so
BusyBox init binds their stdio to `/dev/console` (which resolves to the
framebuffer console, tty0 -- invisible over serial/headless), while only
the explicit `tty1`/`ttyS0` `respawn` entries are bound to those literal
devices. The actual useful diagnostics were never in `geckoOS-boot`'s own
echo output anyway -- they're in `/var/log/{evo,vela,...}.log`,
written by each `start_daemon`'s own redirect, unaffected by this.

**The real root cause of "Profile Missing" (finally confirmed, not
guessed).** With the `[PYXIS-DIAG]` instrumentation from the previous
section rebuilt and a real headless boot's `/var/log/vela.log`
read back via `virt-cat`, the very first diagnostic line answered it
directly: `GetUserAppDataDirectory failed: 0x80004005` (`NS_ERROR_FAILURE`)
-- failing inside `nsToolkitProfileService::Init()` itself (failure path A),
*before* `-profile`/`SelectStartupProfile` is ever reached. So every prior
theory about `-profile` argument handling was chasing the wrong stage
entirely.

`nsXREDirProvider::GetUserDataDirectory` (toolkit/xre/nsXREDirProvider.cpp)
resolves `$HOME` (works fine, `HOME=/home/geckouser` is set explicitly in
`geckoOS-boot`), then calls `EnsureDirectoryExists()`, which does
`aDirectory->Create(nsIFile::DIRECTORY_TYPE, 0700)` to create
`$HOME/.mozilla`. `virt-ls -a <img> -m /dev/sda2 -l /home` showed
`/home/geckouser` owned `0:0` (root) inside the *packaged disk image* --
even though the *source rootfs* on the build host (pre-packaging) had it
correctly `1000:1000`, set by `create-rootfs.sh`'s Stage 11
(`stage_permissions`). Since `vela` runs as uid 1000 (via
`setpriv`), it can't write into a root-owned, group/other-unwritable
`/home/geckouser` to create `.mozilla` -- `Create()` fails, `Init()`
returns `NS_ERROR_FAILURE`, `NS_GetToolkitProfileService()` returns null,
and `XRE_main` shows `ProfileMissingDialog` -- entirely before `-profile`
is ever consulted. This is why fix #9 (adding `-profile`) never actually
resolved it: it was never the right stage.

The actual bug: `package.sh`'s disk-image assembly step used
`sudo cp -r "$ROOTFS_DIR"/* "$ROOT_MOUNT/"` to copy the finished rootfs
onto the mounted ext4 partition. Plain `cp -r`, even run as root, does
**not** preserve source ownership by default -- new destination entries
are created owned by the invoking (root) user unless `-a`/`-p`/
`--preserve=ownership` is given. So every deliberately-`chown`'d file in
the source rootfs (most consequentially `/home/geckouser`, set correctly
by Stage 11) silently reverted to root ownership the moment it was copied
into the actual bootable image -- independent of anything `create-rootfs.sh`
did right. Fixed by changing that one call to `cp -a`. Confirmed via
`virt-ls -a <img> -m /dev/sda2 -l /home` before/after: `geckouser` went
from `0 0` to `1000 1000`, and the next boot's `/var/log/vela.log`
showed `Init() returning NS_OK`, `SelectProfile() SUCCEEDED`, and (per the
one-off diagnostic added to `openPyxisAppWindow` in
`BrowserContentHandler.sys.mjs` to settle the last bit of doubt --
confirmed working then removed, not a permanent patch) `openPyxisAppWindow
called: appId=testapp entry=index.html`.

**A second, related permission bug found the same way.** With the profile
fixed, `vela.log` still showed `libEGL warning: failed to open
/dev/dri/renderD128: Permission denied` followed by a Vulkan/ZINK failure
and fallback to software rendering. `geckoOS-boot`'s `setpriv` invocation
used `--clear-groups`, which (correctly, to avoid leaking root's original
supplementary groups into the unprivileged process) drops ALL supplementary
groups -- but that includes `video`/`render`, the groups that actually own
the DRI render node, so `setgid()` alone (primary gid only) was never
suffient regardless. Fixed by replacing `--clear-groups` with
`--groups video,render`, which resolves those group names directly via
`setgroups()` -- independent of whether `geckouser` is literally listed as
a member in `/etc/group` (it isn't; `setpriv --groups` doesn't need that).
Confirmed via the next headless boot: the `/dev/dri/renderD128` permission
warnings and the ZINK Vulkan failure are both gone entirely from
`vela.log`.

**End-to-end confirmed working**, read back from a real headless boot with
no diagnostic instrumentation left in the tree: `vela --pyxis-app
testapp` starts, loads its per-app profile, opens the chromeless kiosk
window (`openPyxisAppWindow`), connects to Evo's Wayland socket, and
`evo.log` shows Evo mapping exactly one new XDG toplevel window into the
workspace and emitting a `WindowCreated` signal over Echo -- the full
`pyxis://` scheme + kiosk-mode + Evo-compositing chain working together in
the actual booted image, not just on the host. All temporary `[PYXIS-DIAG]`
instrumentation (in `nsAppRunner.cpp`, `nsToolkitProfileService.cpp`, and
the one-off `openPyxisAppWindow` dump in `BrowserContentHandler.sys.mjs`)
has been removed; none of it was mirrored into `gecko/patches/pyxis/`
since it was throwaway debug code, not a permanent patch.

Remaining, lower-priority items observed in this same log (not blocking):
Firefox's normal network-dependent startup services (Region, Remote
Settings, TopSitesFeed, addon update checks) fail with `NetworkError`
since this VM has no working route to the real internet endpoints they
target -- expected and non-fatal, not specific to `pyxis://` or this app.
