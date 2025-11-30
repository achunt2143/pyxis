#!/bin/bash
# ============================================================================
# GeckoOS System Initialization Script
# Runs after real root filesystem is mounted
# Initializes system services and desktop environment
# ============================================================================

set -e

# Color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Logging functions
log_info() { echo -e "${BLUE}[INIT]${NC} $*"; }
log_success() { echo -e "${GREEN}[✓]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }

# ============================================================================
# EARLY SYSTEM SETUP
# ============================================================================

log_info "Starting GeckoOS system initialization..."

# Set hostname
echo "geckoOS" > /etc/hostname
hostname geckoOS
log_success "Hostname set"

# Mount critical filesystems
log_info "Mounting critical filesystems..."
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t cgroup2 cgroup2 /sys/fs/cgroup 2>/dev/null || true
mount -t tmpfs tmpfs /run -o size=256M,nosuid,nodev,noexec
mount -t tmpfs tmpfs /tmp -o size=512M,nosuid,nodev,noexec
mount -t tmpfs tmpfs /var/log -o size=64M,nosuid,nodev,noexec
log_success "Filesystems mounted"

# Set up loopback networking (required for IPC)
log_info "Setting up loopback networking..."
ip link set lo up
ip addr add 127.0.0.1/8 dev lo
log_success "Loopback networking configured"

# ============================================================================
# SYSTEM LOGGING
# ============================================================================

log_info "Starting system logging..."

# Create log directory
mkdir -p /var/log
touch /var/log/syslog /var/log/auth.log /var/log/kern.log

# Start syslog daemon
if command -v rsyslogd >/dev/null 2>&1; then
    rsyslogd -n &
    SYSLOG_PID=$!
    log_success "rsyslogd started (PID: $SYSLOG_PID)"
elif command -v syslogd >/dev/null 2>&1; then
    syslogd -n &
    SYSLOG_PID=$!
    log_success "syslogd started (PID: $SYSLOG_PID)"
else
    log_warn "No syslog daemon found"
fi

# ============================================================================
# SYSTEM SERVICES - Start in order
# ============================================================================

log_info "Starting system services..."

# 1. Start Cronus (Process Manager)
log_info "Starting Cronus (Process Manager)..."
if [ -x /opt/geckoOS/bin/cronus-daemon ]; then
    /opt/geckoOS/bin/cronus-daemon &
    CRONUS_PID=$!
    log_success "Cronus started (PID: $CRONUS_PID)"
else
    log_warn "Cronus binary not found"
fi

sleep 1

# 2. Start Echo (IPC/DBus Manager)
log_info "Starting Echo (IPC Manager)..."
if [ -x /opt/geckoOS/bin/echo-daemon ]; then
    export DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus/system_bus_socket"
    mkdir -p /run/dbus
    /opt/geckoOS/bin/echo-daemon &
    ECHO_PID=$!
    log_success "Echo started (PID: $ECHO_PID)"
else
    log_warn "Echo binary not found"
fi

sleep 1

# 3. Start Gecko (Firefox/Gecko Integration)
log_info "Starting Gecko (Firefox Integration)..."
if [ -x /opt/geckoOS/bin/gecko ]; then
    /opt/geckoOS/bin/gecko &
    GECKO_PID=$!
    log_success "Gecko started (PID: $GECKO_PID)"
else
    log_warn "Gecko binary not found"
fi

# 4. Start Kratos (Performance Optimization)
log_info "Starting Kratos (Performance Manager)..."
if [ -x /opt/geckoOS/bin/kratos ]; then
    /opt/geckoOS/bin/kratos &
    KRATOS_PID=$!
    log_success "Kratos started (PID: $KRATOS_PID)"
else
    log_warn "Kratos binary not found"
fi

# 5. Set up display environment
log_info "Setting up display environment..."
export XDG_RUNTIME_DIR=/run/user/1000
export DISPLAY=:0
export WAYLAND_DISPLAY=wayland-0
mkdir -p $XDG_RUNTIME_DIR
chmod 700 $XDG_RUNTIME_DIR

# 6. Start OURANOS (Wayland Compositor)
log_info "Starting OURANOS (Wayland Compositor)..."
if [ -x /opt/geckoOS/bin/ouranos ]; then
    # Run in background
    /opt/geckoOS/bin/ouranos &
    OURANOS_PID=$!
    log_success "OURANOS started (PID: $OURANOS_PID)"
    
    # Wait for display socket to appear
    COUNT=0
    while [ ! -S "$XDG_RUNTIME_DIR/wayland-0" ] && [ $COUNT -lt 10 ]; do
        sleep 1
        COUNT=$((COUNT + 1))
    done
    
    if [ -S "$XDG_RUNTIME_DIR/wayland-0" ]; then
        log_success "Wayland display socket ready"
    else
        log_warn "Wayland display socket not found after timeout"
    fi
else
    log_warn "OURANOS binary not found"
fi

sleep 2

# ============================================================================
# USERSPACE - Start GAIA (Desktop Environment)
# ============================================================================

log_info "Starting GAIA (Desktop Environment)..."
if [ -x /opt/geckoOS/bin/gaia ]; then
    /opt/geckoOS/bin/gaia &
    GAIA_PID=$!
    log_success "GAIA started (PID: $GAIA_PID)"
else
    log_warn "GAIA binary not found"
fi

# ============================================================================
# SIGNAL HANDLING - Graceful Shutdown
# ============================================================================

log_info "System initialization complete"
log_info "GeckoOS is ready"
echo "========================================="
echo "GeckoOS Desktop Environment Running"
echo "========================================="

# Store PIDs for shutdown
PIDS="$GAIA_PID $OURANOS_PID $KRATOS_PID $GECKO_PID $ECHO_PID $CRONUS_PID"

# Signal handler for graceful shutdown
handle_shutdown() {
    log_info "Received shutdown signal, terminating services..."
    
    # Kill services in reverse order
    for pid in $PIDS; do
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            log_info "Terminating PID $pid..."
            kill -TERM "$pid" 2>/dev/null || true
        fi
    done
    
    # Wait for graceful shutdown
    sleep 2
    
    # Force kill if necessary
    for pid in $PIDS; do
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            log_warn "Force killing PID $pid..."
            kill -9 "$pid" 2>/dev/null || true
        fi
    done
    
    log_success "Shutdown complete"
    exit 0
}

# Register signal handlers
trap handle_shutdown SIGTERM SIGINT

# Wait for all child processes
wait
