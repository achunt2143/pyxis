#!/usr/bin/env python3
"""Drive a booted Pyxis guest over the serial console and capture results.

Serial is the command channel for this project: `/etc/inittab` gives `ttyS0`
a root shell with no login. This harness boots the image, waits for that
shell, runs a list of commands, and prints each one's output. That makes an
on-device check a script that can be re-run, not a manual session that has
to be retyped.

It can also look at the screen. QMP `screendump` cannot: it reports "no
surface" on the virgl GL scanout Evo needs. What works is `grim` inside the
guest. It asks Evo for a copy of the output over wlr-screencopy, so the
picture is what Evo composited, and the PNG comes back out through a
transfer disk (below). A `#!shot <name>` line in a suite saves one to
target/serial-shots/<name>.png. Open that file and look at it: a capture
that succeeds says nothing about whether the screen is right.

It encodes the gotchas that each cost real time to find:

  * Boot-script output never reaches serial. The kernel cmdline ends
    `console=ttyS0,115200 console=tty0`, and the LAST `console=` owns
    /dev/console — so geckoOS-boot, rcS and the initramfs all log to tty0.
    Silence on serial is not evidence that nothing ran; read /var/log/*.log.
  * The shell echoes each command before running it, so a plain marker matches
    its own echo and every result lands one section late. Markers are sent
    split (`echo PYX''BEGIN`) so the echoed line cannot match.
  * `echo-cli` needs XDG_RUNTIME_DIR=/run/user/1000 (else it looks in /tmp and
    reports "Failed to connect" while the daemon is fine) and the FULL
    destination name (`org.geckoOS.nova`, not `nova`, which returns the very
    misleading "Service not registered").

By default the guest runs on a qcow2 overlay, so the pristine image is never
mutated and every run starts from the same state. Pass --no-overlay to write
through to the real image.

Suites may assert. A `#!must <text>` or `#!must-not <text>` line applies to
the next command and fails the run (non-zero exit) if unmet — otherwise a
suite only prints, and a run where every step errored still exits 0.

Matching is a plain SUBSTRING test, not a regex: `#!must-not ^[0-4]$` looks
for those literal characters, never matches, and so passes vacuously. To
assert a numeric condition, make the command say it in words —
`test $(...) -gt 10 && echo MANY || echo TOO-FEW` with `#!must MANY`.

Any sentinel length works. Until 2026-09-25, one of 8 or more characters
was eaten: command echo is stripped from the head of the output, and a
sentinel is by definition a substring of its own command (`LATE-COUNT-3`,
`MEASURED` and `USERNS-OK` each cost a run). The stripper now reassembles
the echo and stops at its end; see _clean.

It can also move the mouse. A virtio tablet takes absolute pointer events
over QMP (`input-send-event`), so `#!pointer click X Y`,
`#!pointer move X Y` and `#!pointer drag X1 Y1 X2 Y2` in a suite act at
those output pixels, on the 1280x800 screen. A coordinate may be `$NAME`,
which is read from the guest shell, so a suite can aim at a window it has
just measured. Pointer bugs like #7's
press/release double-dispatch no longer need a human at `make run`.

And type: the virtio keyboard takes key events the same way. `#!type TEXT`
types the rest of the line one key at a time (letters, digits, space and a
little punctuation), and `#!key QCODE...` presses one chord of QEMU qcodes,
such as `#!key ret`, `#!key esc` or `#!key ctrl a`. `#!keydown QCODE...`
and `#!keyup QCODE...` press and release keys separately, to hold a modifier
across steps (`#!keydown alt`, `#!key tab`, `#!keyup alt` is Alt+Tab with
Alt held in between, which opens the switcher). Keys go wherever Evo
has put keyboard focus, so click into a field first.

It can hear, too. The guest's sound card (an ICH9 HDA, as in emulate.sh)
plays into a WAV file on the host. `#!listen <name>` measures whatever the
guest played since the previous listen, saves it as
target/serial-shots/<name>.wav, and reports its peak and RMS level. The
report says `silent` when nothing got through, so `#!must-not silent`
asserts that something did.

Files move through two virtio disks rather than the console. They are
faster, and nothing the tty or a stray kernel message does can corrupt them:

  * `--push LOCAL:REMOTE` files (a directory is its files, under REMOTE, so a
    built Bloom goes in with
    `--push ../pyxis-experience/dist:/usr/share/geckoOS/apps/bloom/webroot`)
    are packed into a tar on a read-only disk
    (serial `pyxisin`). geckoOS-boot extracts it over the rootfs before it
    starts a single daemon, so a pushed binary (`target/release/evo:/usr/bin/evo`)
    is what the whole boot runs. There is no sudo rootfs rebuild per
    iteration. An image built before that hook existed gets the files after
    boot instead, with a warning, and daemons already running keep their old
    binaries.
  * Pulls (the screenshots) go the other way on a writable disk (serial
    `pyxisout`). The guest writes a tar to the raw device and syncs, and the
    host reads it back. Both directions are sha256-checked.

Usage:
    scripts/serial-test.py --script scripts/serial-tests/alpha4-ipk-e2e.txt
    scripts/serial-test.py -c 'uname -a' -c 'opkg list-installed | head'
    scripts/serial-test.py --push local.ipk:/tmp/local.ipk -c 'ls -l /tmp'
    scripts/serial-test.py --push target/release/evo:/usr/bin/evo --screenshot desk.png
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import io
import os
import shlex
import shutil
import struct
import subprocess
import sys
import re
import tarfile
import tempfile
import time
from pathlib import Path

try:
    import pexpect
except ImportError:
    sys.exit("pexpect is required: pip install pexpect")

PROJECT_ROOT = Path(__file__).resolve().parent.parent
IMAGE = PROJECT_ROOT / "target/build/images/geckoOS.img"
SHOT_DIR = PROJECT_ROOT / "target/serial-shots"

# virtio-blk serials of the two transfer disks, which the guest finds under
# /sys/block/vd*/serial. geckoOS-boot applies XFER_IN before starting any
# daemon (see create-rootfs.sh), and XFER_OUT is scratch for pulls.
XFER_IN_SERIAL = "pyxisin"
XFER_OUT_SERIAL = "pyxisout"
XFER_OUT_BYTES = 256 * 1024 * 1024  # sparse, so free until written

# The guest's one output (the virtio-vga-gl xres/yres below), which is also
# the space `#!pointer` coordinates are in.
SCREEN_W, SCREEN_H = 1280, 800
# The virtio tablet's absolute axis range (QEMU's INPUT_EVENT_ABS_MAX).
ABS_RANGE = 0x8000
# The QEMU qcodes `#!type` uses for characters other than a-z and 0-9, as
# (qcode, shifted) on a US layout.
TYPE_QCODES = {
    " ": ("spc", False), "-": ("minus", False), "_": ("minus", True),
    ".": ("dot", False), ",": ("comma", False), "/": ("slash", False),
    ":": ("semicolon", True), "'": ("apostrophe", False), '"': ("apostrophe", True),
}

# What QEMU's wav audiodev records: fixed here, so #!listen knows how to
# read it without trusting a header QEMU only finalises when it exits.
AUDIO_RATE, AUDIO_CHANNELS, AUDIO_WAV_HEADER = 44100, 2, 44
# Quieter than this, in dBFS, and a clip counts as silence.
SILENCE_DBFS = -60.0

# Markers are split at the call site so the shell's own echo of the command
# cannot match them. See the module docstring.
BEGIN = "PYXBEGIN"
END = "PYXEND"

# CSI/OSC sequences, including readline's bracketed-paste toggles.
ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07")
PROMPT_RE = re.compile(r"^[\w.-]+@\(?[\w.()-]*\)?:[^#$]*[#$]\s?")
# A prompt followed by the echo of a split marker (`echo PYX''END`), found
# anywhere in a line. See _clean for why it can sit mid-line.
# Anchored on `root@`, the serial shell's user: a generic `[\w.-]+@` also
# swallowed the tail of output it followed ("abcroot@..." lost the "abc").
MARKER_ECHO_RE = re.compile(r"root@\(?[\w.()-]*\)?:[^#$]*[#$]\s?echo \w+''\w+")

# Run once, before the first command, in the guest's shell. The runtime dir
# is echo-cli's socket path, and PATH covers the Pyxis binaries, which are not
# on BusyBox's default PATH. `pyxdev <serial>` names a transfer disk's device
# node.
GUEST_ENV = (
    # 1000 columns, so readline never wraps a command's echo. At the serial
    # default of 80 it wrapped with a `\r` and redrew the character at the
    # wrap, and the echo then no longer spelled the command (see _clean).
    "stty cols 1000 rows 50; "
    "export XDG_RUNTIME_DIR=/run/user/1000 PATH=$PATH:/usr/bin:/usr/sbin:/usr/local/bin; "
    "pyxdev() { for b in /sys/block/vd*; do "
    '[ "$(cat $b/serial 2>/dev/null)" = "$1" ] && echo /dev/${b##*/}; done; }'
)


def sha256_of(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def write_overlay_tar(dest: Path, pushes: list[tuple[Path, str]]) -> None:
    """Pack the --push files into a tar laid out from /, owned by root.

    Built by hand rather than with TarInfo-from-lstat, so that a symlinked
    source contributes its target's bytes and a hardlinked one (cargo
    hardlinks target/release binaries) is never recorded as a link.
    """
    with tarfile.open(dest, "w", format=tarfile.GNU_FORMAT) as tar:
        for local, remote in pushes:
            st = local.stat()
            info = tarfile.TarInfo(remote.lstrip("/"))
            info.size = st.st_size
            info.mode = st.st_mode & 0o7777
            info.mtime = int(st.st_mtime)
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            with local.open("rb") as fh:
                tar.addfile(info, fh)


def describe_png(data: bytes) -> str:
    """Size, and with Pillow, how varied the picture is.

    One colour across the whole frame is how a capture that silently copied
    nothing looks, so that case says `uniform`, which a suite can assert
    against with `#!must-not uniform`. (The stub this project shipped until
    2026-09-25 filled every frame with zeros.) Anything more than that needs
    looking at.
    """
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        return "not a PNG"
    width, height = struct.unpack(">II", data[16:24])
    try:
        from PIL import Image
    except ImportError:
        return f"{width}x{height} (install Pillow for colour stats)"
    img = Image.open(io.BytesIO(data)).convert("RGB")
    colors = img.getcolors(maxcolors=width * height)
    count, (r, g, b) = max(colors)
    report = (f"{width}x{height}, {len(colors)} colours, "
              f"#{r:02x}{g:02x}{b:02x} covers {count / (width * height):.0%}")
    return report + (" — uniform" if len(colors) == 1 else "")


def image_cmdline(image: Path) -> str:
    """The kernel command line the image boots with, for `--kernel`.

    `--kernel` boots a kernel directly (QEMU -kernel), so it skips GRUB and
    the command line GRUB would have passed. The root= in that command line
    must name THIS image's root filesystem. That UUID changes with every
    package.sh run, so it is read from the partition itself. The GRUB config
    that boots is on the vfat ESP, which debugfs cannot read, and the copy
    in the root fs's /boot/grub is a stale leftover. Trusting it booted into
    the initramfs's "could not resolve root device" rescue shell (found
    2026-09-25).
    """
    import json as _json
    table = _json.loads(subprocess.run(["sfdisk", "--json", str(image)], capture_output=True,
                                       text=True, check=True).stdout)["partitiontable"]
    sector = table.get("sectorsize", 512)
    for part in table["partitions"]:
        probe = subprocess.run(
            ["blkid", "--probe", "--offset", str(part["start"] * sector), "-o", "export", str(image)],
            capture_output=True, text=True).stdout
        tags = dict(line.split("=", 1) for line in probe.splitlines() if "=" in line)
        if tags.get("TYPE") == "ext4" and tags.get("UUID"):
            return f"root=UUID={tags['UUID']} rw console=ttyS0,115200 console=tty0 loglevel=7"
    sys.exit(f"--kernel: no ext4 root partition found in {image}")


def find_ovmf() -> tuple[str, str]:
    for base in ("/usr/share/OVMF", "/usr/share/ovmf"):
        for variant in ("_4M", ""):
            code = f"{base}/OVMF_CODE{variant}.fd"
            varsf = f"{base}/OVMF_VARS{variant}.fd"
            if os.path.isfile(code) and os.path.isfile(varsf):
                return code, varsf
    sys.exit("OVMF firmware not found (apt install ovmf)")


def kvm_args() -> list[str]:
    if os.access("/dev/kvm", os.R_OK):
        return ["-enable-kvm", "-cpu", "host"]
    print("[warn] /dev/kvm unreadable — software emulation, expect a slow boot")
    return []


class Guest:
    def __init__(self, image: Path, overlay: bool, memory: int, logfile,
                 pushes: list[tuple[Path, str]], kernel: Path | None = None,
                 initrd: Path | None = None, append: str = "") -> None:
        # Next to the image, NOT tempfile.mkdtemp(). /tmp is a 16 GB tmpfs on
        # this machine — i.e. RAM — so putting a qcow2 overlay there means the
        # guest's writes are backed by the host's memory, competing with the
        # 4 GB the guest itself wants. Learned the hard way: image copies and
        # overlays left in /tmp reached 13 GB of a 16 GB tmpfs and took the
        # host down to 3 GB free, at which point subprocesses started failing
        # outright. The image directory is on real disk and already holds
        # multi-GB artefacts.
        scratch = image.parent / ".serial-test"
        scratch.mkdir(parents=True, exist_ok=True)
        self.tmp = Path(tempfile.mkdtemp(prefix="run-", dir=scratch))
        self.logfile = logfile

        if overlay:
            self.disk = self.tmp / "overlay.qcow2"
            subprocess.run(
                ["qemu-img", "create", "-f", "qcow2", "-F", "raw",
                 "-b", str(image), str(self.disk)],
                check=True, stdout=subprocess.DEVNULL,
            )
            drive = f"file={self.disk},format=qcow2,media=disk,if=none,id=root"
        else:
            drive = f"file={image},format=raw,media=disk,if=none,id=root"

        self.pushes = pushes
        self.xfer_out = self.tmp / "xfer-out.img"
        with self.xfer_out.open("wb") as fh:
            fh.truncate(XFER_OUT_BYTES)
        xfer = ["-drive", f"file={self.xfer_out},if=none,id=xout,format=raw",
                "-device", f"virtio-blk-pci,drive=xout,serial={XFER_OUT_SERIAL}"]
        if pushes:
            xfer_in = self.tmp / "xfer-in.img"
            write_overlay_tar(xfer_in, pushes)
            xfer += ["-drive", f"file={xfer_in},if=none,id=xin,format=raw,readonly=on",
                     "-device", f"virtio-blk-pci,drive=xin,serial={XFER_IN_SERIAL}"]

        # The sound card, recorded to a file (see #!listen).
        self.audio = self.tmp / "audio.wav"
        self.audio_mark = AUDIO_WAV_HEADER
        sound = ["-audiodev", f"wav,id=snd0,path={self.audio},out.frequency={AUDIO_RATE},"
                              f"out.channels={AUDIO_CHANNELS},out.format=s16",
                 "-device", "ich9-intel-hda",
                 "-device", "hda-output,audiodev=snd0"]

        self.qmp_path = self.tmp / "qmp.sock"
        code, vars_template = find_ovmf()
        nvram = self.tmp / "OVMF_VARS.fd"
        shutil.copy(vars_template, nvram)

        self.argv = [
            "qemu-system-x86_64",
            "-name", "pyxis-serial-test",
            "-machine", "q35",
            "-m", str(memory),
            "-smp", "4",
            *kvm_args(),
            "-drive", f"if=pflash,format=raw,readonly=on,file={code}",
            "-drive", f"if=pflash,format=raw,file={nvram}",
            "-drive", drive,
            # bootindex: the firmware boots the root disk, never a transfer
            # disk or the NIC, whatever PCI slots the devices land in. See
            # emulate.sh, where a moved slot left the VM at a PXE prompt.
            "-device", "virtio-blk-pci,drive=root,bootindex=0",
            *xfer,
            # egl-headless + virtio-vga-gl: Evo will not start without a GL
            # backend, and a guest whose compositor died answers every Echo
            # call with a timeout that looks like an IPC bug.
            "-vga", "none",
            "-device", f"virtio-vga-gl,xres={SCREEN_W},yres={SCREEN_H},edid=on,max_outputs=1",
            "-display", "egl-headless,gl=on",
            "-device", "virtio-keyboard-pci",
            # Absolute pointer for `#!pointer` (see Guest.pointer). Evo sees
            # it through libinput like any mouse; nothing moves until a suite
            # sends an event.
            "-device", "virtio-tablet-pci",
            "-netdev", "user,id=net0",
            "-device", "e1000,netdev=net0",
            *sound,
            "-serial", "stdio",
            "-monitor", "none",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-no-reboot",
            "-rtc", "base=localtime,driftfix=slew",
        ]
        if kernel:
            # Boot this kernel instead of the image's, with the image's own
            # command line. A kernel change can then be tested without a
            # rootfs rebuild. The image's modules still come from the image,
            # so this suits a kernel whose drivers are built in (=y).
            self.argv += ["-kernel", str(kernel), "-append", f"{image_cmdline(image)} {append}".strip()]
            if initrd:
                self.argv += ["-initrd", str(initrd)]
        self.child: pexpect.spawn | None = None

    def boot(self, timeout: int) -> None:
        print(f"[boot] {' '.join(self.argv[:6])} …")
        self.child = pexpect.spawn(
            self.argv[0], self.argv[1:], encoding="utf-8",
            codec_errors="replace", timeout=timeout, logfile=self.logfile,
            dimensions=(50, 200),
        )
        # Nothing the boot scripts print reaches ttyS0 (see docstring), so
        # there is no banner to wait for. Poke the shell until it answers.
        deadline = time.time() + timeout
        probe = 0
        while time.time() < deadline:
            probe += 1
            self.child.sendline(f"echo SHELL''UP{probe}")
            try:
                self.child.expect(f"SHELLUP{probe}", timeout=10)
                print(f"[boot] serial shell responded after ~{probe * 10}s")
                return
            except pexpect.TIMEOUT:
                continue
            except pexpect.EOF:
                sys.exit("[fail] QEMU exited during boot — see the transcript above")
        sys.exit(f"[fail] no serial shell within {timeout}s")

    def run(self, command: str, timeout: int = 60) -> str:  # noqa: D401
        """Run one command, return everything it printed."""
        assert self.child is not None
        self.child.sendline(f"echo {BEGIN[:3]}''{BEGIN[3:]}")
        self.child.expect(BEGIN, timeout=timeout)
        self.child.sendline(command)
        self.child.sendline(f"echo {END[:3]}''{END[3:]}")
        try:
            self.child.expect(END, timeout=timeout)
        except pexpect.TIMEOUT:
            return f"<TIMEOUT after {timeout}s>\n{self.child.before}"
        return self._clean(self.child.before or "", command)

    @staticmethod
    def _clean(raw: str, command: str) -> str:
        """Strip the terminal noise a bash-on-serial session wraps output in.

        Readline emits bracketed-paste toggles (\\e[?2004h/l) around every
        line and echoes both the prompt and the command, none of which is the
        command's output.
        """
        text = ANSI_RE.sub("", raw)
        # The console echoes the command back, and WRAPS it at the terminal
        # width — so the echo arrives as several lines, none of which equals
        # the command. Comparing whole lines therefore leaves fragments in
        # the "output", which breaks any assertion whose text also appears
        # in the command itself (asserting `must-not unavailable` on a
        # `grep unavailable ...` line was the case that found this).
        # Matching on the whitespace-stripped command handles the wrap.
        flat_cmd = "".join(command.split())
        lines: list[str] = []
        # The echo is always at the HEAD of the stream, so only strip there.
        # Matching echo fragments anywhere ate real output: a command like
        # `... && echo SEES-IN-PROCESS-SERVICES` has its own result as a
        # substring of itself, so the result vanished and the assertion
        # failed on a command that had worked. Once a line is not part of
        # the echo, everything after it is output, verbatim.
        in_echo = True
        # Readline's echo (prompt + command), reassembled: once it spells the
        # whole command, the echo is over, and every later line is output, even
        # one that is a substring of the command. A sentinel like
        # `echo USERNS-OK` is exactly that, and the fragment rule below used to
        # eat any of 8+ characters. It starts at a PROMPT line only. The tty
        # also echoes the typed command before readline takes over, and that
        # raw copy must not count as the end of the echo.
        echoed: str | None = None
        for ln in text.splitlines():
            ln = ln.rstrip("\r").rstrip()
            if not ln:
                continue
            # The split-marker echo, e.g. "echo PYX''END" — these bracket
            # the command and can appear at either end. Output that ends
            # WITHOUT a newline (`... | tr -d '\n'`, `printf x`) runs straight
            # into the next prompt and that echo, all on one line: keep what
            # came before the prompt. Dropping the whole line is how such
            # output used to vanish and fail its assertion.
            if "''" in ln:
                marker = MARKER_ECHO_RE.search(ln)
                head = ln[:marker.start()].rstrip() if marker else ""
                if head and not PROMPT_RE.match(head):
                    lines.append(head)
                    in_echo = False
                continue
            if in_echo:
                # 'root@(none):/# uname -a' — prompt plus the echoed command.
                prompt = PROMPT_RE.match(ln)
                if prompt:
                    echoed = "".join(ln[prompt.end():].split())
                    in_echo = echoed != flat_cmd
                    continue
                flat = "".join(ln.split())
                # Before readline's echo, the tty's raw copy of what was typed.
                # Its first letter or so can be missing: it lands in the
                # middle of readline's `\e[?2004h`, and ANSI_RE takes the
                # letter as the escape's final byte ("uname -r" arrives as
                # "name -r", and the stray "h" joins the prompt as "hroot@").
                if echoed is None and flat and flat_cmd.endswith(flat):
                    continue
                # A wrapped continuation of the echo. Readline redraws the
                # character it wrapped at (`--user --\r-map-root-user`), so
                # allow the continuation to repeat the echo's last few.
                continuation = echoed is not None and next(
                    (flat[k:] for k in range(0, 4)
                     if flat and echoed.endswith(flat[:k]) and flat_cmd.startswith(echoed + flat[k:])),
                    None)
                if continuation:
                    echoed += continuation
                    in_echo = echoed != flat_cmd
                    continue
                # A whole echo, or a wrapped fragment of one. Length guard
                # so a short genuine result ("16") is not eaten.
                if flat and len(flat) >= 8 and flat in flat_cmd:
                    continue
                if ln.strip() == command.strip():
                    continue
                in_echo = False
            lines.append(ln)
        return "\n".join(lines).strip()

    def verify_pushes(self) -> list[str]:
        """Check the --push files landed, and return any failures.

        geckoOS-boot normally applied them before any daemon started. An
        image built before that hook existed did not, so apply them now, and
        say what that costs.
        """
        if not self.pushes:
            return []
        applied = self.run("cat /run/pyxis-xfer-applied 2>/dev/null")
        if any(remote.lstrip("/") not in applied for _, remote in self.pushes):
            print("[push] \033[33mthis image has no boot-time overlay hook, so the "
                  "files go in now, after boot. A daemon that is already running "
                  "keeps its old binary. Rebuild the rootfs to fix that.\033[0m")
            # `tar=$?`, not a word like "extracted": a sentinel of 8+
            # characters is a substring of its own command, and the echo
            # stripper eats it (see the module docstring).
            print("[push] " + self.run(
                f'tar -xpf "$(pyxdev {XFER_IN_SERIAL})" -C / 2>&1; echo tar=$?'))
        failures = []
        for local, remote in self.pushes:
            want = sha256_of(local.read_bytes())
            got = self.run(f"sha256sum {shlex.quote(remote)} 2>/dev/null | cut -c1-64")
            if want in got:
                print(f"[push] {local} -> {remote} ok sha256={want[:16]}…")
            else:
                failures.append(f"{remote}: host={want} guest={got or 'missing'}")
        return failures

    def pull(self, remote: str) -> bytes:
        """Copy a guest file out through the transfer disk, sha256-checked.

        The guest writes a tar straight onto the raw device and syncs. The
        host can then read the disk image while QEMU is still running,
        because QEMU has already written the guest's writes through to the
        file. tarfile stops at the end-of-archive marker, so whatever a
        longer, earlier pull left behind it is never read.
        """
        member = remote.lstrip("/")
        out = self.run(
            f'D=$(pyxdev {XFER_OUT_SERIAL}); [ -b "$D" ] && '
            f"tar -cf \"$D\" -C / {shlex.quote(member)} && sync && "
            f"sha256sum {shlex.quote(remote)} | cut -c1-64", timeout=120)
        match = re.search(r"\b[0-9a-f]{64}\b", out)
        if not match:
            raise RuntimeError(f"guest could not archive {remote}: {out or '(no output)'}")
        with self.xfer_out.open("rb") as fh, tarfile.open(fileobj=fh, mode="r:") as tar:
            extracted = tar.extractfile(tar.getmember(member))
            data = extracted.read() if extracted else b""
        if sha256_of(data) != match.group(0):
            raise RuntimeError(f"{remote}: sha256 mismatch after pull "
                               f"(guest {match.group(0)[:16]}…, host {sha256_of(data)[:16]}…)")
        return data

    def screenshot(self, dest: Path, grim_args: str = "") -> tuple[bool, str]:
        """Capture what Evo composited, via grim, into `dest` as a PNG.
        `grim_args` go before the filename, e.g. `-c` to include the cursor
        or `-g "X,Y WxH"` for a region.

        grim talks wlr-screencopy to Evo (evo/src/enhancements/screencopy.rs),
        which draws the current frame's elements again, offscreen, and reads
        them back. Returns (ok, report). The report ends in "uniform" when
        every pixel is the same colour, which is how an empty capture looks.
        """
        # grim allocates its buffer with shm_open(), which needs /dev/shm.
        # Images from before alpha 5's rcS fix never mounted one, and grim
        # dies there with a bare "failed to create buffer". Mount one for the
        # capture, and say so: it is a bug in the image, not in the harness.
        if "NO-SHM" in self.run("test -d /dev/shm || echo NO-SHM"):
            print("[shot] \033[33mthe guest has no /dev/shm (image predates the "
                  "rcS fix); mounting one for this capture\033[0m")
            self.run("mkdir -p /dev/shm && mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs /dev/shm")
        out = self.run(
            "command -v grim >/dev/null || echo NO-GRIM; rm -f /tmp/pyxshot.png; "
            f"WAYLAND_DISPLAY=wayland-0 grim {grim_args} /tmp/pyxshot.png 2>&1; echo grim=$?")
        if "NO-GRIM" in out:
            return False, "grim is not in this image: rebuild the rootfs (create-rootfs.sh installs it)"
        if "grim=0" not in out:
            return False, f"grim failed: {out}"
        try:
            data = self.pull("/tmp/pyxshot.png")
        except (RuntimeError, KeyError, tarfile.TarError) as err:
            return False, f"pull failed: {err}"
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(data)
        return True, f"{dest} — {describe_png(data)}"

    def qmp(self, **cmd) -> dict:
        """One QMP command over the monitor socket."""
        import json as _json
        import socket as _socket

        with _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM) as sock:
            sock.settimeout(20)
            sock.connect(str(self.qmp_path))
            fh = sock.makefile("rw", encoding="utf-8", newline="\n")
            fh.readline()  # greeting
            for payload in ({"execute": "qmp_capabilities"}, cmd):
                fh.write(_json.dumps(payload) + "\n")
                fh.flush()
                while True:
                    line = fh.readline()
                    if not line:
                        return {"error": "qmp closed"}
                    msg = _json.loads(line)
                    if "event" in msg:
                        continue  # async event, not our reply
                    break
            return msg

    def pointer(self, spec: str) -> tuple[bool, str]:
        """Drive the virtio tablet: `move X Y`, `click X Y` or
        `drag X1 Y1 X2 Y2`, in output pixels. Returns (ok, report).
        `down X Y` moves and presses, `up` releases: with `move` between
        them, a suite can look at a window while it is held (a move grab).

        A drag moves first and pauses before pressing. A press on Evo's
        frame hit-tests against where the pointer was last *seen*, and a grab
        a press starts is installed from an idle callback, so each step has
        to land in its own compositor dispatch or it lands on the wrong thing.
        """
        words = spec.split()
        # `$NAME` is read from the guest shell, so a suite can compute a
        # coordinate (say, from `evo.ListWindows`) and then point at it.
        for i, word in enumerate(words):
            if word.startswith("$") and re.fullmatch(r"\$\w+", word):
                words[i] = self.run(f'echo "{word}"').strip()
        try:
            action, nums = words[0], [int(n) for n in words[1:]]
        except (IndexError, ValueError):
            return False, f"bad pointer step {spec!r} (resolved: {' '.join(words)})"
        want = {"move": 2, "click": 2, "drag": 4, "down": 2, "up": 0}.get(action)
        if want != len(nums):
            return False, f"bad pointer step {spec!r}: move/click/down X Y, drag X1 Y1 X2 Y2, up"

        def at(x: int, y: int) -> list[dict]:
            # libinput maps value v to v * size / ABS_RANGE; the ceiling puts
            # the pointer on pixel (x, y) itself, not the one before it.
            return [{"type": "abs", "data": {"axis": axis, "value": min(-(-v * ABS_RANGE // size), ABS_RANGE - 1)}}
                    for axis, v, size in (("x", x, SCREEN_W), ("y", y, SCREEN_H))]

        def button(down: bool) -> list[dict]:
            return [{"type": "btn", "data": {"down": down, "button": "left"}}]

        steps: list[list[dict]] = [] if action == "up" else [at(nums[0], nums[1])]
        if action in ("click", "drag", "down"):
            steps.append(button(True))
        if action == "drag":
            (x1, y1, x2, y2), n = nums, 12
            steps += [at(x1 + (x2 - x1) * i // n, y1 + (y2 - y1) * i // n) for i in range(1, n + 1)]
        if action in ("click", "drag", "up"):
            steps.append(button(False))

        for events in steps:
            res = self.qmp(execute="input-send-event", arguments={"events": events})
            if "error" in res:
                return False, f"pointer {spec}: {res['error'].get('desc', res['error'])}"
            time.sleep(0.1)
        return True, f"pointer {spec}: ok"

    def key(self, *qcodes: str) -> tuple[bool, str]:
        """Press `qcodes` together as one chord, then release them."""
        def event(code: str, down: bool) -> dict:
            return {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": code}}}

        for events in ([event(c, True) for c in qcodes], [event(c, False) for c in reversed(qcodes)]):
            res = self.qmp(execute="input-send-event", arguments={"events": events})
            if "error" in res:
                return False, f"key {' '.join(qcodes)}: {res['error'].get('desc', res['error'])}"
            time.sleep(0.03)
        return True, f"key {' '.join(qcodes)}: ok"

    def key_hold(self, down: bool, *qcodes: str) -> tuple[bool, str]:
        """Press (`down`) or release `qcodes` and leave them so: a held
        modifier, as Alt+Tab's switcher needs. `#!key` taps a chord whole."""
        events = [{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": c}}}
                  for c in (qcodes if down else reversed(qcodes))]
        res = self.qmp(execute="input-send-event", arguments={"events": events})
        verb = "keydown" if down else "keyup"
        if "error" in res:
            return False, f"{verb} {' '.join(qcodes)}: {res['error'].get('desc', res['error'])}"
        time.sleep(0.03)
        return True, f"{verb} {' '.join(qcodes)}: ok"

    def type_text(self, text: str) -> tuple[bool, str]:
        """Type `text` on the virtio keyboard, one key at a time."""
        for ch in text:
            if ch.isascii() and ch.isalnum():
                chord = ("shift", ch.lower()) if ch.isupper() else (ch,)
            elif ch in TYPE_QCODES:
                code, shifted = TYPE_QCODES[ch]
                chord = ("shift", code) if shifted else (code,)
            else:
                return False, f"type: no key for {ch!r}"
            ok, out = self.key(*chord)
            if not ok:
                return False, out
        return True, f"typed {text!r}"

    def listen(self, dest: Path) -> tuple[bool, str]:
        """Save and measure what the guest played since the last listen.

        Reads the raw s16 samples QEMU appended to its WAV recording after
        the previous mark, writes them to `dest` as a proper WAV, and reports
        the level. The report says `silent` below SILENCE_DBFS.
        """
        import math
        import wave

        try:
            data = self.audio.read_bytes()[self.audio_mark:]
        except FileNotFoundError:
            return False, "no audio recording (QEMU has not opened the sound card)"
        data = data[: len(data) - len(data) % (2 * AUDIO_CHANNELS)]
        self.audio_mark += len(data)
        dest.parent.mkdir(parents=True, exist_ok=True)
        with wave.open(str(dest), "wb") as out:
            out.setnchannels(AUDIO_CHANNELS)
            out.setsampwidth(2)
            out.setframerate(AUDIO_RATE)
            out.writeframes(data)
        samples = struct.unpack(f"<{len(data) // 2}h", data) if data else ()
        seconds = len(samples) / AUDIO_CHANNELS / AUDIO_RATE
        peak = max((abs(v) for v in samples), default=0)
        rms = math.sqrt(sum(v * v for v in samples) / len(samples)) if samples else 0.0

        def dbfs(v: float) -> float:
            return 20 * math.log10(v / 32768) if v > 0 else float("-inf")

        report = (f"{dest} — {seconds:.1f} s, peak {dbfs(peak):.1f} dBFS, "
                  f"rms {dbfs(rms):.1f} dBFS")
        return True, report + (" — silent" if dbfs(peak) < SILENCE_DBFS else "")

    def shutdown(self) -> None:
        if self.child and self.child.isalive():
            self.child.sendline("busybox poweroff -f")
            try:
                self.child.expect(pexpect.EOF, timeout=30)
            except (pexpect.TIMEOUT, pexpect.EOF):
                self.child.terminate(force=True)
        shutil.rmtree(self.tmp, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--image", type=Path, default=IMAGE)
    ap.add_argument("-c", "--command", action="append", default=[],
                    help="command to run (repeatable)")
    ap.add_argument("--script", type=Path,
                    help="file of commands, one per line; # comments and blanks skipped")
    ap.add_argument("--push", action="append", default=[], metavar="LOCAL:REMOTE",
                    help="lay a file (or every file of a directory) over the rootfs "
                         "before the guest boots its daemons")
    ap.add_argument("--boot-timeout", type=int, default=300)
    ap.add_argument("--timeout", type=int, default=60, help="per-command timeout")
    ap.add_argument("--settle", type=int, default=25,
                    help="seconds to wait after the shell appears, for daemons to come up")
    ap.add_argument("--memory", type=int, default=4096,
                    help="guest RAM in MB, matching emulate.sh's default. "
                         "Eight concurrent vela apps need ~2.8GB; at 2048 the "
                         "late ones stay alive but never map a surface.")
    ap.add_argument("--kernel", type=Path,
                    help="boot this kernel directly (QEMU -kernel) with the image's own cmdline")
    ap.add_argument("--initrd", type=Path, default=PROJECT_ROOT / "target/boot/init.cpio.gz",
                    help="initramfs for --kernel (default: target/boot/init.cpio.gz)")
    ap.add_argument("--append", default="",
                    help="extra kernel args for --kernel, e.g. a trailing console=ttyS0,115200 "
                         "to put the initramfs's own output on serial")
    ap.add_argument("--no-overlay", action="store_true",
                    help="write directly to the image instead of a qcow2 overlay")
    ap.add_argument("--transcript", type=Path, help="write the raw serial log here")
    ap.add_argument("--pull", action="append", default=[], metavar="REMOTE:LOCAL",
                    help="copy a guest file out after the commands run (e.g. a log)")
    ap.add_argument("--screenshot", type=Path,
                    help="capture the screen (grim) to this PNG after the commands run")
    ap.add_argument("--shot-dir", type=Path, default=SHOT_DIR,
                    help="where `#!shot <name>` lines save <name>.png")
    ap.add_argument("--keep-running", action="store_true",
                    help="leave the guest up and hand over an interactive console")
    args = ap.parse_args()

    if not args.image.exists():
        sys.exit(f"image not found: {args.image}\nRun: make build-release")

    commands = list(args.command)
    if args.script:
        for line in args.script.read_text().splitlines():
            line = line.strip()
            # `#!must <text>` / `#!must-not <text>` assert on the NEXT
            # command's output, and `#!shot <name>` is a step of its own;
            # every other `#` line is a comment.
            if line.startswith("#!"):
                # A misspelt directive must not quietly become a no-op or,
                # worse, a vacuous must-not (anything not "must" was one).
                kind = line[2:].split(" ", 1)[0]
                if kind not in ("must", "must-not", "shot", "pointer", "listen", "type", "key",
                                "keydown", "keyup"):
                    sys.exit(f"{args.script}: unknown directive {line!r}")
                commands.append(line)
            elif line and not line.startswith("#"):
                commands.append(line)
    # Refuse to boot for a line bash cannot parse. An unclosed quote leaves
    # the guest shell waiting for more input, and the whole run then hangs
    # into timeouts, two minutes of boot later.
    for cmd in commands:
        if cmd.startswith("#!"):
            continue
        check = subprocess.run(["bash", "-n"], input=cmd + "\n", text=True,
                               capture_output=True)
        if check.returncode != 0:
            sys.exit(f"bash cannot parse this command:\n  {cmd}\n{check.stderr.strip()}")

    if not (commands or args.keep_running or args.screenshot or args.pull):
        sys.exit("nothing to do: pass -c/--script/--screenshot/--pull (or --keep-running)")

    # One VM at a time on this host. Each takes 4 GB and a GPU context, and
    # several sessions may run suites at once; polling for "no qemu running"
    # races and starves whichever poller waits longer. The kernel drops the
    # lock when this process exits, however it exits.
    lock_path = PROJECT_ROOT / "target" / "serial-vm.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    lock_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print(f"waiting for another serial-test run to finish ({lock_path}) ...", flush=True)
        fcntl.flock(lock_fd, fcntl.LOCK_EX)

    pushes = []
    for spec in args.push:
        local, _, remote = spec.partition(":")
        source = Path(local)
        if not remote.startswith("/") or not (source.is_file() or source.is_dir()):
            sys.exit(f"--push wants LOCAL_FILE_OR_DIR:/ABSOLUTE/REMOTE, got {spec!r}")
        if source.is_dir():
            # A directory is its files, laid out under REMOTE (a built web
            # app, say: ../pyxis-experience/dist:/usr/share/geckoOS/apps/bloom/webroot).
            # Source maps are only for a debugger and are the bulk of a build.
            for path in sorted(p for p in source.rglob("*") if p.is_file() and p.suffix != ".map"):
                pushes.append((path, remote.rstrip("/") + "/" + path.relative_to(source).as_posix()))
        else:
            pushes.append((source, remote))

    log = args.transcript.open("w") if args.transcript else None
    guest = Guest(args.image, not args.no_overlay, args.memory, log, pushes,
                  args.kernel, args.initrd if args.kernel else None, args.append)
    failures = 0
    try:
        guest.boot(args.boot_timeout)
        # Any shell answers the probe, including the initramfs's rescue
        # shell after "could not resolve root device". Make sure this is the
        # Pyxis root before a suite runs in the wrong system and "fails".
        if "PYX-OS" not in guest.run("test -x /usr/sbin/geckoOS-boot && echo PYX-OS"):
            sys.exit("[fail] booted, but not into the Pyxis root: no /usr/sbin/geckoOS-boot "
                     "(an initramfs rescue shell? see --transcript)")
        guest.run(GUEST_ENV)
        if args.settle:
            print(f"[boot] settling {args.settle}s for daemons")
            guest.run(f"sleep {args.settle}", timeout=args.settle + 30)

        for failure in guest.verify_pushes():
            print(f"[push] \033[31m✗ {failure}\033[0m")
            failures += 1

        # Directives accumulated from `#!must` / `#!must-not` comment lines,
        # applied to the next real command. Without them these suites only
        # *print* — a run where every step errored still exited 0, which is
        # how a stale hardcoded filename in a suite went unnoticed.
        pending: list[tuple[str, str]] = []
        for cmd in commands:
            # A step that could not run has no output to assert on. Without
            # this, a failed shot's error text sailed past `#!must-not
            # uniform`, and printed a green tick for a picture that was never
            # taken.
            step_ok = True
            if cmd.startswith("#!shot"):
                # A step, not an assertion. Its report is its output, so a
                # `#!must-not uniform` above it asserts on the picture.
                # `#!shot <name> [grim args]`, e.g. `#!shot desk-cursor -c`.
                name, _, grim_args = (cmd[len("#!shot"):].strip() or "shot").partition(" ")
                print(f"\n\033[1m[shot] {name}\033[0m")
                step_ok, out = guest.screenshot(args.shot_dir / f"{name}.png", grim_args)
                print(out)
                if not step_ok:
                    failures += 1
            elif cmd.startswith("#!listen"):
                name = cmd[len("#!listen"):].strip() or "listen"
                print(f"\n\033[1m[listen] {name}\033[0m")
                step_ok, out = guest.listen(args.shot_dir / f"{name}.wav")
                print(out)
                if not step_ok:
                    failures += 1
            elif cmd.startswith("#!pointer"):
                step_ok, out = guest.pointer(cmd[len("#!pointer"):].strip())
                print(f"\n\033[1m[{out}]\033[0m")
                if not step_ok:
                    failures += 1
            elif cmd.startswith("#!type "):
                step_ok, out = guest.type_text(cmd[len("#!type "):])
                print(f"\n\033[1m[{out}]\033[0m")
                if not step_ok:
                    failures += 1
            elif cmd.startswith(("#!keydown ", "#!keyup ")):
                verb, _, rest = cmd[2:].partition(" ")
                step_ok, out = guest.key_hold(verb == "keydown", *rest.split())
                print(f"\n\033[1m[{out}]\033[0m")
                if not step_ok:
                    failures += 1
            elif cmd.startswith("#!key "):
                step_ok, out = guest.key(*cmd[len("#!key "):].split())
                print(f"\n\033[1m[{out}]\033[0m")
                if not step_ok:
                    failures += 1
            elif cmd.startswith("#!"):
                kind, _, needle = cmd[2:].partition(" ")
                pending.append((kind.strip(), needle.strip()))
                continue
            else:
                print(f"\n\033[1m$ {cmd}\033[0m")
                out = guest.run(cmd, timeout=args.timeout)
                print(out if out else "(no output)")
                if "<TIMEOUT" in out:
                    failures += 1

            for kind, needle in pending:
                hit = needle in out
                ok = step_ok and (hit if kind == "must" else not hit)
                if ok:
                    print(f"  \033[32m✓ {kind}: {needle}\033[0m")
                else:
                    print(f"  \033[31m✗ {kind}: {needle}\033[0m")
                    failures += 1
            pending = []

        for spec in args.pull:
            remote, _, local = spec.partition(":")
            try:
                data = guest.pull(remote)
            except (RuntimeError, KeyError, tarfile.TarError) as err:
                print(f"[pull] \033[31m✗ {remote}: {err}\033[0m")
                failures += 1
                continue
            Path(local).parent.mkdir(parents=True, exist_ok=True)
            Path(local).write_bytes(data)
            print(f"[pull] {remote} -> {local} ({len(data)} bytes)")

        if args.screenshot:
            ok, out = guest.screenshot(args.screenshot)
            print(f"\n[shot] {out}")
            if not ok:
                failures += 1

        if args.keep_running:
            print("\n[hand-off] interactive serial console; Ctrl-] to detach\n")
            guest.child.interact(escape_character="\x1d")
    finally:
        if not args.keep_running:
            guest.shutdown()
        if log:
            log.close()
            print(f"\n[transcript] {args.transcript}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
