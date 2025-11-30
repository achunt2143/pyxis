#!/bin/bash
# ============================================================================
# GeckoOS Initramfs Build Script
# Creates compressed initial RAM filesystem from BusyBox and kernel modules
# ============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INITRAMFS_DIR="${SCRIPT_DIR}/build"
OUTPUT_FILE="${PROJECT_ROOT}/target/boot/initramfs-geckoOS.img"

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }

# ============================================================================
# CLEANUP AND SETUP
# ============================================================================

log_info "Preparing initramfs build environment..."

# Clean previous build
rm -rf "$INITRAMFS_DIR"
mkdir -p "$INITRAMFS_DIR"
mkdir -p "$(dirname "$OUTPUT_FILE")"

# ============================================================================
# COPY INIT SCRIPT
# ============================================================================

log_info "Copying init script..."
cp "${SCRIPT_DIR}/init" "$INITRAMFS_DIR/init"
chmod +x "$INITRAMFS_DIR/init"
log_success "Init script copied"

# ============================================================================
# COPY BUSYBOX BINARY
# ============================================================================

log_info "Finding BusyBox..."
BUSYBOX_PATH=$(which busybox 2>/dev/null || echo "")

if [ -z "$BUSYBOX_PATH" ]; then
    log_error "BusyBox not found. Install with: apt install busybox-static"
    exit 1
fi

log_success "BusyBox found at: $BUSYBOX_PATH"

# Copy busybox
mkdir -p "$INITRAMFS_DIR/bin"
cp "$BUSYBOX_PATH" "$INITRAMFS_DIR/bin/busybox"
chmod +x "$INITRAMFS_DIR/bin/busybox"

# Create busybox symlinks
log_info "Creating BusyBox symlinks..."
for applet in $(busybox --list); do
    ln -sf /bin/busybox "$INITRAMFS_DIR/bin/$applet" 2>/dev/null || true
done
log_success "BusyBox symlinks created"

# ============================================================================
# COPY ESSENTIAL LIBRARIES
# ============================================================================

log_info "Copying essential libraries..."
mkdir -p "$INITRAMFS_DIR/lib64"
mkdir -p "$INITRAMFS_DIR/lib"

# Find and copy libc
for lib in libc.so.6 libm.so.6 libdl.so.2 libnsl.so.1; do
    if [ -f "/lib/x86_64-linux-gnu/$lib" ]; then
        cp "/lib/x86_64-linux-gnu/$lib" "$INITRAMFS_DIR/lib64/" 2>/dev/null || true
    elif [ -f "/lib64/$lib" ]; then
        cp "/lib64/$lib" "$INITRAMFS_DIR/lib64/" 2>/dev/null || true
    fi
done

# Copy ld-linux
if [ -f "/lib64/ld-linux-x86-64.so.2" ]; then
    cp "/lib64/ld-linux-x86-64.so.2" "$INITRAMFS_DIR/lib64/"
fi

log_success "Libraries copied"

# ============================================================================
# KERNEL MODULES
# ============================================================================

log_info "Copying kernel modules..."
KERNEL_VERSION=$(uname -r)
MODULES_DIR="/lib/modules/$KERNEL_VERSION"

if [ -d "$MODULES_DIR" ]; then
    mkdir -p "$INITRAMFS_DIR/lib/modules/$KERNEL_VERSION"
    
    # Essential modules
    ESSENTIAL_MODULES="kernel/drivers/gpu/drm kernel/drivers/input kernel/fs/ext4 kernel/net"
    
    for module_path in $ESSENTIAL_MODULES; do
        if [ -d "$MODULES_DIR/$module_path" ]; then
            cp -r "$MODULES_DIR/$module_path" "$INITRAMFS_DIR/lib/modules/$KERNEL_VERSION/" 2>/dev/null || true
        fi
    done
    
    # Copy module dependency files if they exist
    [ -f "$MODULES_DIR/modules.dep" ] && cp "$MODULES_DIR/modules.dep" "$INITRAMFS_DIR/lib/modules/$KERNEL_VERSION/"
    [ -f "$MODULES_DIR/modules.order" ] && cp "$MODULES_DIR/modules.order" "$INITRAMFS_DIR/lib/modules/$KERNEL_VERSION/"
    [ -f "$MODULES_DIR/modules.builtin" ] && cp "$MODULES_DIR/modules.builtin" "$INITRAMFS_DIR/lib/modules/$KERNEL_VERSION/"
    
    log_success "Kernel modules copied"
else
    log_warn "No kernel modules directory found at $MODULES_DIR"
fi

# ============================================================================
# CREATE DEVICE NODES
# ============================================================================

log_info "Creating device nodes..."
mkdir -p "$INITRAMFS_DIR/dev"
cd "$INITRAMFS_DIR/dev"

# Create major device nodes
mknod -m 666 null c 1 3 2>/dev/null || true
mknod -m 666 zero c 1 5 2>/dev/null || true
mknod -m 666 full c 1 7 2>/dev/null || true
mknod -m 666 random c 1 8 2>/dev/null || true
mknod -m 666 urandom c 1 9 2>/dev/null || true
mknod -m 644 kmem c 1 2 2>/dev/null || true
mknod -m 640 mem c 1 1 2>/dev/null || true
mknod -m 666 port c 1 4 2>/dev/null || true
mknod -m 666 zero c 1 5 2>/dev/null || true
mknod -m 622 console c 5 1 2>/dev/null || true
mknod -m 666 tty c 5 0 2>/dev/null || true
mknod -m 644 rtc c 10 135 2>/dev/null || true

cd "$SCRIPT_DIR"
log_success "Device nodes created"

# ============================================================================
# CREATE FILESYSTEM DIRECTORIES
# ============================================================================

log_info "Creating filesystem structure..."
mkdir -p "$INITRAMFS_DIR"/{proc,sys,dev,run,root,mnt,oldroot,newroot}
mkdir -p "$INITRAMFS_DIR/etc"
mkdir -p "$INITRAMFS_DIR/sbin"
mkdir -p "$INITRAMFS_DIR/var"/{log,lock}

# Create basic config files
echo "#!/bin/sh" > "$INITRAMFS_DIR/etc/init.d/rcS"
chmod +x "$INITRAMFS_DIR/etc/init.d/rcS"

log_success "Filesystem structure created"

# ============================================================================
# CREATE INITRAMFS ARCHIVE
# ============================================================================

log_info "Creating initramfs archive..."
cd "$INITRAMFS_DIR"

# Create CPIO archive
find . | cpio -o -H newc 2>/dev/null | gzip -9 > "$OUTPUT_FILE"

# Verify archive was created
if [ -f "$OUTPUT_FILE" ]; then
    SIZE=$(du -h "$OUTPUT_FILE" | cut -f1)
    log_success "Initramfs created: $OUTPUT_FILE ($SIZE)"
else
    log_error "Failed to create initramfs archive"
    exit 1
fi

cd "$SCRIPT_DIR"

# ============================================================================
# CLEANUP
# ============================================================================

log_info "Cleaning up build directory..."
rm -rf "$INITRAMFS_DIR"
log_success "Build directory cleaned"

# ============================================================================
# SUMMARY
# ============================================================================

echo ""
echo "========================================="
echo "Initramfs Build Complete!"
echo "========================================="
echo "Output: $OUTPUT_FILE"
echo "Size: $(du -h "$OUTPUT_FILE" | cut -f1)"
echo ""
echo "To boot with this initramfs:"
echo "  1. Copy to /boot/: sudo cp $OUTPUT_FILE /boot/"
echo "  2. Update GRUB: sudo grub-mkconfig -o /boot/grub/grub.cfg"
echo "  3. Reboot: sudo reboot"
echo ""
