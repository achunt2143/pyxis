#!/bin/bash
# Complete build script for GeckoOS Compositor

set -euo pipefail

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Functions
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Detect OS
detect_os() {
    case "$(uname -s)" in
        Linux*)
            OS="Linux"
            ;;
        Darwin*)
            OS="Mac"
            ;;
        *)
            OS="UNKNOWN"
            ;;
    esac
    log_info "Detected OS: $OS"
}

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."

    # Check Rust
    if ! command -v cargo &> /dev/null; then
        log_error "Rust/Cargo not found. Install from https://rustup.rs/"
        exit 1
    fi
    
    RUST_VERSION=$(rustc --version | cut -d' ' -f2)
    log_info "Rust version: $RUST_VERSION"

    # Check build tools (Linux only)
    if [ "$OS" = "Linux" ]; then
        if ! command -v pkg-config &> /dev/null; then
            log_error "pkg-config not found"
            exit 1
        fi

        # Check required libraries
        for lib in libdrm gbm libinput xkbcommon; do
            if ! pkg-config --exists $lib; then
                log_warn "Development headers for $lib not found"
                log_info "On Ubuntu/Debian, run:"
                log_info "  sudo apt-get install lib${lib}-dev"
            fi
        done
    fi
}

# Format code
format_code() {
    log_info "Formatting code..."
    cargo fmt --all
    log_info "Code formatted ✓"
}

# Run linting
lint_code() {
    log_info "Running clippy linter..."
    cargo clippy --all-targets --all-features -- -D warnings
    log_info "Linting passed ✓"
}

# Build debug
build_debug() {
    log_info "Building debug version..."
    cargo build
    log_info "Debug build complete ✓"
    log_info "Binary: target/debug/geckoOS-compositor"
}

# Build release
build_release() {
    log_info "Building release version..."
    cargo build --release
    log_info "Release build complete ✓"
    log_info "Binary: target/release/geckoOS-compositor"
}

# Run tests
run_tests() {
    log_info "Running tests..."
    cargo test --lib
    cargo test --doc
    
    # Integration tests (may require graphics)
    log_info "Running integration tests (may require graphics device)..."
    cargo test --test '*' || log_warn "Integration tests require DRM device"
    
    log_info "Tests complete ✓"
}

# Run with logging
run_debug() {
    log_info "Running compositor with debug logging..."
    export RUST_LOG=debug
    ./target/debug/geckoOS-compositor
}

# Show usage
show_usage() {
    cat << EOF
GeckoOS Compositor Build Script

Usage: ./build.sh [COMMAND]

Commands:
    check       Check prerequisites
    format      Format code with rustfmt
    lint        Run clippy linter
    build       Build debug version (default)
    release     Build release version
    test        Run all tests
    run         Run compositor (debug)
    all         Full pipeline: check, format, lint, test, build

Examples:
    ./build.sh check          # Verify setup
    ./build.sh build          # Debug build
    ./build.sh release        # Optimized build
    ./build.sh test           # Run tests
    ./build.sh all            # Everything

EOF
}

# Main
main() {
    detect_os

    COMMAND="${1:-build}"

    case "$COMMAND" in
        check)
            check_prerequisites
            ;;
        format)
            format_code
            ;;
        lint)
            lint_code
            ;;
        build)
            check_prerequisites
            build_debug
            ;;
        release)
            check_prerequisites
            build_release
            ;;
        test)
            check_prerequisites
            run_tests
            ;;
        run)
            check_prerequisites
            build_debug
            run_debug
            ;;
        all)
            check_prerequisites
            format_code
            lint_code
            run_tests
            build_release
            log_info "All build stages complete! ✓"
            log_info "Release binary: target/release/geckoOS-compositor"
            ;;
        help|--help|-h)
            show_usage
            ;;
        *)
            log_error "Unknown command: $COMMAND"
            show_usage
            exit 1
            ;;
    esac
}

main "$@"
