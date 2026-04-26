#!/bin/bash

# ============================================================================
# GeckoOS UEFI/EFI Disk Image Packaging Script (FIXED)
# Creates a bootable Linux disk image with GPT + GRUB UEFI bootloader
# FIXED: Uses minimal, commonly available GRUB modules
# ============================================================================

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
TEMP_DIR="${BUILD_DIR}/temp"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
IMAGE_DIR="${BUILD_DIR}/images"
IMAGE_FILE="${IMAGE_DIR}/geckoOS.img"
IMAGE_SIZE="${IMAGE_SIZE:-2048}" # MB
platform="$1"

BOOT_FILES_DIR="${PROJECT_ROOT}/target/boot"
KERNEL_FILE="${BOOT_FILES_DIR}/vmlinuz-geckoOS"
INITRAMFS_FILE="${BOOT_FILES_DIR}/init.cpio.gz"
SYSTEM_MAP="${BOOT_FILES_DIR}/System.map-geckoOS"
KERNEL_MODULES="${PROJECT_ROOT}/target/kernel-modules"
GRUB_BOOTSTRAP="${PROJECT_ROOT}/boot/grub/grub-bootstrap.cfg"
GRUB_MODULES_DIR="/usr/lib/grub/x86_64-efi"
BOOTLOADER="${TEMP_DIR}/bootx64.efi"
GRUB_CONFIG="${PROJECT_ROOT}/boot/grub/grub.cfg"

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
log_section() { echo ""; echo "════════════════════════════════════════════"; echo " $*"; echo "════════════════════════════════════════════"; }

# Cleanup function for exit traps
cleanup_on_exit() {
    local exit_code=$?
    
    if [ $exit_code -ne 0 ]; then
        log_error "Script failed with exit code $exit_code"
        log_info "Cleaning up mounted partitions..."
        
        # Unmount partitions
        for mount_point in "$ESP_MOUNT" "$ROOT_MOUNT"; do
            if [ -d "$mount_point" ] && mountpoint -q "$mount_point" 2>/dev/null; then
                log_info "Unmounting $mount_point..."
                sudo umount "$mount_point" 2>/dev/null || true
            fi
        done
        
        # Detach loop device
        if [ -b "$LOOP_DEV" ] 2>/dev/null; then
            log_info "Detaching loop device $LOOP_DEV..."
            sudo losetup -d "$LOOP_DEV" 2>/dev/null || true
        fi
        
        # Remove mount directories
        rmdir "$ESP_MOUNT" "$ROOT_MOUNT" 2>/dev/null || true
    fi
    
    return $exit_code
}

trap cleanup_on_exit EXIT

# Initialize variables
LOOP_DEV=""
ESP_MOUNT="${BUILD_DIR}/mnt-esp"      # EFI System Partition
ROOT_MOUNT="${BUILD_DIR}/mnt-root"

# ============================================================================
# VERIFY PREREQUISITES
# ============================================================================

log_section "Verifying Prerequisites"

# Check for required tools
for tool in dd mkfs.ext4 mkfs.vfat parted losetup partprobe sgdisk; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        log_error "Missing required tool: $tool"
        if [ "$tool" = "sgdisk" ]; then
            log_info "Install with: sudo apt install gptfdisk"
        else
            log_info "Install with: sudo apt install parted e2fsprogs dosfstools"
        fi
        exit 1
    fi
done

# Check if grub-efi-amd64-bin is available
if ! command -v grub-mkimage >/dev/null 2>&1; then
    log_error "grub-mkimage not found"
    log_info "Install with: sudo apt install grub-efi-amd64-bin"
    exit 1
fi

# Check GRUB modules directory
if [ ! -d "/usr/lib/grub/x86_64-efi" ]; then
    log_error "GRUB EFI modules directory not found: /usr/lib/grub/x86_64-efi"
    log_info "Install with: sudo apt install grub-efi-amd64-bin"
    exit 1
fi

# Check available modules (will try different combinations)
log_info "Checking available GRUB modules..."
AVAILABLE_MODULES=$(ls /usr/lib/grub/x86_64-efi/*.mod 2>/dev/null | sed 's|.*/||;s|\.mod||' | tr '\n' ' ')
log_info "Found modules: $(echo $AVAILABLE_MODULES | wc -w) modules available"

# Verify rootfs exists
if [ ! -d "$ROOTFS_DIR" ]; then
    log_error "Rootfs directory not found: $ROOTFS_DIR"
    log_info "Run ./build-rootfs.sh first"
    exit 1
fi
log_success "Rootfs found: $ROOTFS_DIR"

# Verify kernel exists
for file in "$KERNEL_FILE" "$INITRAMFS_FILE"; do
    if [ ! -f "$file" ]; then
        log_error "Required file not found: $file"
        log_info "Run ./scripts/build-kernel.sh and ./scripts/create-rootfs.sh first"
        exit 1
    fi
done
# chmod 644 "$KERNEL_FILE"
# sudo chown root:root "$KERNEL_FILE"
log_success "Kernel: $KERNEL_FILE"

# Warn if initramfs is missing (non-fatal)
if [ ! -f "$INITRAMFS_FILE" ]; then
    log_warn "Initramfs not found: $INITRAMFS_FILE"
    log_info "Run ./build-initramfs.sh first for better boot support"
else
    log_success "Initramfs found: $INITRAMFS_FILE"
fi

# if [ -e "${ROOTFS_DIR}/etc/inittab" ]; then
#     grep -qE '^tty1::' "${ROOTFS_DIR}/etc/inittab" || \
# 	sed -i '/GENERIC_SERIAL/a\
# tty1::respawn:/sbin/getty -L  tty1 0 vt100 # QEMU graphical window' "${ROOTFS_DIR}/etc/inittab"
# fi

# ============================================================================
# CREATE DISK IMAGE FILE
# ============================================================================

log_section "Creating Disk Image File"

mkdir -p "$IMAGE_DIR"
mkdir -p "$TEMP_DIR"

# Check if image already exists
if [ -f "$IMAGE_FILE" ]; then
    log_warn "Image already exists: $IMAGE_FILE"
    read -p "Overwrite? (y/n) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        log_info "Skipping disk image creation"
    else
        rm "$IMAGE_FILE"
        log_info "Creating new image (${IMAGE_SIZE}MB)..."
        dd if=/dev/zero of="$IMAGE_FILE" bs=1M count="$IMAGE_SIZE" status=progress 2>&1
        log_success "Disk image created"
    fi
else
    log_info "Creating ${IMAGE_SIZE}MB disk image..."
    dd if=/dev/zero of="$IMAGE_FILE" bs=1M count="$IMAGE_SIZE" status=progress 2>&1
    log_success "Disk image created: $IMAGE_FILE"
fi

# ============================================================================
# CREATE GPT PARTITION TABLE
# ============================================================================

log_section "Creating GPT Partition Table"

log_info "Creating UEFI/GPT partition layout..."

# Use sgdisk to create GPT partitions
# Partition layout:
#  - Part 1: EFI System Partition (ESP) - 200MB - FAT32 - contains GRUB and kernel
#  - Part 2: Root (ext4) - remainder - contains rootfs

if ! sudo sgdisk --clear \
    --new=1::+200M --typecode=1:ef00 --change-name=1:'EFI System' \
    --new=2::0 --typecode=2:8300 --change-name=2:'Linux Root' \
    "$IMAGE_FILE" 2>&1; then
    log_error "Failed to create GPT partition table"
    exit 1
fi

log_success "GPT partition table created"
sudo sgdisk --print "$IMAGE_FILE"

# ============================================================================
# SETUP LOOP DEVICE
# ============================================================================

log_section "Setting Up Loop Device"

log_info "Finding available loop device..."
LOOP_DEV=$(sudo losetup -f)
log_info "Using loop device: $LOOP_DEV"

log_info "Attaching disk image to loop device..."
if ! sudo losetup "$LOOP_DEV" "$IMAGE_FILE"; then
    log_error "Failed to attach loop device"
    exit 1
fi
log_success "Loop device attached"

# CRITICAL: Rescan partition table on loop device
log_info "Rescanning partition table..."
if ! sudo partprobe "$LOOP_DEV"; then
    log_warn "partprobe failed (may not be critical)"
fi

# Wait for partitions to appear
log_info "Waiting for partitions to appear..."
for i in {1..10}; do
    if [ -b "${LOOP_DEV}p1" ] && [ -b "${LOOP_DEV}p2" ]; then
        log_success "Partitions detected"
        break
    fi
    log_info "  Attempt $i/10..."
    sleep 1
    
    # Try to trigger partition detection
    sudo partx -u "$LOOP_DEV" 2>/dev/null || true
done

# Verify partitions exist
if [ ! -b "${LOOP_DEV}p1" ] || [ ! -b "${LOOP_DEV}p2" ]; then
    log_error "Partitions did not appear on $LOOP_DEV"
    exit 1
fi

log_success "Partitions ready: ${LOOP_DEV}p1 (ESP) and ${LOOP_DEV}p2 (Root)"
ls -la "${LOOP_DEV}p"*

# ============================================================================
# FORMAT PARTITIONS
# ============================================================================

log_section "Formatting Partitions"

# Format ESP as FAT32 (required for UEFI)
log_info "Formatting EFI System Partition as FAT32 (${LOOP_DEV}p1)..."
if ! sudo mkfs.vfat -F 32 -n GECKOOS-ESP "${LOOP_DEV}p1"; then
    log_error "Failed to format EFI partition"
    exit 1
fi
log_success "EFI System Partition formatted"

# Format root partition as ext4
log_info "Formatting root partition as ext4 (${LOOP_DEV}p2)..."
if ! sudo mkfs.ext4 -F -L geckoOS-root "${LOOP_DEV}p2"; then
    log_error "Failed to format root partition"
    exit 1
fi
log_success "Root partition formatted"

# Verify filesystems
log_info "Verifying filesystems..."
sudo blkid "${LOOP_DEV}p1" "${LOOP_DEV}p2"

# # Extract UUIDs from blkid output
# Capture UUIDs and update grub.cfg
# Get the new UUID of the root partition
# ROOT_UUID=$(sudo blkid -s UUID -o value "${LOOP_DEV}p2")
# log_info "Updating grub.cfg with new root UUID: $ROOT_UUID"

# # Use regex to find root=UUID=... and replace it with the new one
# # This works regardless of what the old UUID was
# sudo sed -i "s|root=UUID=[a-zA-Z0-9-]*|root=UUID=$ROOT_UUID|g" "$ROOTFS_DIR/boot/grub/grub.cfg"
# sudo sed -i "s|root=UUID=[a-zA-Z0-9-]*|root=UUID=$ROOT_UUID|g" "$ESP_MOUNT/boot/grub/grub.cfg"

# EFI_UUID=$(sudo blkid "${LOOP_DEV}p1" | grep -oP 'UUID="\K[^"]+' | head -1)
# ROOT_PARTUUID=$(sudo blkid "${LOOP_DEV}p2" | grep -oP 'PARTUUID="\K[^"]+' | head -1)
# ROOT_FS_UUID=$(sudo blkid "${LOOP_DEV}p2" | grep -oP 'UUID="\K[^"]+' | head -1)

# log_info "EFI UUID: $EFI_UUID"
# log_info "Root FS UUID: $ROOT_FS_UUID"
# log_info "Root PARTUUID: $ROOT_PARTUUID"

# # Update GRUB config with UUIDs
# perl -pi -e "s/%%%/$EFI_UUID/" "$GRUB_CONFIG"
# perl -pi -e "s/&&&/$ROOT_FS_UUID/" "$GRUB_CONFIG"

# ============================================================================
# MOUNT PARTITIONS
# ============================================================================

log_section "Mounting Partitions"

mkdir -p "$ESP_MOUNT" "$ROOT_MOUNT"

log_info "Mounting EFI System Partition to $ESP_MOUNT..."
if ! sudo mount -t vfat -o codepage=437,iocharset=iso8859-1 "${LOOP_DEV}p1" "$ESP_MOUNT"; then
    log_error "Failed to mount EFI partition"
    exit 1
fi
log_success "EFI System Partition mounted"

log_info "Mounting root partition to $ROOT_MOUNT..."
if ! sudo mount "${LOOP_DEV}p2" "$ROOT_MOUNT"; then
    log_error "Failed to mount root partition"
    sudo umount "$ESP_MOUNT"
    exit 1
fi
log_success "Root partition mounted"

ROOT_UUID=$(sudo blkid -s UUID -o value "${LOOP_DEV}p2")
log_info "Updating GRUB source config with new root UUID: $ROOT_UUID"

sudo sed -Ei "s#root=UUID=[A-Fa-f0-9-]+#root=UUID=${ROOT_UUID}#g" "$GRUB_CONFIG"

log_success "Updated source grub.cfg with root UUID: $ROOT_UUID"

# Verify mounts
log_info "Mount status:"
mount | grep -E "$ESP_MOUNT|$ROOT_MOUNT"

# ============================================================================
# COPY ROOTFS
# ============================================================================

log_section "Installing Rootfs"

log_info "Copying rootfs to root partition..."
if ! sudo cp -r "$ROOTFS_DIR"/* "$ROOT_MOUNT/" 2>&1; then
    log_error "Failed to copy rootfs"
    exit 1
fi
log_success "Rootfs copied"

# Ensure critical directories exist
log_info "Creating critical directories..."
sudo mkdir -p "$ROOT_MOUNT"/{proc,sys,dev,run,boot}
# chmod 755 "$ROOT_MOUNT"/{proc,sys,dev,run,boot}

log_success "Rootfs installation complete"

# ============================================================================
# INSTALL BOOT FILES
# ============================================================================

log_section "Installing Boot Files"

# Create boot directory structure
sudo mkdir -p "$ESP_MOUNT"/EFI/BOOT
sudo mkdir -p "$ESP_MOUNT"/boot/grub

GRUB_EFI_DIR="${ESP_MOUNT}/boot/grub/efi"
sudo mkdir -p "$GRUB_EFI_DIR"

# Copy kernel to ESP
log_info "Copying kernel to ESP..."
sudo cp "$KERNEL_FILE" "$ESP_MOUNT"/vmlinuz
log_success "Kernel installed"

# Copy kernel to rootfs boot as well
sudo mkdir -p "$ROOT_MOUNT"/boot
sudo cp "$KERNEL_FILE" "$ROOT_MOUNT"/boot/vmlinuz
log_success "Kernel copied to rootfs /boot"

# Copy initramfs
log_info "Copying initramfs to ESP..."
sudo cp "$INITRAMFS_FILE" "$ESP_MOUNT"/init.cpio.gz
log_success "Initramfs installed"

sudo cp "$INITRAMFS_FILE" "$ROOT_MOUNT"/boot/init.cpio.gz
log_success "Initramfs copied to rootfs"

# Copy System.map if available
if [ -f "$SYSTEM_MAP" ]; then
    log_info "Copying System.map..."
    sudo cp "$SYSTEM_MAP" "$ROOT_MOUNT"/boot/System.map
    log_success "System.map installed"
fi

# Copy kernel modules to rootfs if available
if [ -d "$KERNEL_MODULES/lib/modules" ]; then
    log_info "Copying kernel modules..."
    sudo mkdir -p "$ROOT_MOUNT/lib/modules"
    sudo cp -r "$KERNEL_MODULES/lib/modules"/* "$ROOT_MOUNT/lib/modules/" 2>&1 || log_warn "Some modules failed to copy"
    log_success "Kernel modules installed"
fi

# ============================================================================
# BUILD UEFI BOOTLOADER
# ============================================================================
    log_section "Building UEFI Bootloader"

log_info "Installing GRUB UEFI bootloader to ESP..."
sudo grub-install \
    --target=x86_64-efi \
    --efi-directory="$ESP_MOUNT" \
    --bootloader-id=GRUB \
    --no-nvram \
    --removable \
    --modules="efi_gop efi_uga gfxterm gfxterm_background linux normal fat ext2 part_gpt part_msdos" \
    "$LOOP_DEV"

# Verify bootloader was installed
if [ -f "$ESP_MOUNT/EFI/BOOT/bootx64.efi" ]; then
    BOOTLOADER_SIZE=$(sudo du -h "$ESP_MOUNT/EFI/BOOT/bootx64.efi" | cut -f1)
    log_success "UEFI bootloader installed: $ESP_MOUNT/EFI/BOOT/bootx64.efi ($BOOTLOADER_SIZE)"
else
    log_error "UEFI bootloader not found after installation"
    exit 1
fi

# ============================================================================
# CONFIGURE GRUB
# ============================================================================
    log_section "Configuring GRUB Bootloader"
    
    log_info "Installing GRUB configuration files..."
    
    # Ensure grub directory exists in rootfs
    if [ ! -d "$ROOTFS_DIR/boot/grub" ]; then
        log_info "Creating grub directory in rootfs..."
        mkdir -p "$ROOTFS_DIR/boot/grub"
    fi
    
    # Copy grub.cfg to rootfs if not already there
    # Copy grub.cfg to rootfs if not already there
if [ ! -f "$ROOTFS_DIR/boot/grub/grub.cfg" ]; then
    log_info "Copying grub.cfg to rootfs..."
    sudo cp "$GRUB_CONFIG" "$ROOTFS_DIR/boot/grub/grub.cfg"
    log_success "GRUB config installed in rootfs"
else
    log_info "GRUB config already in rootfs, verifying..."
    if ! cmp -s "$GRUB_CONFIG" "$ROOTFS_DIR/boot/grub/grub.cfg"; then
        log_warn "Rootfs config differs from source, updating..."
        sudo cp "$GRUB_CONFIG" "$ROOTFS_DIR/boot/grub/grub.cfg"
    fi
    log_success "GRUB config verified in rootfs"
fi

if [ ! -f "$ROOTFS_DIR/boot/grub/grub-bootstrap.cfg" ]; then
    log_info "Copying grub-bootstrap.cfg to rootfs..."
    sudo cp "$GRUB_BOOTSTRAP" "$ROOTFS_DIR/boot/grub/grub-bootstrap.cfg"
    log_success "GRUB bootstrap installed in rootfs"
else
    log_info "GRUB bootstrap already in rootfs, verifying..."
    if ! cmp -s "$GRUB_BOOTSTRAP" "$ROOTFS_DIR/boot/grub/grub-bootstrap.cfg"; then
        log_warn "Rootfs bootstrap differs from source, updating..."
        sudo cp "$GRUB_BOOTSTRAP" "$ROOTFS_DIR/boot/grub/grub-bootstrap.cfg"
    fi
    log_success "GRUB bootstrap verified in rootfs"
fi
    
    # Verify config
    if grep -q "^menuentry" "$ROOTFS_DIR/boot/grub/grub.cfg"; then
        COUNT=$(grep -c "^menuentry" "$ROOTFS_DIR/boot/grub/grub.cfg")
        log_success "Found $COUNT boot menu entries"
        log_info "Copying grub to the EFI"
        if [ ! -f "$ESP_MOUNT/boot/grub/grub.cfg" ]; then
    sudo cp "$GRUB_CONFIG" "$ESP_MOUNT/boot/grub/grub.cfg"
    log_success "Copied grub.cfg to ESP"
fi
if [ ! -f "$ESP_MOUNT/boot/grub/grub-bootstrap.cfg" ]; then
    sudo cp "$GRUB_BOOTSTRAP" "$ESP_MOUNT/boot/grub/grub-bootstrap.cfg"
    log_success "Copied grub-bootstrap.cfg to ESP"
fi
    else
        log_error "No boot entries found in grub.cfg!"
        exit 1
    fi

# # ============================================================================
# # VERIFY GRUB INSTALLATION
# # ============================================================================

log_section "Verifying GRUB Installation"

if [ -f "$ESP_MOUNT/EFI/BOOT/bootx64.efi" ]; then
    BOOTLOADER_SIZE=$(ls -lh "$ESP_MOUNT/EFI/BOOT/bootx64.efi" | awk '{print $5}')
    log_success "UEFI bootloader created: /EFI/BOOT/bootx64.efi ($BOOTLOADER_SIZE)"
    
    # Try to verify it's a valid EFI image
    if file "$ESP_MOUNT/EFI/BOOT/bootx64.efi" 2>&1 | grep -q "PE32\|EFI"; then
        log_success "Bootloader verified as valid EFI executable"
    else
        log_warn "Bootloader file type verification inconclusive"
    fi
else
    log_section "Installing Bootloader to ESP"
    log_info "Copying bootloader to ESP..."
    if ! sudo cp "$BOOTLOADER" "$ESP_MOUNT/EFI/BOOT/bootx64.efi"; then
        log_error "Failed to copy bootloader to ESP"
        exit 1
    fi
    log_success "Bootloader installed to ESP"
fi

# log_info "Files in ESP/EFI/BOOT:"
# sudo ls -lah "$ESP_MOUNT/EFI/BOOT/"

# log_info "Files in rootfs /boot/grub:"
# sudo ls -lah "$ROOT_MOUNT/boot/grub/"

# ============================================================================
# SYNC AND UNMOUNT
# ============================================================================

log_section "Finalizing Disk Image"

log_info "Syncing filesystems..."
sudo sync

log_info "Unmounting partitions..."
if mountpoint -q "$ESP_MOUNT"; then
    sudo umount "$ESP_MOUNT" || log_warn "Failed to unmount $ESP_MOUNT"
fi

if mountpoint -q "$ROOT_MOUNT"; then
    sudo umount "$ROOT_MOUNT" || log_warn "Failed to unmount $ROOT_MOUNT"
fi

sleep 1

log_info "Detaching loop device..."
if sudo losetup -d "$LOOP_DEV"; then
    log_success "Loop device detached"
    LOOP_DEV=""
else
    log_warn "Failed to detach loop device (may require manual cleanup)"
fi

# Clean up mount directories
rmdir "$ESP_MOUNT" 2>/dev/null || true
rmdir "$ROOT_MOUNT" 2>/dev/null || true

log_success "Disk image finalized"

# ============================================================================
# VERIFICATION AND SUMMARY
# ============================================================================

log_section "UEFI Disk Image Creation Complete!"

if [ ! -f "$IMAGE_FILE" ]; then
    log_error "Image file not found! Creation may have failed."
    exit 1
fi

IMAGE_SIZE_ACTUAL=$(du -h "$IMAGE_FILE" | cut -f1)
echo ""
echo "Disk Image Details:"
echo "  Path:              $IMAGE_FILE"
echo "  Size:              $IMAGE_SIZE_ACTUAL"
echo "  Format:            GPT + ext4 with UEFI/GRUB"
echo "  Partition Table:   GPT (GUID Partition Table)"
echo ""
echo "Partition Layout:"
echo "  Partition 1:       EFI System (ESP) - 200MB, FAT32"
echo "  Partition 2:       Root - remainder, ext4"
echo ""
echo "Boot Configuration:"
echo "  Bootloader:        GRUB 2.x (UEFI/EFI mode)"
echo "  Boot Files:        /EFI/BOOT/bootx64.efi (UEFI loader)"
echo "  Boot Partition:    /dev/sda1 (200MB, FAT32 ESP)"
echo "  Root Partition:    /dev/sda2 (remainder, ext4)"
echo "  Kernel:            /vmlinuz"
echo "  Initramfs:         /init.cpio"
echo ""
echo "To Boot the Image:"
echo ""
echo "  QEMU with UEFI (Recommended - requires OVMF):"
echo "    sudo apt install ovmf"
echo "    qemu-system-x86_64 \\"
echo "      -bios /usr/share/OVMF/OVMF_CODE.fd \\"
echo "      -drive file=$IMAGE_FILE,format=raw,media=disk \\"
echo "      -m 2048 -smp 4 -enable-kvm \\"
echo "      -serial stdio"
echo ""
echo "  QEMU with Legacy BIOS (also works with GPT):"
echo "    qemu-system-x86_64 \\"
echo "      -drive file=$IMAGE_FILE,format=raw,media=disk \\"
echo "      -m 2048 -smp 4 -enable-kvm \\"
echo "      -serial stdio"
echo ""
echo "  VirtualBox (enable UEFI in VM settings):"
echo "    qemu-img convert -f raw -O vdi $IMAGE_FILE geckoos-efi.vdi"
echo "    (Create VM with UEFI firmware and geckoos-efi.vdi as disk)"
echo ""
echo "  Direct Write to USB (WARNING - destructive, requires UEFI system):"
echo "    sudo dd if=$IMAGE_FILE of=/dev/sdX bs=4M status=progress && sync"
echo "    (replace /dev/sdX with actual USB device - must boot as UEFI)"
echo ""
echo "Troubleshooting:"
echo "  - No UEFI boot: Check OVMF firmware installed, verify BIOS setting for UEFI"
echo "  - GRUB menu missing: Check /EFI/BOOT/bootx64.efi exists and is valid"
echo "  - Kernel panic: Check kernel config, verify /boot/grub/grub.cfg"
echo "  - Modules missing: Verify /lib/modules/ in rootfs"
echo "  - Init issues: Check /initramfs.img creation"
echo ""

log_success "UEFI disk image ready for deployment!"