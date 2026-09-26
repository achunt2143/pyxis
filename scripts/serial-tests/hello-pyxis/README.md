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

The page is not just a placeholder. It writes a probe to
`/tmp/pyxis-hello-proof.txt` through `window.geckoOS.filesystem`, which is
how a headless run tells "a window opened" from "the app actually works":
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
