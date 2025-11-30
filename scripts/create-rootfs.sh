#!/bin/bash
# scripts/create-rootfs.sh - GeckoOS Root Filesystem Builder
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
# STAGE 1: Kernel Integration
# ============================================================================
stage_kernel() {
    log_section "Stage 1: Linux Kernel Integration"
    
    if [ "$KERNEL_PREBUILT" -eq 1 ]; then
        log_info "Using prebuilt kernel..."
        if [ -f "${BUILD_DIR}/kernel/bzImage" ]; then
            log_success "Kernel found: ${BUILD_DIR}/kernel/bzImage"
            return
        else
            log_error "Prebuilt kernel not found at ${BUILD_DIR}/kernel/bzImage"
            exit 1
        fi
    fi
    
    log_info "Building Linux kernel..."
    mkdir -p "${KERNEL_BUILD_DIR}"
    
    # Use kernel config from project
    if [ -f "${PROJECT_ROOT}/boot/kernel/config/kernel-config" ]; then
        log_info "Using project kernel config"
        cp "${PROJECT_ROOT}/boot/kernel/config/kernel-config" "${KERNEL_BUILD_DIR}/.config"
    else
        log_error "Kernel config not found at ${PROJECT_ROOT}/boot/kernel/config/kernel-config"
        exit 1
    fi
    
    # For production, would run: make -C linux-src O=${KERNEL_BUILD_DIR} bzImage
    # For now, assume prebuilt or external build
    if [ ! -f "${KERNEL_BUILD_DIR}/arch/x86/boot/bzImage" ]; then
        log_info "⚠ Kernel build skipped (would use: make -C /path/to/linux bzImage)"
        log_info "  Ensure kernel is built separately and placed at:"
        log_info "  ${KERNEL_BUILD_DIR}/arch/x86/boot/bzImage"
    fi
    
    log_success "Kernel stage complete"
}

# ============================================================================
# STAGE 2: Gecko/libxul Installation from Mozilla
# ============================================================================
stage_gecko() {
    log_section "Stage 2: Gecko/libxul Installation"
    
    mkdir -p "${GECKO_BUILD_DIR}/lib"
    
    if [ "$GECKO_PREBUILT" -eq 1 ]; then
        log_info "Using prebuilt Gecko/libxul..."
        if [ -f "${PROJECT_ROOT}/gecko-prebuilt.tar.gz" ]; then
            log_info "Extracting prebuilt Gecko..."
            tar -xzf "${PROJECT_ROOT}/gecko-prebuilt.tar.gz" -C "${GECKO_BUILD_DIR}/"
            log_success "Gecko extracted"
            return
        else
            log_error "Prebuilt Gecko not found at ${PROJECT_ROOT}/gecko-prebuilt.tar.gz"
            log_info "Generate with: ./scripts/gecko-build.sh"
            exit 1
        fi
    fi
    
    log_info "Checking for Gecko sources..."
    
    # Option 1: Use local Gecko build
    if [ -d "${PROJECT_ROOT}/gecko" ]; then
        log_info "Using local Gecko sources from ${PROJECT_ROOT}/gecko"
        log_info "Running: bash ${PROJECT_ROOT}/scripts/gecko-build.sh"
        bash "${PROJECT_ROOT}/scripts/gecko-build.sh" || {
            log_error "Gecko build failed"
            exit 1
        }
    else
        # Option 2: Download prebuilt from Mozilla
        log_info "Downloading Gecko/libxul from Mozilla repositories..."
        
        # Firefox ESR latest stable
        GECKO_VERSION="140.5"  # ESR version
        GECKO_URL="https://archive.mozilla.org/pub/firefox/releases/${GECKO_VERSION}.0esr/linux-x86_64/en-US/firefox-${GECKO_VERSION}.0esr.tar.xz"
        
        log_info "Downloading: $GECKO_URL"
        
        if command -v curl &> /dev/null; then
            curl -L -o "${GECKO_BUILD_DIR}/firefox.tar.bz2" "$GECKO_URL" || {
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
        tar -xjf "${GECKO_BUILD_DIR}/firefox.tar.bz2" -C "${GECKO_BUILD_DIR}/"
        
        # Extract libxul specifically
        if [ -f "${GECKO_BUILD_DIR}/firefox/libxul.so" ]; then
            cp "${GECKO_BUILD_DIR}/firefox/libxul.so" "${GECKO_BUILD_DIR}/lib/"
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
# STAGE 3: Rootfs Directory Structure
# ============================================================================
stage_rootfs_dirs() {
    log_section "Stage 3: Rootfs Directory Structure"
    
    log_info "Creating directory structure..."
    mkdir -p "${ROOTFS_DIR}"/{
        boot,
        dev,
        etc/{init.d,systemd/system,geckoOS},
        home/user,
        lib/{modules,gecko},
        mnt,
        opt,
        proc,
        root,
        run,
        srv,
        sys,
        tmp,
        usr/{bin,lib,sbin,local/bin,share/{applications,geckoOS}},
        var/{cache,log,run,tmp}
    }
    
    log_success "Directory structure created"
}

# ============================================================================
# STAGE 4: System Configuration Files
# ============================================================================
stage_system_config() {
    log_section "Stage 4: System Configuration Files"
    
    log_info "Creating system configuration files..."
    
    # /etc/fstab - Filesystem mount table
    cat > "${ROOTFS_DIR}/etc/fstab" << 'FSTAB_EOF'
# GeckoOS filesystem table
proc        /proc       proc      defaults           0 0
sysfs       /sys        sysfs     defaults           0 0
devtmpfs    /dev        devtmpfs  defaults           0 0
tmpfs       /run        tmpfs     defaults           0 0
tmpfs       /tmp        tmpfs     defaults,size=1G   0 0
FSTAB_EOF
    
    # /etc/hostname
    echo "geckoOS" > "${ROOTFS_DIR}/etc/hostname"
    
    # /etc/hosts
    cat > "${ROOTFS_DIR}/etc/hosts" << 'HOSTS_EOF'
127.0.0.1   localhost
::1         localhost
127.0.0.1   geckoOS
HOSTS_EOF
    
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
XDG_RUNTIME_DIR="/run/user/0"
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
    
    log_success "System configuration files created"
}

# ============================================================================
# STAGE 5: Boot Configuration (GRUB)
# ============================================================================
stage_boot_config() {
    log_section "Stage 5: Boot Configuration"
    
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
# STAGE 6: Initramfs Integration
# ============================================================================
stage_initramfs() {
    log_section "Stage 6: Initramfs Integration"
    
    if [ -f "${PROJECT_ROOT}/boot/initramfs/build-initramfs.sh" ]; then
        log_info "Building initramfs..."
        bash "${PROJECT_ROOT}/boot/initramfs/build-initramfs.sh" || {
            log_error "Initramfs build failed"
            exit 1
        }
        
        # Initramfs should be placed at boot/initramfs.img
        if [ -f "${BUILD_DIR}/initramfs.img" ]; then
            cp "${BUILD_DIR}/initramfs.img" "${ROOTFS_DIR}/boot/"
            log_success "Initramfs copied to ${ROOTFS_DIR}/boot/initramfs.img"
        fi
    else
        log_info "⚠ Initramfs build script not found"
        log_info "  Expected: ${PROJECT_ROOT}/boot/initramfs/build-initramfs.sh"
    fi
}

# ============================================================================
# STAGE 7: System Services Setup
# ============================================================================
stage_services() {
    log_section "Stage 7: System Services Setup"
    
    log_info "Setting up systemd services..."
    
    # Create systemd service directory
    mkdir -p "${ROOTFS_DIR}/etc/systemd/system"
    
    # Copy service files from project
    if [ -d "${PROJECT_ROOT}/boot/startup/systemd/services" ]; then
        log_info "Copying systemd service units..."
        cp "${PROJECT_ROOT}/boot/startup/systemd/services"/*.service \
            "${ROOTFS_DIR}/etc/systemd/system/" 2>/dev/null || true
        log_success "Service units copied"
    fi
    
    # Create init scripts for systems without systemd
    log_info "Creating sysvinit scripts..."
    mkdir -p "${ROOTFS_DIR}/etc/init.d"
    
    # S01mount - Mount essential filesystems
    cat > "${ROOTFS_DIR}/etc/init.d/S01mount" << 'MOUNT_EOF'
#!/bin/sh
# Mount essential filesystems

mkdir -p /dev /sys /proc /run /tmp
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t proc proc /proc 2>/dev/null || true
mount -t tmpfs tmpfs /tmp -o size=1G 2>/dev/null || true
mount -t tmpfs tmpfs /run 2>/dev/null || true

echo "[✓] Essential filesystems mounted"
MOUNT_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S01mount"
    
    # S02udev - Initialize device manager
    cat > "${ROOTFS_DIR}/etc/init.d/S02udev" << 'UDEV_EOF'
#!/bin/sh
# Initialize udev for device management

if [ -x /sbin/udev ]; then
    /sbin/udev start 2>/dev/null || true
fi

echo "[✓] Device manager initialized"
UDEV_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S02udev"
    
    # S10echo - Start ECHO IPC bus
    cat > "${ROOTFS_DIR}/etc/init.d/S10echo" << 'ECHO_EOF'
#!/bin/sh
# Start ECHO IPC bus (required for all other services)

if [ -x /usr/bin/echo_daemon ]; then
    /usr/bin/echo_daemon &
    sleep 1
    echo "[✓] ECHO bus started"
else
    echo "[!] ECHO daemon not found at /usr/bin/echo_daemon"
fi
ECHO_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S10echo"
    
    # S20cronus - Start process manager
    cat > "${ROOTFS_DIR}/etc/init.d/S20cronus" << 'CRONUS_EOF'
#!/bin/sh
# Start CRONUS process manager

if [ -x /usr/bin/cronus_daemon ]; then
    /usr/bin/cronus_daemon &
    echo "[✓] CRONUS started"
else
    echo "[!] CRONUS daemon not found"
fi
CRONUS_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S20cronus"
    
    # S30ouranos - Start Wayland compositor
    cat > "${ROOTFS_DIR}/etc/init.d/S30ouranos" << 'OURANOS_EOF'
#!/bin/sh
# Start OURANOS Wayland compositor

export WAYLAND_DISPLAY=wayland-0
export XDG_RUNTIME_DIR=/run/user/0
mkdir -p $XDG_RUNTIME_DIR

if [ -x /usr/bin/ouranos ]; then
    /usr/bin/ouranos &
    sleep 2
    echo "[✓] OURANOS compositor started"
else
    echo "[!] OURANOS compositor not found"
fi
OURANOS_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S30ouranos"
    
    # S40gecko - Start Gecko rendering engine
    cat > "${ROOTFS_DIR}/etc/init.d/S40gecko" << 'GECKO_EOF'
#!/bin/sh
# Start GECKO rendering engine

export WAYLAND_DISPLAY=wayland-0
export LD_LIBRARY_PATH="/usr/lib/gecko:$LD_LIBRARY_PATH"

if [ -x /usr/bin/gecko ]; then
    /usr/bin/gecko &
    sleep 2
    echo "[✓] GECKO rendering engine started"
else
    echo "[!] GECKO not found"
fi
GECKO_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S40gecko"
    
    # S50kratos - Start performance monitor
    cat > "${ROOTFS_DIR}/etc/init.d/S50kratos" << 'KRATOS_EOF'
#!/bin/sh
# Start KRATOS performance monitor

if [ -x /usr/bin/kratos_daemon ]; then
    /usr/bin/kratos_daemon &
    echo "[✓] KRATOS performance monitor started"
else
    echo "[!] KRATOS daemon not found"
fi
KRATOS_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S50kratos"
    
    # S60persephone - Start Linux app compatibility layer
    cat > "${ROOTFS_DIR}/etc/init.d/S60persephone" << 'PERSEPHONE_EOF'
#!/bin/sh
# Start PERSEPHONE Linux app compatibility

if [ -x /usr/bin/persephone_daemon ]; then
    /usr/bin/persephone_daemon &
    echo "[✓] PERSEPHONE compatibility layer started"
else
    echo "[!] PERSEPHONE daemon not found"
fi
PERSEPHONE_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S60persephone"
    
    # S70gaia - Start GAIA desktop shell
    cat > "${ROOTFS_DIR}/etc/init.d/S70gaia" << 'GAIA_EOF'
#!/bin/sh
# Start GAIA desktop shell

export WAYLAND_DISPLAY=wayland-0
export LD_LIBRARY_PATH="/usr/lib/gecko:$LD_LIBRARY_PATH"

if [ -x /usr/bin/gaia ]; then
    /usr/bin/gaia &
    echo "[✓] GAIA desktop shell started"
else
    echo "[!] GAIA shell not found"
fi
GAIA_EOF
    chmod +x "${ROOTFS_DIR}/etc/init.d/S70gaia"
    
    log_success "Service scripts created"
}

# ============================================================================
# STAGE 8: Copy Compiled Binaries
# ============================================================================
stage_binaries() {
    log_section "Stage 8: Copying Compiled Binaries"
    
    log_info "Copying GeckoOS component binaries..."
    
    # GeckoOS services (from cargo build)
    local services=(
        "echo:echo_daemon"
        "cronus:cronus_daemon"
        "ouranos:ouranos"
        "gecko:gecko"
        "kratos:kratos_daemon"
        "persephone:persephone_daemon"
        "gaia:gaia"
    )
    
    for service_target in "${services[@]}"; do
        IFS=: read -r crate binary <<< "$service_target"
        local binary_path="${PROJECT_ROOT}/target/release/${binary}"
        
        if [ -f "$binary_path" ]; then
            cp "$binary_path" "${ROOTFS_DIR}/usr/bin/"
            log_info "  ✓ ${binary}"
        else
            log_info "  ⚠ ${binary} not found (build with: cargo build --release -p $crate)"
        fi
    done
    
    log_success "Binaries stage complete"
}

# ============================================================================
# STAGE 9: Copy Gecko Libraries
# ============================================================================
stage_gecko_libs() {
    log_section "Stage 9: Gecko Runtime Libraries"
    
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
# STAGE 10: Copy Userspace Applications
# ============================================================================
stage_userspace_apps() {
    log_section "Stage 10: Userspace Applications"
    
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
# STAGE 11: Set Permissions
# ============================================================================
stage_permissions() {
    log_section "Stage 11: Setting Permissions"
    
    log_info "Setting filesystem permissions..."
    
    chmod 755 "${ROOTFS_DIR}"/usr/{bin,sbin,lib} 2>/dev/null || true
    chmod 755 "${ROOTFS_DIR}"/etc/init.d/* 2>/dev/null || true
    chmod 1777 "${ROOTFS_DIR}/tmp"
    chmod 1777 "${ROOTFS_DIR}/var/tmp"
    chmod 755 "${ROOTFS_DIR}/var/log"
    chmod 755 "${ROOTFS_DIR}/var/run"
    
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
    stage_kernel
    stage_gecko
    stage_rootfs_dirs
    stage_system_config
    stage_boot_config
    stage_initramfs
    stage_services
    stage_binaries
    stage_gecko_libs
    stage_userspace_apps
    stage_permissions
    
    log_section "ROOTFS CREATION COMPLETE"
    log_success "Rootfs ready at: ${ROOTFS_DIR}"
    log_info ""
    log_info "Next steps:"
    log_info "  1. Verify binaries are built:"
    log_info "     cargo build --release --all"
    log_info ""
    log_info "  2. Create disk image:"
    log_info "     bash ${PROJECT_ROOT}/scripts/package.sh"
    log_info ""
    log_info "  3. Boot on QEMU:"
    log_info "     qemu-system-x86_64 -drive file=geckoos.img -enable-kvm"
    log_info ""
}

main "$@"
