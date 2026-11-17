.PHONY: help build build-release build-debug run run-release run-debug test clean install-deps \
        clean-dist clean-os clean-all clean-gecko distclean \
        pyxis-build pyxis-install pyxis-dev \
        gecko-build gecko-patches-check gecko-patches-regen install-hooks \
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
	@echo "  make run-release    - Build (release) and run in QEMU"
	@echo "  make test           - Run test suite"
	@echo "  make install-deps   - Install all dependencies"
	@echo "  make format         - Format code (Rust)"
	@echo "  make lint           - Run linters"
	@echo ""
	@echo "$(YELLOW)Cleaning:$(NC)"
	@echo "  make run-release CLEAN=1  - Wipe rootfs+image, then rebuild and run"
	@echo "                              (note: CLEAN=1, not --clean; make"
	@echo "                               rejects unknown leading-dash args)"
	@echo "  make clean          - cargo clean + dist/ + logs"
	@echo "  make clean-os       - Wipe rootfs + disk image (fixes a wedged"
	@echo "                        rootfs, e.g. dpkg 'Group already exists')"
	@echo "  make clean-all      - clean-os + kernel tree + cargo"
	@echo "  make clean-gecko    - Wipe Gecko build (forces a MULTI-HOUR rebuild)"
	@echo "  make distclean      - Everything, including Gecko"
	@echo "  Release images (pyxis-*.img/.tar.*) are preserved by all of the"
	@echo "  above; use 'bash scripts/clean.sh --images --release-artifacts'."
	@echo ""
	@echo "$(YELLOW)Build stages:$(NC)"
	@echo "  stage1: Verify dependencies (Rust, Linux headers)"
	@echo "  stage2: Build components (Rust binary)"
	@echo "  stage3: Package filesystem and create disk image"
	@echo ""
	@echo "$(YELLOW)Note:$(NC) JS/CSS compilation is handled by bloom (pyxis-experience)"
	@echo "  make pyxis-build    - Pull & build Pyxis shell from bloom into gaia/static"
	@echo "  make pyxis-dev      - Start bloom dev server"
	@echo "  make gecko-build    - Build vela (from-source Gecko) via ./mach build"
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
PYXIS_NPM_CI ?= 0

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
#
# SECOND REAL BUG, found via a real report (2026-07-08): `npm run build`
# only builds the root `bloom` package (tsc -b && vite build -> dist/) --
# it does NOT build the `packages/apps/*` npm workspaces (terminal,
# browser, files, notes, contacts, tasks, settings each have their own
# `dist/`, built by their own `npm run build` per package.json). Those are
# what create-rootfs.sh's stage_terminal_app/_stage_pyxis_app stages expect
# to find; when they're missing, every one of those stages logs "No <app>
# build ... skipping" and the resulting image has no launchable apps at
# all besides whatever was built by hand previously. `build:all` (already
# defined in pyxis-experience/package.json) runs
# `npm run build --workspaces --if-present` first, then the root build --
# use that instead so every staged app is actually produced.
# Reuse an existing node_modules tree by default so offline/locally-synced
# packages (e.g. remochi / @duyojs/duyo) and running dev servers are not wiped
# by `npm ci`. Pass PYXIS_NPM_CI=1 to force a clean `npm ci` when deps change.
pyxis-build:
	@if [ ! -d "$(PYXIS_REPO)" ]; then \
		echo "$(YELLOW)⚠ $(PYXIS_REPO) not found -- skipping Pyxis web build (create-rootfs.sh will reuse existing rootfs apps if present).$(NC)"; \
	elif [ "$(PYXIS_NPM_CI)" = "1" ] || [ ! -d "$(PYXIS_REPO)/node_modules" ]; then \
		echo "$(YELLOW)Installing deps & building Pyxis desktop shell + apps from $(PYXIS_REPO)...$(NC)"; \
		cd $(PYXIS_REPO) && npm ci && npm run build:all; \
		echo "[✓] Pyxis shell + apps built at $(PYXIS_REPO)/dist and packages/apps/*/dist"; \
	else \
		echo "$(YELLOW)Building Pyxis desktop shell + apps from $(PYXIS_REPO) (reusing existing node_modules; pass PYXIS_NPM_CI=1 to run npm ci)...$(NC)"; \
		cd $(PYXIS_REPO) && npm run build:all; \
		echo "[✓] Pyxis shell + apps built at $(PYXIS_REPO)/dist and packages/apps/*/dist"; \
	fi

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

# vela (from-source Gecko) build.
#
# REAL BUG, same class as the pyxis-build one above (found live 2026-07-08):
# create-rootfs.sh's stage_gecko() only COPIES the pre-built vela out of the
# objdir (target/gecko-src/gecko-dev/obj-pyxis-webapp/dist/bin) -- it never
# runs `./mach build`. So `make build-release` silently packaged whatever
# vela was last built by hand, with no indication it was stale. That means
# every change to the gecko source or the pyxis patch series (e.g. the
# BrowserContentHandler kiosk/decoration fixes) was NOT in the image unless
# vela happened to have been rebuilt manually first -- the exact reason a
# round of decoration fixes appeared to "do nothing" after a rebuild.
#
# This runs `./mach build` against the pinned checkout with the vela
# mozconfig before the rootfs is staged, so the freshly-built vela is what
# stage_gecko copies. mach build is incremental: fast when nothing changed,
# a full (large/slow) build only on the first run. It does NOT apply the
# gecko/patches/*.PYXIS_DIFF series -- those are already applied as
# working-tree edits in the checkout (see gecko/BUILD_RUNBOOK.md); this just
# compiles the current source state.
#
# Skips (with a clear warning) rather than hard-failing build-release when
# the gecko checkout isn't present: creating it is a heavy, documented
# one-time prerequisite (full clone + `./mach bootstrap`, see
# gecko/BUILD_RUNBOOK.md), not something this target should attempt. When it
# skips, stage_gecko reuses an existing vela if one is present, else skips
# vela too -- same behavior as before this target existed.
GECKO_SRC ?= target/gecko-src/gecko-dev
GECKO_MOZCONFIG ?= $(CURDIR)/gecko/mozconfig-vela

# Guards against the gecko checkout and gecko/patches/pyxis/ drifting apart.
#
# The checkout is gitignored, so gecko/patches/pyxis/ is the only durable copy
# of the vela fork -- but nothing applies it programmatically, which means a
# Gecko change made directly in the checkout gets built, shipped, and then lost
# with no error at any point. An audit (2026-07-25) found exactly that: one
# change (toolkit/actors/moz.build, which registers the bridge actors) was in
# the checkout and in no patch at all.
#
# --check re-applies every diff to a pristine copy of the pinned upstream file
# and asserts the result reproduces the checkout, so it catches silent drift
# rather than just a missing file. Exits 2 (skip) when there's no checkout.
gecko-patches-check:
	@GECKO_SRC=$(GECKO_SRC) $(RUN_SCRIPT) scripts/regen-gecko-patches.sh --check; \
	rc=$$?; \
	if [ $$rc -eq 2 ]; then \
		echo "$(YELLOW)⚠ No gecko checkout -- skipping patch-series check.$(NC)"; \
	elif [ $$rc -ne 0 ]; then \
		echo "$(RED)✗ gecko/patches/pyxis/ is out of sync with the checkout.$(NC)"; \
		echo "$(RED)  Your Gecko change exists only in the gitignored checkout and$(NC)"; \
		echo "$(RED)  would be lost. Run: make gecko-patches-regen$(NC)"; \
		exit 1; \
	fi

gecko-patches-regen:
	@GECKO_SRC=$(GECKO_SRC) $(RUN_SCRIPT) scripts/regen-gecko-patches.sh
	@echo "$(GREEN)✓ Patch series regenerated -- review and commit gecko/patches/$(NC)"

gecko-build: gecko-patches-check
	@if [ -x "$(GECKO_SRC)/mach" ]; then \
		echo "$(YELLOW)Building vela (from-source Gecko) -- incremental; first build is large/slow...$(NC)"; \
		cd $(GECKO_SRC) && MOZCONFIG=$(GECKO_MOZCONFIG) ./mach build; \
		echo "$(GREEN)✓ vela build complete$(NC)"; \
	else \
		echo "$(YELLOW)⚠ Gecko checkout not found at $(GECKO_SRC) -- skipping vela build.$(NC)"; \
		echo "$(YELLOW)  create-rootfs.sh will reuse an existing vela if present, else skip it.$(NC)"; \
		echo "$(YELLOW)  Set up the checkout per gecko/BUILD_RUNBOOK.md (clone + ./mach bootstrap).$(NC)"; \
	fi
build-rootfs: gecko-build pyxis-build
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

check: gecko-patches-check
	@echo "$(YELLOW)Running checks...$(NC)"
	$(CARGO) check
	@echo "$(GREEN)✓ Checks complete$(NC)"

# Installs the repo's git hooks (see scripts/git-hooks/). Opt-in rather than
# automatic -- git will not run hooks from a checked-in directory unless
# core.hooksPath points at it, which this sets.
install-hooks:
	@git config core.hooksPath scripts/git-hooks
	@chmod +x scripts/git-hooks/*
	@echo "$(GREEN)✓ git hooks installed (core.hooksPath=scripts/git-hooks)$(NC)"
	@echo "  pre-commit will verify the gecko patch series is in sync."

# ============================================================================
# RUNTIME
# ============================================================================

run:
	@echo "$(YELLOW)Starting GeckoOS (debug mode)...$(NC)"
	$(RUN_SCRIPT) scripts/emulate.sh

# CLEAN=1 wipes the OS build tree (rootfs + images) before building, so the
# build starts from a fresh debootstrap. Note this is `make run-release
# CLEAN=1`, NOT `make run-release --clean` -- make parses leading-dash words
# as its own options and would reject --clean outright.
#
# The clean must happen before build-release's prerequisites run, which a
# normal prerequisite cannot express (make would be free to order them the
# other way). Hence the recursive $(MAKE) invocation.
run-release:
ifeq ($(CLEAN),1)
	@echo "$(YELLOW)CLEAN=1: wiping build tree before rebuild...$(NC)"
	$(RUN_SCRIPT) scripts/clean.sh --rootfs --images --yes
endif
	@$(MAKE) --no-print-directory build-release
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

# Wipes the rootfs + disk image so the next build re-debootstraps from
# scratch. This is the fix for create-rootfs.sh failing repeatedly with
# 'Group "adm" already exists' -- that is a half-configured dpkg state in a
# reused rootfs, and it will fail identically forever until the tree is gone.
clean-os:
	@echo "$(YELLOW)Cleaning OS build tree (rootfs + images)...$(NC)"
	$(RUN_SCRIPT) scripts/clean.sh --rootfs --images --yes
	@echo "$(GREEN)✓ OS build tree cleaned$(NC)"

# Everything regenerable: rootfs, images, kernel tree, cargo artifacts.
# Excludes the Gecko build (hours to rebuild -- use clean-gecko) and release
# artifacts (not reproducible -- use clean.sh --release-artifacts).
clean-all:
	@echo "$(YELLOW)Cleaning all regenerable build artifacts...$(NC)"
	$(RUN_SCRIPT) scripts/clean.sh --all --yes
	@echo "$(GREEN)✓ Full clean complete$(NC)"

clean-gecko:
	@echo "$(RED)Cleaning Gecko build - this forces a multi-hour rebuild$(NC)"
	$(RUN_SCRIPT) scripts/clean.sh --gecko --yes
	@echo "$(GREEN)✓ Gecko build cleaned$(NC)"

# NOTE: plain `rm -rf target` fails here -- target/build/rootfs is root-owned
# because debootstrap creates it under sudo. clean.sh handles that, and tears
# down any stale loop devices/mounts left behind by an interrupted package.sh
# before deleting (rm -rf into a live mountpoint would delete through it).
distclean: clean
	@echo "$(YELLOW)Removing all build and dependency artifacts...$(NC)"
	$(RUN_SCRIPT) scripts/clean.sh --all --gecko --yes
	rm -rf .cargo
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
