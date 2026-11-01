#!/usr/bin/env python3
"""End-to-end smoke test for `booxmtp tui`, driven through a real pseudo-terminal.

It needs a *connected reader* (the test uploads, downloads and creates a folder) and the `pyte`
package for screen parsing:

    python3 -m venv .venv
    .venv/bin/pip install pyte
    .venv/bin/python scripts/tui-smoke.py --work /tmp/booxmtp-tui-smoke

The script owns its fixture directory: it creates `smoke.bin` and `note.txt` if they are missing
and removes every other entry, so a previous interrupted run cannot shift the selection. Everything it creates on the
device lives in the storage root under a unique name and is printed at the end so you can delete it
manually (the CLI has no `rm` yet).
"""

from __future__ import annotations

import argparse
import hashlib
import os
import sys

try:
    import pyte
except ImportError:  # pragma: no cover - it is a developer tool
    sys.exit(
        "pyte is required for the screen checks:\n"
        "  python3 -m venv .venv && .venv/bin/pip install pyte\n"
        "then run: .venv/bin/python scripts/tui-smoke.py"
    )


class Tui:
    """A `booxmtp tui` process attached to a pty, with a pyte screen behind it."""

    ROWS, COLS = 40, 120

    def __init__(self, binary: str, workdir: str, args: list[str]):
        import fcntl
        import pty
        import struct
        import termios

        self.screen = pyte.Screen(self.COLS, self.ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:  # child
            os.chdir(workdir)
            os.execv(binary, [binary, *args])
        fcntl.ioctl(
            self.fd,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", self.ROWS, self.COLS, 0, 0),
        )

    def pump(self, seconds: float) -> None:
        import select
        import time

        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.stream.feed(data)

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def panel_text(self, side: str) -> str:
        split = self.COLS // 2
        start, end = (0, split) if side == "left" else (split, self.COLS)
        rows = []
        for y in range(self.ROWS):
            line = self.screen.buffer[y]
            rows.append("".join(line[x].data for x in range(start, end)))
        return "\n".join(rows)

    def wait_for(self, needle: str, timeout: float = 45) -> bool:
        import time

        end = time.time() + timeout
        while time.time() < end:
            self.pump(0.2)
            if needle in self.text():
                return True
        return False

    def wait_for_panel(self, side: str, needle: str, timeout: float = 30) -> bool:
        import time

        end = time.time() + timeout
        while time.time() < end:
            self.pump(0.2)
            if needle in self.panel_text(side):
                return True
        return False

    def send(self, keys: bytes, settle: float = 0.4) -> None:
        os.write(self.fd, keys)
        self.pump(settle)

    def quit(self) -> int:
        self.send(b"\x11", 1.5)  # Ctrl-Q
        try:
            return os.waitpid(self.pid, 0)[1] >> 8
        except ChildProcessError:
            return 0

    def dump(self) -> None:
        print("+" + "-" * self.COLS + "+")
        for line in self.screen.display:
            print("|" + line + "|")
        print("+" + "-" * self.COLS + "+")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bin",
        default=os.environ.get("BOOXMTP_BIN", "target/release/booxmtp"),
        help="path to the booxmtp binary",
    )
    parser.add_argument(
        "--work",
        default="/tmp/booxmtp-tui-smoke",
        help="local fixture directory (gets a file named smoke.bin)",
    )
    parser.add_argument(
        "--device",
        default=None,
        help="serial number, passed through as --device",
    )
    parser.add_argument("--storage", default="internal", help="storage for the right panel")
    options = parser.parse_args()

    binary = os.path.abspath(options.bin)
    if not os.access(binary, os.X_OK):
        parser.error(f"{binary} is not executable; build it first (cargo build --release)")

    os.makedirs(options.work, exist_ok=True)
    fixture = os.path.join(options.work, "smoke.bin")
    for entry in os.listdir(options.work):
        if entry in ("smoke.bin", "note.txt"):
            continue
        path = os.path.join(options.work, entry)
        if os.path.isdir(path) and not os.path.islink(path):
            import shutil

            shutil.rmtree(path)
        else:
            os.unlink(path)
    if not os.path.exists(fixture):
        with open(fixture, "wb") as handle:
            handle.write(os.urandom(64 * 1024))
    note = os.path.join(options.work, "note.txt")
    with open(note, "w", encoding="utf-8") as handle:
        handle.write("booxmtp tui smoke test\n")
    before = hashlib.sha256(open(fixture, "rb").read()).hexdigest()
    folder_name = f"booxmtp-tui-smoke-{os.getpid()}"

    args = ["tui", "-s", options.storage]
    if options.device:
        args += ["--device", options.device]
    tui = Tui(binary, options.work, args)

    ok = True

    def check(label: str, condition: bool) -> None:
        nonlocal ok
        print(("PASS " if condition else "FAIL ") + label)
        ok = ok and condition

    check("local panel", tui.wait_for("Local:"))
    check("device header", tui.wait_for("serial"))
    check("remote panel", tui.wait_for(f"{options.storage}:"))
    check("key bar", tui.wait_for("Space Mark") and tui.wait_for("Ctrl-C Copy"))

    # 1) Upload: mark both fixture files with Ctrl-A and copy the batch.
    check("fixtures listed", "smoke.bin" in tui.panel_text("left"))
    tui.send(b"\x01")  # Ctrl-A = mark all files
    check("marks are visible", tui.wait_for("✓ smoke.bin"))
    tui.send(b"\x03")  # Ctrl-C = copy marked files
    # Leftovers from an earlier run trigger the overwrite dialog: accept them.
    for _ in range(40):
        if "Copied 2 files" in tui.text():
            break
        if "Overwrite?" in tui.text():
            tui.send(b"y", 0.5)
        tui.pump(0.5)
    check("batch copy finished", tui.wait_for("Copied 2 files"))
    check(
        "remote panel shows both files",
        tui.wait_for_panel("right", "smoke.bin") and tui.wait_for_panel("right", "note.txt"),
    )

    # 2) Download with overwrite confirmation.
    tui.send(b"\t")
    tui.send(b"\x1b[F")  # End
    tui.send(b"\x03")  # Ctrl-C = copy
    check("overwrite dialog", tui.wait_for("Overwrite?"))
    tui.send(b"y")
    check("download finished", tui.wait_for("Downloaded /smoke.bin"))

    # 3) Create a folder on the device.
    tui.send(b"\x0e")  # Ctrl-N = new folder
    check("mkdir dialog", tui.wait_for("Create folder"))
    tui.send(folder_name.encode())
    tui.send(b"\r")
    check("mkdir finished", tui.wait_for(f"Created /{folder_name}"))
    tui.send(b"\x12", 2.0)  # Ctrl-R
    check("new folder listed", tui.wait_for_panel("right", f"{folder_name}/"))

    # 4) Help and clean shutdown.
    tui.send(b"?")  # help
    check("help", tui.wait_for("Navigation"))
    tui.send(b"\x1b")
    status = tui.quit()
    check("clean exit", status == 0)
    check(
        "roundtrip is byte-identical",
        hashlib.sha256(open(fixture, "rb").read()).hexdigest() == before,
    )

    if not ok:
        tui.dump()
        return 1

    print("TUI smoke test passed")
    print(f"device leftovers to remove manually: /smoke.bin, /note.txt, /{folder_name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
