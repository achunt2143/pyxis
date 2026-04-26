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

    if [[ -d "${ROOTFS_DIR}" ]]; then
        log_info "Removing existing rootfs to ensure a clean debootstrap target..."
        sudo rm -rf "${ROOTFS_DIR}"
    fi

    mkdir -p "${ROOTFS_DIR}"
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

    sudo mount --bind /dev "${ROOTFS_DIR}/dev"
    sudo mount --bind /dev/pts "${ROOTFS_DIR}/dev/pts"
    sudo mount -t proc proc "${ROOTFS_DIR}/proc"
    sudo mount -t sysfs sysfs "${ROOTFS_DIR}/sys"
    sudo mount --bind /run "${ROOTFS_DIR}/run"

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

apt install -y --no-install-recommends \
  apt-utils ca-certificates locales tzdata \
  bash coreutils findutils grep sed gawk diffutils \
  file less procps psmisc util-linux kmod udev \
  iproute2 iputils-ping netbase ifupdown \
  mount passwd login \
  libdrm2 libgbm1 libinput10 libxkbcommon0 libwayland-client0 libwayland-server0 \
  wayland-protocols libegl1 libgles2 mesa-utils libgl1-mesa-dri \
  xwayland libseat1 seatd ${DISPLAY_INFO_PKG:+$DISPLAY_INFO_PKG} \
  dbus dbus-user-session shared-mime-info \
  openssl curl wget

echo "en_US.UTF-8 UTF-8" > /etc/locale.gen
locale-gen
update-locale LANG=en_US.UTF-8

ln -sf /usr/share/zoneinfo/UTC /etc/localtime
echo "UTC" > /etc/timezone
echo "geckoOS" > /etc/hostname

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

stage_services() {
    log_section "Stage 6: Init and service scripts"

    write_rootfs_file "${ROOTFS_DIR}/etc/init.d/rcS" <<'EOF'
#!/bin/sh
echo "=== rcS: Startup sequence beginning ==="

mount -t proc proc /proc -n 2>/dev/null || true
mount -t sysfs sysfs /sys -n 2>/dev/null || true

if ! mountpoint -q /dev; then
    mount -t devtmpfs devtmpfs /dev -n 2>/dev/null || true
fi

[ -c /dev/null ] || mknod -m 666 /dev/null c 1 3
[ -c /dev/tty ] || mknod -m 666 /dev/tty c 5 0
[ -c /dev/console ] || mknod -m 600 /dev/console c 5 1

mkdir -p /dev/pts /run /tmp /var/tmp /run/lock /var/log /run/user/1000
mount -t devpts devpts /dev/pts -o gid=5,mode=620 -n 2>/dev/null || true
mount -t tmpfs tmpfs /run -o mode=755,size=128M -n 2>/dev/null || true
mount -t tmpfs tmpfs /tmp -o mode=1777,size=512M -n 2>/dev/null || true

chmod 1777 /tmp /var/tmp
chmod 700 /run/user/1000

ln -sf /proc/self/fd /dev/fd 2>/dev/null || true
ln -sf /proc/self/fd/0 /dev/stdin 2>/dev/null || true
ln -sf /proc/self/fd/1 /dev/stdout 2>/dev/null || true
ln -sf /proc/self/fd/2 /dev/stderr 2>/dev/null || true

echo "rcS: Startup complete."
EOF
    sudo chmod 755 "${ROOTFS_DIR}/etc/init.d/rcS"

    write_rootfs_file "${ROOTFS_DIR}/etc/init.d/S35evo" <<'EOF'
#!/bin/sh
echo "S35evo: Starting Evo Wayland compositor..."

export WAYLAND_DISPLAY=wayland-0
export XDG_RUNTIME_DIR=/run/user/1000
export GDK_BACKEND=wayland
export QT_QPA_PLATFORM=wayland

mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null || true
chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null || true

if [ -x /usr/bin/evo ]; then
    /usr/bin/evo > /var/log/evo.log 2>&1 &
    sleep 2
    echo "[✓] Evo compositor started"
else
    echo "[!] Evo not found"
fi
EOF
    sudo chmod 755 "${ROOTFS_DIR}/etc/init.d/S35evo"

    log_success "Service scripts created"
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
        cronus
        cronus-daemon
        evo
        gecko-shell
        gpu-accelerator
        persephone-daemon
        persephone-compat
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
    log_section "Stage 8: Gecko runtime"

    sudo mkdir -p "${ROOTFS_DIR}/usr/lib/gecko" "${GECKO_BUILD_DIR}/lib"

    if [[ -f "${GECKO_BUILD_DIR}/firefox/libxul.so" ]]; then
        find "${GECKO_BUILD_DIR}/firefox" -maxdepth 1 -name 'lib*.so*' -exec cp -a {} "${GECKO_BUILD_DIR}/lib/" \;
    elif [[ "${GECKO_PREBUILT}" -eq 0 ]]; then
        local version="140.5"
        local url="https://archive.mozilla.org/pub/firefox/releases/${version}.0esr/linux-x86_64/en-US/firefox-${version}.0esr.tar.xz"
        mkdir -p "${GECKO_BUILD_DIR}"
        curl -L "$url" -o "${GECKO_BUILD_DIR}/firefox.tar.xz"
        tar -xf "${GECKO_BUILD_DIR}/firefox.tar.xz" -C "${GECKO_BUILD_DIR}"
        find "${GECKO_BUILD_DIR}/firefox" -maxdepth 1 -name 'lib*.so*' -exec cp -a {} "${GECKO_BUILD_DIR}/lib/" \;
    fi

    if compgen -G "${GECKO_BUILD_DIR}/lib/*.so*" >/dev/null; then
        sudo cp -a "${GECKO_BUILD_DIR}/lib/." "${ROOTFS_DIR}/usr/lib/gecko/"
        write_rootfs_file "${ROOTFS_DIR}/etc/ld.so.conf.d/gecko.conf" <<'EOF'
/usr/lib/gecko
EOF
        log_success "Gecko libraries installed"
    else
        log_info "No Gecko libs found yet, skipping"
    fi
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

    sudo mkdir -p "${ROOTFS_DIR}/usr/share/geckoOS/static"
    sudo mkdir -p "${ROOTFS_DIR}/usr/share/geckoOS/static/pyxis"
    sudo mkdir -p "${ROOTFS_DIR}/usr/share/geckoOS/apps"

    if [[ -d "${PROJECT_ROOT}/gaia/static" ]]; then
        sudo rsync -a "${PROJECT_ROOT}/gaia/static/" "${ROOTFS_DIR}/usr/share/geckoOS/static/pyxis/"
        log_success "Pyxis shell assets copied to /usr/share/geckoOS/static/pyxis/"
    fi

    # Stage installed app manifests + placeholder launch scripts.
    # Each app lives at apps/<id>/ with a manifest.json and a `launch` script;
    # the AppHost service reads these from /usr/share/geckoOS/apps/ at runtime.
    if [[ -d "${PROJECT_ROOT}/apps" ]]; then
        sudo rsync -a "${PROJECT_ROOT}/apps/" "${ROOTFS_DIR}/usr/share/geckoOS/apps/"
        # Make sure launch scripts are executable inside the rootfs.
        sudo find "${ROOTFS_DIR}/usr/share/geckoOS/apps" -name launch -type f -exec chmod 0755 {} +
        log_success "Sample app manifests copied to /usr/share/geckoOS/apps/"
    else
        log_info "No apps/ directory found at project root; skipping app manifest stage"
    fi

    # Write a minimal landing index.html that redirects to the Pyxis shell
    # if no top-level index has been provided by another stage.
    if ! sudo test -f "${ROOTFS_DIR}/usr/share/geckoOS/static/index.html"; then
        sudo tee "${ROOTFS_DIR}/usr/share/geckoOS/static/index.html" >/dev/null <<'EOF'
<!DOCTYPE html><html><head><meta charset="utf-8"><title>GeckoOS</title>
<script>window.location.replace('./pyxis/index.html');</script></head></html>
EOF
        log_success "Wrote minimal landing index.html"
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
    stage_services
    stage_binaries
    stage_gecko
    stage_boot_assets
    stage_userspace_apps
    stage_permissions

    log_section "ROOTFS CREATION COMPLETE"
    log_success "Rootfs ready at: ${ROOTFS_DIR}"
}

main "$@"