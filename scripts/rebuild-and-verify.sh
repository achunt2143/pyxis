#!/bin/bash
# Rebuild the image from the current tree and run the on-device suites.
#
#     bash scripts/rebuild-and-verify.sh                 # build, then the default suites
#     SKIP_TESTS=1 bash scripts/rebuild-and-verify.sh    # build only
#     SUITES=all bash scripts/rebuild-and-verify.sh      # every scripts/serial-tests/*.txt
#     SUITES="alpha5-identity alpha5-identity-evo" bash scripts/rebuild-and-verify.sh
#
# Exists because the rebuild is several steps, two of them need root, and
# getting the order wrong (staging the test package *after* packaging, say)
# produces an image that looks fine and is missing things. It also keeps the
# sudo prompts together instead of stalling halfway through.
#
# Everything that does not need root -- the Rust release build, Bloom and the
# app packages, the acceptance .ipk, and a check that every artifact the
# rootfs copies is actually there -- is done first, so the root section is as
# short as possible and nothing fails after the password.
#
# Gecko is deliberately NOT rebuilt here (a `mach build` is hours). What that
# means for a change under gecko/patches/pyxis:
#   - JS (*.sys.mjs, browser-init.js, ...): the dev objdir's dist/bin holds
#     these as symlinks into the checkout, and create-rootfs's stage_gecko
#     copies with rsync -L, so the image gets the checkout's current file.
#     A bridge-table change (PyxisBridgeParent.sys.mjs) needs NO mach build.
#   - Anything compiled (C++, IDL, IPDL, moz.build, StaticAtoms.py): needs
#     `make gecko-build` first. The preflight below stops if such a file is
#     newer than the built vela (ALLOW_STALE_VELA=1 to go on anyway).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
PKG_DIR="scripts/serial-tests/hello-pyxis"
SKIP_TESTS="${SKIP_TESTS:-0}"
GECKO_SRC="target/gecko-src/gecko-dev"
VELA_DIST="${GECKO_SRC}/obj-pyxis-webapp/dist/bin"
EXP="../pyxis-experience"

step() { printf '\n\033[1;33m══ %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m✓ %s\033[0m\n' "$*"; }
die()  { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

step "0/6  vela: patch series and build freshness"
# The checkout is gitignored; gecko/patches/pyxis is the durable copy. --check
# only reads (never run the script without it: that rewrites the series).
rc=0; bash scripts/regen-gecko-patches.sh --check || rc=$?
case $rc in
    0) ok "gecko/patches/pyxis reproduces the checkout" ;;
    2) echo "⚠ no gecko checkout: the image keeps whatever vela the rootfs already has" ;;
    *) die "gecko/patches/pyxis is out of sync with the checkout (make gecko-patches-regen, then review)" ;;
esac
if [ -x "${VELA_DIST}/vela" ]; then
    stale=()
    while IFS= read -r entry; do
        rel=${entry#gecko/patches/pyxis/}; rel=${rel%.PYXIS_DIFF}
        f="${GECKO_SRC}/${rel}"
        [ -f "$f" ] && [ "$f" -nt "${VELA_DIST}/vela" ] || continue
        case "$rel" in
            # Symlinked into dist/bin: the image copies the current file.
            *.js|*.mjs|*.ftl|*.properties|*.css|*.svg|*.png) [ -n "$(find "$VELA_DIST" -lname "*$(basename "$rel")" -print -quit)" ] && continue ;;
            browser/branding/unofficial/*) continue ;;
        esac
        stale+=("$rel")
    done < <(find gecko/patches/pyxis -type f)
    if [ ${#stale[@]} -gt 0 ]; then
        printf '  changed since vela was built: %s\n' "${stale[@]}" >&2
        [ "${ALLOW_STALE_VELA:-0}" = 1 ] \
            || die "vela needs a rebuild first: make gecko-build (or ALLOW_STALE_VELA=1 to ship the old one)"
        echo "⚠ ALLOW_STALE_VELA=1: shipping a vela without those changes"
    else
        ok "vela build is current (JS-only changes ride along through dist/bin's symlinks)"
    fi
else
    echo "⚠ no vela build at ${VELA_DIST}: the image keeps whatever vela the rootfs already has"
fi

step "1/6  Rust release build"
cargo build --release
ok "binaries in target/release"

step "2/6  Bloom + app packages"
if [ -d "$EXP" ]; then
    if [ ! -d "$EXP/node_modules" ]; then
        ( cd "$EXP" && npm ci )
    fi
    # Reuse existing node_modules rather than running npm ci unconditionally:
    # npm ci wipes node_modules (breaking a running dev server or locally synced
    # remochi/@duyojs/duyo deps). Pass PYXIS_NPM_CI=1 to `make pyxis-build` or
    # run `npm ci` manually if lockfile deps changed.
    ( cd "$EXP" && npm run build:all )
    ok "shell + 8 app dists rebuilt"
else
    echo "⚠ $EXP not found — the image will keep its current shell"
fi

step "3/6  acceptance package"
./target/release/pyxis-sdk -p "$PKG_DIR" package
IPK=$(ls -t "$PKG_DIR"/dist/*.ipk | head -1)
ok "$IPK"

step "4/6  everything the rootfs copies is here"
# create-rootfs skips a missing artifact with a log line and keeps the
# previous build's copy in the reused rootfs, so a gap only shows on device.
# Catch it now, before root.
missing=()
for bin in $(sed -n '/^    local bins=(/,/^    )/p' scripts/create-rootfs.sh \
             | sed 's/#.*//' | grep -Eo '^[[:space:]]+[a-z][a-z0-9-]*[[:space:]]*$'); do
    [ -f "target/release/$bin" ] || missing+=("target/release/$bin")
done
if [ -d "$EXP" ]; then
    for f in dist/index.html dist/overlay.html; do
        [ -f "$EXP/$f" ] || missing+=("$EXP/$f")
    done
    # Mochi is Prelude with remochi's bundled Lato as the fallback; the woff
    # files ship inside the web build, not in the rootfs's fonts.
    ls "$EXP"/dist/assets/lato-*.woff > /dev/null 2>&1 || missing+=("$EXP/dist/assets/lato-*.woff")
    for app in terminal browser files notes contacts tasks settings installer; do
        [ -f "$EXP/packages/apps/$app/dist/index.html" ] || missing+=("$EXP/packages/apps/$app/dist")
    done
fi
for f in scripts/serial-tests/identity-icons/notes.png scripts/serial-tests/identity-icons/probe.png; do
    [ -f "$f" ] || missing+=("$f")
done
[ ${#missing[@]} -eq 0 ] || die "missing: ${missing[*]}"
ok "binaries, shell, overlay, fonts, 8 apps, test fixtures"

step "5/6  rootfs + image  (needs root — one password, then it is unattended)"
sudo -v
# The suites drive the device through the root shell on the serial console.
PYXIS_DEV_IMAGE=1 bash scripts/create-rootfs.sh --kernel-prebuilt
ok "rootfs built as a DEV image (PYXIS_DEV_IMAGE=1)"
# Order matters: the package has to be in the rootfs BEFORE the image is
# built from it, or the Installer app comes up with nothing to install.
sudo install -Dm644 "$IPK" "target/build/rootfs/var/lib/pyxis/incoming/$(basename "$IPK")"
# stdin from /dev/null, NOT `yes |`. package.sh already checks `[ -t 0 ]`
# and overwrites automatically when stdin is not a terminal, so the pipe was
# never needed — and it actively broke this script: `yes` dies of SIGPIPE
# when package.sh exits, and with `pipefail` that non-zero status tripped
# `set -e`, so the run ended after the image was built and never reached the
# test suites. Silently, because the image itself was fine.
bash scripts/package.sh --x86_64-efi < /dev/null
ok "target/build/images/geckoOS.img"

if [ "$SKIP_TESTS" = "1" ]; then
    step "6/6  skipped (SKIP_TESTS=1)"
    exit 0
fi

step "6/6  on-device suites"
fail=0
# kiosk-relaunch is the regression guard for "apps come back as Firefox
# windows" -- a guard nobody runs is not a guard, and that bug survived four
# diagnoses. It is the slowest suite (~8 min of launch/close cycles).
# The alpha5-* suites see, click and hear the device (screenshots, pointer,
# the sound card); look at target/serial-shots/ after a run. alpha5-fs-confine
# is the guard for "an app's filesystem calls reach only its own data", and
# alpha5-wayland-trust for "an app can't bind screencopy/virtual-keyboard or
# claim a shell app id". SUITES=all runs every suite (hours);
# docs/internal/next-image-checklist.md has the full list.
DEFAULT_SUITES="alpha4-ipk-e2e system-service alpha4-exit-criteria kiosk-relaunch \
alpha5-eyes alpha5-resize alpha5-notifications alpha5-audio alpha5-overlay \
alpha5-fs-confine alpha5-wayland-trust alpha5-identity alpha5-identity-evo \
alpha5-frost alpha5-lock"
SUITES="${SUITES:-$DEFAULT_SUITES}"
if [ "$SUITES" = all ]; then
    SUITES=$(ls scripts/serial-tests/*.txt | xargs -n1 basename | sed 's/\.txt$//' | tr '\n' ' ')
fi
for suite in $SUITES; do
    printf '\n--- %s ---\n' "$suite"
    if python3 scripts/serial-test.py --script "scripts/serial-tests/$suite.txt" \
         --settle 35 --timeout 200 > "/tmp/pyxis-$suite.out" 2>&1; then
        ok "$suite: $(grep -c '✓ must' "/tmp/pyxis-$suite.out") assertions passed"
    else
        printf '\033[31m✗ %s\033[0m\n' "$suite"
        grep -E '✗' "/tmp/pyxis-$suite.out" || true
        echo "  full log: /tmp/pyxis-$suite.out"
        fail=1
    fi
done

if [ "$fail" = 0 ]; then
    printf '\n\033[32mAll suites green. `bash scripts/emulate.sh` to look at it.\033[0m\n'
else
    printf '\n\033[31mSome suites failed — see the logs above.\033[0m\n'
    exit 1
fi
