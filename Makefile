.PHONY: help build build-release build-debug run test clean install-deps pyxis-build pyxis-install pyxis-dev \
        format lint kernel gecko system docker all stage1 stage2 stage3 \
		docker-build docker-shell docker-build-image docker-clean docker-logs \
		docker-images docker-volumes docker-ps docker-inspect docker-help

# Color output
RED = \033[0;31m
GREEN = \033[0;32m
YELLOW = \033[0;33m
NC = \033[0m

# Build variables
CARGO = cargo
CARGO_FLAGS = --verbose
CARGO_RELEASE_FLAGS = --release $(CARGO_FLAGS)
RUST_VERSION = 1.70
LINUX_VERSION = 6.6

# Docker configuration
DOCKER_IMAGE_NAME := geckoOS
DOCKER_IMAGE_TAG := latest
DOCKER_COMPOSE := docker-compose

# Script runner — always invoke via bash so execute-bit is not required.
# The GitHub Contents API writes files as mode 100644 (non-executable), so
# calling ./scripts/foo.sh directly would fail with "Permission denied" after
# any API-sourced commit. Using BASH_SCRIPTS_RUN prevents that permanently.
RUN_SCRIPT = bash

help:
	@echo "$(GREEN)GeckoOS Build System$(NC)"
	@echo "$(YELLOW)Available targets:$(NC)"
	@echo "  make build          - Build all components (debug)"
	@echo "  make build-release  - Build all components (release optimized)"
	@echo "  make build-debug    - Build all components (with debug symbols)"
	@echo "  make system         - Build Rust system components only"
	@echo "  make run            - Run emulated system with QEMU"
	@echo "  make test           - Run test suite"
	@echo "  make clean          - Remove all build artifacts"
	@echo "  make install-deps   - Install all dependencies"
	@echo "  make format         - Format code (Rust)"
	@echo "  make lint           - Run linters"
	@echo ""
	@echo "$(YELLOW)Build stages:$(NC)"
	@echo "  stage1: Verify dependencies (Rust, Linux headers)"
	@echo "  stage2: Build components (Rust binary)"
	@echo "  stage3: Package filesystem and create disk image"
	@echo ""
	@echo "$(YELLOW)Note:$(NC) JS/CSS compilation is handled by bloom (pyxis-experience)"
	@echo "  make pyxis-build    - Pull & build Pyxis shell from bloom into gaia/static"
	@echo "  make pyxis-dev      - Start bloom dev server"
	@echo ""

# ============================================================================
# DEPENDENCY VERIFICATION
# ============================================================================

verify-rust:
	@echo "$(YELLOW)Checking Rust installation...$(NC)"
	@command -v cargo >/dev/null 2>&1 || { echo "$(RED)Rust not found. Install from https://rustup.rs/$(NC)"; exit 1; }
	@rustc --version | grep -q "$(RUST_VERSION)" || echo "$(YELLOW)Warning: Rust version may differ. Expected $(RUST_VERSION)+$(NC)"

verify-linux-headers:
	@echo "$(YELLOW)Checking Linux kernel headers...$(NC)"
	@[ -d /usr/include/linux ] || { echo "$(RED)Linux headers not found. Install linux-headers package$(NC)"; exit 1; }

verify-build-tools:
	@echo "$(YELLOW)Checking build tools...$(NC)"
	@command -v gcc >/dev/null 2>&1 || { echo "$(RED)GCC not found$(NC)"; exit 1; }
	@command -v make >/dev/null 2>&1 || { echo "$(RED)Make not found$(NC)"; exit 1; }
	@command -v pkg-config >/dev/null 2>&1 || { echo "$(RED)pkg-config not found$(NC)"; exit 1; }

verify-system-libs:
	@echo "$(YELLOW)Checking system libraries...$(NC)"
	@pkg-config --exists libdrm || { echo "$(RED)libdrm not found$(NC)"; exit 1; }
	@pkg-config --exists libinput || { echo "$(RED)libinput not found$(NC)"; exit 1; }
	@pkg-config --exists wayland-server || { echo "$(RED)wayland-server not found$(NC)"; exit 1; }

stage1: verify-rust verify-linux-headers verify-build-tools verify-system-libs
	@echo "$(GREEN)✓ All dependencies verified$(NC)"

# ============================================================================
# INSTALLATION
# ============================================================================

install-deps: stage1
	@echo "$(YELLOW)Installing Rust dependencies...$(NC)"
	$(CARGO) fetch
	@echo "$(GREEN)✓ Dependencies installed$(NC)"

install-system-deps:
	@echo "$(YELLOW)Installing system libraries (requires sudo)...$(NC)"
	@command -v apt >/dev/null 2>&1 && sudo apt install -y \
		libdrm-dev libinput-dev wayland-protocols libwayland-dev \
		libgl1-mesa-dev libgbm-dev libudev-dev libasound2-dev \
		libseat-dev linux-headers-$(shell uname -r) || true
	@command -v pacman >/dev/null 2>&1 && sudo pacman -S \
		libdrm libinput wayland-protocols libgl gbm udev alsa-lib \
		linux-headers || true
	@echo "$(GREEN)✓ System libraries installed$(NC)"

# ============================================================================
# BUILD TARGETS
# ============================================================================

stage2-rust: verify-rust
	@echo "$(YELLOW)Building Rust components...$(NC)"
	mkdir -p target/release
	$(CARGO) build $(CARGO_FLAGS)
	@echo "$(GREEN)✓ Rust build complete$(NC)"

stage2-rust-release: verify-rust
	@echo "$(YELLOW)Building Rust components (release)...$(NC)"
	$(CARGO) build $(CARGO_RELEASE_FLAGS)
	@echo "$(GREEN)✓ Rust release build complete$(NC)"

system: stage2-rust-release
	@echo "$(GREEN)✓ System components built$(NC)"

# ============================================================================
# PYXIS DESKTOP SHELL
# ============================================================================

PYXIS_REPO   ?= ../pyxis-experience

# REAL BUG, found via a real report (2026-07-07): `build-release`'s
# dependency chain (stage1 install-deps stage2-rust-release build-kernel
# build-initramfs build-rootfs package) never invoked this target at all --
# `build-rootfs` (scripts/create-rootfs.sh) only COPIES from
# ../pyxis-experience/dist/ (stage_bloom_app/stage_terminal_app), it never
# runs `npm run build` itself. So `make build-release` silently packaged
# whatever was already sitting in dist/ from whenever it was last built by
# hand, with no indication anything was stale. Also removed the old
# `rm -rf gaia/static; cp -r dist gaia/static` step this target used to
# do -- gaia is retired in favor of Bloom (see CLAUDE.md), and
# create-rootfs.sh reads directly from pyxis-experience/dist/ now, so that
# copy was to a target nothing consumes anymore.
pyxis-build:
	@echo "$(YELLOW)Building Pyxis desktop shell from $(PYXIS_REPO)...$(NC)"
	cd $(PYXIS_REPO) && npm ci && npm run build
	@echo "[✓] Pyxis shell built at $(PYXIS_REPO)/dist"

pyxis-install: pyxis-build

pyxis-dev:
	cd $(PYXIS_REPO) && npm run dev


build: stage1 install-deps stage2-rust
	@echo "$(GREEN)✓ Full build complete$(NC)"

build-release: stage1 install-deps stage2-rust-release build-kernel build-initramfs build-rootfs package
	@echo "$(GREEN)✓ Release build complete$(NC)"

build-debug: stage1 install-deps
	@echo "$(YELLOW)Building with debug symbols...$(NC)"
	$(CARGO) build
	@echo "$(GREEN)✓ Debug build complete$(NC)"

build-kernel:
	@echo "$(YELLOW)Building kernel image...$(NC)"
	$(RUN_SCRIPT) scripts/build-kernel.sh
	@echo "$(GREEN)✓ Kernel build complete$(NC)"

build-rootfs: pyxis-build
	@echo "$(YELLOW)Building root fs...$(NC)"
	$(RUN_SCRIPT) scripts/create-rootfs.sh --kernel-prebuilt
	@echo "$(GREEN)✓ Root fs build complete$(NC)"

build-initramfs:
	@echo "$(YELLOW)Building initramfs image...$(NC)"
	$(RUN_SCRIPT) scripts/build-initramfs.sh
	@echo "$(GREEN)✓ Initramfs build complete$(NC)"

build-busybox:
	@echo "$(YELLOW)Building busybox...$(NC)"
	$(RUN_SCRIPT) scripts/busybox-install.sh
	@echo "$(GREEN)✓ Busybox build complete$(NC)"

package:
	@echo "$(YELLOW)Packaging image...$(NC)"
	$(RUN_SCRIPT) scripts/package.sh --x86_64-efi
	@echo "$(GREEN)✓ Packaging complete$(NC)"

# ============================================================================
# TESTING
# ============================================================================

test: build
	@echo "$(YELLOW)Running Rust tests...$(NC)"
	$(CARGO) test $(CARGO_FLAGS)
	@echo "$(GREEN)✓ Tests complete$(NC)"

test-rust:
	@echo "$(YELLOW)Running Rust unit tests...$(NC)"
	$(CARGO) test --lib $(CARGO_FLAGS)

test-integration:
	@echo "$(YELLOW)Running integration tests...$(NC)"
	$(CARGO) test --test '*' $(CARGO_FLAGS)

bench:
	@echo "$(YELLOW)Running benchmarks...$(NC)"
	$(CARGO) bench --release

# ============================================================================
# CODE QUALITY
# ============================================================================

format:
	@echo "$(YELLOW)Formatting Rust code...$(NC)"
	$(CARGO) fmt
	@echo "$(GREEN)✓ Code formatted$(NC)"

lint:
	@echo "$(YELLOW)Linting Rust code...$(NC)"
	$(CARGO) clippy -- -D warnings
	@echo "$(GREEN)✓ Linting complete$(NC)"

check:
	@echo "$(YELLOW)Running checks...$(NC)"
	$(CARGO) check
	@echo "$(GREEN)✓ Checks complete$(NC)"

# ============================================================================
# RUNTIME
# ============================================================================

run:
	@echo "$(YELLOW)Starting GeckoOS (debug mode)...$(NC)"
	$(RUN_SCRIPT) scripts/emulate.sh

run-release: build-release
	@echo "$(YELLOW)Starting GeckoOS (release mode)...$(NC)"
	$(RUN_SCRIPT) scripts/emulate.sh --release

run-debug: build-debug
	@echo "$(YELLOW)Starting GeckoOS with debugging...$(NC)"
	$(RUN_SCRIPT) scripts/emulate.sh --debug

# ============================================================================
# CLEANUP
# ============================================================================

clean:
	@echo "$(YELLOW)Cleaning build artifacts...$(NC)"
	$(CARGO) clean
	rm -rf dist node_modules
	rm -rf *.log
	@echo "$(GREEN)✓ Clean complete$(NC)"

clean-dist:
	@echo "$(YELLOW)Cleaning dist directory...$(NC)"
	rm -rf dist
	@echo "$(GREEN)✓ Dist cleaned$(NC)"

distclean: clean
	@echo "$(YELLOW)Removing all build and dependency artifacts...$(NC)"
	rm -rf .cargo target
	@echo "$(GREEN)✓ Distclean complete$(NC)"

# ============================================================================
# DOCUMENTATION
# ============================================================================

docs:
	@echo "$(YELLOW)Generating documentation...$(NC)"
	$(CARGO) doc --no-deps --open
	@echo "$(GREEN)✓ Documentation generated$(NC)"

# ============================================================================
# COMPLETE BUILD PIPELINE
# ============================================================================

all: stage1 install-deps build lint test
	@echo "$(GREEN)════════════════════════════════════════$(NC)"
	@echo "$(GREEN)✓ GeckoOS build pipeline complete!$(NC)"
	@echo "$(GREEN)════════════════════════════════════════$(NC)"
	@echo "$(YELLOW)Next steps:$(NC)"
	@echo "  make run          - Start the system"
	@echo "  make docs         - Generate documentation"
	@echo "  make clean        - Remove build artifacts"

.DEFAULT_GOAL := help
