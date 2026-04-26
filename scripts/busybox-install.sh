#!/bin/bash
# GeckoOS BusyBox Automation Script

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
BUSYBOX_BUILD="${BUILD_DIR}/busybox-build"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[\u2713]${NC} $*"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_section() {
  echo ""
  echo "════════════════════════════════════════════"
  echo " $*"
  echo "════════════════════════════════════════════"
}

require_cmds() {
  local cmds=(git make gcc file sed)
  for cmd in "${cmds[@]}"; do
    command -v "$cmd" >/dev/null 2>&1 || { log_error "Missing: $cmd"; exit 1; }
  done
}

# Force a BusyBox .config option to 'y', adding it if absent.
enable_config() {
  local key="$1" file="$2"
  if grep -q "^${key}=" "$file"; then
    sed -i "s/^${key}=.*/${key}=y/" "$file"
  elif grep -q "^# ${key} is not set" "$file"; then
    sed -i "s/^# ${key} is not set/${key}=y/" "$file"
  else
    echo "${key}=y" >> "$file"
  fi
}

main() {
  log_section "BusyBox Automated Installation"

  require_cmds

  mkdir -p "$BUSYBOX_BUILD" "$ROOTFS_DIR"
  cd "$BUSYBOX_BUILD"

  if [[ ! -d busybox ]]; then
    log_info "Cloning BusyBox"
    git clone --depth 1 https://github.com/mirror/busybox.git
  fi

  cd busybox

  gcc_version=$(gcc --version | head -n1 | awk '{print $NF}' | cut -d. -f1)
  [[ "$gcc_version" -ge 14 ]] && sed -i '/main()/ { /int main()/! s/main()/int main()/; }' scripts/kconfig/lxdialog/check-lxdialog.sh

  make distclean
  make defconfig

  # -----------------------------------------------------------------------
  # Required applets for GeckoOS initramfs
  # defconfig does NOT enable blkid or mdev; we force them here so the
  # initramfs can resolve root-by-UUID and populate /dev without udev.
  # -----------------------------------------------------------------------

  # blkid — UUID/LABEL superblock scanning (critical for UUID= root lookup)
  enable_config CONFIG_BLKID             .config
  enable_config CONFIG_FEATURE_BLKID_TYPE .config

  # mdev — BusyBox hotplug/device manager (populates /dev from sysfs)
  enable_config CONFIG_MDEV              .config
  enable_config CONFIG_FEATURE_MDEV_CONF .config
  enable_config CONFIG_FEATURE_MDEV_RENAME .config

  # sysfs support (required by mdev -s)
  enable_config CONFIG_FEATURE_SYSFS     .config

  # Disable broken tc applet
  if grep -q '^CONFIG_TC=' .config; then
    sed -i 's/^CONFIG_TC=.*/CONFIG_TC=n/' .config
  else
    echo 'CONFIG_TC=n' >> .config
  fi

  # Enable static linking
  sed -i 's/^# CONFIG_STATIC is not set/CONFIG_STATIC=y/' .config 2>/dev/null \
    || enable_config CONFIG_STATIC .config

  sleep 1
  make oldconfig < /dev/null
  make -j"$(nproc)"

  # Install with sudo into rootfs
  sudo mkdir -p "$ROOTFS_DIR"/{bin,sbin,usr/bin,usr/sbin}
  sudo make CONFIG_PREFIX="$ROOTFS_DIR" install

  sudo rm -f "$ROOTFS_DIR/linuxrc"
  sudo ln -sf /bin/busybox "$ROOTFS_DIR/sbin/init"

  if file "$ROOTFS_DIR/bin/busybox" | grep -q "statically linked"; then
    size=$(du -h "$ROOTFS_DIR/bin/busybox" | cut -f1)
    log_success "BusyBox installed statically ($size)"
  else
    log_error "BusyBox is not statically linked"
    exit 1
  fi

  # Verify critical applets are present
  for applet in blkid mdev; do
    if "$ROOTFS_DIR/bin/busybox" --list 2>/dev/null | grep -q "^${applet}$"; then
      log_success "Applet present: ${applet}"
    else
      log_error "MISSING applet after build: ${applet} — check .config"
      exit 1
    fi
  done

  log_success "BusyBox automation complete"
}

main "$@"
