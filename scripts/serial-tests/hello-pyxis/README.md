# hello-pyxis — the alpha 4 acceptance package

A minimal third-party web app, used as the payload for the `.ipk` end-to-end
test (`../alpha4-ipk-e2e.txt`, `ALPHA_ROADMAP.md` alpha 4 item B). It exists
so that test is reproducible: the flow needs a package that is genuinely
*not* a built-in, built by the real toolchain.

Build it:

```bash
cargo build --release -p geckoOS-vroom     # provides pyxis-sdk
./target/release/pyxis-sdk -p scripts/serial-tests/hello-pyxis package
# -> scripts/serial-tests/hello-pyxis/dist/com.pyxis.hello_0.2.0_all.ipk
```

The page is not just a placeholder. It writes a probe to `~/proof-<ms>.txt`
through `window.geckoOS.filesystem`, one file per launch. `~` is the app's
own storage, so on the device that is
`/var/lib/pyxis/data/com.pyxis.hello/proof-<ms>.txt` (it was `/tmp` until
filesystem calls were confined, on 2026-10-06). The file is how a headless
run tells "a window opened" from "the app actually works":
the file's existence proves the `pyxis://` origin served the document, the
script ran, the bridge attached for a *third-party* origin, and Nova's
`requiredPermissions` → capability mapping really granted
`filesystem:write` to an ordinary opkg-tier app.

`pyxis.json` declares `permissions: ["filesystem.read", "filesystem.write",
"notifications"]`. The field is `permissions` there and comes out as
`requiredPermissions` in the generated `appinfo.json`. None of the three is
privileged, so any opkg app is entitled to them.

Since alpha 5 the page also raises one notification per launch through
`window.geckoOS.notification.show`, and records `notify=ok <id>` (or the
error) in the proof file. That is the app side of the exit criterion "an
app-raised notification appears as a toast and lands in the centre". In
the store, the notification must carry `appId: "com.pyxis.hello"`. The page
never sends that id: the bridge stamps it from the `pyxis://` origin.

Since the bridge became deny-by-default (2026-09-27), the page also calls
nine things it did **not** declare: `settings.set`, `audio.getVolume`,
`network.getStatus`, `terminal.Write`, `evo.ListWindows` and `nova.List`,
each behind a capability this package lacks, and `process.list`,
`Echo.ListServices` and `deviceInfo.getUUID`, which the bridge forwards for
no app at all. Each lands in the proof file as
`deny <service>.<method>=denied: <reason>`, or as `=FORWARDED …` if the
call got through to Echo. Every probe is harmless when it does get through,
so a regression shows up in the file rather than as damage. The control is
`allow settings.get=ok`, which needs no capability because every app reads
its theme that way. `../alpha5-bridge-deny.txt` asserts all of it.

Signals became deny-by-default too (2026-09-28), so the page also listens
for one it may not hear. Before anything else it subscribes to
`notification:NotificationShown`, which carries every app's notifications
and needs the privileged `notifications:manage`. It then raises its own
notification, so the signal certainly fires while it listens. The result
lands in the proof file as `listen notification.NotificationShown=silent`,
or as `=HEARD <title>: <body>` if the bridge delivered it. The shell's
toast for the same notification is the control: it shows the signal fired
and that an app allowed to hear it did. `../alpha5-bridge-signals.txt`
asserts both.

Filesystem calls became confined on 2026-10-06: an app reaches its own
storage and nothing else. So the page also aims eighteen filesystem calls
outside it, each recorded as `fs <label>=refused: <reason>`, or as
`=ESCAPED …` if it worked:

- absolute paths (read, write, chmod, delete, and a copy out to `/tmp`),
  which the bridge refuses outright;
- `..`, to read, write, or move a folder out;
- links the suite plants in the app's directory first (`escape-link` to
  `/etc/os-release`, `escape-dir` to `/tmp`, `up-link` to `../../../..`),
  read, written, listed, copied and chmodded through;
- a scope the page names itself (`system`), and another app's id
  (`notes`, which needs the shell's `filesystem:apps`) or its own;
- a path that is not a string, which the bridge refuses since it can't
  check it;
- deleting the top of its own storage.

Every target can be read or written by the user echo drops to, so only the
confinement stops them, and each is harmless if it does not. The controls
are `fs-ok own=ok paths=~/…` (a nested file made, read back and listed in
the app's own storage) and `fs-ok scope-named=own storage` (naming the
`user` scope lists the app's own storage, not the home). `fs storage
mount=/` shows `getStorageInfo` answering for the root filesystem whatever
path it is given.
`../alpha5-fs-confine.txt` asserts all of it, and that removing the
package, even while the app runs, deletes the app's directory and its
Gecko profile.
