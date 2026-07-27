#!/bin/bash
# scripts/clean.sh - GeckoOS/Pyxis build tree cleaner
#
# Tiered because the artifacts under target/ are wildly different in cost to
# regenerate: the rootfs is a ~5 minute debootstrap, the kernel is a large
# download plus a long compile, and the Gecko build is measured in hours.
# A single "delete everything" would make routine cleanups needlessly
# expensive, so each tier is opted into separately.
#
# Usage:
#   clean.sh [--rootfs] [--images] [--kernel] [--gecko] [--cargo] [--all]
#            [--release-artifacts] [--yes]
#
# With no tier flags, --rootfs --images is assumed: that is the combination
# that clears the broken/partial dpkg state which makes create-rootfs.sh fail
# repeatedly with "Group ... already exists".
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
KERNEL_DIR="${PROJECT_ROOT}/target/kernel"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
IMAGE_DIR="${BUILD_DIR}/images"
GECKO_BUILD_DIR="${BUILD_DIR}/gecko"

DO_ROOTFS=0
DO_IMAGES=0
DO_KERNEL=0
DO_GECKO=0
DO_CARGO=0
DO_RELEASE_ARTIFACTS=0
ASSUME_YES=0
ANY_TIER=0

log_info()    { echo "[INFO] $*"; }
log_warn()    { echo "[WARN] $*"; }
log_error()   { echo "[ERROR] $*" >&2; }
log_success() { echo "[✓] $*"; }

usage() {
    sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --rootfs)            DO_ROOTFS=1; ANY_TIER=1 ;;
        --images)            DO_IMAGES=1; ANY_TIER=1 ;;
        --kernel)            DO_KERNEL=1; ANY_TIER=1 ;;
        --gecko)             DO_GECKO=1; ANY_TIER=1 ;;
        --cargo)             DO_CARGO=1; ANY_TIER=1 ;;
        --release-artifacts) DO_RELEASE_ARTIFACTS=1 ;;
        --all)               DO_ROOTFS=1; DO_IMAGES=1; DO_KERNEL=1; DO_CARGO=1; ANY_TIER=1 ;;
        --yes|-y)            ASSUME_YES=1 ;;
        --help|-h)           usage ;;
        *) log_error "Unknown option: $1"; exit 1 ;;
    esac
    shift
done

if [ "$ANY_TIER" = "0" ]; then
    DO_ROOTFS=1
    DO_IMAGES=1
fi

# ---------------------------------------------------------------------------
# Release artifacts are kept unless explicitly requested. target/build/images
# holds hand-made release builds (pyxis-alpha1.img / .tar.xz) alongside the
# regenerable geckoOS.img -- those are not reproducible by a rebuild, so a
# "clean" that silently ate them would be destroying the only copy.
# ---------------------------------------------------------------------------
RELEASE_GLOBS=("pyxis-*.img" "pyxis-*.tar.xz" "pyxis-*.tar.gz" "pyxis-*.zip")

is_release_artifact() {
    local base
    base="$(basename "$1")"
    for glob in "${RELEASE_GLOBS[@]}"; do
        # shellcheck disable=SC2053
        [[ "$base" == $glob ]] && return 0
    done
    return 1
}

# ---------------------------------------------------------------------------
# package.sh attaches the disk image to a loop device and mounts its two
# partitions under target/build/mnt-{esp,root}. If a packaging run died before
# its cleanup trap fired, those are still live -- and rm -rf into a live
# mountpoint would delete through it into the mounted filesystem. Always tear
# these down before removing anything.
# ---------------------------------------------------------------------------
release_mounts_and_loops() {
    local mp
    for mp in "${BUILD_DIR}/mnt-esp" "${BUILD_DIR}/mnt-root"; do
        if mountpoint -q "$mp" 2>/dev/null; then
            log_warn "Stale mount found at $mp - unmounting"
            sudo umount "$mp" || {
                log_error "Could not unmount $mp. Something still has it open:"
                sudo fuser -vm "$mp" 2>&1 || true
                exit 1
            }
        fi
    done

    # Detach any loop device still backed by an image in our image dir.
    local dev backing
    while read -r dev backing; do
        [ -n "${dev:-}" ] || continue
        case "$backing" in
            "${IMAGE_DIR}"/*)
                log_warn "Stale loop device $dev -> $backing - detaching"
                sudo losetup -d "$dev" || log_warn "Could not detach $dev"
                ;;
        esac
    done < <(losetup -ln -O NAME,BACK-FILE 2>/dev/null || true)
}

# rm that copes with the rootfs being root-owned (sudo debootstrap creates it),
# which is why a plain `rm -rf target` fails with permission denied.
nuke() {
    local target="$1"
    [ -e "$target" ] || return 0
    # Refuse to act outside the project's target/ tree, so a mangled variable
    # can never turn this into an rm -rf of something important.
    case "$(readlink -f "$target")" in
        "${PROJECT_ROOT}/target"/*) ;;
        *) log_error "Refusing to remove '$target' - outside ${PROJECT_ROOT}/target"; exit 1 ;;
    esac
    log_info "Removing $target"
    if [ -w "$(dirname "$target")" ] && [ "$(stat -c %U "$target")" = "$(id -un)" ]; then
        rm -rf "$target"
    else
        sudo rm -rf "$target"
    fi
}

# ---------------------------------------------------------------------------
# Plan, confirm, execute.
# ---------------------------------------------------------------------------
PLAN=()
[ "$DO_ROOTFS" = "1" ] && PLAN+=("rootfs        ${ROOTFS_DIR} (~5 min to rebuild: debootstrap + apt)")
[ "$DO_IMAGES" = "1" ] && PLAN+=("disk image    ${IMAGE_DIR}/geckoOS.img + staging dirs (~2 min)")
[ "$DO_KERNEL" = "1" ] && PLAN+=("kernel        ${KERNEL_DIR} (LONG: re-download + full kernel compile)")
[ "$DO_GECKO"  = "1" ] && PLAN+=("gecko         ${GECKO_BUILD_DIR} (VERY LONG: full Gecko rebuild, hours)")
[ "$DO_CARGO"  = "1" ] && PLAN+=("cargo         cargo clean + dist/ + *.log")
[ "$DO_RELEASE_ARTIFACTS" = "1" ] && PLAN+=("release       pyxis-*.img / pyxis-*.tar.* -- NOT REPRODUCIBLE BY A REBUILD")

echo
log_info "Clean plan:"
for line in "${PLAN[@]}"; do echo "    - $line"; done
echo

if [ "$DO_RELEASE_ARTIFACTS" = "1" ] && [ "$ASSUME_YES" != "1" ]; then
    log_warn "This will delete release artifacts that no build step can regenerate:"
    for f in "${IMAGE_DIR}"/*; do
        [ -e "$f" ] || continue
        is_release_artifact "$f" && echo "        $(basename "$f")  ($(du -h "$f" | cut -f1))"
    done
    read -rp "Type 'delete' to confirm: " reply
    [ "$reply" = "delete" ] || { log_info "Aborted."; exit 1; }
fi

release_mounts_and_loops

if [ "$DO_ROOTFS" = "1" ]; then
    nuke "$ROOTFS_DIR"
    log_success "rootfs cleared - next build will re-debootstrap from scratch"
fi

if [ "$DO_IMAGES" = "1" ]; then
    nuke "${IMAGE_DIR}/geckoOS.img"
    nuke "${BUILD_DIR}/temp"
    nuke "${BUILD_DIR}/mnt-esp"
    nuke "${BUILD_DIR}/mnt-root"
    # OVMF_VARS.fd is the emulator's persistent UEFI NVRAM. It lives in the
    # image dir but is emulator state, not a build artifact -- dropping it
    # resets any firmware-level display/boot-order settings.
    nuke "${IMAGE_DIR}/OVMF_VARS.fd"
    if [ "$DO_RELEASE_ARTIFACTS" = "1" ]; then
        for f in "${IMAGE_DIR}"/*; do
            [ -e "$f" ] || continue
            is_release_artifact "$f" && nuke "$f"
        done
    else
        for f in "${IMAGE_DIR}"/*; do
            [ -e "$f" ] || continue
            is_release_artifact "$f" && log_info "Keeping release artifact $(basename "$f") (--release-artifacts to remove)"
        done
    fi
    log_success "images cleared"
fi

if [ "$DO_KERNEL" = "1" ]; then
    nuke "$KERNEL_DIR"
    log_success "kernel tree cleared"
fi

if [ "$DO_GECKO" = "1" ]; then
    nuke "$GECKO_BUILD_DIR"
    log_success "gecko build cleared"
fi

if [ "$DO_CARGO" = "1" ]; then
    log_info "Running cargo clean..."
    (cd "$PROJECT_ROOT" && cargo clean)
    rm -rf "${PROJECT_ROOT}/dist" "${PROJECT_ROOT}"/*.log
    log_success "cargo artifacts cleared"
fi

echo
log_success "Clean complete"
