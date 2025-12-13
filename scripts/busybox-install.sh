#!/bin/bash

# ============================================================================
# GeckoOS BusyBox Installation Script (ALIGNED WITH MINILINUX2)
# ============================================================================
# Downloads static BusyBox binary and installs it to rootfs
# Creates symlinks for all BusyBox applets
# ============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BUILD_DIR="${PROJECT_ROOT}/target"
INITRAMFS="${BUILD_DIR}/initramfs"
BUSYBOX_DIR="${BUILD_DIR}/busybox"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ============================================================================
# VERIFY PREREQUISITES
# ============================================================================

REQUIRED_CMDS="git curl"

for cmd in $REQUIRED_CMDS; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    log_error "Missing dependency: $cmd"
    exit 1
  fi
done

log_success "Prerequisites verified"

# ============================================================================
# SETUP INITRAMFS
# ============================================================================

# log_info "Setting up rootfs directory structure..."
# mkdir -p "$INITRAMFS"/{bin,sbin,etc,dev,proc,sys,tmp,newroot}
# sudo cp --archive /dev/{null,console,tty,sda1,sda} "$INITRAMFS"/dev/
# sudo cp --archive "$INITRAMFS"/dev/sda "$INITRAMFS"/dev/sda2
# # mkdir -p "$ROOTFS"/home/root
# # chmod 1777 "$ROOTFS/tmp"
# log_success "Rootfs directories created"

# ============================================================================
# DOWNLOAD STATIC BUSYBOX
# ============================================================================

log_info "Downloading BusyBox source..."
cd "$BUILD_DIR"
if [ ! -d "$BUSYBOX_DIR" ]; then
    log_info "Busybox source not already downloaded..."
    git clone https://github.com/mirror/busybox.git --depth=1
fi
cd "$BUSYBOX_DIR"
gcc_version=$(gcc --version | head -n1 | awk '{print $NF}' | cut -d. -f1)
gcc_version=$((gcc_version + 0))
if [ "$gcc_version" -ge 14 ]; then
    # Fix check-lxdialog.sh for GCC 14
    echo "GCC 14 or higher detected. Patching check-lxdialog.sh..."
    sed -i '/main()/ { /int main()/! s/main()/int main()/; }' "$BUSYBOX_DIR/scripts/kconfig/lxdialog/check-lxdialog.sh"
fi
make menuconfig
make
make install CONFIG_PREFIX="$INITRAMFS"
rm "$INITRAMFS"/linuxrc
# chmod 755 "$INITRAMFS/bin/busybox"
# sudo chown root:root "$INITRAMFS/bin/busybox"




# BUSYBOX_VERSION="1.35.0"
# BUSYBOX_URL="https://busybox.net/downloads/binaries/${BUSYBOX_VERSION}-x86_64-linux-musl/busybox"
# BUSYBOX_PATH="${BUILD_DIR}/busybox"

# if [ ! -f "$BUSYBOX_PATH" ]; then
#     if command -v wget >/dev/null 2>&1; then
#         if ! wget -O "$BUSYBOX_PATH" "$BUSYBOX_URL" 2>&1; then
#             log_error "Failed to download BusyBox with wget"
#             exit 1
#         fi
#     elif command -v curl >/dev/null 2>&1; then
#         if ! curl -L -o "$BUSYBOX_PATH" "$BUSYBOX_URL" 2>&1; then
#             log_error "Failed to download BusyBox with curl"
#             exit 1
#         fi
#     fi
#     log_success "BusyBox downloaded: $BUSYBOX_PATH"
# else
#     log_info "BusyBox already downloaded (skipping)"
# fi

# # Make it executable
# chmod +x "$BUSYBOX_PATH"

# ============================================================================
# VERIFY BUSYBOX IS STATIC
# ============================================================================

log_info "Verifying BusyBox is statically linked..."

BUSYBOX_PATH="$INITRAMFS/bin/busybox"
BUSYBOX_SYM="$INITRAMFS/sbin/init"

if ! file "$BUSYBOX_PATH" | grep -o "statically linked"; then
    log_error "BusyBox is NOT statically linked!"
    log_error "File info: $(file "$BUSYBOX_PATH")"
    log_error "Static BusyBox is required for initramfs/initrd"
    exit 1
fi

if [ ! -f "$BUSYBOX_SYM" ]; then
    log_error "BusyBox is NOT symlinked!"
    log_error "File info: $(file "$BUSYBOX_SYM")"
    log_error "Check the install and make sure symlinks get created."
    exit 1
fi

BUSYBOX_SIZE=$(du -h "$BUSYBOX_PATH" | cut -f1)
log_success "BusyBox verified (static, $BUSYBOX_SIZE)"

# ============================================================================
# INSTALL BUSYBOX TO ROOTFS
# ============================================================================

# log_info "Installing BusyBox to rootfs..."
# cp "$BUSYBOX_PATH" "$INITRAMFS/bin/busybox"
# chmod 755 "$INITRAMFS/bin/busybox"
# log_success "BusyBox installed to $INITRAMFS/bin/busybox"

# ============================================================================
# CREATE BUSYBOX SYMLINKS (ALIGNED WITH MINILINUX2)
# ============================================================================

# log_info "Creating BusyBox symlinks..."

# cd "$INITRAMFS/bin"

# # Get list of applets from busybox
# APPLETS=$("./busybox" --list 2>/dev/null || echo "")

# if [ -z "$APPLETS" ]; then
#     log_error "Failed to get BusyBox applet list"
#     exit 1
# fi

# # Create symlinks using parallel processing like minilinux2
# if echo "$APPLETS" | xargs -n1 -P8 ln -sf busybox >/dev/null 2>&1; then
#     log_success "Symlinks created with parallel processing"
# else
#     # Fallback: create symlinks sequentially
#     log_info "Parallel symlink creation failed, using sequential approach..."
#     echo "$APPLETS" | while read cmd; do
#         rm -f "$cmd"
#         ln -sf busybox "$cmd"
#     done
#     log_success "Symlinks created (sequential)"
# fi

# cd - >/dev/null

# log_success "BusyBox symlinks created"

# ============================================================================
# CREATE ESSENTIAL CONFIG FILES (MINIMAL)
# ============================================================================

# log_info "Creating essential configuration files..."

# log_info "Creating initramfs init script..."

# cat > "$BUILD_DIR/init" <<'INITSCRIPT'
# #!/bin/busybox sh

# # Mount virtual filesystems
# mount -t devtmpfs devtmpfs /dev 2>/dev/null || mount -t tmpfs tmpfs /dev
# mount -t proc proc /proc
# mount -t sysfs sysfs /sys
# mount -t tmpfs tmpfs /run

# # Create device pts
# mkdir -p /dev/pts /dev/shm
# mount -t devpts devpts /dev/pts 2>/dev/null || true

# echo "All mounted now and ready for root!" > /dev/tty0

# # Load kernel modules (example for ext4 and SATA)
# modprobe ext4
# modprobe sata_ahci

# # Wait for block devices to be detected
# sleep 5
# mount -o ro /dev/sda2 /newroot

# cd /newroot
# exec switch_root -c /dev/tty0 . /bin/init
# INITSCRIPT

# chmod 755 "$BUILD_DIR/init"

# # log_info "Removing old init"
# # rm -f "$BUILD_DIR/rootfs/bin/init"

# log_info "Copying $BUILD_DIR/init to $BUILD_DIR/rootfs/init"
# cp "$BUILD_DIR/init" "$INITRAMFS/init"

# log_success "Init script created"

# log_success "Configuration files created"

# ============================================================================
# Create device nodes for init
# ============================================================================

# log_info "Creating device nodes for initramfs"

# # Create device nodes
# mknod -m 0600 $ROOTFS/dev/console c 5 1
# mknod -m 0600 $ROOTFS/dev/null c 1 3
# mknod -m 0600 $ROOTFS/dev/zero c 1 5

# ============================================================================
# Create cpio.init
# ============================================================================

# cd "$ROOTFS"

# OUTPUT_CPIO="$PROJECT_ROOT/target/boot/init.cpio"

# log_info "Creating the init.cpio"

# if [ ! -f "${OUTPUT_CPIO}" ]; then
#     if find . | cpio -o -H newc --owner=+0:+0 > "$OUTPUT_CPIO" 2>/dev/null; then
#         CPIO_SIZE=$(du -h "$OUTPUT_CPIO" | cut -f1)
#         log_success "CPIO archive created: $OUTPUT_CPIO ($CPIO_SIZE)"
#     else
#         log_error "Failed to create CPIO archive"
#         cd - >/dev/null
#         exit 1
#     fi
# fi

cd - >/dev/null

# ============================================================================
# SUMMARY
# ============================================================================

echo ""
echo "════════════════════════════════════════════════════════════════════════════════"
echo "BusyBox Installation Complete!"
echo "════════════════════════════════════════════════════════════════════════════════"
echo ""
echo "BusyBox: $INITRAMFS/bin/busybox ($BUSYBOX_SIZE)"
echo ""