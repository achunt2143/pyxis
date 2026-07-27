#!/bin/bash
# GeckoOS BusyBox Automation Script

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target/build"
ROOTFS_DIR="${BUILD_DIR}/rootfs"
BUSYBOX_BUILD="${BUILD_DIR}/busybox-build"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[\u2713]${NC} $*"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }
log_section() {
  echo ""
  echo "════════════════════════════════════════════"
  echo " $*"
  echo "════════════════════════════════════════════"
}

require_cmds() {
  local cmds=(git make gcc file sed)
  for cmd in "${cmds[@]}"; do
    command -v "$cmd" >/dev/null 2>&1 || { log_error "Missing: $cmd"; exit 1; }
  done
}

# Force a BusyBox .config option to 'y', adding it if absent.
enable_config() {
  local key="$1" file="$2"
  if grep -q "^${key}=" "$file"; then
    sed -i "s/^${key}=.*/${key}=y/" "$file"
  elif grep -q "^# ${key} is not set" "$file"; then
    sed -i "s/^# ${key} is not set/${key}=y/" "$file"
  else
    echo "${key}=y" >> "$file"
  fi
}

main() {
  log_section "BusyBox Automated Installation"

  require_cmds

  mkdir -p "$BUSYBOX_BUILD" "$ROOTFS_DIR"
  cd "$BUSYBOX_BUILD"

  if [[ ! -d busybox ]]; then
    log_info "Cloning BusyBox"
    git clone --depth 1 https://github.com/mirror/busybox.git
  fi

  cd busybox

  gcc_version=$(gcc --version | head -n1 | awk '{print $NF}' | cut -d. -f1)
  [[ "$gcc_version" -ge 14 ]] && sed -i '/main()/ { /int main()/! s/main()/int main()/; }' scripts/kconfig/lxdialog/check-lxdialog.sh

  make distclean
  make defconfig

  # -----------------------------------------------------------------------
  # Required applets for GeckoOS initramfs
  # defconfig does NOT enable blkid or mdev; we force them here so the
  # initramfs can resolve root-by-UUID and populate /dev without udev.
  # -----------------------------------------------------------------------

  # blkid — UUID/LABEL superblock scanning (critical for UUID= root lookup)
  enable_config CONFIG_BLKID             .config
  enable_config CONFIG_FEATURE_BLKID_TYPE .config

  # mdev — BusyBox hotplug/device manager (populates /dev from sysfs)
  enable_config CONFIG_MDEV              .config
  enable_config CONFIG_FEATURE_MDEV_CONF .config
  enable_config CONFIG_FEATURE_MDEV_RENAME .config

  # sysfs support (required by mdev -s)
  enable_config CONFIG_FEATURE_SYSFS     .config

  # httpd — minimal static file server, used to serve Bloom's built dist/
  # (started as "pyxis-static-server" from geckoOS-boot in create-rootfs.sh)
  # until the real vela/Gecko kiosk build exists. defconfig usually
  # enables this already; force it so we don't silently lose it if upstream
  # defaults change.
  enable_config CONFIG_HTTPD              .config
  enable_config CONFIG_FEATURE_HTTPD_RANGES .config

  # Disable broken tc applet
  if grep -q '^CONFIG_TC=' .config; then
    sed -i 's/^CONFIG_TC=.*/CONFIG_TC=n/' .config
  else
    echo 'CONFIG_TC=n' >> .config
  fi

  # Enable static linking
  sed -i 's/^# CONFIG_STATIC is not set/CONFIG_STATIC=y/' .config 2>/dev/null \
    || enable_config CONFIG_STATIC .config

  sleep 1
  make oldconfig < /dev/null
  make -j"$(nproc)"

  # Deliberately NOT running `make CONFIG_PREFIX="$ROOTFS_DIR" install`
  # here (found as a real, active bug via a real boot: "/usr/bin/setpriv:
  # unrecognized option '--reuid=1000'" -- launch broke because `setpriv`
  # resolved to BusyBox's own minimal applet instead of the real
  # util-linux binary). BusyBox's `make install` unconditionally symlinks
  # EVERY enabled applet -- including `setpriv`, `mount`, `login`, etc. --
  # directly over whatever already exists at that path in $ROOTFS_DIR, with
  # no awareness of (or respect for) files a Debian package already owns
  # there. `create-rootfs.sh`'s own `stage_busybox_init` comment already
  # warned about exactly this danger, which is why THAT stage only ever
  # copies the single compiled binary to /bin/busybox + symlinks
  # /sbin/init -- this script used to run its own conflicting `make
  # install` into the SAME shared $ROOTFS_DIR on top of that, and whether
  # it clobbers the real util-linux setpriv/mount/etc. depends entirely on
  # ordering: harmless if this runs before `stage_install_base_packages`
  # apt-installs util-linux (dpkg overwrites busybox's symlink back), but
  # a real regression if this ever runs again afterward (e.g. busybox
  # rebuilt after the rootfs already has real Debian utilities installed)
  # -- exactly what happened here. The fix is to never install applets
  # into the shared rootfs from this script at all; `stage_busybox_init`
  # is the only place that should ever touch $ROOTFS_DIR for BusyBox, and
  # it already does so surgically.
  local built_bin="${BUSYBOX_BUILD}/busybox/busybox"
  if file "$built_bin" | grep -q "statically linked"; then
    size=$(du -h "$built_bin" | cut -f1)
    log_success "BusyBox built statically ($size) at $built_bin"
  else
    log_error "BusyBox is not statically linked"
    exit 1
  fi

  # Verify critical applets are present in the BUILT binary (not a rootfs
  # copy -- this script no longer installs anything into $ROOTFS_DIR).
  # udhcpc (defconfig already enables it, unlike blkid/mdev/httpd above --
  # verified here rather than force-enabled, just to catch a future upstream
  # default change) is what create-rootfs.sh's geckoOS-boot invokes to bring
  # up the guest NIC; see its default.script for the other half.
  for applet in blkid mdev httpd udhcpc; do
    if "$built_bin" --list 2>/dev/null | grep -q "^${applet}$"; then
      log_success "Applet present: ${applet}"
    else
      # Dump the state that decides this rather than just saying "check
      # .config": this fired once for httpd and then would not reproduce
      # from a clean distclean+defconfig build, so the evidence has to be
      # captured at failure time or it is gone. Three things can drop an
      # applet -- the symbol never got set, `make oldconfig` silently
      # dropped it for an unmet dependency, or the binary being checked is
      # stale relative to .config (mtimes distinguish that last one).
      local sym="CONFIG_$(echo "$applet" | tr '[:lower:]' '[:upper:]')"
      log_error "MISSING applet after build: ${applet}"
      grep -E "^(# )?${sym}\b" .config >&2 \
        || log_error "  ${sym} absent from .config entirely"
      log_error "  .config mtime: $(stat -c %y .config 2>/dev/null)"
      log_error "  binary mtime:  $(stat -c %y "$built_bin" 2>/dev/null)"
      log_error "  applets built: $("$built_bin" --list 2>/dev/null | wc -l)"
      exit 1
    fi
  done

  log_success "BusyBox automation complete"
}

main "$@"
