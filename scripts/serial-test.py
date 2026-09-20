#!/usr/bin/env python3
"""Drive a booted Pyxis guest over the serial console and capture results.

Serial is the verification channel for this project: headless screenshots are
impossible (Evo needs virgl; QMP screendump reports "no surface"), and
`/etc/inittab` gives `ttyS0` a root shell with no login. This harness boots the
image, waits for that shell, runs a list of commands, and prints each one's
output — so an on-device check is a script that can be re-run, not a manual
session that has to be retyped.

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

Usage:
    scripts/serial-test.py --script scripts/serial-tests/alpha4-ipk-e2e.txt
    scripts/serial-test.py -c 'uname -a' -c 'opkg list-installed | head'
    scripts/serial-test.py --push local.ipk:/tmp/local.ipk -c 'ls -l /tmp'
"""

from __future__ import annotations

import argparse
import base64
import os
import shutil
import subprocess
import sys
import re
import tempfile
import time
from pathlib import Path

try:
    import pexpect
except ImportError:
    sys.exit("pexpect is required: pip install pexpect")

PROJECT_ROOT = Path(__file__).resolve().parent.parent
IMAGE = PROJECT_ROOT / "target/build/images/geckoOS.img"

# Markers are split at the call site so the shell's own echo of the command
# cannot match them. See the module docstring.
BEGIN = "PYXBEGIN"
END = "PYXEND"

# CSI/OSC sequences, including readline's bracketed-paste toggles.
ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07")
PROMPT_RE = re.compile(r"^[\w.-]+@\(?[\w.()-]*\)?:[^#$]*[#$]\s?")

# Prepended to every command. The runtime dir is echo-cli's socket path; PATH
# covers the Pyxis binaries, which are not on BusyBox's default PATH.
GUEST_ENV = "export XDG_RUNTIME_DIR=/run/user/1000 PATH=$PATH:/usr/bin:/usr/sbin:/usr/local/bin"


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
    def __init__(self, image: Path, overlay: bool, memory: int, logfile) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="pyxis-serial-"))
        self.logfile = logfile

        if overlay:
            self.disk = self.tmp / "overlay.qcow2"
            subprocess.run(
                ["qemu-img", "create", "-f", "qcow2", "-F", "raw",
                 "-b", str(image), str(self.disk)],
                check=True, stdout=subprocess.DEVNULL,
            )
            drive = f"file={self.disk},format=qcow2,media=disk,if=virtio"
        else:
            drive = f"file={image},format=raw,media=disk,if=virtio"

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
            # egl-headless + virtio-vga-gl: Evo will not start without a GL
            # backend, and a guest whose compositor died answers every Echo
            # call with a timeout that looks like an IPC bug.
            "-vga", "none",
            "-device", "virtio-vga-gl,xres=1280,yres=800,edid=on,max_outputs=1",
            "-display", "egl-headless,gl=on",
            "-device", "virtio-keyboard-pci",
            "-netdev", "user,id=net0",
            "-device", "e1000,netdev=net0",
            "-serial", "stdio",
            "-monitor", "none",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-no-reboot",
            "-rtc", "base=localtime,driftfix=slew",
        ]
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
        lines: list[str] = []
        for ln in text.splitlines():
            ln = ln.rstrip("\r").rstrip()
            if not ln:
                continue
            # 'root@(none):/# uname -a' — prompt plus the echoed command.
            if PROMPT_RE.match(ln):
                continue
            if ln.strip() == command.strip():
                continue
            # The split-marker echo, e.g. "echo PYX''END".
            if "''" in ln:
                continue
            lines.append(ln)
        return "\n".join(lines).strip()

    def push(self, local: Path, remote: str, timeout: int = 120,
             chunk: int = 2048) -> str:
        """Copy a local file into the guest through the serial console.

        There is no shared filesystem and no sshd, and mounting the image to
        drop a file in needs root — which the whole point of this harness is to
        avoid. base64 in chunks is slow but needs nothing on either side.

        Chunk size is the throughput knob and it has a hard ceiling: the pty
        line discipline buffers a canonical-mode line in about 4 KiB, and a
        longer line is silently truncated. 2 KiB leaves headroom for the
        `printf %s '...' >> file` wrapper. The sha256 check at the end is not
        decoration — a truncated chunk yields a file of plausible size that is
        quietly wrong.
        """
        assert self.child is not None
        data = base64.b64encode(local.read_bytes()).decode()
        parts = [data[i:i + chunk] for i in range(0, len(data), chunk)]
        print(f"[push] {local.name} -> {remote} "
              f"({local.stat().st_size} bytes, {len(parts)} chunks)")
        self.run(f"rm -f {remote}.b64 {remote}")
        started = time.time()
        for i, part in enumerate(parts):
            self.run(f"printf %s '{part}' >> {remote}.b64", timeout=timeout)
            if (i + 1) % 25 == 0 or i + 1 == len(parts):
                rate = (i + 1) / max(time.time() - started, 0.001)
                print(f"[push]   {i + 1}/{len(parts)} ({rate:.1f} chunks/s)")
        self.run(f"base64 -d {remote}.b64 > {remote} && rm -f {remote}.b64",
                 timeout=timeout)
        # Verify rather than assume: a dropped chunk produces a file that
        # exists, has a plausible size, and is silently corrupt.
        want = subprocess.run(["sha256sum", str(local)], capture_output=True,
                              text=True, check=True).stdout.split()[0]
        got = self.run(f"sha256sum {remote} 2>/dev/null | cut -d' ' -f1")
        if want not in got:
            return f"MISMATCH host={want} guest={got}"
        return f"ok sha256={want[:16]}…"

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

    def screenshot(self, dest: Path) -> str:
        """Capture the guest framebuffer to a PNG.

        Worth trying rather than assuming: screendump long failed with
        "no surface" on a GL scanout, which is where this project's
        "headless screenshots are impossible" note comes from. QEMU has
        since learnt to read back a dmabuf scanout, so the capability
        depends on the host QEMU, not on the setup being wrong.
        """
        dest = dest.resolve()
        res = self.qmp(execute="screendump",
                       arguments={"filename": str(dest), "format": "png"})
        if "error" in res:
            return f"screendump failed: {res['error'].get('desc', res['error'])}"
        if not dest.exists() or dest.stat().st_size == 0:
            return "screendump reported success but wrote nothing"
        return f"ok {dest} ({dest.stat().st_size} bytes)"

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
                    help="copy a file into the guest before running commands")
    ap.add_argument("--chunk", type=int, default=2048,
                    help="base64 chunk size per serial write (max ~4000; see push())")
    ap.add_argument("--boot-timeout", type=int, default=300)
    ap.add_argument("--timeout", type=int, default=60, help="per-command timeout")
    ap.add_argument("--settle", type=int, default=25,
                    help="seconds to wait after the shell appears, for daemons to come up")
    ap.add_argument("--memory", type=int, default=2048,
                    help="guest RAM in MB. The default matches emulate.sh's "
                         "reference config; 8 concurrent vela apps do not fit "
                         "in it (see PROJECT_STATUS alpha 4 exit criteria).")
    ap.add_argument("--no-overlay", action="store_true",
                    help="write directly to the image instead of a qcow2 overlay")
    ap.add_argument("--transcript", type=Path, help="write the raw serial log here")
    ap.add_argument("--screenshot", type=Path,
                    help="capture the guest framebuffer to this PNG after the commands run")
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
            # command's output; every other `#` line is a comment.
            if line.startswith("#!"):
                commands.append(line)
            elif line and not line.startswith("#"):
                commands.append(line)
    if not commands and not args.keep_running:
        sys.exit("nothing to do: pass -c/--script (or --keep-running)")

    log = args.transcript.open("w") if args.transcript else None
    guest = Guest(args.image, not args.no_overlay, args.memory, log)
    failures = 0
    try:
        guest.boot(args.boot_timeout)
        guest.run(GUEST_ENV)
        if args.settle:
            print(f"[boot] settling {args.settle}s for daemons")
            guest.run(f"sleep {args.settle}", timeout=args.settle + 30)

        for spec in args.push:
            local, _, remote = spec.partition(":")
            print(f"[push] {guest.push(Path(local), remote, chunk=args.chunk)}")

        # Directives accumulated from `#!must` / `#!must-not` comment lines,
        # applied to the next real command. Without them these suites only
        # *print* — a run where every step errored still exited 0, which is
        # how a stale hardcoded filename in a suite went unnoticed.
        pending: list[tuple[str, str]] = []
        for cmd in commands:
            if cmd.startswith("#!"):
                kind, _, needle = cmd[2:].partition(" ")
                pending.append((kind.strip(), needle.strip()))
                continue

            print(f"\n\033[1m$ {cmd}\033[0m")
            out = guest.run(cmd, timeout=args.timeout)
            print(out if out else "(no output)")
            if "<TIMEOUT" in out:
                failures += 1

            for kind, needle in pending:
                hit = needle in out
                ok = hit if kind == "must" else not hit
                if ok:
                    print(f"  \033[32m✓ {kind}: {needle}\033[0m")
                else:
                    print(f"  \033[31m✗ {kind}: {needle}\033[0m")
                    failures += 1
            pending = []

        if args.screenshot:
            print(f"\n[shot] {guest.screenshot(args.screenshot)}")

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
