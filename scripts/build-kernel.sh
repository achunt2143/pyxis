#!/bin/bash

# ============================================================================
# GeckoOS Kernel Build Script (FIXED)
# Downloads, configures, and compiles Linux kernel for GeckoOS
# Produces: bzImage, System.map, and kernel modules
# ============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/kernel"
KERNEL_VERSION="6.1"
KERNEL_MAJOR="6"

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_section() { echo ""; echo "════════════════════════════════════════════"; echo " $*"; echo "════════════════════════════════════════════"; }

# ============================================================================
# VERIFY DEPENDENCIES
# ============================================================================

log_section "Verifying Build Dependencies"

REQUIRED_CMDS="gcc make bc bison flex openssl grep sed find"

for cmd in $REQUIRED_CMDS; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    log_error "Missing dependency: $cmd"
    exit 1
  fi
done

REQUIRED_LIBS="libelf-dev libssl-dev"

if ! dpkg -l | grep -o "libelf-dev"; then
  log_error "Missing development library: libelf-dev"
  log_info "Install with: sudo apt install build-essential libelf-dev libssl-dev"
  exit 1
fi

log_success "All dependencies verified"

# ============================================================================
# FIND KERNEL CONFIG FILE (FIX FOR PATH ISSUE)
# ============================================================================

log_section "Locating Kernel Configuration File"

CONFIG_FILE=""

# Try different possible locations
if [ -f "${SCRIPT_DIR}/.config" ]; then
    CONFIG_FILE=".config"
    log_success "Found config at: .config (current directory)"
else
    log_error "Kernel configuration file not found!"
    echo "Searched locations:"
    # echo "  - ${PROJECT_ROOT}/boot/config"
    # echo "  - ${PROJECT_ROOT}/boot/kernel/config"
    # echo "  - ${PROJECT_ROOT}/kernel/.config"
    echo "  - .config"
    echo ""
    echo "Please place your kernel config file at one of these locations"
    exit 1
fi

# ============================================================================
# SETUP BUILD DIRECTORY
# ============================================================================

log_section "Setting Up Build Environment"

mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

log_success "Build directory: $BUILD_DIR"

# ============================================================================
# DOWNLOAD KERNEL
# ============================================================================

log_section "Downloading Linux Kernel"

KERNEL_ARCHIVE="linux-${KERNEL_VERSION}.tar.xz"
KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/${KERNEL_ARCHIVE}"

if [ ! -f "$KERNEL_ARCHIVE" ]; then
  log_info "Downloading Linux kernel ${KERNEL_VERSION}..."
  log_info "URL: $KERNEL_URL"
  
  if ! wget -q --show-progress "$KERNEL_URL"; then
    log_error "Failed to download kernel from $KERNEL_URL"
    exit 1
  fi
  
  log_success "Kernel downloaded"
else
  log_info "Kernel archive already exists (skipping download)"
fi

# Verify archive integrity
if [ ! -f "$KERNEL_ARCHIVE" ]; then
  log_error "Kernel archive not found or download failed"
  exit 1
fi

# ============================================================================
# EXTRACT KERNEL
# ============================================================================

log_section "Extracting Kernel Source"

if [ ! -d "linux-${KERNEL_VERSION}" ]; then
  log_info "Extracting kernel source..."
  
  if ! tar -xf "$KERNEL_ARCHIVE"; then
    log_error "Failed to extract kernel"
    exit 1
  fi
  
  log_success "Kernel extracted"
else
  log_info "Kernel already extracted (skipping)"
fi

cd "linux-${KERNEL_VERSION}"

log_success "Working directory: $(pwd)"

# ============================================================================
# LOAD KERNEL CONFIGURATION (FIXED PATH)
# ============================================================================

# log_section "Configuring Kernel"

# log_info "Using kernel config from: $CONFIG_FILE"
# log_info "Copying to .config..."

# cp "$CONFIG_FILE" ".config"

# log_success "Kernel configuration loaded"

# Show the config being used
# log_info "CONFIG_SATA_AHCI setting:"
# if grep -q "^CONFIG_SATA_AHCI=" ".config"; then
#     AHCI_SETTING=$(grep "^CONFIG_SATA_AHCI=" ".config")
#     echo "  $AHCI_SETTING"
#     if [[ "$AHCI_SETTING" == "CONFIG_SATA_AHCI=y" ]]; then
#         log_success "✓ SATA_AHCI is built-in (=y) - CORRECT!"
#     elif [[ "$AHCI_SETTING" == "CONFIG_SATA_AHCI=m" ]]; then
#         log_error "⚠ SATA_AHCI is modular (=m) - THIS WILL CAUSE BOOT FAILURE!"
#         echo ""
#         echo "FIX: Your kernel config has CONFIG_SATA_AHCI=m"
#         echo "Change it to: CONFIG_SATA_AHCI=y"
#         echo ""
#         echo "Then rebuild and try again."
#         exit 1
#     fi
# else
#     log_error "CONFIG_SATA_AHCI not found in config!"
#     exit 1
# fi

# Validate/update config
log_info "Validating kernel configuration..."

if ! make oldconfig < /dev/null 2>/dev/null; then
  log_error "Kernel configuration validation failed"
  exit 1
fi

log_success "Kernel configuration validated"

# ============================================================================
# BUILD KERNEL
# ============================================================================

log_section "Building Linux Kernel"

log_info "This may take 10-30 minutes depending on system speed..."

make menuconfig

NUM_JOBS=$(nproc 2>/dev/null || echo 4)

log_info "Building with $NUM_JOBS parallel jobs..."

if ! make -j"$NUM_JOBS" bzImage; then
  log_error "Kernel build failed"
  log_info "Check kernel configuration: make menuconfig"
  exit 1
fi

log_success "Kernel build complete"

# ============================================================================
# VERIFY KERNEL IMAGE
# ============================================================================

log_section "Verifying Kernel Image"

KERNEL_IMAGE=""

if [ -f "arch/x86/boot/bzImage" ]; then
  KERNEL_IMAGE="arch/x86/boot/bzImage"
elif [ -f "arch/x86_64/boot/bzImage" ]; then
  KERNEL_IMAGE="arch/x86_64/boot/bzImage"
else
  log_error "Kernel image (bzImage) not found"
  log_info "Check kernel build output above"
  exit 1
fi

KERNEL_SIZE=$(du -h "$KERNEL_IMAGE" | cut -f1)

log_success "Kernel image created: $KERNEL_IMAGE ($KERNEL_SIZE)"

# log_info "signing the kernel..."

# if [ ! -f "PK.prv" ]; then
#   openssl req -new -x509 -newkey rsa:2048 -subj "/CN=GeckoOS Kernel PK" -keyout PK.key -out PK.crt -days 9999 -sha256
#   openssl req -new -x509 -newkey rsa:2048 -subj "/CN=GeckoOS Kernel KEK" -keyout KEK.key -out KEK.crt -days 9999 -sha256
#   openssl req -new -x509 -newkey rsa:2048 -subj "/CN=GeckoOS Kernel db" -keyout db.key -out db.crt -days 9999 -sha256
# fi
# # openssl x509 -req -in MOK.csr -signkey MOK.priv -out MOK.der -outform DER -days 36500 -sha256

# scripts/sign-file sha256 db.key db.crt arch/x86/boot/bzImage

# ============================================================================
# BUILD KERNEL MODULES
# ============================================================================

# log_section "Building Kernel Modules"

# log_info "Building kernel modules..."

# if ! make -j"$NUM_JOBS" modules; then
#   log_error "Kernel module build failed"
#   exit 1
# fi

# log_success "Kernel modules built"

# ============================================================================
# INSTALL KERNEL IMAGE AND MODULES
# ============================================================================

log_section "Installing Kernel Image and Modules"

mkdir -p "${PROJECT_ROOT}/target/boot"
mkdir -p "${PROJECT_ROOT}/target/kernel-modules"

log_info "Copying kernel image to target/boot/..."

cp "$KERNEL_IMAGE" "${PROJECT_ROOT}/target/boot/vmlinuz-geckoOS"

log_success "Kernel image: ${PROJECT_ROOT}/target/boot/vmlinuz-geckoOS"

# Copy System.map for kernel debugging
if [ -f "System.map" ]; then
  cp "System.map" "${PROJECT_ROOT}/target/boot/System.map-geckoOS"
  log_success "System.map copied"
fi

# Install modules to target directory
# log_info "Installing kernel modules..."

# if ! make INSTALL_MOD_PATH="${PROJECT_ROOT}/target/kernel-modules" modules_install; then
#   log_error "Module installation failed"
#   exit 1
# fi

# # Generate module dependency information
# log_info "Generating module dependencies..."

KERNEL_RELEASE=$(make kernelrelease)

# depmod -a -b "${PROJECT_ROOT}/target/kernel-modules" "$KERNEL_RELEASE" || log_error "depmod failed (non-critical)"

# log_success "Kernel modules installed to: ${PROJECT_ROOT}/target/kernel-modules/"
log_info "Kernel release version: $KERNEL_RELEASE"

# ============================================================================
# SUMMARY
# ============================================================================

log_section "Kernel Build Complete!"

echo ""
echo "Output Files:"
echo " Kernel Image: ${PROJECT_ROOT}/target/boot/vmlinuz-geckoOS"
echo " System Map: ${PROJECT_ROOT}/target/boot/System.map-geckoOS"
echo " Modules: ${PROJECT_ROOT}/target/kernel-modules/lib/modules/$KERNEL_RELEASE/"
echo " Kernel Release: $KERNEL_RELEASE"
echo ""