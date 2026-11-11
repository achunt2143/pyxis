#!/bin/bash
# Rebuild the image from the current tree and run the on-device suites.
#
#     bash scripts/rebuild-and-verify.sh
#
# Exists because the rebuild is four steps, two of them need root, and
# getting the order wrong (staging the test package *after* packaging, say)
# produces an image that looks fine and is missing things. It also keeps the
# sudo prompts together at the start instead of stalling halfway through.
#
# Everything that does not need root — the Rust release build, Bloom and the
# app packages, the acceptance .ipk — is done first, so the root section is
# as short as possible. Gecko is deliberately NOT rebuilt: create-rootfs's
# stage_gecko only copies the prebuilt vela out of the objdir, so unless
# something under gecko/patches changed there is nothing to do, and a
# `mach build` is hours.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
PKG_DIR="scripts/serial-tests/hello-pyxis"
SKIP_TESTS="${SKIP_TESTS:-0}"

step() { printf '\n\033[1;33m══ %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m✓ %s\033[0m\n' "$*"; }

step "1/5  Rust release build"
cargo build --release
ok "binaries in target/release"

step "2/5  Bloom + app packages"
if [ -d ../pyxis-experience ]; then
    # `npm run build:all`, not the Makefile's `npm ci && npm run build:all`:
    # npm ci wipes node_modules, which breaks a dev server the user may have
    # running against this tree. Run npm ci yourself if deps changed.
    ( cd ../pyxis-experience && npm run build:all )
    ok "shell + 8 app dists rebuilt"
else
    echo "⚠ ../pyxis-experience not found — the image will keep its current shell"
fi

step "3/5  acceptance package"
./target/release/pyxis-sdk -p "$PKG_DIR" package
IPK=$(ls -t "$PKG_DIR"/dist/*.ipk | head -1)
ok "$IPK"

step "4/5  rootfs + image  (needs root — one password, then it is unattended)"
sudo -v
bash scripts/create-rootfs.sh --kernel-prebuilt
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
    step "5/5  skipped (SKIP_TESTS=1)"
    exit 0
fi

step "5/5  on-device suites"
fail=0
# kiosk-relaunch is the regression guard for "apps come back as Firefox
# windows" -- a guard nobody runs is not a guard, and that bug survived four
# diagnoses. It is the slowest suite (~8 min of launch/close cycles).
# The alpha5-* suites see, click and hear the device (screenshots, pointer,
# the sound card); look at target/serial-shots/ after a run. alpha5-fs-confine
# is the guard for "an app's filesystem calls reach only its own data", and
# alpha5-wayland-trust for "an app can't bind screencopy/virtual-keyboard or
# claim a shell app id".
for suite in alpha4-ipk-e2e system-service alpha4-exit-criteria kiosk-relaunch \
             alpha5-eyes alpha5-resize alpha5-notifications alpha5-audio alpha5-overlay \
             alpha5-fs-confine alpha5-wayland-trust; do
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
