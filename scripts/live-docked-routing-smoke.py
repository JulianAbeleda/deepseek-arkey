#!/usr/bin/env python3
"""Live docked routing smoke.

Requires DEEPSEEK_API_KEY in the environment and network access. This is not a
CI-default test; it validates the real provider through the docked chat path.
"""

import argparse
import codecs
import fcntl
import os
import pty
import select
import shutil
import struct
import subprocess
import tempfile
import time
import termios
from pathlib import Path


ROWS = 24
COLS = 100
ENV_KEY = "DEEPSEEK_API_KEY"
FORCE_TTY_ENV = "DEEPSEEK_FORCE_TTY_SIZE"


class Screen:
    def __init__(self, rows=ROWS, cols=COLS):
        self.rows = rows
        self.cols = cols
        self.cells = [[" "] * cols for _ in range(rows)]
        self.row = 0
        self.col = 0
        self.scroll_top = 0
        self.scroll_bottom = rows - 1
        self.history = []
        self.decoder = codecs.getincrementaldecoder("utf-8")("ignore")
        self.pending_escape = ""

    def feed(self, data):
        if self.pending_escape:
            data = self.pending_escape + data
            self.pending_escape = ""
        index = 0
        while index < len(data):
            ch = data[index]
            if ch == "\x1b":
                next_index = self._escape(data, index + 1)
                if next_index is None:
                    self.pending_escape = data[index:]
                    break
                index = next_index + 1
                continue
            if ch == "\r":
                self.col = 0
            elif ch == "\n":
                self._linefeed()
            elif ch == "\b":
                self.col = max(0, self.col - 1)
            elif ch >= " ":
                self._put(ch)
            index += 1

    def all_text(self):
        return "\n".join(self.history + [self.line(row) for row in range(self.rows)])

    def contains(self, text):
        return text in self.all_text()

    def line(self, row):
        return "".join(self.cells[row]).rstrip()

    def dump(self):
        return "\n".join(f"{row:02d}: {self.line(row)}" for row in range(self.rows))

    def _put(self, ch):
        if self.col >= self.cols:
            self.col = 0
            self._linefeed()
        self.cells[self.row][self.col] = ch
        self.col += 1

    def _linefeed(self):
        if self.row == self.scroll_bottom:
            self.history.append("".join(self.cells[self.scroll_top]).rstrip())
            del self.cells[self.scroll_top]
            self.cells.insert(self.scroll_bottom, [" "] * self.cols)
        else:
            self.row = min(self.rows - 1, self.row + 1)

    def _escape(self, data, index):
        if index >= len(data):
            return None
        if data[index] in ("7", "8"):
            return index
        if data[index] != "[":
            return min(len(data) - 1, index)
        index += 1
        start = index
        while index < len(data) and not ("@" <= data[index] <= "~"):
            index += 1
        if index >= len(data):
            return None
        params = data[start:index]
        final = data[index]
        nums = []
        for part in params.split(";"):
            if part in ("", "?25") or part.startswith("?"):
                continue
            try:
                nums.append(int(part))
            except ValueError:
                pass
        if final in ("H", "f"):
            row = nums[0] if len(nums) > 0 and nums[0] else 1
            col = nums[1] if len(nums) > 1 and nums[1] else 1
            self.row = max(0, min(self.rows - 1, row - 1))
            self.col = max(0, min(self.cols - 1, col - 1))
        elif final == "G":
            col = nums[0] if nums and nums[0] else 1
            self.col = max(0, min(self.cols - 1, col - 1))
        elif final == "J":
            mode = nums[0] if nums else 0
            if mode == 2:
                self.cells = [[" "] * self.cols for _ in range(self.rows)]
                self.row = 0
                self.col = 0
        elif final == "K":
            mode = nums[0] if nums else 0
            if mode in (0, 2):
                for col in range(self.col, self.cols):
                    self.cells[self.row][col] = " "
        elif final == "r":
            if len(nums) >= 2:
                self.scroll_top = max(0, nums[0] - 1)
                self.scroll_bottom = max(self.scroll_top, min(self.rows - 1, nums[1] - 1))
            else:
                self.scroll_top = 0
                self.scroll_bottom = self.rows - 1
            self.row = 0
            self.col = 0
        return index


def read_available(fd, screen, timeout=0.1):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        ready, _, _ = select.select([fd], [], [], 0.02)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        text = screen.decoder.decode(chunk)
        screen.feed(text)
        if "\x1b[6n" in text:
            os.write(fd, f"\x1b[{screen.row + 1};{screen.col + 1}R".encode())


def wait_for(predicate, fd, screen, label, timeout=30.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        read_available(fd, screen, 0.05)
        if predicate():
            return
    raise AssertionError(f"timed out waiting for {label}\n{screen.dump()}")


def prompt_visible(screen, name):
    text = screen.all_text()
    return name in text and "›" in text


def assert_not_legacy_handoff(screen):
    text = screen.all_text()
    forbidden = ["agent task:", "route: agent task", "Run this as an agent task?"]
    found = [item for item in forbidden if item in text]
    if found:
        raise AssertionError(f"legacy handoff rendered: {found}\n{screen.dump()}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek")
    parser.add_argument("--expect", default="DEEPSEEK_DOCKED_LIVE_OK")
    parser.add_argument("--timeout", type=float, default=45.0)
    args = parser.parse_args()

    if not os.environ.get(ENV_KEY):
        raise SystemExit(f"{ENV_KEY} is not set")

    binary = args.binary or str(Path.cwd() / "target" / "release" / args.name)
    if not os.path.exists(binary):
        binary = shutil.which(args.name) or binary
    if not os.path.exists(binary):
        raise SystemExit(f"binary not found: {binary}")
    binary = os.path.abspath(binary)

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-live-docked-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        home.mkdir()
        workspace.mkdir()
        env = os.environ.copy()
        env["HOME"] = str(home)
        env[FORCE_TTY_ENV] = f"{COLS}x{ROWS}"

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        proc = subprocess.Popen(
            [binary, "chat"],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            cwd=str(workspace),
            close_fds=True,
        )
        os.close(slave)
        screen = Screen()
        try:
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "initial dock")
            prompt = (
                "Do not use tools. Return exactly one JSON object and no prose: "
                f'{{"final_answer":"{args.expect}"}}\r'
            )
            os.write(master, prompt.encode())
            wait_for(lambda: screen.contains("context: scanning"), master, screen, "scan row")
            wait_for(
                lambda: screen.contains(args.expect),
                master,
                screen,
                "live provider response",
                timeout=args.timeout,
            )
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "dock restored")
            assert_not_legacy_handoff(screen)
            proc.terminate()
            proc.wait(timeout=3)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} live docked routing smoke: ok")


if __name__ == "__main__":
    main()
