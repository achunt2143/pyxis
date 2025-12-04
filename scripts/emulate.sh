#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
IMAGE_DIR="${BUILD_DIR}/images"
IMAGE_FILE="${IMAGE_DIR}/geckoOS.img"

RELEASE_MODE="${1:-debug}"
QEMU_MEMORY="${QEMU_MEMORY:-2048}"
QEMU_CPUS="${QEMU_CPUS:-4}"
QEMU_NET="${QEMU_NET:-user}"

log_info() {
    echo "[INFO] $*"
}

log_error() {
    echo "[ERROR] $*" >&2
}

main() {
    log_info "Starting GeckoOS emulation..."
    
    if [ ! -f "${IMAGE_FILE}" ]; then
        log_error "Disk image not found: ${IMAGE_FILE}"
        log_info "Run './scripts/package.sh' first"
        exit 1
    fi
    
    if ! command -v qemu-system-x86_64 &> /dev/null; then
        log_error "QEMU not found"
        exit 1
    fi
    
    log_info "Launching QEMU with ${QEMU_CPUS} CPUs, ${QEMU_MEMORY}MB RAM"
    
    qemu-system-x86_64 \
        -drive file="${IMAGE_FILE}",format=raw,media=disk \
        -m "${QEMU_MEMORY}" \
        -smp "${QEMU_CPUS}" \
        -net "${QEMU_NET}" \
        -serial mon:stdio \
        -display gtk \
        -enable-kvm \
        -bios /usr/share/ovmf/OVMF.fd
}

main "$@"