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

GECKO_PREBUILT=${GECKO_PREBUILT:-0}
KERNEL_PREBUILT=${KERNEL_PREBUILT:-0}
FORCE_REBUILD=${FORCE_REBUILD:-0}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --gecko-prebuilt) GECKO_PREBUILT=1; shift ;;
        --kernel-prebuilt) KERNEL_PREBUILT=1; shift ;;
        --force) FORCE_REBUILD=1; shift ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

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
  file less procps psmisc util-linux kmod udev \
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

    # Sequential service startup, mirroring the dependency order already
    # expressed in boot/startup/systemd/services/*.service (shift -> echo ->
    # evo -> {gecko, rockhopper, vroom}) — those unit files aren't consumed
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
    "$bin" "$@" > "/var/log/${name}.log" 2>&1 &
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
start_daemon evo /usr/bin/evo --tty-udev
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
if [ -x "$VELA_BIN" ] && [ -d /usr/share/geckoOS/apps/terminal/webroot ]; then
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
    rm -rf /var/lib/pyxis/profiles/terminal
    mkdir -p /var/lib/pyxis/profiles/terminal
    chown 1000:1000 /var/lib/pyxis/profiles/terminal
    chmod 700 /var/lib/pyxis/profiles/terminal

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
    cat > /var/lib/pyxis/profiles/terminal/user.js <<'PREFS'
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
PREFS
    chown 1000:1000 /var/lib/pyxis/profiles/terminal/user.js

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
    # PYXIS_DISPLAY_MODE=maximized (not "windowed", the smoketest's choice):
    # boots straight into a fullscreen terminal (2026-07-06 simplified MVP
    # boot target -- see stage_terminal_app's comment above for why Bloom's
    # full shell chrome was swapped out for this minimal single-purpose app
    # while its own dock/launcher/window-manager overhead gets debugged
    # separately). Evo's own SSD title bar still wraps it for now (no
    # borderless/undecorated window hint wired up yet -- a real but
    # low-priority cosmetic gap for a first alpha, not a functional one).
    #
    # --app-entry terminal.html is NOT optional here (real bug hit on the
    # first try at this simplification): BrowserContentHandler.sys.mjs's
    # `--pyxis-app <id>` only sets the app-id/origin -- the entry PATH
    # defaults to "index.html" unless `--app-entry` says otherwise. Because
    # stage_terminal_app's webroot is a full copy of pyxis-experience's
    # shared `dist/` (same assets/ as Bloom's vite multi-page build), it
    # ALSO contains Bloom's own index.html -- so without this flag,
    # `pyxis://terminal/index.html` would silently load the FULL Bloom
    # shell (still working fine per the earlier fix, just not the minimal
    # terminal-only surface this stage exists to provide).
    start_daemon vela /usr/bin/setpriv --reuid=1000 --regid=1000 --groups video,render \
        /usr/bin/env MOZ_ENABLE_WAYLAND=1 PYXIS_APP_ID=terminal \
        PYXIS_PROFILE_DIR=/var/lib/pyxis/profiles/terminal \
        PYXIS_DISPLAY_MODE=maximized XDG_RUNTIME_DIR=/run/user/1000 \
        WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland \
        HOME=/home/geckouser \
        "$VELA_BIN" --profile /var/lib/pyxis/profiles/terminal \
        --pyxis-app terminal --app-entry terminal.html
else
    echo "[boot] SKIP vela: binary (${VELA_BIN}) or Terminal app not staged"
fi

start_daemon rockhopper /usr/bin/rockhopper-daemon
start_daemon vroom-monitor /usr/bin/performance-monitor

echo "=== GeckoOS boot sequence complete ==="
EOF
    sudo chmod 755 "${ROOTFS_DIR}/usr/sbin/geckoOS-boot"

    log_success "Init and service scripts created"
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
        gpu-accelerator
        rockhopper-daemon
        rockhopper-compat
        performance-monitor
        performance-tuner
        pyxis
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
  "display": "maximized",
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
    # Reuses pyxis-experience's SAME `dist/` build (vite.config.ts now
    # builds terminal.html as a second Rollup input alongside index.html,
    # sharing the same assets/ output) -- just points entry at
    # "terminal.html" instead of "index.html". Bloom stays staged
    # separately (stage_bloom_app, above) for later use once the shell
    # chrome itself gets debugged; this is a boot-target swap, not a
    # removal.
    local src="${PROJECT_ROOT}/../pyxis-experience/dist"
    local app_dir="${ROOTFS_DIR}/usr/share/geckoOS/apps/terminal"

    if [[ ! -d "$src" ]]; then
        log_info "No pyxis-experience build at ${src} -- run 'npm run build' in pyxis-experience/ first, skipping"
        return
    fi
    if [[ ! -f "${src}/terminal.html" ]]; then
        log_info "No terminal.html in ${src} -- rebuild pyxis-experience with the terminal.html Rollup input, skipping"
        return
    fi

    sudo mkdir -p "${app_dir}/webroot"
    sudo rsync -a --delete "${src}/" "${app_dir}/webroot/"
    write_rootfs_file "${app_dir}/manifest.webapp" <<'EOF'
{
  "id": "terminal",
  "name": "Terminal",
  "version": "0.1.0",
  "type": "web",
  "entry": "terminal.html",
  "category": "utilities",
  "singleInstance": true,
  "display": "maximized",
  "permissions": []
}
EOF
    log_success "Standalone Terminal app (app-id 'terminal') installed at ${app_dir}"
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

    if [[ -d "${PROJECT_ROOT}/apps" ]]; then
        sudo rsync -a "${PROJECT_ROOT}/apps/" "${ROOTFS_DIR}/usr/share/geckoOS/apps/"
        sudo find "${ROOTFS_DIR}/usr/share/geckoOS/apps" -name launch -type f -exec chmod 0755 {} +
        log_success "Sample app manifests copied to /usr/share/geckoOS/apps/"
    else
        log_info "No apps/ directory found at project root; skipping app manifest stage"
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
    stage_boot_assets
    stage_userspace_apps
    stage_permissions

    log_section "ROOTFS CREATION COMPLETE"
    log_success "Rootfs ready at: ${ROOTFS_DIR}"
}

main "$@"
