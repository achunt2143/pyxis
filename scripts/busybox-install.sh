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

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
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

  # Disable broken tc applet
  if grep -q '^CONFIG_TC=' .config; then
    sed -i 's/^CONFIG_TC=.*/CONFIG_TC=n/' .config
  else
    echo 'CONFIG_TC=n' >> .config
  fi

  # Enable static linking
  sed -i 's/^# CONFIG_STATIC is not set/CONFIG_STATIC=y/' .config 2>/dev/null || echo 'CONFIG_STATIC=y' >> .config

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

  log_success "BusyBox automation complete"
}

main "$@"