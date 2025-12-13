#!/bin/bash
# scripts/create-rootfs.sh - GeckoOS Root Filesystem Builder (REFACTORED)
#
# Comprehensive rootfs creation including:
# - Linux kernel integration
# - Gecko/libxul installation from Mozilla
# - Service startup configuration
# - Boot infrastructure setup
#
# Usage: ./scripts/create-rootfs.sh [--gecko-prebuilt] [--kernel-prebuilt]


set -euo pipefail


PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
KERNEL_BUILD_DIR="${BUILD_DIR}/kernel"
GECKO_BUILD_DIR="${BUILD_DIR}/gecko"
KERNEL_PREBUILT_DIR="${PROJECT_ROOT}/target/boot/vmlinuz-geckoOS"
BOOT_DIR="${PROJECT_ROOT}/target/boot"


# Options
GECKO_PREBUILT=${GECKO_PREBUILT:-0}
KERNEL_PREBUILT=${KERNEL_PREBUILT:-0}


# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --gecko-prebuilt) GECKO_PREBUILT=1; shift ;;
        --kernel-prebuilt) KERNEL_PREBUILT=1; shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done


# Logging functions
log_info() {
    echo "[INFO] $*"
}


log_success() {
    echo "[✓] $*"
}


log_error() {
    echo "[ERROR] $*" >&2
}


log_section() {
    echo ""
    echo "════════════════════════════════════════════"
    echo "  $*"
    echo "════════════════════════════════════════════"
}

# ============================================================================
# STAGE 1: Initramfs Integration
# ============================================================================
stage_init_cpio() {
    
    if [ ! -f "${BOOT_DIR}/init.cpio.gz" ]; then
        log_section "Stage 1: Creating init.cpio Archive"
        mkdir -p "$BOOT_DIR"
        
        log_info "Creating CPIO archive from rootfs..."
        log_info "  Current directory: $ROOTFS_DIR"
        
        cd "$ROOTFS_DIR/target/initramfs"
        
        # Create CPIO archive using same method as minilinux2
        OUTPUT_CPIO="$ROOTFS_DIR/target/init.cpio"
        
        # Step 1: Create the archive
        if find . | cpio -o -H newc --owner=+0:+0 > "$OUTPUT_CPIO" 2>/dev/null; then
            CPIO_SIZE=$(du -h "$OUTPUT_CPIO" | cut -f1)
            log_success "CPIO archive created: $OUTPUT_CPIO ($CPIO_SIZE)"
        else
            log_error "Failed to create CPIO archive"
            cd - >/dev/null
            exit 1
        fi
        cd - >/dev/null
    else
        log_section "Stage 1: Using existing init.cpio Archive"
    fi
    # # Step 2: Verify archive and show contents
    # log_info "Archive contents (first 30 files):"
    
    # # Use a subshell to list files - this won't hang
    # if cpio -t -i < "$OUTPUT_CPIO" 2>/dev/null | head -30; then
    #     # Count total files - use a different method
    #     TOTAL_FILES=$(cpio -t -i < "$OUTPUT_CPIO" 2>/dev/null | wc -l)
    #     log_info "  ... ($TOTAL_FILES total files)"
    # else
    #     log_error "Failed to read CPIO archive"
    #     cd - >/dev/null
    #     exit 1
    # fi
    
    
}


# ============================================================================
# STAGE 2: Kernel Integration
# ============================================================================
stage_kernel() {
    log_section "Stage 2: Linux Kernel Integration"
    
    if [ "$KERNEL_PREBUILT" -eq 1 ]; then
        log_info "Using prebuilt kernel..."
        if [ -f "${KERNEL_PREBUILT_DIR}" ]; then
            log_success "Kernel found: ${KERNEL_PREBUILT_DIR}"
            return
        else
            log_error "Prebuilt kernel not found at ${KERNEL_PREBUILT_DIR}"
            exit 1
        fi
    fi
    
    # log_info "Building Linux kernel..."
    # mkdir -p "${KERNEL_BUILD_DIR}"
    
    # # Use kernel config from project
    # if [ -f "${PROJECT_ROOT}/boot/kernel/config/kernel-config" ]; then
    #     log_info "Using project kernel config"
    #     cp "${PROJECT_ROOT}/boot/kernel/config/kernel-config" "${KERNEL_BUILD_DIR}/.config"
    # else
    #     log_error "Kernel config not found at ${PROJECT_ROOT}/boot/kernel/config/kernel-config"
    #     exit 1
    # fi
    
    # # For production, would run: make -C linux-src O=${KERNEL_BUILD_DIR} bzImage
    # # For now, assume prebuilt or external build
    # if [ ! -f "${KERNEL_BUILD_DIR}/arch/x86/boot/bzImage" ]; then
    #     log_info "⚠ Kernel build skipped (would use: make -C /path/to/linux bzImage)"
    #     log_info "  Ensure kernel is built separately and placed at:"
    #     log_info "  ${KERNEL_BUILD_DIR}/arch/x86/boot/bzImage"
    # fi
    
    log_success "Kernel stage complete"
}


# ============================================================================
# STAGE 3: Gecko/libxul Installation from Mozilla
# ============================================================================
stage_gecko() {
    log_section "Stage 3: Gecko/libxul Installation"
    
    mkdir -p "${GECKO_BUILD_DIR}/lib"
    
    # if [ "$GECKO_PREBUILT" -eq 1 ]; then
    #     log_info "Using prebuilt Gecko/libxul..."
    #     if [ -f "${PROJECT_ROOT}/gecko-prebuilt.tar.gz" ]; then
    #         log_info "Extracting prebuilt Gecko..."
    #         tar -xzf "${PROJECT_ROOT}/gecko-prebuilt.tar.gz" -C "${GECKO_BUILD_DIR}/"
    #         log_success "Gecko extracted"
    #         return
    #     else
    #         log_error "Prebuilt Gecko not found at ${PROJECT_ROOT}/gecko-prebuilt.tar.gz"
    #         log_info "Generate with: ./scripts/gecko-build.sh"
    #         exit 1
    #     fi
    # fi
    
    log_info "Checking for Gecko sources..."
    
    # Option 1: Use local Gecko build
    if [ -f "${GECKO_BUILD_DIR}/firefox/libxul.so" ]; then
        log_info "Using local Gecko sources from ${GECKO_BUILD_DIR}/firefox"
        cp "${GECKO_BUILD_DIR}/firefox"/lib*.so* "${GECKO_BUILD_DIR}/lib/"
        if [ -f "${GECKO_BUILD_DIR}/lib/libxul.so" ]; then
            log_success "libxul extracted: ${GECKO_BUILD_DIR}/lib/libxul.so"
        else
            log_error "libxul.so not found in Firefox archive"
            exit 1
        fi
    else
        # Option 2: Download prebuilt from Mozilla
        log_info "Downloading Gecko/libxul from Mozilla repositories..."
        
        # Firefox ESR latest stable
        GECKO_VERSION="140.5"  # ESR version
        GECKO_URL="https://archive.mozilla.org/pub/firefox/releases/${GECKO_VERSION}.0esr/linux-x86_64/en-US/firefox-${GECKO_VERSION}.0esr.tar.xz"
        
        log_info "Downloading: $GECKO_URL"
        
        if command -v curl &> /dev/null; then
            curl -L -o "${GECKO_BUILD_DIR}/firefox.tar.xz" "$GECKO_URL" || {
                log_error "Failed to download Gecko"
                exit 1
            }
        elif command -v wget &> /dev/null; then
            wget -O "${GECKO_BUILD_DIR}/firefox.tar.bz2" "$GECKO_URL" || {
                log_error "Failed to download Gecko"
                exit 1
            }
        else
            log_error "Neither curl nor wget found. Cannot download Gecko."
            exit 1
        fi
        
        log_info "Extracting Gecko..."
        tar -xf "${GECKO_BUILD_DIR}/firefox.tar.xz" -C "${GECKO_BUILD_DIR}/"
        
        # Extract libxul specifically
        if [ -f "${GECKO_BUILD_DIR}/firefox/libxul.so" ]; then
            cp "${GECKO_BUILD_DIR}/firefox"/lib*.so* "${GECKO_BUILD_DIR}/lib/"
            log_success "libxul extracted: ${GECKO_BUILD_DIR}/lib/libxul.so"
        else
            log_error "libxul.so not found in Firefox archive"
            exit 1
        fi
    fi
    
    # Copy Gecko libraries and dependencies
    log_info "Copying Gecko libraries..."
    if [ -d "${GECKO_BUILD_DIR}/lib" ]; then
        find "${GECKO_BUILD_DIR}/lib" -name "*.so*" -exec cp {} "${GECKO_BUILD_DIR}/lib/" \; 2>/dev/null || true
        log_success "Gecko libraries ready"
    fi
}


# ============================================================================
# STAGE 4: Rootfs Directory Structure
# ============================================================================
stage_rootfs_dirs() {
    log_section "Stage 4: Rootfs Directory Structure"
    
    log_info "Creating directory structure..."
    
    # Create all directories in ROOTFS_DIR
    mkdir -p "${ROOTFS_DIR}"/{boot,dev,mnt,opt,proc,root,run,srv,sys,tmp}
    mkdir -p "${ROOTFS_DIR}"/etc/{init.d,systemd/system,geckoOS,rc.d,ld.so.conf.d}
    mkdir -p "${ROOTFS_DIR}"/home/user
    mkdir -p "${ROOTFS_DIR}"/lib/{modules,gecko}
    mkdir -p "${ROOTFS_DIR}"/usr/{bin,lib,sbin,local/bin,share/{applications,geckoOS}}
    mkdir -p "${ROOTFS_DIR}"/var/{cache,log,run,tmp}
    
    # Create GeckoOS-specific directories
    mkdir -p "${ROOTFS_DIR}/opt/geckoOS/bin"
    mkdir -p "${ROOTFS_DIR}/run/user/1000"
    log_section "Creating /dev nodes"
    
    # fakeroot bash -c "
    #     mkdir -p '$ROOTFS_DIR/dev'
    #     cd '$ROOTFS_DIR/dev'
        
    #     mknod -m 666 null c 1 3
    #     mknod -m 666 zero c 1 5
    #     mknod -m 666 console c 5 1
    #     mknod -m 666 tty c 5 0
        
    #     mknod -m 640 sda b 8 0
    #     mknod -m 640 sda1 b 8 1
    #     mknod -m 640 sda2 b 8 2
    # "
    
    log_success "/dev nodes created"

    
    log_success "Directory structure created"
}

# ============================================================================
# STAGE 4b: Create Essential /dev Nodes
# ============================================================================
stage_dev_nodes() {
    log_section "Stage 4b: Creating Essential /dev Nodes"
    
    log_info "Creating static device nodes for initramfs handoff..."
    
    mkdir -p "${ROOTFS_DIR}/dev"
    cd "${ROOTFS_DIR}/dev"
    
    # Create device nodes without fakeroot (numeric permissions)
    # null, zero, tty, console (character devices)
    mknod -m 666 null c 1 3 2>/dev/null || true
    mknod -m 666 zero c 1 5 2>/dev/null || true
    mknod -m 666 full c 1 7 2>/dev/null || true
    mknod -m 666 random c 1 8 2>/dev/null || true
    mknod -m 666 urandom c 1 9 2>/dev/null || true
    
    # TTY devices (CRITICAL - these are what /dev/tty errors refer to)
    mknod -m 666 tty c 5 0 2>/dev/null || true
    mknod -m 666 console c 5 1 2>/dev/null || true
    
    # PTY devices
    mknod -m 666 ptmx c 5 2 2>/dev/null || true
    
    # Create /dev/pts directory (will be a mount point)
    mkdir -p pts
    chmod 755 pts
    
    # Create common TTY pseudo-devices
    for i in 0 1 2 3 4 5 6 7 8 9; do
        mknod -m 666 "tty$i" c 4 "$i" 2>/dev/null || true
    done
    
    cd - >/dev/null
    
    log_success "/dev nodes created"
}

# ============================================================================
# STAGE 5: System Configuration Files
# ============================================================================
stage_system_config() {
    log_section "Stage 5: System Configuration Files"

    log_info "Putting busybox in the rootfs..."
    cd "$PROJECT_ROOT/target/busybox"
    make install CONFIG_PREFIX="$ROOTFS_DIR"
    cd - >/dev/null
    rm "$ROOTFS_DIR"/linuxrc
    chmod 755 "$ROOTFS_DIR/bin/busybox"
    # sudo chown root:root "$ROOTFS_DIR/bin/busybox"

    # log_info "Making sure correct init is there"

    # rm -rf "${ROOTFS_DIR}/bin/init"
    # rm -rf "${ROOTFS_DIR}/sbin/init"
    # rm -rf "${ROOTFS_DIR}/init"
    # cp "$ROOTFS_DIR/bin/init" "$ROOTFS_DIR/sbin/init"
    # rm -rf "${ROOTFS_DIR}/etc/init"

#     cat > "${ROOTFS_DIR}/sbin/init" << 'INIT_EOF'
# #!/bin/sh

# # Full system init for root filesystem
# # This runs after switch_root from EFI init

# # set -e

# echo "=== Root Init: System startup ===" > /dev/tty0

# mount -t proc proc /proc 2>/dev/null || true
# mount -t sysfs sysfs /sys 2>/dev/null || true
# mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
# ln -sf /proc/self/fd /dev/fd  


# # Run inittab-based init if configured
# if [ -f /etc/inittab ]; then
#     echo "Root Init: Starting inittab-based initialization" > /dev/tty0
#     while true; do
#         /bin/sh -i  # ← If shell exits, loop respawns it
#         echo "Shell exited, respawning..." > /dev/tty0
#     done
# fi

# # Alternative: run rc scripts
# if [ -d /etc/init.d ]; then
#     echo "Root Init: Running /etc/init.d/rcS" > /dev/tty0
#     if [ -x /etc/init.d/rcS ]; then
#         /etc/init.d/rcS
#     fi
# fi

# # Drop to shell if no init strategy worked
# echo "Root Init: No init system found, dropping to shell" > /dev/tty0
# echo "Root Init: You can run 'exit' to halt the system" > /dev/tty0

# while true; do
#     /bin/sh -i  # ← If shell exits, loop respawns it
#     echo "Shell exited, respawning..." > /dev/tty0
# done
# INIT_EOF

#     chmod 755 "$ROOTFS_DIR/sbin/init"
    # cp "$ROOTFS_DIR/bin/init" "$ROOTFS_DIR/sbin/init"
    # cp "$ROOTFS_DIR/bin/init" "$ROOTFS_DIR/etc/init"
    
    log_info "Creating system configuration files..."

    # /etc/hostname
echo "geckoOS" > "$ROOTFS_DIR/etc/hostname"
log_info "  Created: /etc/hostname"

# /etc/hosts
cat > "$ROOTFS_DIR/etc/hosts" <<'EOF'
127.0.0.1   localhost
::1         localhost
127.0.0.1   geckoOS
EOF
log_info "  Created: /etc/hosts"

# /etc/fstab (for mounting filesystems)
cat > "$ROOTFS_DIR/etc/fstab" <<'EOF'
proc           /proc          proc     nosuid,noexec,nodev 0     0
sysfs          /sys           sysfs    nosuid,noexec,nodev 0     0
devpts         /dev/pts       devpts   gid=5,mode=620      0     0
tmpfs          /run           tmpfs    defaults            0     0
devtmpfs       /dev           devtmpfs mode=0755,nosuid    0     0
tmpfs          /dev/shm       tmpfs    nosuid,nodev        0     0
cgroup2        /sys/fs/cgroup cgroup2  nosuid,noexec,nodev 0     0
EOF
log_info "  Created: /etc/fstab"

# /etc/inittab (for init process - minilinux2 style)
# cat > "$ROOTFS_DIR/etc/inittab" <<'EOF'
# # Minimal init table for GeckoOS
# # BusyBox inittab

# # Startup scripts
# ::sysinit:/bin/ln -sf /proc/self/fd /dev/fd 2>/dev/null || true
# ::sysinit:/bin/ln -sf /proc/self/fd/0 /dev/stdin 2>/dev/null || true
# ::sysinit:/bin/ln -sf /proc/self/fd/1 /dev/stdout 2>/dev/null || true
# ::sysinit:/bin/ln -sf /proc/self/fd/2 /dev/stderr 2>/dev/null || true
# ::sysinit:/etc/init.d/rcS

# # Shell access
# ::askfirst:-/bin/sh

# # TTY shells (respawn means they restart when exited)
# tty2::respawn:-/bin/sh
# tty3::respawn:-/bin/sh
# tty4::respawn:-/bin/sh

# # Ctrl-Alt-Del
# ::ctrlaltdel:/sbin/reboot

# # Shutdown
# ::shutdown:/bin/umount -a -r
# ::shutdown:/sbin/swapoff -a
# EOF
# log_info "  Created: /etc/inittab"

# /etc/passwd (minimal)
cat > "$ROOTFS_DIR/etc/passwd" <<'EOF'
root:x:0:0:root:/root:/bin/sh
EOF
log_info "  Created: /etc/passwd"

# /etc/group (minimal)
cat > "$ROOTFS_DIR/etc/group" <<'EOF'
root:x:0:
EOF
log_info "  Created: /etc/group"

# /etc/shadow (minimal)
cat > "$ROOTFS_DIR/etc/shadow" <<'EOF'
root:PASSWORD HASH:20005::::::
EOF
log_info "  Created: /etc/shadow"
# chmod 600 "$ROOTFS/etc/shadow"
cat > "$ROOTFS_DIR/etc/environment" <<'ENV'
PATH="/bin"
ENV
# cat > "$ROOTFS_DIR/home/root/.profile" <<'PFP'
# PS1='[\[\e[32m\]\u@\h \W\[\e[0m\]]\$ '

# alias ls="ls --color=auto"
# alias ll="ls -l"
# PFP
    
    # /etc/os-release
    cat > "${ROOTFS_DIR}/etc/os-release" << 'OSREL_EOF'
NAME="GeckoOS"
VERSION="0.1.0"
ID=geckoos
ID_LIKE="linux"
PRETTY_NAME="GeckoOS 0.1.0"
VERSION_CODENAME="gecko"
HOME_URL="https://github.com/gecko-os/geckoos"
SUPPORT_URL="https://github.com/gecko-os/geckoos/issues"
BUG_REPORT_URL="https://github.com/gecko-os/geckoos/issues"
LOGO="geckoos"
OSREL_EOF
    
    # /etc/environment - Environment variables
    cat > "${ROOTFS_DIR}/etc/environment" << 'ENV_EOF'
PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
SHELL="/bin/sh"
LANG="en_US.UTF-8"
WAYLAND_DISPLAY="wayland-0"
XDG_RUNTIME_DIR="/run/user/1000"
XDG_CONFIG_HOME="/etc/geckoOS"
ENV_EOF
    
    # /etc/geckoOS/config.toml - GeckoOS configuration
    mkdir -p "${ROOTFS_DIR}/etc/geckoOS"
    cat > "${ROOTFS_DIR}/etc/geckoOS/config.toml" << 'CONFIG_EOF'
# GeckoOS System Configuration


[system]
hostname = "geckoOS"
locale = "en_US.UTF-8"
timezone = "UTC"


[display]
default_width = 1920
default_height = 1080
refresh_rate = 60


[services]
# Enable/disable system services
echo_bus = true
ouranos_compositor = true
cronus_process_manager = true
gecko_rendering = true
kratos_performance = true
persephone_compat = true
gaia_shell = true


[gecko]
# Gecko rendering engine configuration
libxul_path = "/usr/lib/gecko/libxul.so"
profile_dir = "/var/gecko"
enable_gpu = true
cache_size_mb = 256


[performance]
# Performance tuning
profile = "balanced"  # balanced, performance, powersave
max_fps = 60
enable_vsync = true


CONFIG_EOF

log_info "Creating profile..."
    cat > "$ROOTFS_DIR/etc/profile" <<'EOF'
export PATH="/bin:/sbin:/usr/bin:/usr/sbin"
export HOME="/root"
export SHELL="/bin/sh"
PS1="[\033[32m\u@\h\033[0m:\w]\$ "
EOF
    
    log_info "Creating motd..."
    cat > "$ROOTFS_DIR/etc/motd" <<'EOF'
Welcome to GeckoOS 0.1.0
Built with Gecko rendering engine
EOF
    
    log_success "System configuration files created"
}


# ============================================================================
# STAGE 6: Boot Configuration (GRUB)
# ============================================================================
stage_boot_config() {
    log_section "Stage 6: Boot Configuration"
    
    log_info "Setting up boot infrastructure..."
    
    # Copy GRUB configuration
    if [ -f "${PROJECT_ROOT}/boot/grub/grub.cfg" ]; then
        mkdir -p "${ROOTFS_DIR}/boot/grub"
        cp "${PROJECT_ROOT}/boot/grub/grub.cfg" "${ROOTFS_DIR}/boot/grub/"
        log_success "GRUB configuration copied"
    fi
    
    # Create boot directory
    mkdir -p "${ROOTFS_DIR}/boot"
    
    # Kernel will be installed here
    log_info "Boot directory prepared at ${ROOTFS_DIR}/boot/"
    
    log_success "Boot configuration complete"
}





# ============================================================================
# STAGE 7: System Services Setup
# ============================================================================
stage_services() {
    log_section "Stage 7: System Services Setup"
    
    log_info "Creating init system using BusyBox inittab..."
    
    mkdir -p "${ROOTFS_DIR}/etc/init.d"
    
    # Main rcS - runs at startup (FIXED - mounts first!)
        # Main rcS - runs at startup (FIXED - Remount RW FIRST)
    cat > "${ROOTFS_DIR}/etc/init.d/rcS" << 'RCS_EOF'
#!/bin/sh
# /etc/init.d/rcS - Startup script

echo "=== rcS: Startup sequence beginning ==="

# 1. CRITICAL: Mount /proc FIRST so 'mount' command works properly
# busybox mount often needs /proc/mounts to perform remounts
mount -t proc proc /proc -n 2>/dev/null
if [ $? -ne 0 ]; then
    echo "rcS: Warning - Failed to mount /proc (or already mounted)"
fi

# 2. CRITICAL: Remount root filesystem as Read-Write
# Now that /proc is there, this should succeed
echo "rcS: Remounting root filesystem Read-Write..."
mount -o remount,rw / 
if [ $? -ne 0 ]; then
    echo "rcS: FATAL - Failed to remount root RW. Expect errors."
    # Emergency fallback: try mounting /dev anyway
fi

# 3. Mount /sys (needed for device management)
mount -t sysfs sysfs /sys -n 2>/dev/null

# 3. Handle /dev
# If devtmpfs isn't mounted by kernel/initramfs, mount it now
if ! mountpoint -q /dev; then
    mount -t devtmpfs devtmpfs /dev -n 2>/dev/null || echo "  Note: devtmpfs failed or already mounted"
fi

# 4. Create critical device nodes if devtmpfs failed or is missing
# (This acts as a fallback if devtmpfs didn't populate)
if [ ! -c /dev/null ]; then
    mknod -m 666 /dev/null c 1 3
fi
if [ ! -c /dev/tty ]; then
    mknod -m 666 /dev/tty c 5 0
fi
if [ ! -c /dev/console ]; then
    mknod -m 600 /dev/console c 5 1
fi

# 5. Setup PTYs (Crucial for shells)
mkdir -p /dev/pts
mount -t devpts devpts /dev/pts -o gid=5,mode=620 -n 2>/dev/null || echo "  Note: devpts mount failed"

# 6. Setup symlinks for standard I/O
ln -sf /proc/self/fd /dev/fd 2>/dev/null || true
ln -sf /proc/self/fd/0 /dev/stdin 2>/dev/null || true
ln -sf /proc/self/fd/1 /dev/stdout 2>/dev/null || true
ln -sf /proc/self/fd/2 /dev/stderr 2>/dev/null || true

# 7. Mount /run and /tmp
mount -t tmpfs tmpfs /run -o size=128M -n 2>/dev/null || true
mount -t tmpfs tmpfs /tmp -o size=512M -n 2>/dev/null || true

# 8. Create structure
mkdir -p /run/lock /var/log /var/tmp /run/user/1000
chmod 1777 /tmp /var/tmp
chmod 755 /var/log /run /run/lock

echo "rcS: Startup complete."
RCS_EOF

chmod 755 "${ROOTFS_DIR}/etc/init.d/rcS"

    # S10echo - Start ECHO IPC bus
    cat > "${ROOTFS_DIR}/etc/init.d/S10echo" << 'ECHO_EOF'
#!/bin/sh
echo "S10echo: Starting ECHO IPC bus..."

if [ -x /usr/bin/echo-daemon ]; then
    /usr/bin/echo-daemon > /dev/null 2>&1 &
    sleep 1
    echo "[✓] ECHO bus started"
else
    echo "[!] echo-daemon not found (optional)"
fi
ECHO_EOF
    chmod 755 "${ROOTFS_DIR}/etc/init.d/S10echo"
    
    # S20cronus - Start process manager
    cat > "${ROOTFS_DIR}/etc/init.d/S20cronus" << 'CRONUS_EOF'
#!/bin/sh
echo "S20cronus: Starting process manager..."

if [ -x /usr/bin/cronus ]; then
    /usr/bin/cronus > /dev/null 2>&1 &
    echo "[✓] CRONUS started"
else
    echo "[!] CRONUS not found (optional)"
fi
CRONUS_EOF
    chmod 755 "${ROOTFS_DIR}/etc/init.d/S20cronus"
    
    # S30ouranos - Start Wayland compositor
    cat > "${ROOTFS_DIR}/etc/init.d/S30ouranos" << 'OURANOS_EOF'
#!/bin/sh
echo "S30ouranos: Starting Wayland compositor..."

export WAYLAND_DISPLAY=wayland-0
export XDG_RUNTIME_DIR=/run/user/1000
mkdir -p $XDG_RUNTIME_DIR 2>/dev/null || true
chmod 700 $XDG_RUNTIME_DIR 2>/dev/null || true

if [ -x /usr/bin/ouranos ]; then
    /usr/bin/ouranos > /dev/null 2>&1 &
    sleep 2
    echo "[✓] OURANOS started"
else
    echo "[!] OURANOS not found (optional)"
fi
OURANOS_EOF
    chmod 755 "${ROOTFS_DIR}/etc/init.d/S30ouranos"
    
    # S40gecko - Start Gecko rendering engine
    cat > "${ROOTFS_DIR}/etc/init.d/S40gecko" << 'GECKO_EOF'
#!/bin/sh
echo "S40gecko: Starting Gecko rendering engine..."

export WAYLAND_DISPLAY=wayland-0
export LD_LIBRARY_PATH="/usr/lib/gecko:$LD_LIBRARY_PATH"
export XDG_RUNTIME_DIR=/run/user/1000

if [ -x /usr/bin/gecko ]; then
    /usr/bin/gecko > /dev/null 2>&1 &
    sleep 2
    echo "[✓] GECKO started"
else
    echo "[!] GECKO not found (optional)"
fi
GECKO_EOF
    chmod 755 "${ROOTFS_DIR}/etc/init.d/S40gecko"
    
    # S50kratos - Start performance monitor
    cat > "${ROOTFS_DIR}/etc/init.d/S50kratos" << 'KRATOS_EOF'
#!/bin/sh
echo "S50kratos: Starting performance monitor..."

if [ -x /usr/bin/kratos ]; then
    /usr/bin/kratos > /dev/null 2>&1 &
    echo "[✓] KRATOS started"
else
    echo "[!] KRATOS not found (optional)"
fi
KRATOS_EOF
    chmod 755 "${ROOTFS_DIR}/etc/init.d/S50kratos"
    
    # S60persephone - Start compatibility layer
    cat > "${ROOTFS_DIR}/etc/init.d/S60persephone" << 'PERSEPHONE_EOF'
#!/bin/sh
echo "S60persephone: Starting compatibility layer..."

if [ -x /usr/bin/persephone ]; then
    /usr/bin/persephone > /dev/null 2>&1 &
    echo "[✓] PERSEPHONE started"
else
    echo "[!] PERSEPHONE not found (optional)"
fi
PERSEPHONE_EOF
    chmod 755 "${ROOTFS_DIR}/etc/init.d/S60persephone"
    
    # S70gaia - Start GAIA desktop shell
    cat > "${ROOTFS_DIR}/etc/init.d/S70gaia" << 'GAIA_EOF'
#!/bin/sh
echo "S70gaia: Starting GAIA desktop shell..."

export WAYLAND_DISPLAY=wayland-0
export LD_LIBRARY_PATH="/usr/lib/gecko:$LD_LIBRARY_PATH"
export XDG_RUNTIME_DIR=/run/user/1000

if [ -x /usr/bin/gaia ]; then
    /usr/bin/gaia > /dev/null 2>&1 &
    echo "[✓] GAIA started"
else
    echo "[!] GAIA not found (optional)"
fi
GAIA_EOF
    chmod 755 "${ROOTFS_DIR}/etc/init.d/S70gaia"

    log_success "Service scripts created"
}

# ============================================================================
# BUSYBOX INITTAB - Use this instead of individual S* scripts
# ============================================================================
# Call this INSTEAD of stage_services if you prefer pure inittab

stage_busybox_inittab() {
    log_section "Stage 7: BusyBox Inittab Configuration"
    
    log_info "Creating BusyBox inittab (unified init system)..."
    
    mkdir -p "${ROOTFS_DIR}/etc/init.d"
    
    # Create unified rcS that handles everything
    cat > "${ROOTFS_DIR}/etc/init.d/rcS" << 'RCS_EOF'
#!/bin/sh
echo "=== System startup ==="

# Mount essential filesystems FIRST
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

# Setup /dev symlinks
ln -sf /proc/self/fd /dev/fd 2>/dev/null || true
ln -sf /proc/self/fd/0 /dev/stdin 2>/dev/null || true
ln -sf /proc/self/fd/1 /dev/stdout 2>/dev/null || true
ln -sf /proc/self/fd/2 /dev/stderr 2>/dev/null || true

# Setup /dev/pts BEFORE any TTY usage
mkdir -p /dev/pts
mount -t devpts devpts /dev/pts -o gid=5,mode=620 2>/dev/null || true

# Create critical device nodes if missing
[ -c /dev/tty ] || mknod /dev/tty c 5 0 2>/dev/null || true
[ -c /dev/console ] || mknod /dev/console c 5 1 2>/dev/null || true

# Mount /run and /tmp
mount -t tmpfs tmpfs /run -o size=512M 2>/dev/null || true
mount -t tmpfs tmpfs /tmp -o size=1G 2>/dev/null || true

# Create runtime directories
mkdir -p /run/lock /var/log /run/user/1000 2>/dev/null || true
chmod 755 /var/log /run /run/lock 2>/dev/null || true

# Remount root rw
mount -o remount,rw / 2>/dev/null || true

echo "System startup complete"
RCS_EOF
chmod 755 "${ROOTFS_DIR}/etc/init.d/rcS"

    # Simple BusyBox inittab
    cat > "${ROOTFS_DIR}/etc/inittab" << 'INITTAB_EOF'
# BusyBox inittab for GeckoOS

# First run rcS (system initialization)
::sysinit:/etc/init.d/rcS

# Run optional S* startup scripts
::sysinit:/bin/sh -c 'for f in /etc/init.d/S*; do [ -x "$f" ] && "$f"; done'

# Provide an interactive shell on the console
::askfirst:/bin/sh -i

# Respawn getty on tty1
tty1::respawn:/sbin/getty 115200 tty1

# Handle Ctrl-Alt-Del
::ctrlaltdel:/sbin/reboot

# Shutdown sequence
::shutdown:/bin/umount -a -r
::shutdown:/sbin/swapoff -a
INITTAB_EOF

    log_success "BusyBox inittab created"
}


# ============================================================================
# STAGE 8: User Accounts and System Initialization (NEW - CORRECT LOCATION)
# ============================================================================
stage_users_and_init() {
    log_section "Stage 8: User Accounts and System Initialization"
    
    log_info "Setting up user accounts in rootfs..."
    
    # Create geckouser account in rootfs
    # Use chroot or directly modify /etc/passwd and /etc/shadow
    
    # Manually create user in rootfs without running system commands
    # UID 1000, GID 1000, home directory /home/geckouser
    
    # if ! grep -q "^geckouser:" "${ROOTFS_DIR}/etc/passwd"; then
    #     echo "geckouser:x:1000:1000:GeckoOS User:/home/geckouser:/bin/bash" >> "${ROOTFS_DIR}/etc/passwd"
    #     log_info "Added geckouser to passwd"
    # fi
    
    # if ! grep -q "^geckouser:" "${ROOTFS_DIR}/etc/shadow"; then
    #     # Locked account (no password)
    #     echo "geckouser:!:18000:0:99999:7:::" >> "${ROOTFS_DIR}/etc/shadow"
    #     log_info "Added geckouser to shadow"
    # fi
    
    if ! grep -q "^geckouser:" "${ROOTFS_DIR}/etc/group"; then
        echo "geckouser:x:1000:" >> "${ROOTFS_DIR}/etc/group"
        log_info "Added geckouser to group"
    fi
    
    # Create home directory for geckouser
    mkdir -p "${ROOTFS_DIR}/home/geckouser"
    chmod 700 "${ROOTFS_DIR}/home/geckouser"
    
    # Create .bashrc for geckouser
    cat > "${ROOTFS_DIR}/home/geckouser/.bashrc" << 'BASHRC_EOF'
# GeckoOS User Bashrc
export PATH="/usr/local/bin:/usr/bin:/bin"
export HOME="/home/geckouser"
export WAYLAND_DISPLAY="wayland-0"
export XDG_RUNTIME_DIR="/run/user/1000"

alias ls='ls --color=auto'
alias ll='ls -lah'
PS1='geckouser@geckoOS:\w\$ '
BASHRC_EOF
    
    chmod 644 "${ROOTFS_DIR}/home/geckouser/.bashrc"
    
    log_info "Setting directory ownership..."
    
    # Set proper ownership for geckouser directories (as numeric UID:GID)
    # Note: We use numeric values since the user might not exist on host system
    chmod 755 "${ROOTFS_DIR}/home/geckouser"
    
    # Create runtime directory with correct permissions
    log_info "Creating XDG runtime directory..."
    mkdir -p "${ROOTFS_DIR}/run/user/1000"
    chmod 700 "${ROOTFS_DIR}/run/user/1000"
    
    # Note: Ownership will be set when rootfs is booted on target system
    # On host, we can't chown to non-existent user
    
    # Create getty service for login prompt
    log_info "Creating getty service..."
    cat > "${ROOTFS_DIR}/etc/systemd/system/getty@tty1.service" << 'GETTY_EOF'
[Unit]
Description=Virtual Terminal on tty1
After=systemd-logind.service
Wants=systemd-logind.service

[Service]
Type=simple
ExecStart=/sbin/agetty --noclear --keep-baud 115200,38400,9600 tty1 linux
Restart=always
RestartSec=0
StandardInput=tty
StandardOutput=tty
StandardError=tty
TTYReset=yes
TTYVHangup=yes

[Install]
WantedBy=multi-user.target
GETTY_EOF

    chmod 755 "${ROOTFS_DIR}/etc/systemd/system/getty@tty1.service"
    
    log_success "User accounts and init setup complete"
}


# ============================================================================
# STAGE 9: Copy Compiled Binaries (FROM target/release)
# ============================================================================
stage_binaries() {
    log_section "Stage 9: Copying Compiled Binaries"
    
    log_info "Copying GeckoOS component binaries from target/release..."
    
    # Check if target/release directory exists
    if [ ! -d "${PROJECT_ROOT}/target/release" ]; then
        log_error "target/release directory not found"
        log_info "Build with: cargo build --release"
        return 1
    fi
    
    # List of binaries to copy (without the -daemon suffix if using proper binary names)
    local binaries=(
        "echo-daemon"
        "echo-cli"
        "cronus"
        "cronus-daemon"
        "geckoOS-ouranos"
        "gecko-shell"
        "gpu-accelerator"
        "persephone-daemon"
        "persephone-compat"
        "performance-monitor"
        "performance-tuner"
    )
    
    for entry in "${binaries[@]}"; do
        IFS=: read -r binary_name install_name <<< "$entry"
        local binary_path="${PROJECT_ROOT}/target/release/${binary_name}"
        
        if [ -f "$binary_path" ]; then
            cp "$binary_path" "${ROOTFS_DIR}/usr/bin/${install_name}"
            chmod 755 "${ROOTFS_DIR}/usr/bin/${install_name}"
            log_success "Copied ${binary_name} → /usr/bin/${install_name}"
        else
            log_info "⚠ ${binary_name} not found at ${binary_path}"
            log_info "  Build with: cargo build --release"
        fi
    done
    
    log_success "Binaries stage complete"
}


# ============================================================================
# STAGE 10: Copy Gecko Libraries
# ============================================================================
stage_gecko_libs() {
    log_section "Stage 10: Gecko Runtime Libraries"
    
    log_info "Installing Gecko/libxul runtime libraries..."
    
    mkdir -p "${ROOTFS_DIR}/usr/lib/gecko"
    
    if [ -d "${GECKO_BUILD_DIR}/lib" ]; then
        cp "${GECKO_BUILD_DIR}/lib"/*.so* "${ROOTFS_DIR}/usr/lib/gecko/" 2>/dev/null || true
        log_success "Gecko libraries installed at ${ROOTFS_DIR}/usr/lib/gecko/"
    else
        log_info "⚠ Gecko libraries directory not found"
    fi
    
    # Set library path
    echo "/usr/lib/gecko" > "${ROOTFS_DIR}/etc/ld.so.conf.d/gecko.conf"
}


# ============================================================================
# STAGE 11: Copy Userspace Applications
# ============================================================================
stage_userspace_apps() {
    log_section "Stage 11: Userspace Applications"
    
    log_info "Copying userspace applications..."
    
    mkdir -p "${ROOTFS_DIR}/usr/share/geckoOS/static"
    
    # GAIA frontend (HTML/CSS/JS)
    if [ -d "${PROJECT_ROOT}/gaia/static" ]; then
        cp -r "${PROJECT_ROOT}/gaia/static"/* "${ROOTFS_DIR}/usr/share/geckoOS/static/" 2>/dev/null || true
        log_success "GAIA frontend copied"
    fi
    
    # Additional applications directory
    mkdir -p "${ROOTFS_DIR}/usr/share/geckoOS/apps"
    log_info "Application directory ready at ${ROOTFS_DIR}/usr/share/geckoOS/apps/"
}


# ============================================================================
# STAGE 12: Set Permissions
# ============================================================================
stage_permissions() {
    log_section "Stage 12: Setting Permissions"
    
    log_info "Setting filesystem permissions..."
    
    chmod 755 "${ROOTFS_DIR}"/usr/{bin,sbin,lib} 2>/dev/null || true
    chmod 755 "${ROOTFS_DIR}"/etc/init.d/* 2>/dev/null || true
    chmod 1777 "${ROOTFS_DIR}/tmp"
    chmod 1777 "${ROOTFS_DIR}/var/tmp"
    chmod 755 "${ROOTFS_DIR}/var/log"
    chmod 755 "${ROOTFS_DIR}/var/run"
    # chmod 755 "$ROOTFS_DIR/bin/init"
    # chmod 755 "$ROOTFS_DIR/bin/busybox"
    
    # Set getty permissions
    chmod 755 "${ROOTFS_DIR}/etc/systemd/system/getty@tty1.service"

    find "$ROOTFS_DIR/bin" -type f -exec chmod 755 {} \;
    find "$ROOTFS_DIR/sbin" -type f -exec chmod 755 {} \;
    find "$ROOTFS_DIR/etc/init.d" -type f -exec chmod 755 {} \;
    find "$ROOTFS_DIR/etc" -type f -exec chmod 755 {} \;
    find "$ROOTFS_DIR/etc/systemd/system" -type f -exec chmod 755 {} \;
    
    log_success "Permissions set"
}


# ============================================================================
# MAIN EXECUTION
# ============================================================================
main() {
    log_section "GeckoOS ROOT FILESYSTEM BUILDER"
    log_info "Project: $PROJECT_ROOT"
    log_info "Rootfs: $ROOTFS_DIR"
    log_info ""
    
    # Execute all stages
    stage_init_cpio
    stage_kernel
    stage_gecko
    stage_rootfs_dirs
    stage_dev_nodes
    stage_system_config
    stage_boot_config
    # Choose ONE init system approach:
    stage_services               # SysVinit approach
    # stage_busybox_inittab      # Alternative: pure BusyBox inittab
    stage_users_and_init
    stage_binaries
    stage_gecko_libs
    stage_userspace_apps
    stage_permissions
    
    log_section "ROOTFS CREATION COMPLETE"
    log_success "Rootfs ready at: ${ROOTFS_DIR}"
    log_info ""
    log_info "Next steps:"
    log_info ""
    log_info "  Create disk image:"
    log_info "    bash ${PROJECT_ROOT}/scripts/package.sh"
    log_info ""
}


main "$@"