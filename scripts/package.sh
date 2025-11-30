#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
IMAGE_DIR="${BUILD_DIR}/images"
IMAGE_FILE="${IMAGE_DIR}/geckoOS.img"
IMAGE_SIZE="${IMAGE_SIZE:-2048}" # MB

log_info() {
    echo "[INFO] $*"
}

log_success() {
    echo "[✓] $*"
}

log_error() {
    echo "[ERROR] $*" >&2
}

main() {
    log_info "Packaging GeckoOS disk image..."
    
    mkdir -p "${IMAGE_DIR}"
    
    if [ ! -d "${ROOTFS_DIR}" ]; then
        log_error "Rootfs directory not found: ${ROOTFS_DIR}"
        exit 1
    fi
    
    log_info "Creating ${IMAGE_SIZE}MB disk image..."
    dd if=/dev/zero of="${IMAGE_FILE}" bs=1M count="${IMAGE_SIZE}"
    
    log_success "Disk image created"
    
    log_info "Formatting image as ext4..."
    mkfs.ext4 -F "${IMAGE_FILE}"
    
    log_success "Image formatted"
    
    log_info "Mounting image..."
    local mount_point="/mnt/geckoOS-build"
    mkdir -p "${mount_point}"
    sudo mount -o loop "${IMAGE_FILE}" "${mount_point}"
    
    log_info "Copying rootfs to image..."
    sudo cp -r "${ROOTFS_DIR}"/* "${mount_point}/"
    
    log_info "Installing bootloader..."
    # GRUB installation would happen here
    # grub-install --root-directory="${mount_point}" "${IMAGE_FILE}"
    
    log_info "Syncing..."
    sync
    
    log_info "Unmounting image..."
    sudo umount "${mount_point}"
    rmdir "${mount_point}"
    
    log_success "Disk image packaged: ${IMAGE_FILE}"
    log_info "Image size: $(du -h "${IMAGE_FILE}" | cut -f1)"
}

main "$@"