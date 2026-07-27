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
# Default to 1600x900, NOT the host's full 1920x1080. QEMU sizes its window to
# the guest scanout, so a 1920x1080 guest on a 1920x1080 host produces a window
# larger than the screen once titlebar/panels are added -- the WM then shrinks
# it and GTK scales the guest down into it, which looks tiny and blurry.
# 1600x900 leaves room for decorations and renders 1:1.
# For a real full-resolution desktop use QEMU_FULLSCREEN=1 (see below), which
# gives pixel-perfect 1:1 at the host's native mode.
QEMU_XRES="${QEMU_XRES:-1600}"
QEMU_YRES="${QEMU_YRES:-900}"
QEMU_FULLSCREEN="${QEMU_FULLSCREEN:-0}"
# Configurable so a second instance can be launched while one is already
# running -- a hardcoded 2222 makes QEMU abort outright with "Could not set up
# host forwarding rule" rather than just skipping the forward.
QEMU_SSH_PORT="${QEMU_SSH_PORT:-2222}"

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

    # CODE and VARS must come from the same directory AND the same size
    # variant (2M vs 4M) - mixing them makes the firmware fail to boot.
    for path in /usr/share/OVMF /usr/share/ovmf; do
        for variant in "" "_4M"; do
            if [ -f "${path}/OVMF_CODE${variant}.fd" ] && \
               [ -f "${path}/OVMF_VARS${variant}.fd" ]; then
                code="${path}/OVMF_CODE${variant}.fd"
                varstemplate="${path}/OVMF_VARS${variant}.fd"
                echo "$code" "$varstemplate"
                return 0
            fi
        done
    done

    log_error "OVMF firmware not found"
    log_info "Install with: sudo apt update && sudo apt install ovmf"
    return 1
}

# virtio-vga-gl aborts QEMU outright if the display backend can't do GL, so
# probe once and degrade to plain virtio-vga rather than failing to launch.
probe_gl() {
    printf 'quit\n' | qemu-system-x86_64 \
        -machine q35 -display egl-headless,gl=on -device virtio-vga-gl \
        -monitor stdio -S &> /dev/null
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

    # NVRAM must PERSIST across runs. A throwaway mktemp copy resets UEFI vars
    # every boot, which resets the OVMF GOP console back to its built-in
    # default (800x600) no matter what the guest or firmware menu selected.
    OVMF_VARS="${IMAGE_DIR}/OVMF_VARS.fd"
    if [ ! -f "$OVMF_VARS" ] || [ "${QEMU_RESET_NVRAM:-0}" = "1" ]; then
        cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"
        log_info "Initialized UEFI NVRAM from $OVMF_VARS_TEMPLATE"
    fi

    log_success "OVMF CODE: $OVMF_CODE"
    log_success "OVMF VARS: $OVMF_VARS (persistent; QEMU_RESET_NVRAM=1 to reset)"

    # Pick the best available VGA device.
    if [ "${QEMU_DISPLAY}" != "none" ] && probe_gl; then
        VGA_DEVICE="virtio-vga-gl"
        DISPLAY_OPTS="${QEMU_DISPLAY},gl=on,show-cursor=on"
        log_success "virtio-vga-gl with host OpenGL (virgl) enabled"
    else
        VGA_DEVICE="virtio-vga"
        DISPLAY_OPTS="${QEMU_DISPLAY},gl=off,show-cursor=on"
        log_warn "Host OpenGL unavailable - falling back to virtio-vga (no virgl)"
    fi

    # zoom-to-fit keeps the aspect ratio when the window is resized instead of
    # letterboxing the guest into a corner. gtk-only suboption.
    if [ "${QEMU_DISPLAY}" = "gtk" ]; then
        DISPLAY_OPTS="${DISPLAY_OPTS},zoom-to-fit=on"
    fi

    # Drop the SSH forward rather than aborting if the port is taken (another
    # VM already running). QEMU treats a bind failure here as fatal.
    SSH_FORWARD=",hostfwd=tcp::${QEMU_SSH_PORT}-:22"
    if ss -ltn 2>/dev/null | grep -q ":${QEMU_SSH_PORT}\b"; then
        log_warn "Port ${QEMU_SSH_PORT} already in use - starting without SSH forwarding"
        log_info "Set QEMU_SSH_PORT=<n> to use a different port"
        SSH_FORWARD=""
    fi

    FULLSCREEN_OPTS=""
    if [ "${QEMU_FULLSCREEN}" = "1" ]; then
        FULLSCREEN_OPTS="-full-screen"
        log_info "Fullscreen requested (ungrab/exit with Ctrl+Alt+F or Ctrl+Alt+G)"
    fi

    # KVM check
    KVM_OPTS=""
    if [ -r /dev/kvm ] && grep -q -E "vmx|svm" /proc/cpuinfo; then
        KVM_OPTS="-enable-kvm -cpu host"
        log_success "KVM acceleration enabled"
    else
        log_warn "KVM unavailable (software emulation - slow)"
    fi
    
    log_info "Launch config:"
    log_info "  CPUs:       $QEMU_CPUS"
    log_info "  Memory:     ${QEMU_MEMORY}MB"
    log_info "  Display:    $VGA_DEVICE via $DISPLAY_OPTS + serial"
    log_info "  Resolution: ${QEMU_XRES}x${QEMU_YRES}"
    log_info "  Network:    ${SSH_FORWARD:+SSH ${QEMU_SSH_PORT} → 22}${SSH_FORWARD:-user-mode NAT (no SSH forward)}"
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
        -vga none \
        -device "${VGA_DEVICE}",xres="${QEMU_XRES}",yres="${QEMU_YRES}",edid=on,max_outputs=1 \
        -display "${DISPLAY_OPTS}" \
        ${FULLSCREEN_OPTS} \
        -device virtio-keyboard-pci \
        -serial mon:stdio \
        -netdev user,id=net0${SSH_FORWARD} \
        -device e1000,netdev=net0 \
        -no-reboot \
        -usb \
        -device usb-tablet \
        -rtc base=localtime,driftfix=slew
}

main "$@"