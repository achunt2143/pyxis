#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
IMAGE_DIR="${BUILD_DIR}/images"
IMAGE_FILE="${IMAGE_DIR}/geckoOS.img"
IMAGE_SIZE="${IMAGE_SIZE:-2048}" # MB

# Kernel and boot files
KERNEL_DIR="${PROJECT_ROOT}/target/kernel"
KERNEL_FILE="${KERNEL_DIR}/arch/x86_64/boot/bzImage"
INITRAMFS_FILE="${PROJECT_ROOT}/target/boot/initramfs-geckoOS.img"

log_info() {
    echo "[INFO] $*"
}

log_success() {
    echo "[✓] $*"
}

log_error() {
    echo "[ERROR] $*" >&2
}

log_warn() {
    echo "[WARN] $*"
}

# ============================================================================
# VERIFY FILES EXIST
# ============================================================================

verify_files() {
    log_info "Verifying required files..."
    
    if [ ! -d "${ROOTFS_DIR}" ]; then
        log_error "Rootfs directory not found: ${ROOTFS_DIR}"
        exit 1
    fi
    log_success "Rootfs found: ${ROOTFS_DIR}"
    
    if [ ! -f "${KERNEL_FILE}" ]; then
        log_warn "Kernel not found at ${KERNEL_FILE}"
        log_info "Skipping kernel installation"
        KERNEL_FILE=""
    else
        log_success "Kernel found: ${KERNEL_FILE}"
    fi
    
    if [ ! -f "${INITRAMFS_FILE}" ]; then
        log_warn "Initramfs not found at ${INITRAMFS_FILE}"
        log_info "Skipping initramfs installation"
        INITRAMFS_FILE=""
    else
        log_success "Initramfs found: ${INITRAMFS_FILE}"
    fi
}

# ============================================================================
# COPY BOOT FILES TO ROOTFS
# ============================================================================

copy_boot_files() {
    log_info "Copying boot files to rootfs..."
    
    mkdir -p "${ROOTFS_DIR}/boot"
    
    if [ -n "${KERNEL_FILE}" ] && [ -f "${KERNEL_FILE}" ]; then
        log_info "Copying kernel..."
        cp "${KERNEL_FILE}" "${ROOTFS_DIR}/boot/vmlinuz-geckoOS"
        log_success "Kernel copied"
    fi
    
    if [ -n "${INITRAMFS_FILE}" ] && [ -f "${INITRAMFS_FILE}" ]; then
        log_info "Copying initramfs..."
        cp "${INITRAMFS_FILE}" "${ROOTFS_DIR}/boot/initramfs-geckoOS.img"
        log_success "Initramfs copied"
    fi
    
    # Copy any kernel modules
    if [ -d "${KERNEL_DIR}/lib/modules" ]; then
        log_info "Copying kernel modules..."
        mkdir -p "${ROOTFS_DIR}/lib/modules"
        cp -r "${KERNEL_DIR}/lib/modules"/* "${ROOTFS_DIR}/lib/modules/" 2>/dev/null || true
        log_success "Kernel modules copied"
    fi
}

# ============================================================================
# CREATE GRUB CONFIGURATION
# ============================================================================

create_grub_config() {
    log_info "Creating GRUB configuration..."
    
    mkdir -p "${ROOTFS_DIR}/boot/grub"
    
    cat > "${ROOTFS_DIR}/boot/grub/grub.cfg" << 'EOF'
# GeckoOS GRUB Configuration
set default=0
set timeout=5

menuentry "GeckoOS" {
    insmod gzio
    insmod part_msdos
    insmod ext2
    set root='(hd0,msdos1)'
    echo 'Loading GeckoOS kernel...'
    linux   /boot/vmlinuz-geckoOS root=/dev/vda1 ro quiet
    echo 'Loading initramfs...'
    initrd  /boot/initramfs-geckoOS.img
}

menuentry "GeckoOS (recovery)" {
    insmod gzio
    insmod part_msdos
    insmod ext2
    set root='(hd0,msdos1)'
    echo 'Loading GeckoOS kernel...'
    linux   /boot/vmlinuz-geckoOS root=/dev/vda1 ro
    echo 'Loading initramfs...'
    initrd  /boot/initramfs-geckoOS.img
}
EOF
    
    log_success "GRUB configuration created"
}

# ============================================================================
# CREATE DISK IMAGE
# ============================================================================

create_disk_image() {
    log_info "Creating ${IMAGE_SIZE}MB disk image..."
    dd if=/dev/zero of="${IMAGE_FILE}" bs=1M count="${IMAGE_SIZE}"
    log_success "Disk image created"
    
    log_info "Formatting image as ext4..."
    mkfs.ext4 -F -L geckoOS "${IMAGE_FILE}"
    log_success "Image formatted"
}

# ============================================================================
# MOUNT AND COPY ROOTFS
# ============================================================================

mount_and_copy() {
    log_info "Mounting image..."
    local mount_point="/mnt/geckoOS-build"
    mkdir -p "${mount_point}"
    
    if ! sudo mount -o loop "${IMAGE_FILE}" "${mount_point}"; then
        log_error "Failed to mount image"
        exit 1
    fi
    log_success "Image mounted at ${mount_point}"
    
    log_info "Copying rootfs to image..."
    sudo cp -r "${ROOTFS_DIR}"/* "${mount_point}/"
    log_success "Rootfs copied"
    
    # Create additional directories if needed
    sudo mkdir -p "${mount_point}/"{proc,sys,dev,run,tmp}
    sudo chmod 1777 "${mount_point}/tmp"
    
    log_info "Syncing..."
    sync
    
    log_info "Unmounting image..."
    sudo umount "${mount_point}"
    rmdir "${mount_point}"
    log_success "Image unmounted"
}

# ============================================================================
# INSTALL BOOTLOADER (Optional - commented out as it's complex)
# ============================================================================

install_bootloader() {
    log_info "Bootloader installation..."
    log_warn "GRUB installation requires special handling in QEMU environment"
    log_info "GRUB is configured but not installed to the image"
    log_info "When booting in QEMU, use: -bios /usr/share/ovmf/OVMF.fd"
    log_info "Or configure GRUB manually after first boot"
}

# ============================================================================
# MAIN
# ============================================================================

main() {
    log_info "========================================="
    log_info "GeckoOS Disk Image Packaging"
    log_info "========================================="
    log_info ""
    
    mkdir -p "${IMAGE_DIR}"
    
    verify_files
    log_info ""
    
    copy_boot_files
    log_info ""
    
    create_grub_config
    log_info ""
    
    create_disk_image
    log_info ""
    
    mount_and_copy
    log_info ""
    
    install_bootloader
    log_info ""
    
    log_success "========================================="
    log_success "Disk image packaging complete!"
    log_success "========================================="
    log_info ""
    log_info "Output: ${IMAGE_FILE}"
    log_info "Size: $(du -h "${IMAGE_FILE}" | cut -f1)"
    log_info ""
    log_info "Boot the image with:"
    log_info "  qemu-system-x86_64 -drive file=${IMAGE_FILE},format=raw -m 2048 -smp 4 -enable-kvm"
    log_info ""
}

main "$@"