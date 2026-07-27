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

  # -----------------------------------------------------------------------
  # Block / storage
  # -----------------------------------------------------------------------
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

  # -----------------------------------------------------------------------
  # Display / GPU
  # -----------------------------------------------------------------------
  # CONFIG_DRM_FBDEV_EMULATION is the one that actually controls console
  # resolution. CONFIG_FB + CONFIG_FRAMEBUFFER_CONSOLE alone give you fbcon,
  # but a KMS driver like virtio-gpu registers NO fbdev without the emulation
  # layer -- so fbcon stays bound to whatever early framebuffer the firmware
  # left behind (OVMF's 800x600 GOP) and the console never picks up the mode
  # QEMU advertises over EDID, even though Wayland/Evo on /dev/dri/card0 does.
  ensure_y CONFIG_DRM
  ensure_y CONFIG_DRM_VIRTIO_GPU
  ensure_y CONFIG_DRM_FBDEV_EMULATION
  ensure_y CONFIG_FB
  ensure_y CONFIG_FRAMEBUFFER_CONSOLE
  # simpledrm takes over the UEFI GOP framebuffer as a real DRM device, then
  # hands the aperture off to virtio-gpu when it probes. Without a DRM-based
  # early console the handoff is a plain efifb teardown, which is what leaves
  # the console stranded at the firmware mode.
  ensure_y CONFIG_SYSFB_SIMPLEFB
  ensure_y CONFIG_DRM_SIMPLEDRM
  # Bind fbcon to the primary (virtio-gpu) device rather than the first one
  # registered, which is the firmware framebuffer.
  ensure_y CONFIG_FRAMEBUFFER_CONSOLE_DETECT_PRIMARY

  # -----------------------------------------------------------------------
  # Serial console — QEMU's -serial mon:stdio exposes an ISA 16550-compatible
  # UART (ttyS0). Without the 8250 driver there's no /dev/ttyS0 node at all,
  # so `console=ttyS0,115200` on the kernel cmdline silently does nothing
  # and anything (busybox init respawn, getty) that opens /dev/ttyS0 fails
  # with "No such file or directory".
  # -----------------------------------------------------------------------
  ensure_y CONFIG_SERIAL_8250
  ensure_y CONFIG_SERIAL_8250_CONSOLE
  ensure_y CONFIG_SERIAL_CORE
  ensure_y CONFIG_SERIAL_CORE_CONSOLE
  # CONFIG_SERIAL_8250_NR_UARTS/RUNTIME_UARTS are integer-valued, not
  # bool -- leave them to olddefconfig's defaults (4 UARTs) below.

  # -----------------------------------------------------------------------
  # Input devices -- emulate.sh attaches virtio-keyboard-pci and a USB
  # usb-tablet (absolute pointer). Without these, none of them
  # produce a /dev/input/event* node, so libinput enumerates zero devices
  # (it initializes fine, it just never sees anything) and the compositor
  # renders but never receives motion/key events.
  # -----------------------------------------------------------------------
  ensure_y CONFIG_INPUT_EVDEV
  ensure_y CONFIG_INPUT_MOUSEDEV
  ensure_y CONFIG_INPUT_KEYBOARD
  ensure_y CONFIG_VIRTIO_INPUT
  ensure_y CONFIG_USB
  ensure_y CONFIG_HID
  ensure_y CONFIG_USB_HID
  ensure_y CONFIG_USB_XHCI_HCD
  ensure_y CONFIG_USB_EHCI_HCD
  ensure_y CONFIG_USB_UHCI_HCD
  ensure_y CONFIG_USB_OHCI_HCD

  # -----------------------------------------------------------------------
  # Device management
  # -----------------------------------------------------------------------
  ensure_y CONFIG_DEVTMPFS
  ensure_y CONFIG_DEVTMPFS_MOUNT
  ensure_y CONFIG_SYSFS
  ensure_y CONFIG_PROC_FS

  # -----------------------------------------------------------------------
  # Initramfs decompression — REQUIRED or the kernel panics with
  # "Initramfs unpacking failed: decompressor failed" at boot.
  # Must be =y (not =m) because modules are not yet loaded at unpack time.
  # -----------------------------------------------------------------------
  ensure_y CONFIG_BLK_DEV_INITRD
  ensure_y CONFIG_RD_GZIP

  # -----------------------------------------------------------------------
  # Filesystem: tmpfs
  # Without CONFIG_TMPFS the kernel rejects mount options such as mode=
  # with "Unknown parameter 'mode'" which causes systemd to fail mounting
  # /run, /dev/shm, and /tmp at startup.
  # -----------------------------------------------------------------------
  ensure_y CONFIG_TMPFS
  ensure_y CONFIG_TMPFS_XATTR

  # -----------------------------------------------------------------------
  # Filesystem: autofs
  # Renamed from autofs4 to autofs in Linux 5.7. systemd logs
  # "Failed to find module 'autofs4'" when this is absent.
  # -----------------------------------------------------------------------
  ensure_y CONFIG_AUTOFS_FS

  # -----------------------------------------------------------------------
  # Cgroups — required for systemd and container isolation.
  # CONFIG_CGROUP_BPF + CONFIG_MEMCG are needed for the cgroup2 mount
  # options nsdelegate and memory_recursiveprot that systemd requests.
  # Without them: "Failed to mount cgroup2 ... No such file or directory"
  # -----------------------------------------------------------------------
  ensure_y CONFIG_CGROUPS
  ensure_y CONFIG_CGROUP_BPF
  ensure_y CONFIG_MEMCG
  ensure_y CONFIG_CGROUP_WRITEBACK
  ensure_y CONFIG_CGROUP_SCHED
  ensure_y CONFIG_CGROUP_PIDS
  ensure_y CONFIG_CGROUP_FREEZER
  ensure_y CONFIG_CGROUP_DEVICE
  ensure_y CONFIG_CPUSETS

  # -----------------------------------------------------------------------
  # Syscall / IPC interfaces required by systemd, dbus, and udev
  # -----------------------------------------------------------------------
  ensure_y CONFIG_INOTIFY_USER
  ensure_y CONFIG_SIGNALFD
  ensure_y CONFIG_TIMERFD
  ensure_y CONFIG_EPOLL
  ensure_y CONFIG_UNIX       # AF_UNIX sockets (dbus, systemd socket activation)
  ensure_y CONFIG_NET        # base networking (AF_UNIX depends on this)

  # -----------------------------------------------------------------------
  # NIC driver — emulate.sh attaches `-device e1000,netdev=net0`. Found live
  # (booted the packaged image under QEMU): CONFIG_NET was enough for the
  # network *stack*, but with no matching NIC driver the e1000 PCI device
  # never gets a netdev at all -- `ip addr show` listed only lo/dummy0/sit0
  # (CONFIG_DUMMY's own always-there interface, plus the sit tunnel driver),
  # nothing hardware-backed for busybox udhcpc to request a lease on.
  # -----------------------------------------------------------------------
  ensure_y CONFIG_NET_VENDOR_INTEL
  ensure_y CONFIG_E1000
  ensure_y CONFIG_E1000E
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
