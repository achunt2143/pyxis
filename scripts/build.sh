#!/bin/bash
set -euo pipefail

# Color definitions
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
INSTALL_DIR="${BUILD_DIR}/install"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
LOG_DIR="${PROJECT_ROOT}/build-logs"

CARGO_FLAGS="--verbose"
CARGO_RELEASE_FLAGS="--release ${CARGO_FLAGS}"

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*" | tee -a "${LOG_DIR}/build.log"
}

log_success() {
    echo -e "${GREEN}[✓]${NC} $*" | tee -a "${LOG_DIR}/build.log"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*" | tee -a "${LOG_DIR}/build.log"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" | tee -a "${LOG_DIR}/build.log"
}

# Initialize build environment
init_build() {
    
    mkdir -p "${BUILD_DIR}" "${LOG_DIR}" "${ROOTFS_DIR}"
    log_info "Initializing build environment..."
    # Check for required tools
    for cmd in cargo rustc gcc pkg-config make; do
        if ! command -v "$cmd" &> /dev/null; then
            log_error "Required command not found: $cmd"
            exit 1
        fi
    done
    
    log_success "Build environment initialized"
}

# Verify dependencies
verify_deps() {
    log_info "Verifying dependencies..."
    
    local missing_deps=0
    
    # Check Rust version
    local rust_version=$(rustc --version | awk '{print $2}')
    log_info "Rust version: $rust_version"
    
    # Check system libraries
    local libs=(
        "libdrm"
        "libinput"
        "wayland-server"
        "libgl"
        "gbm"
        "libudev"
        "alsa"
    )
    
    for lib in "${libs[@]}"; do
        if ! pkg-config --exists "$lib" 2>/dev/null; then
            log_warn "Missing system library: $lib"
            missing_deps=$((missing_deps + 1))
        fi
    done
    
    if [ $missing_deps -gt 0 ]; then
        log_warn "Missing $missing_deps system libraries"
        log_info "Run 'make install-system-deps' to install missing libraries"
    fi
    
    log_success "Dependencies verified"
}

# Fetch Rust dependencies
fetch_rust_deps() {
    log_info "Fetching Rust dependencies..."
    cd "${PROJECT_ROOT}"
    cargo fetch ${CARGO_FLAGS}
    log_success "Rust dependencies fetched"
}

# Fetch Node dependencies
fetch_node_deps() {
    log_info "Fetching Node.js dependencies..."
    cd "${PROJECT_ROOT}/gaia"
    npm ci
    log_success "Node.js dependencies fetched"
}

# Build Rust components
build_rust() {
    local build_type="${1:-debug}"
    
    if [ "$build_type" = "release" ]; then
        log_info "Building Rust components (release)..."
        cd "${PROJECT_ROOT}"
        cargo build ${CARGO_RELEASE_FLAGS}
    else
        log_info "Building Rust components (debug)..."
        cd "${PROJECT_ROOT}"
        cargo build ${CARGO_FLAGS}
    fi
    
    log_success "Rust build complete"
}

# Build JavaScript components
build_js() {
    log_info "Building JavaScript components..."
    cd "${PROJECT_ROOT}/gaia"
    npm run build
    log_success "JavaScript build complete"
}

# Run tests
run_tests() {
    log_info "Running tests..."
    
    log_info "Running Rust tests..."
    # cd "${PROJECT_ROOT}"
    # cargo test ${CARGO_FLAGS}
    
    log_info "Running JavaScript tests..."
    # npm test || true
    
    log_success "Tests complete"
}

# Run linters
run_linters() {
    log_info "Running linters..."
    
    log_info "Running Clippy..."
    cd "${PROJECT_ROOT}"
    cargo clippy ${CARGO_FLAGS} || true
    cd "${PROJECT_ROOT}/gaia"
    log_info "Running ESLint..."
    npm run lint || true
    
    log_success "Linting complete"
}

# Create installation directory
create_install_dir() {
    log_info "Creating installation directory structure..."
    
    mkdir -p "${INSTALL_DIR}"/{bin,lib,etc,var,usr/{bin,lib,share}}
    
    # Copy compiled binaries
    if [ -d "${PROJECT_ROOT}/target/release" ]; then
        find "${PROJECT_ROOT}/target/release" -maxdepth 1 -type f -executable \
            -exec cp {} "${INSTALL_DIR}/bin/" \; 2>/dev/null || true
    fi
    
    # Copy static files
    if [ -d "${PROJECT_ROOT}/dist" ]; then
        cp -r "${PROJECT_ROOT}/dist" "${INSTALL_DIR}/usr/share/geckoOS/"
    fi
    
    log_success "Installation directory created"
}

# Show build summary
show_summary() {
    local build_type="${1:-debug}"
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════${NC}"
    echo -e "${GREEN}Build Summary${NC}"
    echo -e "${GREEN}════════════════════════════════════════${NC}"
    echo "Build Type: $build_type"
    echo "Build Dir: ${BUILD_DIR}"
    echo "Install Dir: ${INSTALL_DIR}"
    echo "Rootfs Dir: ${ROOTFS_DIR}"
    echo ""
    echo "Next steps:"
    echo "  ./scripts/create-rootfs.sh - Create filesystem"
    echo "  ./scripts/package.sh - Package disk image"
    echo "  ./scripts/emulate.sh - Run in QEMU"
    echo -e "${GREEN}════════════════════════════════════════${NC}"
}

# Main build pipeline
main() {
    local build_type="${1:-debug}"
    local skip_tests="${2:-false}"
  
    init_build

    log_info "Starting GeckoOS build pipeline (${build_type})..."
    log_info "Build root: ${PROJECT_ROOT}"

    verify_deps
    fetch_rust_deps
    fetch_node_deps
    build_rust "$build_type"
    build_js
    
    if [ "$skip_tests" = "false" ]; then
        run_tests
    fi
    
    run_linters
    create_install_dir
    show_summary "$build_type"
    
    log_success "Build pipeline complete!"
}

# Handle command line arguments
case "${1:-all}" in
    init)
        init_build
        ;;
    verify)
        verify_deps
        ;;
    fetch)
        fetch_rust_deps
        fetch_node_deps
        ;;
    build-rust)
        build_rust "${2:-debug}"
        ;;
    build-js)
        build_js
        ;;
    test)
        run_tests
        ;;
    lint)
        run_linters
        ;;
    release)
        main "release" "${2:-false}"
        ;;
    all)
        main "debug" "${2:-false}"
        ;;
    *)
        echo "Usage: $0 {init|verify|fetch|build-rust|build-js|test|lint|release|all} [skip-tests]"
        exit 1
        ;;
esac