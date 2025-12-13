#!/bin/bash
# GeckoOS Initramfs Build - CORRECTED
# ============================================================================
# This script builds a minimal initramfs that:
# 1. Mounts /proc, /sys, /dev
# 2. Mounts the real root filesystem (/dev/sda2)
# 3. Pivots to the real root
# 4. Executes /sbin/init from the real rootfs
# ============================================================================



set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BUILD_DIR="${PROJECT_ROOT}/target/build"
INITRAMFS="${PROJECT_ROOT}/target/initramfs"
ROOTFS="${PROJECT_ROOT}/target/build/rootfs"
INIT_FILE="init.cpio.gz"
OUTPUT_FILE_DIR="${PROJECT_ROOT}/target/boot/${INIT_FILE}"



# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ============================================================================
# SETUP
# ============================================================================

log_info "Preparing initramfs build..."

# Clean previous build
# rm -rf "$BUILD_DIR"
mkdir -p "$INITRAMFS"
# mkdir -p "$(dirname "$OUTPUT_FILE")"
log_info "Setting up rootfs directory structure..."
mkdir -p "$INITRAMFS"/{bin,sbin,etc,dev,proc,sys,tmp,newroot,lib,usr,var}
mkdir -p "$INITRAMFS"/usr/{bin,sbin}
# sudo cp --archive /dev/{null,console,tty,tty0,ttyS0,sda1,sda} "$INITRAMFS"/dev/
# sudo cp --archive "$INITRAMFS"/dev/sda "$INITRAMFS"/dev/sda2
# for d in null console tty tty0 ttyS0 sda1 sda sda2 ; do
#     # sudo chown root:root "$INITRAMFS/dev/$d"
#     chmod 666 "$INITRAMFS/dev/$d"
# done
# touch "$INITRAMFS"/tmp/boot.log
# sudo chown root:root /tmp/boot.log
# chmod 666 "$INITRAMFS"/tmp/boot.log
# mkdir -p "$ROOTFS"/home/root
# chmod 1777 "$ROOTFS/tmp"
log_success "Rootfs directories created"

# ============================================================================
# COPY BUSYBOX FROM BUILD ROOTFS (NOT HOST SYSTEM)
# ============================================================================

log_info "Installing BusyBox to GeckoOS initramfs..."
cd "${SCRIPT_DIR}"
./busybox-install.sh
log_info "Making sure build was success and files are present"
if [ ! -f "$INITRAMFS/bin/busybox" ]; then
    log_error "BusyBox is NOT installed to initramfs!"
    log_error "File info: $(file "$INITRAMFS/bin/busybox")"
    log_error "Check the install and make sure symlinks get created."
    exit 1
fi

# # Use the busybox YOU installed in the rootfs, not the host's busybox
# # ROOTFS="${PROJECT_ROOT}/target/build/rootfs"
# BUSYBOX_PATH="$ROOTFS/bin/busybox"

# if [ ! -f "$BUSYBOX_PATH" ]; then
#     log_error "BusyBox not found at $BUSYBOX_PATH"
#     log_error "Did you run the busybox installation script?"
#     exit 1
# fi

# # Verify it's statically linked
# if ! file "$BUSYBOX_PATH" | grep -q "statically linked"; then
#     log_error "BusyBox is not statically linked!"
#     log_error "Static BusyBox is required for initramfs"
#     file "$BUSYBOX_PATH"
#     exit 1
# fi

# log_success "BusyBox found (static): $BUSYBOX_PATH"

# # Copy to initramfs
# mkdir -p "$BUILD_DIR/bin"
# cp "$BUSYBOX_PATH" "$BUILD_DIR/bin/busybox"
# chmod +x "$BUILD_DIR/bin/busybox"

# # Create symlinks for common commands
# log_info "Creating BusyBox symlinks..."
# cd "$BUILD_DIR/bin"
# for cmd in sh mount umount cat ls mkdir rm cp mv touch chmod; do
#     rm -f "$cmd"
#     ln -s busybox "$cmd"
# done

# # cd - > /dev/null
# log_success "Symlinks created"

# ============================================================================
# COPY ESSENTIAL LIBRARIES (ONLY IF BUSYBOX IS DYNAMIC)
# ============================================================================

# log_info "Checking if libraries are needed..."

# if file "$BUSYBOX_PATH" | grep -q "dynamically linked"; then
#     log_info "BusyBox is dynamically linked, copying libraries..."
#     mkdir -p "$BUILD_DIR/lib64"
    
#     # Get library dependencies
#     LIBS=$(ldd "$BUSYBOX_PATH" 2>/dev/null | grep "=>" | awk '{print $3}' | grep "^/")
    
#     for lib in $LIBS; do
#         if [ -f "$lib" ]; then
#             log_info "  Copying: $lib"
#             cp "$lib" "$BUILD_DIR/lib64/"
#         fi
#     done
    
#     # Copy ld-linux interpreter
#     if [ -f "/lib64/ld-linux-x86-64.so.2" ]; then
#         cp "/lib64/ld-linux-x86-64.so.2" "$BUILD_DIR/lib64/"
#     fi
    
#     log_success "Libraries copied"
# else
#     log_success "BusyBox is statically linked (no libraries needed)"
# fi

# ============================================================================
# CREATE INIT SCRIPT FOR INITRAMFS
# ============================================================================

log_info "Creating initramfs init script..."

cat > "$INITRAMFS/init" <<'INITSCRIPT'
#!/bin/busybox sh

# Export PATH so child processes inherit it
export PATH="/bin:/sbin:/usr/bin:/usr/sbin"

# Path to directory to mount $ROOTDEV under in initrd
NEWROOT="/newroot"

# Commands
BBINSTALL="/bin/busybox --install -s"
CAT="/bin/busybox cat"
MOUNT="/bin/busybox mount"
UMOUNT="/bin/busybox umount"
SHELL="/bin/busybox sh"
MKDIR="/bin/busybox mkdir"
GREP="/bin/busybox grep"
AWK="/bin/busybox awk"
SWITCH="/bin/busybox switch_root"

# Kernel command-line
KCMD="/proc/cmdline"

# Function for dropping to a shell
shell () {
    echo ''
    echo '  Entering rescue shell.'
    echo '  Type rootdev root_device to set device to boot.'
    echo '     ex: rootdev /dev/sda1'
    echo '  Exit shell to continue booting.'
    $SHELL
}

tryresume () {
    echo ''
    echo '  Attempting to resume from suspend-to-disk.'
    # Ensure /bin/resume actually exists in your initramfs!
    /bin/resume || echo '  resume: error: could not resume'
    echo ''
}

# Create rootdev function helper
echo '#!/bin/sh' > /bin/rootdev
echo 'echo "$1" > /rootdev' >> /bin/rootdev
echo 'exit $?' >> /bin/rootdev
# busybox chmod is not defined as variable above, using direct path or variable if you add it
/bin/busybox chmod 0755 /bin/rootdev
RDEV="/bin/rootdev"

# EXECUTION BEGINS HERE
echo "Entering initramfs"

echo "Creating busybox symlinks"
$BBINSTALL

# Ensure basic directories exist
for dir in /proc /sys /dev /tmp /run
do
    [ -d $dir ] || $MKDIR -p $dir
done

# 1. Mount devtmpfs (Modern method: Kernel populates /dev automatically)
echo "Mounting devtmpfs"
$MOUNT -t devtmpfs devtmpfs /dev

# 2. Mount /proc and /sys
echo "Mounting procfs"
$MOUNT -t proc proc /proc
echo "Mounting sysfs"
$MOUNT -t sysfs sys /sys

# 3. Setup PTYs (Important for shell interaction)
$MKDIR -p /dev/pts
$MOUNT -t devpts devpts /dev/pts

# Drop to shell if "shell" was passed
$GREP -q 'shell' /proc/cmdline && shell

# Try resume
$GREP -q 'noresume' /proc/cmdline || tryresume

# Parse Init Args
# Get all args, remove key=val pairs (kernel params)
INIT_ARGS="$($AWK '{gsub(/[[:graph:]]+=[[:graph:]]+/,""); print}' $KCMD)"
# Clean up specific initramfs flags
INIT_ARGS="$(echo $INIT_ARGS | $AWK '{gsub(/noresume/,""); gsub(/shell/,""); print}')"

# Determine Init program
INIT="$($AWK '/.*init=/ {sub(/.*init=/,""); sub(/[ ].*/,""); print}' $KCMD)"
INIT="${INIT:-/sbin/init}"

# Determine Root Device
RTDEV="$($AWK '/root=/ {sub(/.*root=/,""); sub(/[ ].*/,""); print}' $KCMD)"

# Write to /rootdev if empty
if [ ! -s /rootdev ]; then
    $RDEV "$RTDEV" || echo "Failed to make /rootdev"
fi

# Check if ROOTDEV is NOT a /dev/ path (e.g. it is a major:minor number)
# FIX: Use exit code directly, do not use [ ! $(...) ]
if ! $GREP -q '^/dev/.*' /rootdev; then
    # This block handles converting major:minor numbers to device names
    # Only runs if root=801 (example) instead of root=/dev/sda1
    
    MAJOR="$($AWK '{ print substr($0,1,1) }' /rootdev)"
    MINOR="$($AWK '{ print substr($0,2) }' /rootdev)"
    MINOR="$(echo $MINOR | $AWK '{ sub(/^0+/,"",$0); print $0 }')"

    DISKS="/proc/diskstats"
    BOOT="$($AWK '$1 ~'$MAJOR' && $2 ~'$MINOR' { print $3 }' $DISKS)"
    
    if [ -n "$BOOT" ]; then
        $RDEV "/dev/$BOOT"
    fi
fi

# Mount the root partition
SUCCESS=0
[ -d "$NEWROOT" ] || $MKDIR "$NEWROOT"
while [ $SUCCESS -ne 1 ]; do
    ROOTDEV="$($CAT /rootdev)"
    
    # Check if device node exists before mounting
    if [ ! -b "$ROOTDEV" ]; then
        echo "Waiting for $ROOTDEV..."
        sleep 1
        # Optional: mdev -s could be run here if specific devices are missing, 
        # but devtmpfs usually handles it.
    fi

    if ! $MOUNT -o ro "$ROOTDEV" "$NEWROOT"; then
        echo ""
        echo "Couldn't mount root FS ($ROOTDEV) read-only!"
        shell
    else
        SUCCESS=1
    fi
done

if [ ! -e "$NEWROOT/$INIT" ]; then
    echo ""
    echo "$ROOTDEV mounted, but $INIT not found!"
    shell
fi

# ... (after your rootfs mount loop) ...

# Prepare to switch root
echo "Moving essential filesystems to new root..."

# 1. Move /dev (Preserves your device nodes!)
# We assume $NEWROOT/dev exists because your rootfs creation script made it
if mountpoint -q /dev; then
    mount --move /dev "$NEWROOT/dev" || echo "Warning: Failed to move /dev"
else
    # Fallback if it wasn't mounted for some reason
    echo "Warning: /dev was not mounted, real init will have to handle it"
fi

# 2. Move /proc (Optional, but good practice if real init expects it)
if mountpoint -q /proc; then
    mount --move /proc "$NEWROOT/proc" || umount /proc
fi

# 3. Move /sys (Optional)
if mountpoint -q /sys; then
    mount --move /sys "$NEWROOT/sys" || umount /sys
fi

# 4. Unmount temporary mounts that we don't need
# Only unmount /run or /tmp if they are NOT needed in the new system
umount /tmp 2>/dev/null
umount /run 2>/dev/null
umount /dev/pts 2>/dev/null

echo "Executing switch_root..."

# exec switch_root (BusyBox version)
# Usage: switch_root [-c /dev/console] NEW_ROOT NEW_INIT [ARGS]
# Note: switch_root automatically wipes the old initramfs contents
exec $SWITCH "$NEWROOT" "$INIT" $INIT_ARGS

# If we get here, something went terribly wrong
echo "FATAL: switch_root failed!"
# Emergency recovery: try to remount /proc and /dev to give a useful shell
mount -t proc proc /proc
mount -t devtmpfs devtmpfs /dev
exec $SHELL
INITSCRIPT


chmod 755 "$INITRAMFS/init"
# sudo chown root:root "$INITRAMFS/init"
# echo "INITRAMFS STARTED" > /dev/console 2>&1 || true
# echo "INITRAMFS STARTED" > /dev/tty0 2>&1 || true
# echo "INITRAMFS STARTED" > /dev/tty 2>&1 || true
# # Initramfs init - mounts real root and pivots to it

# for console_dev in /dev/console /dev/tty0 /dev/tty1; do
#     if [ -e "$console_dev" ]; then
#         exec >"$console_dev" 2>&1 <"$console_dev"
#         break
#     fi
# done

# set -e

# echo "=========================================="
# echo ">> GeckoOS Initramfs Init Starting"
# echo "=========================================="
# echo ""

# # ============================================================================
# # MOUNT ESSENTIAL FILESYSTEMS
# # ============================================================================

# echo ">> Mounting kernel filesystems..."
# mount -t proc proc /proc 2>/dev/null || true
# mount -t sysfs sysfs /sys 2>/dev/null || true
# mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
# mount -t tmpfs tmpfs /run 2>/dev/null || true

# # ============================================================================
# # CREATE ESSENTIAL DEVICE NODES
# # ============================================================================

# echo ">> Creating essential device nodes..."

# [ ! -e /dev/console ] && mknod /dev/console c 5 1 2>/dev/null || true
# [ ! -e /dev/null ] && mknod /dev/null c 1 3 2>/dev/null || true
# [ ! -e /dev/zero ] && mknod /dev/zero c 1 5 2>/dev/null || true
# [ ! -e /dev/tty ] && mknod /dev/tty c 5 0 2>/dev/null || true

# # ============================================================================
# # DETERMINE ROOT DEVICE
# # ============================================================================

# echo ">> Determining root device..."

# # Try to get root device from kernel command line
# ROOT_DEVICE=$(cat /proc/cmdline | grep -o 'root=[^ ]*' | cut -d= -f2 || echo "")

# # Fall back to default if not specified
# if [ -z "$ROOT_DEVICE" ]; then
#     ROOT_DEVICE="/dev/sda2"
#     echo ">> No root device in cmdline, using default: $ROOT_DEVICE"
# else
#     echo ">> Root device from cmdline: $ROOT_DEVICE"
# fi

# # ============================================================================
# # WAIT FOR ROOT DEVICE
# # ============================================================================

# echo ">> Waiting for root device to appear..."

# TIMEOUT=30
# WAIT_COUNT=0

# while [ ! -b "$ROOT_DEVICE" ] && [ $WAIT_COUNT -lt $TIMEOUT ]; do
#     WAIT_COUNT=$((WAIT_COUNT + 1))
#     if [ $((WAIT_COUNT % 5)) -eq 0 ]; then
#         echo "   Still waiting ($WAIT_COUNT/$TIMEOUT)..."
#     fi
#     sleep 1
# done

# if [ ! -b "$ROOT_DEVICE" ]; then
#     echo ""
#     echo "ERROR: Root device not found: $ROOT_DEVICE"
#     echo ""
#     echo "Available block devices:"
#     ls -la /dev/sd* 2>/dev/null || echo "  (none found)"
#     echo ""
#     echo "Dropping to emergency shell for debugging..."
#     echo "Try: mount /dev/sdXY /newroot"
#     echo ""
#     exec /bin/sh -i
# fi

# echo ">> Found root device: $ROOT_DEVICE"

# # ============================================================================
# # MOUNT ROOT FILESYSTEM
# # ============================================================================

# echo ">> Mounting root filesystem..."

# mkdir -p /newroot

# # Try read-write first
# if ! mount "$ROOT_DEVICE" /newroot 2>/dev/null; then
#     echo "   Read-write mount failed, trying read-only..."
#     if ! mount -r "$ROOT_DEVICE" /newroot 2>/dev/null; then
#         echo ""
#         echo "ERROR: Failed to mount $ROOT_DEVICE"
#         echo "Dropping to emergency shell..."
#         exec /bin/sh -i
#     fi
# fi

# echo ">> Root filesystem mounted successfully"

# # ============================================================================
# # VERIFY REAL INIT EXISTS
# # ============================================================================

# echo ">> Verifying real init system..."

# if [ ! -f /newroot/sbin/init ]; then
#     echo ""
#     echo "ERROR: /sbin/init not found in root filesystem"
#     echo ""
#     echo "Root filesystem contents:"
#     ls -la /newroot/ 2>/dev/null | head -20
#     echo ""
#     echo "Dropping to emergency shell..."
#     exec /bin/sh -i
# fi

# if [ ! -x /newroot/sbin/init ]; then
#     echo "ERROR: /sbin/init is not executable"
#     ls -la /newroot/sbin/init
#     exec /bin/sh -i
# fi

# echo ">> Found executable /sbin/init"

# # ============================================================================
# # PIVOT ROOT AND EXECUTE REAL INIT
# # ============================================================================

# echo ">> Pivoting to real root filesystem..."

# # Create oldroot directory if it doesn't exist
# mkdir -p /newroot/oldroot 2>/dev/null || true

# echo ">> Handing control to real init system..."
# echo ""

# # Use chroot to pivot to the real root
# # The kernel's init process (PID 1) will continue as /sbin/init
# exec chroot /newroot /sbin/init

# # If we get here, something went wrong
# echo "ERROR: chroot /sbin/init failed"
# exec /bin/sh -i
# INITSCRIPT

# chmod +x "$BUILD_DIR/init"

# log_success "Init script created"

# ============================================================================
# CREATE ESSENTIAL DIRECTORIES
# ============================================================================

# log_info "Creating filesystem structure..."

# mkdir -p "$BUILD_DIR"/{proc,sys,dev,run,newroot,oldroot}
# mkdir -p "$BUILD_DIR/etc"
# mkdir -p "$BUILD_DIR/sbin"

# log_success "Directories created"

# ============================================================================
# CREATE INITRAMFS ARCHIVE
# ============================================================================

log_info "Creating CPIO archive..."

cd "$INITRAMFS"

log_info "Creating the init.cpio"

if [ ! -f "${OUTPUT_FILE_DIR}" ]; then
    if find . | cpio -H newc -o | gzip > "$OUTPUT_FILE_DIR" 2>/dev/null; then
        CPIO_SIZE=$(du -h "$OUTPUT_FILE_DIR" | cut -f1)
        log_success "CPIO archive created: $OUTPUT_FILE_DIR ($CPIO_SIZE)"
    else
        log_error "Failed to create CPIO archive"
        cd - >/dev/null
        exit 1
    fi
fi

cd - >/dev/null

# cd "$BUILD_DIR"
# find . | cpio -o -H newc 2>/dev/null | gzip -9 > "$OUTPUT_FILE"

# if [ -f "$OUTPUT_FILE" ]; then
#     SIZE=$(du -h "$OUTPUT_FILE" | cut -f1)
#     log_success "Initramfs created: $OUTPUT_FILE ($SIZE)"
    
#     # Show contents
#     echo ""
#     echo "Contents of initramfs:"
#     zcat "$OUTPUT_FILE" | cpio -t 2>/dev/null | head -20
#     echo "... ($(zcat "$OUTPUT_FILE" | cpio -t 2>/dev/null | wc -l) total files)"
# else
#     log_error "Failed to create initramfs"
#     exit 1
# fi

# cp "$OUTPUT_FILE" "$BUILD_DIR/../initramfs"

# cd - > /dev/null

# ============================================================================
# SUMMARY
# ============================================================================

echo ""
echo "========================================="
echo "Initramfs Build Complete!"
echo "========================================="
echo ""
echo "File: $OUTPUT_FILE_DIR"
echo "Size: $(du -h "$OUTPUT_FILE_DIR" | cut -f1)"
echo ""
