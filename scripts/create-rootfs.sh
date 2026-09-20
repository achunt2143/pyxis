#!/bin/bash
# scripts/create-rootfs.sh - GeckoOS / Pyxis rootfs builder

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
GECKO_BUILD_DIR="${BUILD_DIR}/gecko"
BOOT_DIR="${PROJECT_ROOT}/target/boot"
KERNEL_PREBUILT_PATH="${PROJECT_ROOT}/target/boot/vmlinuz-geckoOS"

DEBIAN_SUITE="${DEBIAN_SUITE:-sid}"
DEBIAN_ARCH="${DEBIAN_ARCH:-amd64}"
DEBIAN_MIRROR="${DEBIAN_MIRROR:-http://deb.debian.org/debian/}"

# opkg, built from source in stage_opkg(). Debian packages no opkg at all
# (verified against the sid package index -- neither `opkg` nor `opkg-utils`
# exists in any suite), so it cannot simply be added to the apt list above.
# The Yocto Project is upstream and publishes release tarballs.
OPKG_VERSION="${OPKG_VERSION:-0.10.0}"
OPKG_SHA256="${OPKG_SHA256:-4d099e010a5d262b34184fee4766bc60a8c7439741fb210aa11d51704afd04d1}"
OPKG_URL_BASE="${OPKG_URL_BASE:-https://downloads.yoctoproject.org/releases/opkg}"

GECKO_PREBUILT=${GECKO_PREBUILT:-0}
KERNEL_PREBUILT=${KERNEL_PREBUILT:-0}
FORCE_REBUILD=${FORCE_REBUILD:-0}
# Which staged pyxis:// app vela launches into at boot. Both "terminal" and
# "bloom" are always staged (stage_terminal_app / stage_bloom_app run
# unconditionally below) -- this only picks which one the boot script
# actually execs, baked into the generated /usr/sbin/geckoOS-boot via the
# @@BOOT_APP@@ placeholder substitution in stage_services().
#
# Default is "bloom" (2026-07-08): "terminal" was a deliberate MVP
# simplification (see stage_terminal_app's comment) while Bloom's own
# window.geckoOS bridge calls were being debugged -- that's since been
# verified working end-to-end (real PTY, real Echo calls), so the real
# desktop shell is now what a plain `make build-release` boots into.
# Terminal/browser/files/notes/contacts/tasks/settings stay reachable the
# normal way: launched FROM Bloom's dock/launcher via Nova, each in its own
# chromeless kiosk window per its own manifest.webapp `display` field.
BOOT_APP=${BOOT_APP:-bloom}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --gecko-prebuilt) GECKO_PREBUILT=1; shift ;;
        --kernel-prebuilt) KERNEL_PREBUILT=1; shift ;;
        --force) FORCE_REBUILD=1; shift ;;
        --boot-app=*) BOOT_APP="${1#*=}"; shift ;;
        --boot-app) BOOT_APP="$2"; shift 2 ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

case "$BOOT_APP" in
    # Terminal used to build as a second Rollup input (terminal.html)
    # sharing Bloom's own dist/; it's now its own independent
    # npm-workspace package (pyxis-experience/packages/apps/terminal/)
    # that builds a plain index.html at its own dist root, same as every
    # other extracted app -- see stage_terminal_app.
    #
    # BOOT_DISPLAY_MODE picks the PYXIS_DISPLAY_MODE the boot-launched app
    # gets (see gecko/src/gecko.rs's DisplayMode / the
    # BrowserContentHandler.sys.mjs patch): Bloom is the desktop shell
    # itself, so it gets real "fullscreen" (win.fullScreen = true -- no
    # Evo SSD title bar, edge-to-edge) rather than "maximized" (still a
    # normal decorated window, just sized to fill the screen). Every OTHER
    # app -- whether boot-launched here in single-app MVP mode or launched
    # normally from within Bloom via Nova -- keeps the chromeless "kiosk"
    # window style (no URL bar/tabs/menu, per kPyxisAppWindowFeatures) that
    # ALL `--pyxis-app` launches already get regardless of display mode;
    # "maximized" is just this script's boot-time default for that case.
    terminal) BOOT_APP_ENTRY=index.html; BOOT_DISPLAY_MODE=maximized ;;
    bloom) BOOT_APP_ENTRY=index.html; BOOT_DISPLAY_MODE=fullscreen ;;
    *)
        echo "Unknown --boot-app: ${BOOT_APP} (expected 'terminal' or 'bloom')" >&2
        exit 1
        ;;
esac

log_info()    { echo "[INFO] $*"; }
log_success() { echo "[✓] $*"; }
log_error()   { echo "[ERROR] $*" >&2; }
log_section() {
    echo ""
    echo "════════════════════════════════════════════"
    echo "  $*"
    echo "════════════════════════════════════════════"
}

write_rootfs_file() {
    local target="$1"
    sudo mkdir -p "$(dirname "$target")"
    if ! sudo tee "$target" >/dev/null; then
        log_error "Failed to write ${target}"
        return 1
    fi
}

require_tools() {
    log_section "Checking host tools"

    local required=(sudo rsync chroot tar)
    local missing=()

    for tool in "${required[@]}"; do
        command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
    done

    if ! command -v debootstrap >/dev/null 2>&1; then
        missing+=("debootstrap")
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing required host tools: ${missing[*]}"
        log_info "Install with: sudo apt update && sudo apt install -y debootstrap rsync"
        exit 1
    fi

    log_success "Host tools available"
}

cleanup_mounts() {
    for mp in dev/pts dev proc sys run; do
        if mountpoint -q "${ROOTFS_DIR}/${mp}" 2>/dev/null; then
            sudo umount -lf "${ROOTFS_DIR:?}/${mp}" || true
        fi
    done
}

trap cleanup_mounts EXIT

stage_prepare_rootfs() {
    log_section "Stage 0: Prepare rootfs directory"

    mkdir -p "${BUILD_DIR}"

    # Only wipe an existing rootfs if --force was passed (FORCE_REBUILD=1).
    # This flag was declared and settable at the top of this script but
    # never actually consumed anywhere -- meaning every single run,
    # including plain `make build-release`, always re-debootstrapped a
    # fresh Debian base from the network, even for a change that's just a
    # boot-script text edit (the overwhelming majority of this session's
    # iteration). stage_debootstrap_base below already has its own
    # "skip if /etc/debian_version exists" check, but it could never
    # actually fire, since this function deleted the whole directory out
    # from under it on every prior run. All the individual stage_* functions
    # after this one are already idempotent (write_rootfs_file overwrites,
    # rsync -a --delete / cp -a overwrite, apt install of already-present
    # packages is a fast no-op) -- the debootstrap itself was the one
    # genuinely expensive, unnecessary-to-repeat step.
    if [[ "${FORCE_REBUILD}" -eq 1 && -d "${ROOTFS_DIR}" ]]; then
        log_info "--force passed: removing existing rootfs for a clean debootstrap..."
        sudo rm -rf "${ROOTFS_DIR}"
    elif [[ -d "${ROOTFS_DIR}" ]]; then
        log_info "Reusing existing rootfs at ${ROOTFS_DIR} (pass --force to wipe and re-debootstrap from scratch)"
    fi

    mkdir -p "${ROOTFS_DIR}"

    # This directory becomes `/` once chrooted/switch_rooted into. Left
    # owned by the invoking user, systemd's sysusers/tmpfiles path-canonicalization
    # safety check (chase_symlinks) sees an ownership transition from a
    # non-root `/` to root-owned `/etc`, `/run`, etc. and refuses to run,
    # failing the systemd package's postinst (exit 73) and aborting the
    # whole apt install. debootstrap fixes ownership of everything it
    # unpacks, but never the pre-existing top-level directory itself.
    sudo chown root:root "${ROOTFS_DIR}"
    log_success "Rootfs directory ready: ${ROOTFS_DIR}"
}

stage_debootstrap_base() {
    log_section "Stage 1: Debootstrap Debian base"

    if [[ -f "${ROOTFS_DIR}/etc/debian_version" ]]; then
        log_info "Debian base already present, skipping debootstrap"
        return
    fi

    sudo debootstrap \
        --variant=minbase \
        --arch="${DEBIAN_ARCH}" \
        "${DEBIAN_SUITE}" \
        "${ROOTFS_DIR}" \
        "${DEBIAN_MIRROR}"

    log_success "Debian base bootstrapped into ${ROOTFS_DIR}"
}

stage_mount_chroot_support() {
    log_section "Stage 2: Mount chroot support filesystems"

    sudo mkdir -p \
        "${ROOTFS_DIR}/dev" \
        "${ROOTFS_DIR}/dev/pts" \
        "${ROOTFS_DIR}/proc" \
        "${ROOTFS_DIR}/sys" \
        "${ROOTFS_DIR}/run"

    # mountpoint -q guards make these idempotent -- with rootfs reuse (see
    # stage_prepare_rootfs), a prior run's EXIT trap (cleanup_mounts) should
    # already have unmounted these, but guarding here too means an
    # interrupted prior run (killed before the trap could fire) doesn't
    # turn a normal re-run into a hard "already mounted" failure.
    mountpoint -q "${ROOTFS_DIR}/dev" || sudo mount --bind /dev "${ROOTFS_DIR}/dev"
    mountpoint -q "${ROOTFS_DIR}/dev/pts" || sudo mount --bind /dev/pts "${ROOTFS_DIR}/dev/pts"
    mountpoint -q "${ROOTFS_DIR}/proc" || sudo mount -t proc proc "${ROOTFS_DIR}/proc"
    mountpoint -q "${ROOTFS_DIR}/sys" || sudo mount -t sysfs sysfs "${ROOTFS_DIR}/sys"
    mountpoint -q "${ROOTFS_DIR}/run" || sudo mount --bind /run "${ROOTFS_DIR}/run"

    if [[ -f /etc/resolv.conf ]]; then
        sudo cp /etc/resolv.conf "${ROOTFS_DIR}/etc/resolv.conf"
    fi

    log_success "Chroot support mounts ready"
}

stage_install_base_packages() {
    log_section "Stage 3: Install Debian + Wayland runtime packages"

    sudo chroot "${ROOTFS_DIR}" /bin/bash <<'CHROOT_EOF'
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

apt update

# Prune stale dpkg statoverride entries (2026-07-08, found live): a
# statoverride can reference a system user/group that has since been
# removed from the rootfs (seen: dbus's `root messagebus 4754
# /usr/lib/dbus-1.0/dbus-daemon-launch-helper` override outliving the
# `messagebus` group in /etc/group). dpkg validates the ENTIRE
# statoverride file up front on every operation, so one dangling entry
# makes every subsequent `apt install`/dpkg action abort with
# "unknown system group '<g>' in statoverride file" before doing any
# work -- which is exactly how adding `nano` to a reused rootfs blew up.
# dpkg itself points at `dpkg-statoverride --remove` as the recovery, so
# prune any override whose owning user OR group no longer resolves in
# this rootfs's own /etc/passwd//etc/group (checked directly rather than
# via getent, which needs nss wiring a minimal chroot may lack). Read the
# whole file first, THEN remove, so rewriting the file mid-iteration
# can't drop entries. No-op on a clean rootfs (no file / nothing stale).
if [ -f /var/lib/dpkg/statoverride ]; then
  stale_paths=""
  while read -r so_user so_group so_mode so_path; do
    [ -n "${so_path:-}" ] || continue
    if ! grep -q "^${so_user}:" /etc/passwd 2>/dev/null \
       || ! grep -q "^${so_group}:" /etc/group 2>/dev/null; then
      stale_paths="${stale_paths}${so_path}
"
    fi
  done < /var/lib/dpkg/statoverride
  printf '%s' "$stale_paths" | while IFS= read -r sp; do
    [ -n "$sp" ] || continue
    echo "[rootfs] pruning stale dpkg statoverride: $sp"
    dpkg-statoverride --remove "$sp" 2>/dev/null || true
  done
fi

DISPLAY_INFO_PKG=""
if apt-cache show libdisplay-info3 >/dev/null 2>&1; then
  DISPLAY_INFO_PKG="libdisplay-info3"
elif apt-cache show libdisplay-info2 >/dev/null 2>&1; then
  DISPLAY_INFO_PKG="libdisplay-info2"
elif apt-cache show libdisplay-info1 >/dev/null 2>&1; then
  DISPLAY_INFO_PKG="libdisplay-info1"
fi

# ALSA and glycin package names have shifted across Debian's 64-bit-time_t
# (t64) transition and glycin's own versioning respectively -- probe for
# the real name rather than hardcoding one, same pattern as
# DISPLAY_INFO_PKG above.
ASOUND_PKG=""
for cand in libasound2t64 libasound2; do
  if apt-cache show "$cand" >/dev/null 2>&1; then ASOUND_PKG="$cand"; break; fi
done

GLYCIN_PKG=""
for cand in libglycin-2-0 libglycin1-0 libglycin0; do
  if apt-cache show "$cand" >/dev/null 2>&1; then GLYCIN_PKG="$cand"; break; fi
done

# GTK3/desktop-stack runtime libraries (2026-07-05): vela (the
# from-source Gecko build) uses --enable-default-toolkit=cairo-gtk3-wayland
# and dynamically loads libmozgtk.so/libxul.so, which in turn need the full
# GTK3 widget-toolkit stack -- confirmed via `ldd` against the real build
# output (libgtk-3, libatk, atk-bridge/atspi for a11y, epoxy for GL
# dispatch, lcms2 for color management, seccomp for its own sandboxing,
# libasound for audio). None of this was needed before vela existed
# (Evo/Wayland alone don't touch GTK), so it wasn't in this list until a
# real "Couldn't load XPCOM" failure surfaced it live. Most of GTK3's own
# transitive deps (pango/cairo/glib/gdk-pixbuf/fontconfig/most X11 client
# libs) come along automatically via libgtk-3-0's real (not just
# Recommends) Depends chain, but several are listed explicitly below rather
# than gambled on, since a second missing-library round-trip is expensive
# (each retest needs a full image rebuild + boot).
apt install -y --no-install-recommends \
  apt-utils ca-certificates locales tzdata \
  bash coreutils findutils grep sed gawk diffutils \
  ncurses-base ncurses-term bash-completion \
  file less procps psmisc util-linux kmod udev nano \
  iproute2 iputils-ping netbase ifupdown \
  mount passwd login \
  libdrm2 libgbm1 libinput10 libxkbcommon0 libwayland-client0 libwayland-server0 \
  libwayland-egl1 libwayland-cursor0 \
  wayland-protocols libegl1 libgles2 mesa-utils libgl1-mesa-dri \
  xwayland libseat1 seatd ${DISPLAY_INFO_PKG:+$DISPLAY_INFO_PKG} \
  dbus dbus-user-session shared-mime-info \
  libgtk-3-0 libatk-bridge2.0-0 libatspi2.0-0 libepoxy0 \
  libseccomp2 liblcms2-2 libsystemd0 ${ASOUND_PKG:+$ASOUND_PKG} ${GLYCIN_PKG:+$GLYCIN_PKG} \
  libx11-6 libx11-xcb1 libxcomposite1 libxcursor1 libxdamage1 libxext6 \
  libxfixes3 libxi6 libxinerama1 libxrandr2 libxrender1 libxres1 \
  libxau6 libxdmcp6 libxcb1 libxcb-render0 libxcb-shm0 \
  libpci3 fontconfig fonts-dejavu-core \
  openssl curl wget

echo "en_US.UTF-8 UTF-8" > /etc/locale.gen
locale-gen
update-locale LANG=en_US.UTF-8

ln -sf /usr/share/zoneinfo/UTC /etc/localtime
echo "UTC" > /etc/timezone
echo "geckoOS" > /etc/hostname

# Pre-build the SYSTEM fontconfig cache (/var/cache/fontconfig) now, as root
# in the chroot, so it's populated and world-readable at boot -- geckouser
# then just READS it and never needs to write the un-writable root-owned
# system cache. Complements the per-user ~/.cache/fontconfig fallback dir
# geckoOS-boot creates. Best-effort: some minimal images lack fc-cache.
if command -v fc-cache >/dev/null 2>&1; then
  fc-cache -f -s || true
fi

apt clean
rm -rf /var/lib/apt/lists/*
CHROOT_EOF

    log_success "Base packages installed"
}

stage_opkg() {
    log_section "Stage 3b: opkg package manager (built from source)"

    # WHY THIS IS BUILT RATHER THAN apt-installed
    # ------------------------------------------------------------------
    # opkg is the backend for the entire Vroom SDK/PDK -> Installer app
    # pipeline: `echo/src/services/installer.rs` gates every mutating call on
    # `require_opkg()` and fails with "opkg is not available on this system"
    # without it. Until alpha3 it was simply absent from the image, which is
    # why the install flow had never once been runnable on-device -- the Rust
    # was correct, the binary was never there.
    #
    # It can't be apt-installed: Debian ships no `opkg` (and no `opkg-utils`)
    # package in any suite -- checked against the sid package index, not
    # assumed. Upstream is the Yocto Project, which publishes release
    # tarballs, so this mirrors what scripts/busybox-install.sh already does
    # for BusyBox: build the one missing piece from pinned, checksummed source.
    #
    # Built INSIDE the chroot, not on the host, so it links against the
    # rootfs's own libarchive/liblzma rather than the build machine's -- the
    # host here is Ubuntu and the target is Debian sid, so host-built binaries
    # would be a version-skew bug waiting to happen.
    #
    # FEATURE SET: deliberately minimal. Pyxis only ever installs *local*
    # .ipk files (installer.rs validates a path and hands it to `opkg
    # install`), so the network half of opkg is dead weight:
    #   WITH_CURL=OFF          no remote feeds -> no libcurl, no TLS stack
    #   WITH_GPGME=OFF         signature checking is an alpha6 roadmap item;
    #                          turning it on now would demand a keyring the
    #                          project doesn't have yet
    #   USE_SOLVER_LIBSOLV=OFF the built-in solver is enough for self-contained
    #                          bundled apps and drops the libsolv dependency
    #   WITH_XZ=ON             kept (upstream default): Vroom emits gzip today,
    #                          but xz .ipks are legal and cheap to support
    # libarchive is the one hard, unconditional dependency.
    #
    # Build deps are purged afterwards so they don't ship in the image; only
    # the runtime libs (libarchive, liblzma) stay behind.

    if sudo test -x "${ROOTFS_DIR}/usr/bin/opkg" && [[ "${FORCE_REBUILD}" != "1" ]]; then
        log_info "opkg already present in rootfs, skipping build (pass --force to rebuild)"
        return 0
    fi

    sudo chroot "${ROOTFS_DIR}" /bin/bash <<CHROOT_EOF
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

OPKG_VERSION="${OPKG_VERSION}"
OPKG_SHA256="${OPKG_SHA256}"
OPKG_URL_BASE="${OPKG_URL_BASE}"

# Build-only toolchain. Recorded so the purge below removes exactly what
# this stage added and nothing the runtime still needs.
BUILD_DEPS="build-essential cmake pkg-config libarchive-dev liblzma-dev"

apt update
apt install -y --no-install-recommends \$BUILD_DEPS libarchive13 liblzma5

cd /tmp
rm -rf "opkg-\${OPKG_VERSION}" "opkg-\${OPKG_VERSION}.tar.gz"

echo "[rootfs] fetching opkg \${OPKG_VERSION}"
curl -fsSL -o "opkg-\${OPKG_VERSION}.tar.gz" \
    "\${OPKG_URL_BASE}/opkg-\${OPKG_VERSION}.tar.gz"

# Fail loudly on a checksum mismatch: this tarball is fetched over the
# network at build time and then compiled and run as root on the device.
echo "\${OPKG_SHA256}  opkg-\${OPKG_VERSION}.tar.gz" | sha256sum -c -

tar xzf "opkg-\${OPKG_VERSION}.tar.gz"
cd "opkg-\${OPKG_VERSION}"

# The 0.10.0 release tarball SHIPS a populated \`build/\` directory containing
# the maintainer's own CMakeCache.txt, with CMAKE_CACHEFILE_DIR baked to
# \`/usr/local/src/opkg/build\`. Configuring into \`build\` therefore reuses that
# stale cache and CMake hard-fails:
#   "The current CMakeCache.txt directory ... is different than the directory
#    /usr/local/src/opkg/build where CMakeCache.txt was created"
# Delete the shipped directory and configure into a distinct one, so neither
# the stale cache nor a future tarball shipping the same layout can collide.
rm -rf build
cmake -B build-pyxis \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/usr \
    -DWITH_CURL=OFF \
    -DWITH_GPGME=OFF \
    -DUSE_SOLVER_LIBSOLV=OFF \
    -DWITH_XZ=ON
cmake --build build-pyxis -j"\$(nproc)"
cmake --install build-pyxis

# Prove the binary actually runs in this rootfs before declaring success --
# a build that links against something the image doesn't ship would
# otherwise only surface at first use on-device.
/usr/bin/opkg --version

cd /tmp
rm -rf "opkg-\${OPKG_VERSION}" "opkg-\${OPKG_VERSION}.tar.gz"

apt purge -y \$BUILD_DEPS
apt autoremove -y --purge
apt clean

# The purge above can take libarchive13/liblzma5 with it as autoremoved
# dependencies of the -dev packages; reinstall them explicitly, since opkg
# links against both at runtime.
apt install -y --no-install-recommends libarchive13 liblzma5
CHROOT_EOF

    log_success "opkg ${OPKG_VERSION} built and installed"
}

stage_rootfs_dirs() {
    log_section "Stage 4: GeckoOS directory layout"

    sudo mkdir -p "${ROOTFS_DIR}/boot"
    sudo mkdir -p "${ROOTFS_DIR}/mnt"
    sudo mkdir -p "${ROOTFS_DIR}/opt"
    sudo mkdir -p "${ROOTFS_DIR}/opt/geckoOS/bin"
    sudo mkdir -p "${ROOTFS_DIR}/root"
    sudo mkdir -p "${ROOTFS_DIR}/srv"
    sudo mkdir -p "${ROOTFS_DIR}/tmp"
    sudo mkdir -p "${ROOTFS_DIR}/tmp/.X11-unix"

    sudo mkdir -p "${ROOTFS_DIR}/etc/init.d"
    sudo mkdir -p "${ROOTFS_DIR}/etc/systemd/system"
    sudo mkdir -p "${ROOTFS_DIR}/etc/geckoOS"
    sudo mkdir -p "${ROOTFS_DIR}/etc/rc.d"
    sudo mkdir -p "${ROOTFS_DIR}/etc/ld.so.conf.d"

    sudo mkdir -p "${ROOTFS_DIR}/home/user"
    sudo mkdir -p "${ROOTFS_DIR}/home/geckouser"

    sudo mkdir -p "${ROOTFS_DIR}/lib/modules"
    sudo mkdir -p "${ROOTFS_DIR}/lib/gecko"

    sudo mkdir -p "${ROOTFS_DIR}/usr/bin"
    sudo mkdir -p "${ROOTFS_DIR}/usr/lib"
    sudo mkdir -p "${ROOTFS_DIR}/usr/sbin"
    sudo mkdir -p "${ROOTFS_DIR}/usr/local/bin"
    sudo mkdir -p "${ROOTFS_DIR}/usr/share/applications"
    sudo mkdir -p "${ROOTFS_DIR}/usr/share/geckoOS"
    sudo mkdir -p "${ROOTFS_DIR}/usr/share/wayland-sessions"

    sudo mkdir -p "${ROOTFS_DIR}/var/cache"
    sudo mkdir -p "${ROOTFS_DIR}/var/log"
    sudo mkdir -p "${ROOTFS_DIR}/var/run"
    sudo mkdir -p "${ROOTFS_DIR}/var/tmp"
    sudo mkdir -p "${ROOTFS_DIR}/var/lib/wayland"

    sudo mkdir -p "${ROOTFS_DIR}/run/user/1000"

    write_rootfs_file "${ROOTFS_DIR}/usr/share/wayland-sessions/pyxis.desktop" <<'EOF'
[Desktop Entry]
Name=Pyxis (GeckoOS)
Comment=Wayland session for GeckoOS
Exec=/usr/bin/pyxis
Type=Application
EOF

    log_success "Directory structure ready"
}

stage_system_config() {
    log_section "Stage 5: System configuration"

    write_rootfs_file "${ROOTFS_DIR}/etc/hosts" <<'EOF'
127.0.0.1 localhost
::1 localhost
127.0.1.1 geckoOS
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/fstab" <<'EOF'
proc      /proc          proc      nosuid,noexec,nodev  0 0
sysfs     /sys           sysfs     nosuid,noexec,nodev  0 0
devtmpfs  /dev           devtmpfs  mode=0755,nosuid     0 0
devpts    /dev/pts       devpts    gid=5,mode=620       0 0
tmpfs     /run           tmpfs     nosuid,nodev,mode=755 0 0
tmpfs     /tmp           tmpfs     mode=1777            0 0
tmpfs     /dev/shm       tmpfs     nosuid,nodev         0 0
cgroup2   /sys/fs/cgroup cgroup2   nosuid,noexec,nodev  0 0
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/environment" <<'EOF'
PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
LANG="en_US.UTF-8"
WAYLAND_DISPLAY="wayland-0"
XDG_RUNTIME_DIR="/run/user/1000"
XDG_CONFIG_HOME="/etc/geckoOS"
XDG_SESSION_TYPE="wayland"
GDK_BACKEND="wayland"
QT_QPA_PLATFORM="wayland"
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/profile" <<'EOF'
export PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export HOME="/root"
export SHELL="/bin/bash"
PS1="[\u@\h \W]\\$ "
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/passwd" <<'EOF'
root:x:0:0:root:/root:/bin/bash
geckouser:x:1000:1000:Gecko User:/home/geckouser:/bin/bash
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/group" <<'EOF'
root:x:0:
tty:x:5:
video:x:44:
audio:x:29:
input:x:105:
render:x:109:
seat:x:110:
geckouser:x:1000:
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/shadow" <<'EOF'
root:*:20000:0:99999:7:::
geckouser:*:20000:0:99999:7:::
EOF
    sudo chmod 640 "${ROOTFS_DIR}/etc/shadow"

    write_rootfs_file "${ROOTFS_DIR}/etc/os-release" <<'EOF'
NAME="GeckoOS"
VERSION="0.1.0"
ID=geckoos
ID_LIKE=debian
PRETTY_NAME="GeckoOS 0.1.0"
VERSION_CODENAME=gecko
HOME_URL="https://github.com/gecko-os/geckoos"
SUPPORT_URL="https://github.com/gecko-os/geckoos/issues"
BUG_REPORT_URL="https://github.com/gecko-os/geckoos/issues"
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/motd" <<'EOF'
Welcome to GeckoOS 0.1.0
Built with Gecko rendering engine and Pyxis
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/geckoOS/config.toml" <<'EOF'
[system]
hostname = "geckoOS"
locale = "en_US.UTF-8"
timezone = "UTC"

[display]
default_width = 1920
default_height = 1080
refresh_rate = 60

[gecko]
libxul_path = "/usr/lib/gecko/libxul.so"
profile_dir = "/var/gecko"
enable_gpu = true
cache_size_mb = 256
EOF

    # opkg is the package manager behind the whole Vroom SDK/PDK -> Installer
    # pipeline: `echo/src/services/installer.rs` gates every mutating call on
    # `require_opkg()` and errors with "opkg is not available on this system"
    # when it can't find the binary. It was missing from the image entirely
    # until alpha3, which is why the installer flow had never once been
    # runnable on-device -- the Rust was correct, the binary just wasn't there.
    #
    # `dest root /` installs into the real filesystem rather than a prefix,
    # which is what puts apps under /usr/pyxis/applications where Nova's
    # registry scan (nova.rs) actually looks for them.
    #
    # The arch list must cover everything Vroom stamps into CONTROL/control
    # (vroom/src/control.rs, vroom/src/template.rs::host_arch): `all` for web
    # apps and the concrete target arch for native ones. An unlisted
    # architecture is refused by opkg at install time, so a native .ipk built
    # by pyxis-pdk would be rejected on the machine it was built for. Higher
    # priority wins when a package is available for several.
    sudo mkdir -p "${ROOTFS_DIR}/etc/opkg" \
                  "${ROOTFS_DIR}/var/lib/opkg" \
                  "${ROOTFS_DIR}/var/lib/opkg/lists" \
                  "${ROOTFS_DIR}/var/lib/pyxis/incoming"

    # Debian's arch names aren't Vroom's: Vroom derives the value it stamps
    # into CONTROL/control from Rust's std::env::consts::ARCH, so an amd64
    # image must accept `x86_64`, not `amd64`. Hardcoding either one breaks
    # the other target silently -- opkg simply refuses the package.
    local pyxis_arch
    case "${DEBIAN_ARCH}" in
        amd64) pyxis_arch="x86_64" ;;
        arm64) pyxis_arch="aarch64" ;;
        armhf) pyxis_arch="armv7" ;;
        *)
            log_info "WARN: no Vroom arch mapping for DEBIAN_ARCH=${DEBIAN_ARCH}, using it verbatim in opkg.conf"
            pyxis_arch="${DEBIAN_ARCH}"
            ;;
    esac

    write_rootfs_file "${ROOTFS_DIR}/etc/opkg/opkg.conf" <<EOF
# Pyxis package configuration. See vroom/docs/pyxis-sdk-pdk-spec.md.
#
# Line syntax is "TYPE NAME VALUE", where TYPE is one of
# option/dest/arch/src/src-gz/dist (libopkg/opkg_conf.c's dispatch). Anything
# else is reported as "Ignoring invalid line" on EVERY opkg invocation --
# "lists_dir ext PATH" is an old OpenWrt-ism that opkg 0.10.0 rejects; the
# directory settings are plain options.
dest root /
option lists_dir /var/lib/opkg/lists
option tmp_dir /tmp

# Architectures accepted on this machine, lowest priority first.
# \`all\`  -> web apps  (pyxis-sdk; architecture is always "all")
# native -> PDK apps  (pyxis-pdk; x86_64 / aarch64 / armv7)
arch all 1
arch ${pyxis_arch} 10
EOF

    log_success "System configuration written"
}

stage_busybox_init() {
    log_section "Stage 6a: BusyBox (PID 1)"

    # BusyBox is already built once by scripts/busybox-install.sh (invoked
    # from build-initramfs.sh) — reuse that compiled binary directly rather
    # than re-running the full install script, which symlinks *every*
    # enabled applet over top of this rootfs's real Debian utilities
    # (coreutils/bash/mount/login, installed in stage_install_base_packages).
    # We only want BusyBox to provide /sbin/init here.
    local busybox_bin="${BUILD_DIR}/busybox-build/busybox/busybox"

    if [[ ! -x "$busybox_bin" ]]; then
        log_info "BusyBox not built yet; building via busybox-install.sh"
        bash "${PROJECT_ROOT}/scripts/busybox-install.sh"
    fi

    if [[ ! -x "$busybox_bin" ]]; then
        log_error "BusyBox binary still missing at ${busybox_bin}"
        exit 1
    fi

    sudo cp "$busybox_bin" "${ROOTFS_DIR}/bin/busybox"
    sudo chmod 755 "${ROOTFS_DIR}/bin/busybox"
    sudo ln -sf /bin/busybox "${ROOTFS_DIR}/sbin/init"

    log_success "BusyBox installed: /bin/busybox, /sbin/init -> busybox"
}

stage_services() {
    log_section "Stage 6b: Init and service scripts"

    # BusyBox's init applet reads /etc/inittab. sysinit runs once at boot
    # (filesystem/device setup left over after the initramfs's switch_root);
    # geckoOS-boot (::wait) starts the system daemons in dependency order and
    # must return before init proceeds to the respawn entries below.
    write_rootfs_file "${ROOTFS_DIR}/etc/inittab" <<'EOF'
::sysinit:/etc/init.d/rcS
::wait:/usr/sbin/geckoOS-boot
::ctrlaltdel:/sbin/reboot
::shutdown:/bin/sync
tty1::respawn:/bin/bash
ttyS0::respawn:/bin/bash
EOF

    write_rootfs_file "${ROOTFS_DIR}/etc/init.d/rcS" <<'EOF'
#!/bin/sh
echo "=== rcS: Startup sequence beginning ==="

# /proc, /sys, /dev were already mounted (and moved) by the initramfs before
# switch_root; this only covers what's left for the real rootfs.
mkdir -p /dev/pts /run /tmp /var/tmp
mount -t devpts devpts /dev/pts -o gid=5,mode=620 -n 2>/dev/null || true
mount -t tmpfs tmpfs /run -o mode=755,size=128M -n 2>/dev/null || true
mount -t tmpfs tmpfs /tmp -o mode=1777,size=512M -n 2>/dev/null || true
mount -t cgroup2 cgroup2 /sys/fs/cgroup -n 2>/dev/null || true

# Must come after the /run and /tmp tmpfs mounts above -- creating these
# first would just get hidden underneath the fresh (empty) tmpfs once it's
# mounted. (/tmp/.X11-unix is baked into the disk image by
# stage_rootfs_dirs, but that copy is invisible once /tmp gets its own
# tmpfs here -- XWayland needs it to exist for real, at boot.)
mkdir -p /run/lock /run/user/1000 /tmp/.X11-unix
chmod 1777 /tmp /var/tmp /tmp/.X11-unix
chown 1000:1000 /run/user/1000
chmod 700 /run/user/1000

echo "rcS: Startup complete."
EOF
    sudo chmod 755 "${ROOTFS_DIR}/etc/init.d/rcS"

    # busybox udhcpc has no built-in network-config logic of its own -- on
    # every deconfig/bound/renew event it just execs whatever script lives at
    # this fixed, compiled-in path (unless overridden via udhcpc's own -s)
    # with the negotiated lease passed in environment variables ($interface,
    # $ip, $subnet, $router, $dns, ...). Without this script present,
    # geckoOS-boot's udhcpc call below would successfully negotiate a lease
    # and then do nothing with it: no address, no default route, no
    # resolv.conf. Written with `ip` (iproute2) rather than ifconfig/route
    # (net-tools), since only iproute2 is in this rootfs's package list.
    write_rootfs_file "${ROOTFS_DIR}/usr/share/udhcpc/default.script" <<'EOF'
#!/bin/bash
set -u

RESOLV_CONF=/etc/resolv.conf

# Converts a dotted-quad netmask (e.g. 255.255.255.0) to its CIDR prefix
# length (e.g. 24) -- `ip addr add` wants a prefix length, but udhcpc only
# hands back a dotted mask via $subnet.
netmask_to_prefix() {
    local mask="$1" prefix=0 octet
    for octet in ${mask//./ }; do
        case "$octet" in
            255) prefix=$((prefix + 8)) ;;
            254) prefix=$((prefix + 7)) ;;
            252) prefix=$((prefix + 6)) ;;
            248) prefix=$((prefix + 5)) ;;
            240) prefix=$((prefix + 4)) ;;
            224) prefix=$((prefix + 3)) ;;
            192) prefix=$((prefix + 2)) ;;
            128) prefix=$((prefix + 1)) ;;
        esac
    done
    echo "$prefix"
}

case "${1:-}" in
    deconfig)
        ip addr flush dev "$interface" 2>/dev/null || true
        ip link set dev "$interface" up
        ;;
    bound|renew)
        prefix=$(netmask_to_prefix "${subnet:-255.255.255.0}")
        ip addr flush dev "$interface" 2>/dev/null || true
        ip addr add "${ip}/${prefix}" dev "$interface"
        ip link set dev "$interface" up

        if [ -n "${router:-}" ]; then
            ip route replace default via "${router%% *}" dev "$interface"
        fi

        {
            for ns in ${dns:-}; do
                echo "nameserver $ns"
            done
        } > "$RESOLV_CONF"
        ;;
esac

exit 0
EOF
    sudo chmod 755 "${ROOTFS_DIR}/usr/share/udhcpc/default.script"

    # Sequential service startup, mirroring the dependency order already
    # expressed in boot/startup/systemd/services/*.service (shift -> echo ->
    # evo -> {gecko, rockhopper}) — those unit files aren't consumed
    # by BusyBox init, so this script is the authoritative boot order for now.
    write_rootfs_file "${ROOTFS_DIR}/usr/sbin/geckoOS-boot" <<'EOF'
#!/bin/bash
set -u

echo "=== GeckoOS boot sequence starting ==="

export XDG_RUNTIME_DIR=/run/user/1000
export WAYLAND_DISPLAY=wayland-0
export GDK_BACKEND=wayland
export QT_QPA_PLATFORM=wayland

mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

start_daemon() {
    local name="$1" bin="$2"
    shift 2
    if [ ! -x "$bin" ]; then
        echo "[boot] SKIP ${name}: ${bin} not found"
        return
    fi
    echo "[boot] starting ${name}..."
    # `setsid` + `</dev/null` is load-bearing, NOT tidiness (found 2026-08-16
    # on a real boot). This script runs from /etc/inittab as `::wait:`, so
    # BusyBox init waits for it and then reaps it -- and every daemon
    # backgrounded here is in this script's process group, sharing
    # /dev/console as its controlling terminal. When the script exits, that
    # group gets SIGHUP and every daemon dies with it.
    #
    # The symptom was brutal to read: /var/log/{echo,evo,vela}.log were all
    # written normally at boot and then simply stopped mid-line with no error,
    # and by the time you got a shell `ps` showed nothing but the two getty
    # bashes -- an apparently daemon-less boot that came back perfectly when
    # you ran this same script by hand from that shell (where the interactive
    # shell keeps the session alive, so no SIGHUP is ever sent).
    #
    # setsid puts each daemon in its own session with no controlling terminal,
    # which is what a daemon should have anyway; </dev/null stops any of them
    # blocking on console reads.
    setsid "$bin" "$@" > "/var/log/${name}.log" 2>&1 < /dev/null &
}

wait_for_socket() {
    local path="$1" tries="${2:-20}" i=0
    while [ ! -S "$path" ] && [ "$i" -lt "$tries" ]; do
        sleep 0.5
        i=$((i + 1))
    done
    [ -S "$path" ]
}

# systemd-udevd is used standalone here purely as a device manager (never
# as init/PID 1 -- BusyBox owns /sbin/init). Evo's --tty-udev backend
# enumerates devices via libudev, which wants /run/udev/data/* populated;
# without a running udevd that cache is empty and device properties
# (seat tags, DRM node info) may be missing even though /dev itself has
# nodes from the initramfs's mdev pass.
UDEVD_BIN=""
for candidate in /usr/lib/systemd/systemd-udevd /lib/systemd/systemd-udevd; do
    [ -x "$candidate" ] && UDEVD_BIN="$candidate" && break
done
if [ -n "$UDEVD_BIN" ]; then
    start_daemon udevd "$UDEVD_BIN"
    sleep 1
    udevadm trigger --action=add >/dev/null 2>&1 || true
    udevadm settle --timeout=10 || true
    echo "[boot] udevd running, devices triggered"
else
    echo "[boot] WARN: systemd-udevd not found, relying on initramfs mdev device nodes only"
fi

# Networking: bring up the first real NIC via DHCP. Nothing in this rootfs
# runs a DHCP client by default -- iproute2 and busybox's udhcpc applet are
# both present, but without this the guest NIC (QEMU's e1000, see
# scripts/emulate.sh's -netdev user/-device e1000) sits administratively
# down with no address, even though QEMU's own SLIRP/usermode backend is
# already serving DHCP on the other end. The interface name isn't hardcoded
# ("eth0") since systemd-udevd (just started above, for Evo's own device
# enumeration) applies predictable-network-interface-naming rules that can
# rename it (e.g. ens3/enp0s3) depending on the PCI topology.
#
# Found live (booted the packaged image under QEMU): naively picking
# "first non-lo interface alphabetically" picks "dummy0" every time, not
# the real NIC -- this kernel's CONFIG_DUMMY always creates a dummy0
# interface (and CONFIG_IPV6_SIT a sit0 tunnel one), and "dummy0"/"sit0"
# both sort before any real predictable-named NIC ("ens*"/"enp*"/"eth*").
# Only an interface with a real backing device -- a /sys/class/net/*/device
# symlink to its PCI/USB parent, which synthetic interfaces (dummy, sit,
# lo, ...) never have -- can actually reach QEMU's DHCP server.
NET_IFACE=""
for candidate in $(ls /sys/class/net 2>/dev/null); do
    [ -e "/sys/class/net/${candidate}/device" ] && NET_IFACE="$candidate" && break
done
if [ -n "$NET_IFACE" ]; then
    ip link set dev "$NET_IFACE" up 2>/dev/null || true
    # -b: fork to background even if a lease isn't obtained immediately,
    # so a slow/absent DHCP server can never block the rest of boot --
    # it just keeps retrying (and renewing) in the background via
    # /usr/share/udhcpc/default.script.
    start_daemon network /bin/busybox udhcpc -i "$NET_IFACE" -b
    echo "[boot] requesting DHCP lease on ${NET_IFACE}"
else
    echo "[boot] WARN: no network interface found under /sys/class/net, skipping DHCP"
fi

start_daemon shift /usr/bin/shift-daemon
sleep 1

start_daemon echo /usr/bin/echo-daemon
if wait_for_socket "${XDG_RUNTIME_DIR}/geckoOS/echo.sock" 20; then
    echo "[boot] echo socket ready"
else
    echo "[boot] WARN: echo socket not seen after 10s, continuing anyway"
fi

# seatd (found via a real boot, not by inspection): Evo's --tty-udev
# backend uses LibSeatSession (evo/src/udev.rs) for VT/device access
# mediation, which needs EITHER seatd OR systemd-logind running -- this
# image has neither running by default: the `seatd` package was already
# in the apt list above, but nothing ever actually STARTED the daemon, and
# there is no systemd-logind at all (BusyBox is PID 1). Without it,
# LibSeatSession::new() has nothing to connect to and Evo fails before it
# ever reaches the point of creating its Wayland socket -- which is
# exactly why waiting longer for that socket (the fix above) didn't help;
# Evo was never going to create it. `-g video` matches seatd's own
# upstream systemd unit (`ExecStart=seatd -g video`) -- the seatd socket
# is created group-owned by `video`, though Evo itself still runs as root
# here so group membership isn't actually the gating factor for it.
start_daemon seatd /usr/sbin/seatd -g video
if wait_for_socket "/run/seatd.sock" 20; then
    echo "[boot] seatd socket ready"
else
    echo "[boot] WARN: seatd socket not seen after 10s -- evo will likely fail to acquire a seat"
fi

# Force the seatd libseat backend explicitly rather than relying on
# auto-detection order -- there is no D-Bus system bus running at all in
# this image (no dbus-daemon started anywhere in this script), so the
# logind backend would fail on connect anyway, but being explicit removes
# any ambiguity about which backend libseat tries first.
export LIBSEAT_BACKEND=seatd
# RUST_BACKTRACE=full scoped to just this invocation (not exported
# globally): evo has died silently mid-session with zero panic message and
# no kernel-logged segfault in /var/log/evo.log -- if it IS a Rust panic
# rather than a native abort (e.g. from Mesa/virgl hitting an assertion on
# the abrupt full-output resize maximize_request does), this is what would
# finally surface it there instead of the process just going quiet.
RUST_BACKTRACE=full start_daemon evo /usr/bin/evo --tty-udev
# NOT just `sleep 1` (found via a real boot: vela raced ahead of
# Evo and failed with "Failed to connect to Wayland display ... No such
# file or directory") -- Evo's own compositor init (DRM/GPU probing, seat
# enumeration) can take longer than a fixed sleep, especially cold under
# QEMU/virtio-gpu. Same wait_for_socket pattern already used for echo's
# socket above, just for Evo's Wayland socket instead.
if wait_for_socket "${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}" 40; then
    echo "[boot] evo wayland socket ready"
    # Evo runs as root and creates ${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}
    # owned root:root (mode ~0755 from root's umask). But vela runs
    # as geckouser (uid 1000) -- Gecko REFUSES to run as root -- and a
    # unix socket owned by root can't be connect()'d by a different user:
    # connecting requires WRITE on the socket, and "other" only gets r-x.
    # That's exactly the "Failed to connect ... Permission denied" a real
    # boot showed once the socket-name desync (wayland-1 vs wayland-0) was
    # fixed and the socket finally EXISTED. Hand the socket to geckouser so
    # the unprivileged app can connect. Also hand over echo's socket + dir
    # (created root:root by echo-daemon) so the window.geckoOS bridge can
    # reach Echo later when app JS actually calls a service -- not needed
    # for the bare index.html render, but cheap to do here and avoids the
    # identical permission problem surfacing on the first bridge call.
    #
    # This is the pragmatic bring-up fix, not the final architecture: the
    # "proper" model runs the whole session (evo, echo, ...) as geckouser
    # with only seatd/udevd as root, so every session socket is
    # geckouser-owned from creation. That's a larger refactor (seatd video-
    # group membership, echo/shift root-need audit) deferred until the
    # smoke test passes.
    chown 1000:1000 "${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}" 2>/dev/null || true
    chown 1000:1000 "${XDG_RUNTIME_DIR}/geckoOS" 2>/dev/null || true
    chown 1000:1000 "${XDG_RUNTIME_DIR}/geckoOS/echo.sock" 2>/dev/null || true
else
    echo "[boot] WARN: evo wayland socket not seen after 20s, continuing anyway"
fi

# Bloom (pyxis-experience) is now a real, Nova-discoverable pyxis:// app
# (manifest.webapp + webroot staged by stage_bloom_app to
# /usr/share/geckoOS/apps/bloom/) -- launched below as the actual desktop
# shell, replacing the earlier plain-HTTP interim measure (BusyBox httpd
# serving static files with no real Echo bridge active on that origin,
# which is why testapp -- not Bloom -- was the thing actually exercised
# end-to-end up to this point). Bloom's own Terminal/Files/Settings apps
# now talk to the real Echo services (Nova, AppHost, Power, Network,
# Terminal) through the compiled-in window.geckoOS bridge exactly like
# testapp already proved out.
#
# origins.map is no longer hand-written here for a single app: Nova is a
# real service now (echo/src/services/nova.rs, registered inside
# echo-daemon) and already scanned /usr/share/geckoOS/apps/* and wrote a
# correct /run/pyxis/nova/origins.map covering EVERY discovered
# manifest.webapp (bloom, testapp) during echo-daemon's own startup
# (register_nova_handlers's initial reload(), which runs well before this
# point in the boot sequence). Hand-writing a single-entry origins.map
# here the way earlier revisions of this script did would silently
# clobber Nova's real one instead of adding to it -- Nova's writer always
# overwrites the whole file from its own registry, so whichever one runs
# last wins, and there is no reason for this script to race it.
VELA_BIN=/usr/lib/pyxis-gecko/vela
# @@BOOT_APP@@ / @@BOOT_APP_ENTRY@@ are literal placeholder tokens, not shell
# variables -- this whole block is inside a quoted heredoc (<<'EOF' above), so
# nothing in it is expanded when create-rootfs.sh generates this file. The
# outer script substitutes these two tokens with the build's --boot-app
# choice via `sed` right after the heredoc is written (see stage_services()),
# baking the chosen boot target into this exact image rather than deciding it
# at guest boot time.
if [ -x "$VELA_BIN" ] && [ -d /usr/share/geckoOS/apps/@@BOOT_APP@@/webroot ]; then
    # rm -rf before mkdir -p (real bug found via a real report, 2026-07-07):
    # the disk image is a plain QEMU `if=virtio` drive with no `-snapshot`
    # flag (scripts/emulate.sh), so writes -- including anything under
    # /var/lib/pyxis/profiles/terminal -- persist across every `make run`
    # of the SAME packaged image, not just within one boot. Gecko's own
    # per-profile startupCache/ (compiled JS bytecode + XPCOM/JSWindowActor
    # registration data) lives inside this exact directory. A profile
    # created by an EARLIER boot of an older vela build (e.g.
    # before the PyxisBridge actor-registration fix) can leave a stale
    # cache behind that a later boot of a newer, fixed build then reuses --
    # silently reintroducing the "window.geckoOS never appears" symptom
    # even though the fix is genuinely present in the binary. There is
    # nothing in this profile worth persisting for a single-purpose kiosk
    # app (no bookmarks/history/logins), so the simplest correct fix is to
    # never trust cross-boot profile state at all: wipe it before every
    # single boot, unconditionally.
    rm -rf /var/lib/pyxis/profiles/@@BOOT_APP@@
    mkdir -p /var/lib/pyxis/profiles/@@BOOT_APP@@
    chown 1000:1000 /var/lib/pyxis/profiles/@@BOOT_APP@@
    chmod 700 /var/lib/pyxis/profiles/@@BOOT_APP@@

    # Rendering prefs via user.js in the profile dir (found via a real boot:
    # the profile now LOADS -- fix #9's -profile worked, proven because the
    # log reached rendering code, which Firefox only does AFTER a successful
    # profile load; a missing profile exits before any rendering -- but
    # WebRender fell to its SOFTWARE compositor SWGL and failed:
    # "RenderCompositorSWGL failed mapping default framebuffer, no dt").
    # Under QEMU's virtio-gpu/virgl, Firefox tends to BLOCKLIST the virtual
    # GPU and drop to software, and that software path is what's breaking --
    # even though Evo itself drives the SAME virgl GPU with hardware GL fine
    # (per evo.log's "GL Renderer: virgl"). Force hardware WebRender on,
    # overriding the blocklist, and disable the GPU process (simpler,
    # single-process rendering is more robust in this constrained VM and
    # avoids GPU-process sandbox/IPC failure modes). Written as user.js
    # (applied every startup, unlike prefs.js) owned by geckouser so the
    # unprivileged app can read it. If hardware WebRender still can't init,
    # the fallback pref set keeps it from hard-failing on the SWGL path.
    cat > /var/lib/pyxis/profiles/@@BOOT_APP@@/user.js <<'PREFS'
// Pyxis shell rendering prefs -- see scripts/create-rootfs.sh.
user_pref("gfx.webrender.all", true);
user_pref("gfx.webrender.software", false);
user_pref("gfx.webrender.compositor", false);
user_pref("gfx.webrender.compositor.force-enabled", false);
user_pref("layers.gpu-process.enabled", false);
user_pref("media.gpu-process-decoder", false);
user_pref("dom.ipc.processCount", 1);
user_pref("browser.tabs.remote.autostart", false);
// Quiet non-fatal a11y/dbus autolaunch noise seen in the boot log.
user_pref("accessibility.force_disabled", 1);
// Single-purpose app window: never crash-restore a prior session as a normal
// browser window (kept in lockstep with nova.rs's write_profile_prefs).
user_pref("browser.sessionstore.resume_from_crash", false);
user_pref("browser.sessionstore.max_resumed_crashes", 0);
user_pref("browser.startup.page", 0);
// No WebExtensions in a single-purpose profile. Its hidden background-page
// host (chrome://extensions/content/dummy.xhtml) maps as a real Wayland
// toplevel here, with no pyxisApp stamp, so it reaches Evo as vela's default
// `vela-default` app id -- which AppHost then synthesizes a phantom session
// for, and which can present as a GTK-decorated window over the real kiosk
// one. Safe because the window.geckoOS bridge is a compiled-in JSWindowActor
// (PyxisBridge), not an extension. Kept in lockstep with nova.rs's
// write_profile_prefs.
user_pref("extensions.autoDisableScopes", 15);
user_pref("extensions.enabledScopes", 0);
user_pref("extensions.installDistroAddons", false);
user_pref("extensions.systemAddon.update.enabled", false);
user_pref("xpinstall.enabled", false);
PREFS
    chown 1000:1000 /var/lib/pyxis/profiles/@@BOOT_APP@@/user.js

    # geckouser's cache/config dirs (found via a real boot: "Fontconfig
    # error: No writable cache directories" for /home/geckouser/.cache/
    # fontconfig, plus GTK/mesa want $HOME/.cache and $HOME/.config).
    # /var/cache/fontconfig is root-owned and un-writable by geckouser, so
    # fontconfig falls back to the per-user cache -- which has to exist and
    # be writable. Pre-create them owned by geckouser here (idempotent;
    # /home is persistent ext4, but doing it every boot guarantees correct
    # ownership regardless of image-build drift). Non-fatal warnings, but
    # cheap to silence and they slow font enumeration.
    mkdir -p /home/geckouser/.cache/fontconfig /home/geckouser/.config
    chown -R 1000:1000 /home/geckouser/.cache /home/geckouser/.config

    # geckoOS-boot runs as root (started by BusyBox init, itself root), but
    # Gecko REFUSES to run as root ("Running Nightly as root in a regular
    # user's session is not supported") -- found via a real boot. Drop to
    # geckouser (uid 1000, the owner of $XDG_RUNTIME_DIR=/run/user/1000
    # per rcS's `chown 1000:1000 /run/user/1000` above) rather than trying
    # to run the whole boot sequence as a non-root user (evo/shift/echo/
    # udevd all still need root).
    #
    # Uses `setpriv` (util-linux, already installed), NOT `su`: `su` goes
    # through PAM for session setup, and this minimal debootstrapped rootfs
    # has no /etc/pam.d/ configuration for it -- a real, likely failure
    # mode in a context with no interactive login anywhere else in this
    # image. `setpriv --reuid/--regid` drops privileges via direct
    # setuid()/setgid() syscalls with no PAM/login-session machinery at
    # all, which is exactly what a non-interactive boot script needs.
    # `--groups video,render` (NOT --clear-groups, found via a real boot:
    # content rendering fell back to software -- "failed to open
    # /dev/dri/renderD128: Permission denied" then "ZINK: vkCreateInstance
    # failed" -- even before this fix, geckouser was never actually added
    # as a member of video/render in /etc/group, so --clear-groups wasn't
    # even the whole story: setgid() alone only sets the primary gid, and
    # nothing was granting the DRI device's group ownership either way).
    # --groups resolves these group names directly and sets exactly this
    # supplementary list via setgroups(), independent of /etc/group's
    # member list, so it's sufficient on its own -- no separate `usermod
    # -aG` step needed.
    # NOT --app: collides with Gecko's own native `-app <path>` flag
    # (browser/app/nsBrowserApp.cpp, an unrelated alternate-application.ini
    # feature, checked at argv[1] before XPCOM/JS starts) -- a real boot
    # showed this exact collision ("Error: unrecognized path: testapp").
    # Use --pyxis-app, matching the rename in BrowserContentHandler.sys.mjs
    # and gecko/src/gecko.rs's compose_launch.
    # HOME=/home/geckouser (owned 1000:1000, created by stage_rootfs_dirs +
    # chowned by stage_permissions) is set explicitly: setpriv --reuid does
    # NOT change HOME, so without this vela would inherit root's
    # HOME (/root, mode 700 root-owned) and every GTK/Gecko write under
    # $HOME/.cache, $HOME/.config, dconf, etc. would be denied -- the exact
    # same permission class as the socket problem, just one step later.
    # Set here proactively rather than discovering it on the next boot.
    #
    # --profile <dir> (found via a real boot: "Your Firefox profile cannot
    # be loaded. It may be missing or inaccessible."): PYXIS_PROFILE_DIR is
    # only an ENV var -- nothing in Gecko natively consumes it to actually
    # SELECT the profile, so without an explicit -profile Firefox falls back
    # to its default $HOME/.mozilla/firefox/profiles.ini resolution and
    # fails. `-profile <path>` is Gecko's native "use this exact directory
    # as the profile" flag (parsed by nsAppRunner before the JS command-line
    # handler, so it composes fine with our --pyxis-app JS flag) -- point it
    # at the same per-app dir PYXIS_PROFILE_DIR names, which is pre-created
    # and chowned to geckouser 700 above, i.e. writable by this process.
    # This is also the per-app storage isolation the pyxis:// origin model
    # wants (each app gets its own profile), so it's the right thing, not
    # just a workaround.
    # PYXIS_DISPLAY_MODE=@@BOOT_DISPLAY_MODE@@ (see the BOOT_DISPLAY_MODE
    # comment above the BOOT_APP case statement): "fullscreen" for Bloom
    # (the desktop shell should own the whole screen, no Evo SSD title bar),
    # "maximized" for the standalone-terminal MVP boot target (Evo's own SSD
    # title bar still wraps it there -- no borderless/undecorated window
    # hint wired up for that path, a real but low-priority cosmetic gap).
    #
    # --app-entry is passed explicitly (now BOOT_APP_ENTRY=index.html for
    # both terminal and bloom, see the BOOT_APP case above) even though it
    # now matches BrowserContentHandler.sys.mjs's own "index.html" default
    # for `--pyxis-app <id>` with no `--app-entry` override. It used to be
    # load-bearing: terminal's webroot was a full copy of Bloom's shared
    # multi-page `dist/` (so it ALSO contained Bloom's own index.html, and
    # omitting the flag would silently boot the full Bloom shell instead).
    # Terminal is now its own independent package with its own isolated
    # dist/ (see stage_terminal_app), so that collision can't happen
    # anymore -- kept explicit anyway since relying on an unstated default
    # is exactly the kind of thing that bites you the next time this
    # bootstrapping mechanism changes.
    # WINDOW DECORATION MODEL -- current as of 2026-07-25, verified live.
    #
    # **Evo owns app-window decoration.** Ordinary app windows get Evo's
    # server-side title bar (shell/ssd.rs's HeaderBar) and vela draws no
    # chrome of its own. This is the deliberate choice: one compositor-drawn
    # frame keeps decoration consistent across every client instead of
    # inheriting whatever theme GTK happens to apply.
    #
    # MOZ_GTK_TITLEBAR_DECORATION=none is the CLIENT-SIDE HALF of that and is
    # load-bearing -- do not drop it as cosmetic. GTK3 (and therefore vela)
    # never binds `zxdg_decoration_manager_v1` at all; the protocol it does
    # speak is `org_kde_kwin_server_decoration`, which Evo offers with default
    # mode Server (state.rs's KdeDecorationState::new). Measured with
    # WAYLAND_DEBUG protocol traces:
    #   vela with no env override        -> request_mode(Client) -> CSD
    #   vela with this env var set        -> request_mode(Server) -> Evo SSD
    # So the env var is what makes GDK ask for Server; without it Firefox
    # forces its own CSD and you get vela's titlebar instead of Evo's.
    # xdg.rs's ack_configure derives `is_ssd` from whichever protocol the
    # client actually negotiated, so the two decoration sources stay mutually
    # exclusive by negotiation rather than by hardcoding.
    #
    # GTK_CSD=0 is a no-op on Wayland (GetSystemGtkWindowDecoration returns
    # GTK_DECORATION_CLIENT before it ever reads it) -- kept only as belt and
    # braces for any X11/XWayland client that does honour it.
    #
    # NOTE: an earlier 2026-07-08 revision of this comment recorded the
    # opposite model ("app windows keep GTK's titlebar, Evo draws no SSD").
    # That was correct only for xdg-decoration, and was superseded once the
    # KDE protocol path landed. It is called out here because the stale text
    # outlived the code and sent a later audit hunting a contradiction that
    # no longer existed.
    #
    # --kiosk (added below, only for the fullscreen/shell boot mode) is what
    # makes the shell chromeless: it sets nsAppRunner's native gKioskMode,
    # which makes IsAlwaysUndecoratedWindow() true -> gtk_window_set_decorated
    # (false) AND gtk_window_fullscreen() at WIDGET-creation time
    # (widget/gtk/nsWindow.cpp) -- deterministic, unlike setting
    # window.fullScreen from JS after open (which raced the widget realize and
    # was silently lost). Evo composites the shell's resulting fullscreen
    # NON-exclusively (xdg.rs's SHELL_APP_ID branch), so app windows launched
    # from the dock still stack on top. --kiosk affects ONLY this boot vela;
    # Nova-launched apps have their own command lines (no --kiosk) and stay
    # decorated.
    #
    # Bloom is ALSO exempted from SSD compositor-side (xdg.rs's `is_shell`
    # check). Both halves are needed: bloom is launched with the same
    # MOZ_GTK_TITLEBAR_DECORATION=none as every app, so it negotiates Server
    # like they do, and only --kiosk makes it request Client instead. Since
    # --kiosk is passed only when BOOT_DISPLAY_MODE=fullscreen, without that
    # compositor-side guard a windowed-mode boot would put an Evo header bar
    # across the top of the desktop.
    KIOSK_FLAG=""
    if [ "@@BOOT_DISPLAY_MODE@@" = "fullscreen" ]; then
        KIOSK_FLAG="--kiosk"
    fi
    start_daemon vela /usr/bin/setpriv --reuid=1000 --regid=1000 --groups video,render \
        /usr/bin/env MOZ_ENABLE_WAYLAND=1 PYXIS_APP_ID=@@BOOT_APP@@ \
        PYXIS_PROFILE_DIR=/var/lib/pyxis/profiles/@@BOOT_APP@@ \
        PYXIS_DISPLAY_MODE=@@BOOT_DISPLAY_MODE@@ XDG_RUNTIME_DIR=/run/user/1000 \
        WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland \
        MOZ_GTK_TITLEBAR_DECORATION=none GTK_CSD=0 \
        HOME=/home/geckouser \
        "$VELA_BIN" $KIOSK_FLAG --profile /var/lib/pyxis/profiles/@@BOOT_APP@@ \
        --pyxis-app @@BOOT_APP@@ --app-entry @@BOOT_APP_ENTRY@@
else
    echo "[boot] SKIP vela: binary (${VELA_BIN}) or @@BOOT_APP@@ app not staged"
fi

start_daemon rockhopper /usr/bin/rockhopper-daemon

echo "=== GeckoOS boot sequence complete ==="
EOF
    # Bake this build's boot-target choice into the generated script (see the
    # @@BOOT_APP@@ placeholder comment above the vela launch block).
    sudo sed -i \
        "s|@@BOOT_APP_ENTRY@@|${BOOT_APP_ENTRY}|g; s|@@BOOT_DISPLAY_MODE@@|${BOOT_DISPLAY_MODE}|g; s|@@BOOT_APP@@|${BOOT_APP}|g" \
        "${ROOTFS_DIR}/usr/sbin/geckoOS-boot"
    sudo chmod 755 "${ROOTFS_DIR}/usr/sbin/geckoOS-boot"

    log_success "Init and service scripts created (boot target: ${BOOT_APP})"
}

stage_binaries() {
    log_section "Stage 7: Copy project binaries"

    if [[ ! -d "${PROJECT_ROOT}/target/release" ]]; then
        log_info "target/release not found yet, skipping binary copy"
        return
    fi

    local bins=(
        echo-daemon
        echo-cli
        shift
        shift-daemon
        evo
        gecko-shell
        rockhopper-daemon
        rockhopper-compat
        pyxis
        pyxis-appctl
    )

    for bin in "${bins[@]}"; do
        if [[ -f "${PROJECT_ROOT}/target/release/${bin}" ]]; then
            sudo cp "${PROJECT_ROOT}/target/release/${bin}" "${ROOTFS_DIR}/usr/bin/${bin}"
            sudo chmod 755 "${ROOTFS_DIR}/usr/bin/${bin}"
            log_success "Installed ${bin}"
        fi
    done
}

stage_gecko() {
    log_section "Stage 8: vela (from-source Gecko) runtime"

    # SUPERSEDED (2026-07-05): this stage previously downloaded a stock
    # Firefox ESR tarball and cherry-picked its .so files for a planned
    # libxul-FFI embedding that was abandoned (see gecko/src/gecko.rs's
    # top-of-file notice and pyxis-gecko-embedding-plan.md's "Approach A").
    # The real from-source vela build now exists and runs as an
    # ordinary Wayland client (kiosk mode, `--pyxis-app=<id>`) -- this stage now
    # installs that build's full runtime tree, not just a handful of shared
    # libraries for in-process embedding.
    #
    # Install path MUST match gecko/src/gecko.rs's DEFAULT_VELA_PATH
    # ("/usr/lib/pyxis-gecko/vela") exactly -- that constant is what
    # every real launch (gecko-shell today, Nova/appHost once built) resolves
    # against unless VELA_BIN overrides it.
    #
    # The dist/bin directory itself still lives under obj-pyxis-webapp/ (the
    # objdir name; see mozconfig-vela's comment on why that wasn't renamed
    # alongside the binary -- it would force a full from-scratch Gecko
    # rebuild for no functional benefit). Only the BINARY file inside it is
    # now named "vela" (MOZ_APP_NAME, set via mozconfig-vela's
    # --with-app-name=vela), not the objdir/checkout path around it.
    local vela_dist="${PROJECT_ROOT}/target/gecko-src/gecko-dev/obj-pyxis-webapp/dist/bin"
    local pyxis_gecko_root="${ROOTFS_DIR}/usr/lib/pyxis-gecko"

    if [[ ! -d "$vela_dist" || ! -x "${vela_dist}/vela" ]]; then
        log_info "vela build not found at ${vela_dist} -- skipping. Build it first per gecko/BUILD_RUNBOOK.md (MOZCONFIG=gecko/mozconfig-vela ./mach build)."
        return
    fi

    log_info "Installing vela runtime from ${vela_dist} (this is a large, unstripped dev build -- expect several GB and a couple of minutes to copy+strip)"
    sudo mkdir -p "$pyxis_gecko_root"
    # -L (dereference symlinks into real file copies), NOT plain -a: dist/bin
    # has real symlinks pointing OUTSIDE itself -- some relative into sibling
    # objdir/source-tree directories we never copy (application.ini ->
    # ../../build/application.ini, dependentlibs.list -> ../../toolkit/...),
    # some absolute host paths (libonnxruntime.so -> /home/.../onnxruntime-*/
    # libonnxruntime.so, fix_stacks.py, dmd.py). Plain `rsync -a` preserves
    # these AS symlinks, which are only valid relative to this host's build
    # tree -- copied verbatim into the rootfs they become dangling links.
    # application.ini specifically is what XPCOM reads at startup to find
    # the Gecko Runtime Environment; a dangling symlink there is the real
    # cause of a "Couldn't load XPCOM." failure that survived a full rebuild
    # (found by comparing dist/bin's own symlinks against what actually got
    # copied, not by inspection alone -- every other diagnosis attempt before
    # this assumed a missing shared-library dependency).
    sudo rsync -aL --delete "${vela_dist}/" "${pyxis_gecko_root}/"

    # The dev objdir build embeds full debug symbols (libxul.so alone is
    # ~3.4GB unstripped vs. ~150-300MB stripped) -- strip in place so the
    # disk image stays a reasonable size. `file` filters to real ELF
    # objects first since dist/bin also contains scripts/data files that
    # `strip` would just fail (harmlessly) on; loop instead of a single
    # `find -exec strip {} +` so one bad file can't abort the whole batch.
    log_info "Stripping debug symbols from installed ELF binaries..."
    local stripped=0
    while IFS= read -r -d '' f; do
        if sudo file "$f" | grep -q 'ELF'; then
            sudo strip --strip-unneeded "$f" 2>/dev/null && stripped=$((stripped + 1))
        fi
    done < <(sudo find "$pyxis_gecko_root" -maxdepth 1 -type f -print0)
    log_success "Stripped ${stripped} ELF files"

    sudo chmod 755 "${pyxis_gecko_root}/vela" "${pyxis_gecko_root}/vela-bin" 2>/dev/null || true

    log_success "vela installed at ${pyxis_gecko_root}/vela"
}

stage_pyxis_smoketest_app() {
    log_section "Stage 8b: pyxis:// smoketest app"

    # A minimal, already-live-tested pyxis:// app (confirmed working on the
    # host: registration, origin-map resolution, path confinement, and
    # secure-context all verified via the Browser Console -- see
    # gecko/BUILD_RUNBOOK.md's dated entries) for validating the FULL stack
    # inside the real booted OS: kernel -> busybox init -> Evo -> vela
    # kiosk mode -> pyxis:// protocol handler -> (once Echo is up) the
    # window.geckoOS bridge.
    #
    # Now staged as a REAL Nova-discoverable app (manifest.webapp +
    # webroot/ under /usr/share/geckoOS/apps/testapp/), not the earlier
    # ad-hoc /usr/share/geckoOS/pyxis-apps/testapp/webroot path with a
    # hand-written /run/pyxis/nova/origins.map entry -- Nova is a real
    # service now (echo/src/services/nova.rs) and its own registry scan
    # would otherwise just overwrite that hand-written entry the moment it
    # (re)generates origins.map from whatever it actually discovered.
    local src="${PROJECT_ROOT}/target/pyxis-test-app/webroot"
    local app_dir="${ROOTFS_DIR}/usr/share/geckoOS/apps/testapp"

    if [[ ! -d "$src" ]]; then
        log_info "No smoketest app at ${src}, skipping (see BUILD_RUNBOOK.md for how it was created)"
        return
    fi

    sudo mkdir -p "${app_dir}/webroot"
    sudo rsync -a --delete "${src}/" "${app_dir}/webroot/"
    write_rootfs_file "${app_dir}/manifest.webapp" <<'EOF'
{
  "id": "testapp",
  "name": "Pyxis Smoketest",
  "version": "0.1.0",
  "type": "web",
  "entry": "index.html",
  "category": "utilities",
  "singleInstance": true,
  "display": "windowed",
  "permissions": []
}
EOF
    log_success "pyxis:// smoketest app (app-id 'testapp') installed at ${app_dir}"
}

stage_bloom_app() {
    log_section "Stage 8c: Bloom (pyxis-experience) shell app"

    # Bloom is the real desktop shell (status bar, dock, launcher,
    # window/card management, Terminal/Files/Settings apps -- see
    # pyxis-experience/src/) built via `npm run build` in the sibling
    # pyxis-experience/ repo. Staged as a real Nova-discoverable app the
    # same way testapp is (manifest.webapp + webroot/ under
    # /usr/share/geckoOS/apps/bloom/), and launched at boot below in place
    # of the earlier plain-HTTP interim measure -- Bloom's own
    # EchoClient.ts/useEcho.ts already call the real org.geckoOS.* methods
    # (appHost, nova, power, network, terminal, ...) this session wired
    # up; this stage is what actually gets it running through the
    # compiled-in window.geckoOS bridge instead of the
    # window.geckoOS-absent mock fallback that plain-HTTP serving left it
    # stuck on.
    #
    # Broad permission set: Bloom genuinely is the shell managing
    # everything else (launching/killing apps via the dock and launcher,
    # showing notifications, reading/writing files in its Files app,
    # controlling brightness/sleep from Settings) -- see
    # nova-runtime-host-plan.md's KNOWN_CAPABILITIES vocabulary
    # (echo/src/services/nova.rs), which this manifest's permissions[]
    # must be a subset of or Nova's parse-time validation rejects it.
    local src="${PROJECT_ROOT}/../pyxis-experience/dist"
    local app_dir="${ROOTFS_DIR}/usr/share/geckoOS/apps/bloom"

    if [[ ! -d "$src" ]]; then
        log_info "No Bloom build at ${src} -- run 'npm run build' in pyxis-experience/ first, skipping"
        return
    fi

    sudo mkdir -p "${app_dir}/webroot"
    sudo rsync -a --delete "${src}/" "${app_dir}/webroot/"
    write_rootfs_file "${app_dir}/manifest.webapp" <<'EOF'
{
  "id": "bloom",
  "name": "Bloom",
  "version": "0.1.0",
  "type": "web",
  "entry": "index.html",
  "category": "system",
  "singleInstance": true,
  "display": "fullscreen",
  "permissions": [
    "filesystem:read",
    "filesystem:write",
    "power",
    "notifications",
    "appManager:install",
    "appManager:manage"
  ]
}
EOF
    log_success "Bloom shell (app-id 'bloom') installed at ${app_dir}"
}

stage_terminal_app() {
    log_section "Stage 8d: standalone Terminal app"

    # Simplified MVP boot target (2026-07-06): the full Bloom shell's real
    # window.geckoOS bridge now genuinely works end-to-end (verified: real
    # PTY spawned, real TerminalOutput signal observed in a live boot), but
    # Bloom's desktop-shell chrome (dock, launcher, window manager, TWO
    # windows auto-opened on first mount via useWindows.ts's initial state)
    # adds real surface area on top of that -- multiple simultaneous
    # Echo calls firing the instant the page mounts, competing for
    # attention with whatever race/timing issues a real (non-headless)
    # boot surfaces that this session's headless VM testing didn't
    # reproduce. Staging just TerminalApp.tsx standalone (same component,
    # same real useEcho/EchoClient code -- nothing reimplemented) removes
    # that chrome entirely, giving a minimal, single-purpose surface for
    # verifying the real PTY-backed terminal works from an actual
    # interactive session.
    #
    # Terminal is now its own independent npm-workspace package in
    # pyxis-experience (packages/apps/terminal/), not a second Rollup input
    # sharing Bloom's own dist/ -- it builds a plain index.html at its own
    # dist root, same as every other extracted app. Bloom stays staged
    # separately (stage_bloom_app, above) for later use once the shell
    # chrome itself gets debugged; this is a boot-target swap, not a
    # removal.
    local src="${PROJECT_ROOT}/../pyxis-experience/packages/apps/terminal/dist"
    local app_dir="${ROOTFS_DIR}/usr/share/geckoOS/apps/terminal"

    if [[ ! -d "$src" ]]; then
        log_info "No terminal build at ${src} -- run 'npm run build' in pyxis-experience/packages/apps/terminal first, skipping"
        return
    fi

    sudo mkdir -p "${app_dir}/webroot"
    sudo rsync -a --delete "${src}/" "${app_dir}/webroot/"
    # Prefer the app package's own manifest.webapp (source of truth); fall
    # back to the embedded one so an older checkout without it still boots.
    # (Boot-as-terminal still gets PYXIS_DISPLAY_MODE=maximized from the boot
    # script's env regardless of the manifest's display field.)
    local pkg="${PROJECT_ROOT}/../pyxis-experience/packages/apps/terminal"
    if [[ -f "${pkg}/manifest.webapp" ]]; then
        sudo cp "${pkg}/manifest.webapp" "${app_dir}/manifest.webapp"
    else
        write_rootfs_file "${app_dir}/manifest.webapp" <<'EOF'
{
  "id": "terminal",
  "name": "Terminal",
  "version": "0.1.0",
  "type": "web",
  "entry": "index.html",
  "category": "utilities",
  "singleInstance": true,
  "display": "maximized",
  "permissions": []
}
EOF
    fi
    log_success "Standalone Terminal app (app-id 'terminal') installed at ${app_dir}"
}

# Shared body for the six remaining pyxis-experience apps (browser, files,
# notes, contacts, tasks, settings) -- each is its own independent
# npm-workspace package (pyxis-experience/packages/apps/<id>/) that builds a
# plain index.html + dist/, same shape stage_terminal_app already stages.
# One tiny wrapper per app (below) just supplies id/name/category/
# permissions/display and calls this with them, rather than repeating the
# rsync/manifest boilerplate six times.
_stage_pyxis_app() {
    local id="$1" name="$2" category="$3" permissions="$4" display="$5"
    local pkg="${PROJECT_ROOT}/../pyxis-experience/packages/apps/${id}"
    local src="${pkg}/dist"
    local app_dir="${ROOTFS_DIR}/usr/share/geckoOS/apps/${id}"

    if [[ ! -d "$src" ]]; then
        log_info "No ${id} build at ${src} -- run 'npm run build' in pyxis-experience/packages/apps/${id} first, skipping"
        return
    fi

    sudo mkdir -p "${app_dir}/webroot"
    sudo rsync -a --delete "${src}/" "${app_dir}/webroot/"

    # Source of truth for the manifest is the app package's OWN
    # manifest.webapp (it ships with the app, author-controlled -- like a real
    # web-app package). Copy it when present; only fall back to generating one
    # from the id/name/category/permissions/display args if the package
    # doesn't carry its own, so the build still works during the transition.
    if [[ -f "${pkg}/manifest.webapp" ]]; then
        sudo cp "${pkg}/manifest.webapp" "${app_dir}/manifest.webapp"
    else
        write_rootfs_file "${app_dir}/manifest.webapp" <<EOF
{
  "id": "${id}",
  "name": "${name}",
  "version": "0.1.0",
  "type": "web",
  "entry": "index.html",
  "category": "${category}",
  "singleInstance": true,
  "display": "${display}",
  "permissions": [${permissions}]
}
EOF
    fi
    log_success "Standalone ${name} app (app-id '${id}') installed at ${app_dir}"
}

stage_browser_app() {
    log_section "Stage 8e: standalone Browser app"
    # No filesystem/power/notification calls (BrowserApp.tsx only reads
    # network status), so no declared permissions needed.
    _stage_pyxis_app "browser" "Browser" "system" "" "windowed"
}

stage_files_app() {
    log_section "Stage 8f: standalone Files app"
    # Real directory browsing via org.geckoOS.filesystem's readDirectory.
    _stage_pyxis_app "files" "File Manager" "system" \
        '"filesystem:read","filesystem:write"' "windowed"
}

stage_notes_app() {
    log_section "Stage 8g: standalone Notes app"
    # Persists JSON docs through org.geckoOS.filesystem (see
    # @pyxis/app-sdk's appStorage.ts).
    _stage_pyxis_app "notes" "Notes" "pim" \
        '"filesystem:read","filesystem:write"' "windowed"
}

stage_contacts_app() {
    log_section "Stage 8h: standalone Contacts app"
    _stage_pyxis_app "contacts" "Contacts" "pim" \
        '"filesystem:read","filesystem:write"' "windowed"
}

stage_tasks_app() {
    log_section "Stage 8i: standalone Tasks app"
    _stage_pyxis_app "tasks" "Tasks" "pim" \
        '"filesystem:read","filesystem:write"' "windowed"
}

stage_settings_app() {
    log_section "Stage 8j: standalone Settings app"
    # Theme/wallpaper go through org.geckoOS.settings, which isn't in
    # nova.rs's KNOWN_CAPABILITIES gated vocabulary -- no permissions to
    # declare.
    _stage_pyxis_app "settings" "Settings" "system" "" "windowed"
}

stage_installer_app() {
    log_section "Stage 8k: standalone App Installer"
    # Drives org.geckoOS.installer (opkg .ipk install/remove), which then
    # refreshes Nova's registry. appManager:* are the two KNOWN_CAPABILITIES
    # entries covering install/manage rights.
    _stage_pyxis_app "installer" "App Installer" "system" \
        '"appManager:install","appManager:manage"' "windowed"

    # Drop directory the installer scans first (see installer.rs's
    # PACKAGE_SEARCH_ROOTS). Created here so the app has a stable,
    # writable place to find packages on a fresh image.
    sudo mkdir -p "${ROOTFS_DIR}/var/lib/pyxis/incoming"
}

stage_boot_assets() {
    log_section "Stage 9: Boot assets"

    sudo mkdir -p "${ROOTFS_DIR}/boot"

    if [[ -f "${PROJECT_ROOT}/boot/grub/grub.cfg" ]]; then
        sudo mkdir -p "${ROOTFS_DIR}/boot/grub"
        sudo cp "${PROJECT_ROOT}/boot/grub/grub.cfg" "${ROOTFS_DIR}/boot/grub/grub.cfg"
    fi

    if [[ "${KERNEL_PREBUILT}" -eq 1 && -f "${KERNEL_PREBUILT_PATH}" ]]; then
        sudo cp "${KERNEL_PREBUILT_PATH}" "${ROOTFS_DIR}/boot/vmlinuz"
        log_success "Prebuilt kernel copied"
    fi

    if [[ -f "${BOOT_DIR}/init.cpio.gz" ]]; then
        sudo cp "${BOOT_DIR}/init.cpio.gz" "${ROOTFS_DIR}/boot/init.cpio.gz"
        log_success "Initramfs copied"
    fi
}

stage_userspace_apps() {
    log_section "Stage 10: Userspace assets"

    # The old gaia/static -> /usr/share/geckoOS/static/pyxis staging (served
    # over plain HTTP with no real Echo bridge active, see this file's
    # git history) is gone -- Bloom is now staged as a real pyxis:// app by
    # stage_bloom_app instead, which is what actually gets it a working
    # window.geckoOS bridge.
    sudo mkdir -p "${ROOTFS_DIR}/usr/share/geckoOS/apps"

    # Only stage real pyxis:// apps -- an app dir is one carrying a
    # manifest.webapp (Nova's input; its webroot/ is served over pyxis://).
    # The repo's apps/ ALSO still contains legacy "app host MVP" stub apps
    # (native manifest.json + a sleep-loop `launch` script, entry:
    # <ComponentName> from the retired in-DOM app model). Those are vestigial:
    # blindly rsync'ing all of apps/ used to (a) inject fake apps into
    # AppHost.ListApps -- their `launch` stub just sleeps, it's not a real app
    # -- polluting the shell's launcher, (b) make Nova log "skipping invalid
    # app" for each (no manifest.webapp), and (c) splat a stale manifest.json
    # + launch over the REAL terminal app dir (stage_terminal_app stages the
    # pyxis:// terminal to the same path). Filtering on manifest.webapp keeps
    # any real pyxis:// app in apps/ working while ignoring the stubs. The
    # apps/ stub dirs are safe to delete from the repo entirely.
    if [[ -d "${PROJECT_ROOT}/apps" ]]; then
        local staged=0
        for app_dir in "${PROJECT_ROOT}/apps"/*/; do
            [[ -f "${app_dir}manifest.webapp" ]] || continue
            sudo rsync -a "${app_dir}" \
                "${ROOTFS_DIR}/usr/share/geckoOS/apps/$(basename "$app_dir")/"
            staged=$((staged + 1))
        done
        sudo find "${ROOTFS_DIR}/usr/share/geckoOS/apps" -name launch -type f \
            -exec chmod 0755 {} + 2>/dev/null || true
        if [[ ${staged} -gt 0 ]]; then
            log_success "Staged ${staged} pyxis:// app(s) from apps/ (manifest.webapp-bearing)"
        else
            log_info "No pyxis:// apps in apps/ (legacy stubs skipped)"
        fi
    else
        log_info "No apps/ directory found at project root; skipping app manifest stage"
    fi

    # Prune stale non-pyxis app dirs from the target. The rootfs dir is reused
    # across incremental builds, so a legacy stub app staged by an OLDER build
    # (before the manifest.webapp filter above) would otherwise persist and
    # keep polluting AppHost.ListApps / triggering Nova "skipping invalid app"
    # warnings even after this script stopped staging it. By this point the
    # real pyxis:// apps (testapp/bloom/terminal) are already staged WITH a
    # manifest.webapp by their own stages, so any app dir lacking one is stale
    # and safe to remove.
    local pruned=0
    for target_dir in "${ROOTFS_DIR}/usr/share/geckoOS/apps"/*/; do
        [[ -d "$target_dir" ]] || continue
        if [[ ! -f "${target_dir}manifest.webapp" ]]; then
            sudo rm -rf "$target_dir"
            pruned=$((pruned + 1))
        fi
    done
    if [[ ${pruned} -gt 0 ]]; then
        log_info "Pruned ${pruned} stale non-pyxis app dir(s) from target"
    fi
}

stage_permissions() {
    log_section "Stage 11: Final permissions"

    sudo chmod 1777 "${ROOTFS_DIR}/tmp" "${ROOTFS_DIR}/var/tmp"
    sudo chmod 700 "${ROOTFS_DIR}/run/user/1000"
    sudo chmod 755 "${ROOTFS_DIR}/etc/init.d"/* 2>/dev/null || true
    sudo chmod 755 "${ROOTFS_DIR}/usr/bin" 2>/dev/null || true

    if [[ -d "${ROOTFS_DIR}/home/geckouser" ]]; then
        sudo chown -R 1000:1000 "${ROOTFS_DIR}/home/geckouser"
    fi

    log_success "Permissions finalized"
}

main() {
    log_section "GeckoOS ROOT FILESYSTEM BUILDER"
    log_info "Project: ${PROJECT_ROOT}"
    log_info "Rootfs:  ${ROOTFS_DIR}"

    require_tools
    stage_prepare_rootfs
    stage_debootstrap_base
    stage_mount_chroot_support
    stage_install_base_packages
    # Must run while the chroot mounts are still up: it needs /proc and
    # working DNS inside the chroot to fetch and compile the tarball.
    stage_opkg
    cleanup_mounts

    stage_rootfs_dirs
    stage_system_config
    stage_busybox_init
    stage_services
    stage_binaries
    stage_gecko
    stage_pyxis_smoketest_app
    stage_bloom_app
    stage_terminal_app
    stage_browser_app
    stage_files_app
    stage_notes_app
    stage_contacts_app
    stage_tasks_app
    stage_settings_app
    stage_installer_app
    stage_boot_assets
    stage_userspace_apps
    stage_permissions

    log_section "ROOTFS CREATION COMPLETE"
    log_success "Rootfs ready at: ${ROOTFS_DIR}"
}

main "$@"
