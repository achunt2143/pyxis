#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/kernel"
BOOT_DIR="${PROJECT_ROOT}/target/boot"
MODULES_INSTALL_DIR="${PROJECT_ROOT}/target/kernel-modules"

KERNEL_VERSION="${KERNEL_VERSION:-7.0.1}"
KERNEL_MAJOR="${KERNEL_MAJOR:-7}"
KERNEL_CONFIG="${KERNEL_CONFIG:-$SCRIPT_DIR/.config}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
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
  for cmd in gcc make bc bison flex wget tar xz cpio perl; do
    command -v "$cmd" >/dev/null 2>&1 || { log_error "Missing command: $cmd"; exit 1; }
  done
  dpkg -s libelf-dev >/dev/null 2>&1 || { log_error "Missing package: libelf-dev"; exit 1; }
  dpkg -s libssl-dev >/dev/null 2>&1 || { log_error "Missing package: libssl-dev"; exit 1; }
}

force_kconfig() {
  local cfg="$1"

  ensure_y() {
    local sym="$1"
    if grep -q "^# ${sym} is not set" "$cfg"; then
      sed -i "s/^# ${sym} is not set/${sym}=y/" "$cfg"
    elif grep -q "^${sym}=m" "$cfg"; then
      sed -i "s/^${sym}=m/${sym}=y/" "$cfg"
    elif grep -q "^${sym}=n" "$cfg"; then
      sed -i "s/^${sym}=n/${sym}=y/" "$cfg"
    elif ! grep -q "^${sym}=" "$cfg"; then
      echo "${sym}=y" >> "$cfg"
    fi
  }

  ensure_y CONFIG_PCI
  ensure_y CONFIG_VIRTIO
  ensure_y CONFIG_VIRTIO_PCI
  ensure_y CONFIG_VIRTIO_BLK
  ensure_y CONFIG_VIRTIO_RING
  ensure_y CONFIG_SCSI
  ensure_y CONFIG_BLK_DEV_SD
  ensure_y CONFIG_ATA
  ensure_y CONFIG_SATA_AHCI
  ensure_y CONFIG_EXT4_FS
  ensure_y CONFIG_JBD2
  ensure_y CONFIG_MBCACHE
  ensure_y CONFIG_DRM
  ensure_y CONFIG_DRM_VIRTIO_GPU
  ensure_y CONFIG_FB
  ensure_y CONFIG_FRAMEBUFFER_CONSOLE
  ensure_y CONFIG_DEVTMPFS
  ensure_y CONFIG_DEVTMPFS_MOUNT

  # -----------------------------------------------------------------------
  # initramfs decompression support — REQUIRED or the kernel will panic with
  # "Initramfs unpacking failed: decompressor failed" at boot.
  # build-initramfs.sh produces a gzip-compressed cpio archive; the kernel
  # must have both BLK_DEV_INITRD (initramfs support at all) and RD_GZIP
  # (gzip decompressor) compiled in (=y, not =m — cannot be a module because
  # modules are not yet loaded when initramfs is unpacked).
  # -----------------------------------------------------------------------
  ensure_y CONFIG_BLK_DEV_INITRD
  ensure_y CONFIG_RD_GZIP
}

main() {
  log_section "GeckoOS Kernel Build"
  log_info "Target kernel: ${KERNEL_VERSION}"
  log_info "Config file: ${KERNEL_CONFIG}"

  require_cmds
  [[ -f "$KERNEL_CONFIG" ]] || { log_error "Config not found: $KERNEL_CONFIG"; exit 1; }

  mkdir -p "$BUILD_DIR" "$BOOT_DIR"
  cd "$BUILD_DIR"

  local archive="linux-${KERNEL_VERSION}.tar.xz"
  local url="https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/${archive}"

  if [[ ! -f "$archive" ]]; then
    log_info "Downloading ${archive}"
    wget -O "$archive" "$url"
  fi

  if [[ ! -d "linux-${KERNEL_VERSION}" ]]; then
    log_info "Extracting source"
    tar -xf "$archive"
  fi

  cd "linux-${KERNEL_VERSION}"

  log_info "Applying config"
  cp "$KERNEL_CONFIG" .config
  force_kconfig .config
  make olddefconfig

  log_info "Building kernel and modules"
  make -j"$(nproc)" bzImage modules

  log_info "Installing kernel modules to ${MODULES_INSTALL_DIR}"
  rm -rf "$MODULES_INSTALL_DIR"
  mkdir -p "$MODULES_INSTALL_DIR"
  make INSTALL_MOD_PATH="$MODULES_INSTALL_DIR" modules_install

  local kernel_image="arch/x86/boot/bzImage"
  [[ -f "$kernel_image" ]] || { log_error "Kernel image not found"; exit 1; }

  cp "$kernel_image" "${BOOT_DIR}/vmlinuz-geckoOS"
  [[ -f System.map ]] && cp System.map "${BOOT_DIR}/System.map-geckoOS"

  log_success "Kernel built: ${BOOT_DIR}/vmlinuz-geckoOS"
  log_success "Kernel modules installed under: ${MODULES_INSTALL_DIR}/lib/modules/$(make -s kernelrelease)"
  log_info "Kernel release: $(make -s kernelrelease)"
}

main "$@"
