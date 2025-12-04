#!/bin/bash
# ============================================================================
# GeckoOS Kernel Build Script
# Downloads, configures, and compiles Linux kernel for GeckoOS
# ============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/kernel"
KERNEL_VERSION="6.1"
KERNEL_URL="https://www.kernel.org/releases/download.php?v=linux-${KERNEL_VERSION}.tar.xz"

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ============================================================================
# VERIFY DEPENDENCIES
# ============================================================================

log_info "Verifying build dependencies..."

for cmd in gcc make bc bison flex openssl libelf-dev libdw-dev binutils-dev; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        log_error "Missing dependency: $cmd"
        echo "Install with: sudo apt install build-essential bison flex libelf-dev libssl-dev"
        exit 1
    fi
done

log_success "All dependencies found"

# ============================================================================
# SETUP BUILD DIRECTORY
# ============================================================================

log_info "Setting up build directory..."
mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

log_success "Build directory: $BUILD_DIR"

# ============================================================================
# DOWNLOAD KERNEL
# ============================================================================

KERNEL_ARCHIVE="linux-${KERNEL_VERSION}.tar.xz"

if [ ! -f "$KERNEL_ARCHIVE" ]; then
    log_info "Downloading Linux kernel ${KERNEL_VERSION}..."
    # Alternative: Use direct link instead of download.php
    KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v6.x/${KERNEL_ARCHIVE}"
    
    if ! wget "$KERNEL_URL" -O "$KERNEL_ARCHIVE"; then
        log_error "Failed to download kernel"
        exit 1
    fi
    log_success "Kernel downloaded"
else
    log_info "Kernel archive already exists"
fi

# ============================================================================
# EXTRACT KERNEL
# ============================================================================

if [ ! -d "linux-${KERNEL_VERSION}" ]; then
    log_info "Extracting kernel..."
    tar -xf "$KERNEL_ARCHIVE"
    log_success "Kernel extracted"
else
    log_info "Kernel already extracted"
fi

cd "linux-${KERNEL_VERSION}"

# ============================================================================
# COPY KERNEL CONFIGURATION
# ============================================================================

log_info "Copying kernel configuration..."
cp "${SCRIPT_DIR}/config" ".config"
log_success "Configuration copied"

# ============================================================================
# CONFIGURE KERNEL
# ============================================================================

log_info "Configuring kernel..."

# Make oldconfig to fill in any missing options
make oldconfig < /dev/null || {
    log_error "Kernel configuration failed"
    exit 1
}

log_success "Kernel configured"

# ============================================================================
# BUILD KERNEL
# ============================================================================

log_info "Building kernel (this may take 10-30 minutes)..."

# Use all available CPU cores
NUM_JOBS=$(nproc || echo 4)
log_info "Using $NUM_JOBS build jobs..."

if ! make -j"$NUM_JOBS"; then
    log_error "Kernel build failed"
    exit 1
fi

log_success "Kernel build complete"

# ============================================================================
# INSTALL KERNEL
# ============================================================================

log_info "Installing kernel modules..."

# Create modules install directory
INSTALL_MOD_PATH="${BUILD_DIR}/root" make modules_install

log_success "Modules installed"

# ============================================================================
# COPY KERNEL IMAGE
# ============================================================================

log_info "Copying kernel image..."
mkdir -p "${PROJECT_ROOT}/target/boot"

# Copy kernel image
KERNEL_IMAGE=""
if [ -f "arch/x86/boot/bzImage" ]; then
    KERNEL_IMAGE="arch/x86/boot/bzImage"
elif [ -f "arch/x86_64/boot/bzImage" ]; then
    KERNEL_IMAGE="arch/x86_64/boot/bzImage"
else
    log_error "Kernel image not found"
    exit 1
fi

cp "$KERNEL_IMAGE" "${PROJECT_ROOT}/target/boot/bzImage-geckoOS"
log_success "Kernel image copied: ${PROJECT_ROOT}/target/boot/bzImage-geckoOS"

# ============================================================================
# COPY SYSTEM.MAP
# ============================================================================

if [ -f "System.map" ]; then
    cp "System.map" "${PROJECT_ROOT}/target/boot/System.map-geckoOS"
    log_success "System.map copied"
fi

# ============================================================================
# SUMMARY
# ============================================================================

echo ""
echo "========================================="
echo "Kernel Build Complete!"
echo "========================================="
echo "Kernel image: ${PROJECT_ROOT}/target/boot/bzImage-geckoOS"
echo "Modules path: ${BUILD_DIR}/root/lib/modules/"
echo ""
echo "Next steps:"
echo "  1. Build initramfs: cd boot/initramfs && ./build-initramfs.sh"
echo "  2. Install to /boot: sudo make install"
echo "  3. Update bootloader: sudo grub-mkconfig"
echo ""
