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
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }

log_info "Preparing initramfs build..."
rm -rf "$INITRAMFS"
mkdir -p "$INITRAMFS"/{bin,sbin,etc,dev,proc,sys,tmp,newroot,lib,usr,var,run}
mkdir -p "$INITRAMFS"/usr/{bin,sbin}
mkdir -p "$MODULES_DST"
log_success "Initramfs directory structure created"

log_info "Building/installing BusyBox..."
bash "$SCRIPT_DIR/busybox-install.sh"

# Source the busybox binary directly from the build output
# (target/build/busybox-build/busybox/busybox), NOT from
# "$ROOTFS/bin/busybox" — busybox-install.sh no longer installs anything
# into the shared rootfs at all (see that script's own comment: its
# `make install` used to symlink every applet, including `setpriv`, over
# whatever the real rootfs already had, which clobbered util-linux's real
# `setpriv` the moment it ran a second time after apt packages were
# already installed — a real regression found via an actual boot). That
# fix broke THIS script in turn: it used to depend on that same `make
# install` step to seed "$ROOTFS/bin/busybox" *before* create-rootfs.sh
# (which runs after build-initramfs in the Makefile's dependency order,
# so create-rootfs.sh's own stage_busybox_init had never populated that
# path yet at this point) ever touched the rootfs — a fragile,
# order-dependent coupling between two otherwise-independent scripts.
# Reading the build artifact directly removes that coupling entirely, and
# matches exactly what create-rootfs.sh's stage_busybox_init already does
# for the same reason.
BUSYBOX_BUILD_BIN="${PROJECT_ROOT}/target/build/busybox-build/busybox/busybox"
if [ ! -f "$BUSYBOX_BUILD_BIN" ]; then
    log_error "BusyBox build artifact not found at $BUSYBOX_BUILD_BIN"
    exit 1
fi

# Just the one static binary — busybox's own `--install -s` bootstrap
# (see the init script below) creates every applet symlink (sh, mount,
# blkid, mdev, modprobe, awk, grep, cut, ...) this init script needs
# entirely from that single file at boot time. There is nothing else in
# the real rootfs's /bin, /sbin, /usr/{bin,sbin} this minimal early-boot
# environment actually calls — copying them in was dead weight (dynamically
# linked Debian binaries with no matching /lib64/ld-linux.so.2 or shared
# libraries staged into the initramfs to actually run them).
log_info "Copying BusyBox build artifact into initramfs..."
cp -a "$BUSYBOX_BUILD_BIN" "$INITRAMFS/bin/busybox"
chmod 755 "$INITRAMFS/bin/busybox"

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
# Uses awk to read /proc/partitions so whitespace padding and the header
# line are handled correctly -- plain `read` in BusyBox sh mis-parses the
# space-padded columns and the header row.
# ---------------------------------------------------------------------------
resolve_root() {
    local spec="$1"

    # Already a device path -- nothing to do.
    case "$spec" in
        /dev/*)
            echo "$spec"
            return
            ;;
    esac

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

    # awk skips the header (NF<4) and comment lines, prints the device name
    # field (4th column) trimmed of whitespace.
    while IFS= read -r devname; do
        [ -z "$devname" ] && continue
        case "$devname" in loop*|ram*|fd*) continue ;; esac
        local dev="/dev/${devname}"
        [ -b "$dev" ] || continue

        # BusyBox's blkid applet doesn't support util-linux's `-s TAG -o value`
        # selector/output-format flags -- it always prints the full
        # `DEVICE: TAG="val" TAG="val" ...` form. Extract the tag we want
        # from that instead of relying on flags it silently doesn't honor.
        # The leading [[:space:]] anchor keeps a UUID match from also
        # matching inside PARTUUID="..." (which ends in "UUID=" too).
        local got
        got="$(blkid "$dev" 2>/dev/null | grep -oE "[[:space:]]${tag}=\"[^\"]*\"" | cut -d'"' -f2)"
        if [ "$got" = "$value" ]; then
            echo "$dev"
            return
        fi
    done <<EOF
$(awk 'NF==4 && $1+0>0 { print $4 }' /proc/partitions)
EOF
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
    awk 'NF==4 && $1+0>0 { print $4 }' /proc/partitions | while IFS= read -r n; do
        [ -b "/dev/$n" ] && blkid "/dev/$n" 2>/dev/null || true
    done
    exec sh
fi

echo "[initramfs] resolved root device: $ROOTDEV"

# Mount rw -- the kernel cmdline carries 'rw' and systemd expects a
# writable root. Mounting ro here causes systemd to fail remounting.
mount -t ext4 "$ROOTDEV" "$NEWROOT" || {
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
