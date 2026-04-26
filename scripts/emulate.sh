#!/bin/bash
# scripts/emulate.sh - GeckoOS QEMU Emulation Script (Pyxis/Wayland Ready)
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
IMAGE_DIR="${BUILD_DIR}/images"
IMAGE_FILE="${IMAGE_DIR}/geckoOS.img"

RELEASE_MODE="${1:-debug}"
QEMU_MEMORY="${QEMU_MEMORY:-2048}"
QEMU_CPUS="${QEMU_CPUS:-4}"
QEMU_DISPLAY="${QEMU_DISPLAY:-gtk}"

log_info() {
    echo "[INFO] $*"
}

log_warn() {
    echo "[WARN] $*"
}

log_error() {
    echo "[ERROR] $*" >&2
}

log_success() {
    echo "[✓] $*"
}

find_ovmf_files() {
    local code varstemplate
    
    # Ubuntu/Debian common paths (newer ovmf uses _4M)
    for path in /usr/share/OVMF /usr/share/ovmf; do
        if [ -f "${path}/OVMF_CODE.fd" ]; then
            code="${path}/OVMF_CODE.fd"
        elif [ -f "${path}/OVMF_CODE_4M.fd" ]; then
            code="${path}/OVMF_CODE_4M.fd"
        fi
        if [ -f "${path}/OVMF_VARS.fd" ]; then
            varstemplate="${path}/OVMF_VARS.fd"
        elif [ -f "${path}/OVMF_VARS_4M.fd" ]; then
            varstemplate="${path}/OVMF_VARS_4M.fd"
        fi
    done
    
    if [ -n "$code" ] && [ -n "$varstemplate" ]; then
        echo "$code" "$varstemplate"
        return 0
    fi
    
    log_error "OVMF firmware not found"
    log_info "Install with: sudo apt update && sudo apt install ovmf"
    return 1
}

main() {
    log_info "Starting GeckoOS emulation (Pyxis/Wayland)..."
    
    # Verify image exists
    if [ ! -f "${IMAGE_FILE}" ]; then
        log_error "Disk image not found: ${IMAGE_FILE}"
        log_info "Run './scripts/package.sh' first"
        exit 1
    fi
    log_success "Disk image found: ${IMAGE_FILE}"
    
    # Verify QEMU
    if ! command -v qemu-system-x86_64 &> /dev/null; then
        log_error "QEMU not found. Install: sudo apt install qemu-system-x86"
        exit 1
    fi
    log_success "QEMU found"
    
    # Find OVMF firmware with fallbacks
    read OVMF_CODE OVMF_VARS_TEMPLATE <<< "$(find_ovmf_files)" || exit 1
    OVMF_VARS=$(mktemp /tmp/OVMF_VARS.XXXXXX.fd)
    cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"
    trap "rm -f '$OVMF_VARS'" EXIT
    
    log_success "OVMF CODE:  $OVMF_CODE"
    log_success "OVMF VARS: $OVMF_VARS_TEMPLATE → $OVMF_VARS"
    
    # KVM check
    KVM_OPTS=""
    if [ -r /dev/kvm ] && grep -q -E "vmx|svm" /proc/cpuinfo; then
        KVM_OPTS="-enable-kvm -cpu host"
        log_success "KVM acceleration enabled"
    else
        log_warn "KVM unavailable (software emulation - slow)"
    fi
    
    log_info "Launch config:"
    log_info "  CPUs:     $QEMU_CPUS"
    log_info "  Memory:   ${QEMU_MEMORY}MB"
    log_info "  Display:  $QEMU_DISPLAY + serial"
    log_info "  Network:  SSH 2222 → 22"
    log_info ""
    log_info "Expect: Serial login in terminal + graphics in window"
    log_info "Pyxis/Evo Wayland should auto-start"
    
    qemu-system-x86_64 \
        -name "GeckoOS-Pyxis" \
        -machine q35 \
        -m "${QEMU_MEMORY}" \
        -smp "${QEMU_CPUS}" \
        ${KVM_OPTS} \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$OVMF_VARS" \
        -drive file="${IMAGE_FILE}",format=raw,media=disk,if=virtio \
        -device virtio-vga-gl \
        -display "${QEMU_DISPLAY}",gl=on,show-cursor=on \
        -device virtio-keyboard-pci \
        -device virtio-mouse-pci \
        -serial mon:stdio \
        -netdev user,id=net0,hostfwd=tcp::2222-:22 \
        -device e1000,netdev=net0 \
        -no-reboot \
        -usb \
        -device usb-tablet \
        -rtc base=localtime,driftfix=slew
}

main "$@"