#!/bin/bash
# GeckoOS Initramfs Build
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

INITRAMFS="${PROJECT_ROOT}/target/initramfs"
ROOTFS="${PROJECT_ROOT}/target/build/rootfs"
OUTPUT_FILE="${PROJECT_ROOT}/target/boot/init.cpio.gz"

KERNEL_BUILD_DIR="${PROJECT_ROOT}/target/kernel/linux-7.0.1"
KERNEL_RELEASE="$(make -s -C "$KERNEL_BUILD_DIR" kernelrelease)"
MODULES_SRC="${PROJECT_ROOT}/target/kernel-modules/lib/modules/${KERNEL_RELEASE}"
MODULES_DST="${INITRAMFS}/lib/modules/${KERNEL_RELEASE}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[\u2713]${NC} $*"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }

log_info "Preparing initramfs build..."
rm -rf "$INITRAMFS"
mkdir -p "$INITRAMFS"/{bin,sbin,etc,dev,proc,sys,tmp,newroot,lib,usr,var,run}
mkdir -p "$INITRAMFS"/usr/{bin,sbin}
mkdir -p "$MODULES_DST"
log_success "Initramfs directory structure created"

log_info "Building/installing BusyBox..."
cd "$SCRIPT_DIR"
./busybox-install.sh

if [ ! -f "$ROOTFS/bin/busybox" ]; then
    log_error "BusyBox was not installed to rootfs at $ROOTFS/bin/busybox"
    exit 1
fi

log_info "Copying BusyBox from rootfs into initramfs..."
cp -a "$ROOTFS/bin/"*  "$INITRAMFS/bin/"
cp -a "$ROOTFS/sbin/"* "$INITRAMFS/sbin/"
if [ -d "$ROOTFS/usr/bin"  ]; then cp -a "$ROOTFS/usr/bin/"*  "$INITRAMFS/usr/bin/";  fi
if [ -d "$ROOTFS/usr/sbin" ]; then cp -a "$ROOTFS/usr/sbin/"* "$INITRAMFS/usr/sbin/"; fi

if [ ! -f "$INITRAMFS/bin/busybox" ]; then
    log_error "BusyBox is NOT installed to initramfs at $INITRAMFS/bin/busybox"
    exit 1
fi
log_success "BusyBox copied into initramfs"

log_info "Copying kernel modules into initramfs..."
if [ ! -d "$MODULES_SRC" ]; then
    log_error "Kernel modules not found: $MODULES_SRC"
    log_error "Make sure build-kernel installs modules into target/kernel-modules"
    exit 1
fi

cp -a "$MODULES_SRC"/* "$MODULES_DST"/
depmod -b "$INITRAMFS" "$KERNEL_RELEASE"
log_success "Kernel modules copied"

log_info "Creating init script..."
cat > "$INITRAMFS/init" <<'INITSCRIPT'
#!/bin/busybox sh

# Bootstrap: install all busybox applets as symlinks.
/bin/busybox --install -s

export PATH="/bin:/sbin:/usr/bin:/usr/sbin"
NEWROOT="/newroot"

echo ""
echo "=============================="
echo " GeckoOS initramfs starting"
echo "=============================="

mkdir -p /proc /sys /dev /tmp /run /newroot
mount -t devtmpfs devtmpfs /dev
mount -t proc     proc     /proc
mount -t sysfs    sysfs    /sys
mkdir -p /dev/pts
mount -t devpts devpts /dev/pts || true

# Ensure basic device nodes exist
[ -e /dev/console ] || mknod /dev/console c 5 1
[ -e /dev/null ]    || mknod /dev/null    c 1 3
[ -e /dev/tty ]     || mknod /dev/tty     c 5 0

# First mdev pass: populate /dev from sysfs before module loading
/sbin/mdev -s

echo "[initramfs] cmdline: $(cat /proc/cmdline)"

ROOT_SPEC="$(awk '{
  for (i=1;i<=NF;i++) if ($i ~ /^root=/) { sub(/^root=/,"",$i); print $i; exit }
}' /proc/cmdline)"

INIT_BIN="$(awk '{
  for (i=1;i<=NF;i++) if ($i ~ /^init=/) { sub(/^init=/,"",$i); print $i; exit }
}' /proc/cmdline)"
INIT_BIN="${INIT_BIN:-/sbin/init}"

echo "[initramfs] requested root: ${ROOT_SPEC}"
echo "[initramfs] requested init: ${INIT_BIN}"

echo "[initramfs] loading modules..."
modprobe virtio      2>/dev/null || true
modprobe virtio_ring 2>/dev/null || true
modprobe virtio_pci  2>/dev/null || true
modprobe virtio_blk  2>/dev/null || true
modprobe virtio_gpu  2>/dev/null || true
modprobe ahci        2>/dev/null || true
modprobe sd_mod      2>/dev/null || true
modprobe ext4        2>/dev/null || true
modprobe mbcache     2>/dev/null || true
modprobe jbd2        2>/dev/null || true

# Second mdev pass: catch any new block devices registered after modprobe
/sbin/mdev -s
sleep 2

echo "[initramfs] block devices:"
ls -l /dev/sd* /dev/vd* /dev/nvme* 2>/dev/null || true

# ---------------------------------------------------------------------------
# resolve_root: translate a root= spec into a block device path.
#
# For UUID/PARTUUID/LABEL we iterate every block device listed in
# /proc/partitions and call `blkid -s <TAG> -o value <dev>` on each one.
# This approach works with both BusyBox blkid and util-linux blkid, and
# does NOT rely on -U, -t, or cache flags that may be absent in minimal
# BusyBox builds compiled from defconfig.
# ---------------------------------------------------------------------------
resolve_root() {
    local spec="$1"

    case "$spec" in
        /dev/*)
            # Direct device path — return as-is
            echo "$spec"
            return
            ;;
    esac

    # Determine which blkid tag and value to match
    local tag value
    case "$spec" in
        UUID=*)     tag=UUID;     value="${spec#UUID=}"     ;;
        PARTUUID=*) tag=PARTUUID; value="${spec#PARTUUID=}" ;;
        LABEL=*)    tag=LABEL;    value="${spec#LABEL=}"    ;;
        *)
            echo "[initramfs] WARN: unrecognised root spec: $spec" >&2
            return
            ;;
    esac

    # Walk every partition listed in /proc/partitions (skip header lines)
    # Format: major minor #blocks name
    while read -r major minor blocks name; do
        # Skip non-device lines (header, blank, loop, ram, etc.)
        [ -z "$name" ] && continue
        case "$name" in
            loop*|ram*|fd*) continue ;;
        esac
        local dev="/dev/${name}"
        [ -b "$dev" ] || continue

        local got
        got="$(blkid -s "$tag" -o value "$dev" 2>/dev/null)" || continue
        if [ "$got" = "$value" ]; then
            echo "$dev"
            return
        fi
    done < /proc/partitions
}

# Wait up to 5 extra seconds for block device nodes to appear
_blk_wait=0
while [ "$_blk_wait" -lt 5 ]; do
    ls /dev/vd* /dev/sd* /dev/nvme* >/dev/null 2>&1 && break
    sleep 1
    _blk_wait=$(( _blk_wait + 1 ))
done

ROOTDEV=""
for _try in 1 2 3 4 5 6 7 8 9 10; do
    ROOTDEV="$(resolve_root "$ROOT_SPEC" 2>/dev/null || true)"
    if [ -n "$ROOTDEV" ] && [ -b "$ROOTDEV" ]; then
        break
    fi
    echo "[initramfs] waiting for root device: $ROOT_SPEC (attempt ${_try}/10)"
    sleep 1
done

if [ -z "$ROOTDEV" ] || [ ! -b "$ROOTDEV" ]; then
    echo "[initramfs] ERROR: could not resolve root device '${ROOT_SPEC}'"
    echo "[initramfs] Available block devices:"
    # Dump all blkid info for diagnostics
    while read -r major minor blocks name; do
        [ -z "$name" ] && continue
        case "$name" in loop*|ram*|fd*) continue ;; esac
        [ -b "/dev/${name}" ] && blkid "/dev/${name}" 2>/dev/null || true
    done < /proc/partitions
    exec sh
fi

echo "[initramfs] resolved root device: $ROOTDEV"
mount -t ext4 -o ro "$ROOTDEV" "$NEWROOT" || {
    echo "[initramfs] ERROR: failed to mount root"
    exec sh
}

echo "[initramfs] mounted root"
ls -l "$NEWROOT"       || true
ls -l "$NEWROOT/sbin"  || true

if [ ! -e "$NEWROOT/$INIT_BIN" ]; then
    echo "[initramfs] ERROR: init not found: $NEWROOT/$INIT_BIN"
    exec sh
fi

if [ ! -x "$NEWROOT/$INIT_BIN" ]; then
    echo "[initramfs] ERROR: init not executable: $NEWROOT/$INIT_BIN"
    exec sh
fi

mkdir -p "$NEWROOT"/dev "$NEWROOT"/proc "$NEWROOT"/sys
mount --move /dev  "$NEWROOT/dev"  || true
mount --move /proc "$NEWROOT/proc" || true
mount --move /sys  "$NEWROOT/sys"  || true

echo "[initramfs] switching root to $NEWROOT, init=$INIT_BIN"
exec switch_root "$NEWROOT" "$INIT_BIN"

echo "[initramfs] FATAL: switch_root returned"
exec sh
INITSCRIPT

chmod 755 "$INITRAMFS/init"
log_success "Init script created"

log_info "Creating initramfs archive..."
cd "$INITRAMFS"
find . -print0 | cpio --null -o -H newc 2>/dev/null | gzip -9 > "$OUTPUT_FILE"
cd - >/dev/null

log_success "Initramfs created: $OUTPUT_FILE"
echo "Kernel release: $KERNEL_RELEASE"
echo "Size: $(du -h "$OUTPUT_FILE" | cut -f1)"
