#!/usr/bin/env python3
"""Shared helpers for local DeepSeek smoke/debug scripts."""

import codecs
import fcntl
import os
import pty
import select
import shutil
import stat
import struct
import subprocess
import time
import termios
from pathlib import Path


def display_width(text):
    """Return terminal display width for plain text used by smoke screens."""
    width = 0
    for ch in text:
        code = ord(ch)
        if ch == "\x1b" or ch < " ":
            continue
        if (
            0x1100 <= code <= 0x115F
            or 0x2329 <= code <= 0x232A
            or 0x2E80 <= code <= 0xA4CF
            or 0xAC00 <= code <= 0xD7A3
            or 0xF900 <= code <= 0xFAFF
            or 0xFE10 <= code <= 0xFE19
            or 0xFE30 <= code <= 0xFE6F
            or 0xFF00 <= code <= 0xFF60
            or 0xFFE0 <= code <= 0xFFE6
            or 0x1F300 <= code <= 0x1FAFF
        ):
            width += 2
        else:
            width += 1
    return width


class Screen:
    """Small ANSI terminal model used by PTY smoke scripts."""

    def __init__(self, rows=24, cols=80, wide_chars=False):
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
        self.raw = ""
        self.scroll_region_set = False
        self.full_clears = 0
        self.saved_position = None
        self.wide_chars = wide_chars

    def feed_bytes(self, chunk):
        """Decode raw PTY bytes, preserve them, and feed visible text."""
        text = self.decoder.decode(chunk)
        self.raw += text
        self.feed(text)

    def feed(self, data):
        """Feed decoded terminal output into the screen model."""
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

    def contains(self, text):
        """Return whether text appears in visible rows or scrollback history."""
        return text in self.all_text()

    def all_text(self):
        """Return scrollback plus current screen rows."""
        return "\n".join(self.history + self.lines())

    def text(self):
        """Compatibility alias for all_text."""
        return self.all_text()

    def line(self, row):
        """Return one visible screen row without trailing spaces."""
        return "".join(self.cells[row]).rstrip()

    def lines(self):
        """Return all visible rows without trailing spaces."""
        return [self.line(row) for row in range(self.rows)]

    def bottom(self):
        """Return the bottom visible row."""
        return self.line(self.rows - 1)

    def above_bottom(self):
        """Return the row above the bottom visible row."""
        return self.line(self.rows - 2)

    def dock_text(self, rows=7):
        """Return the bottom dock region used by current TUI smokes."""
        start = max(0, self.rows - rows)
        return "\n".join(self.line(row) for row in range(start, self.rows))

    def line_with(self, text):
        """Return the first visible row containing text, or (None, None)."""
        for row, line in enumerate(self.lines()):
            if text in line:
                return row, line
        return None, None

    def dump(self):
        """Return a numbered screen dump for assertion errors."""
        return "\n".join(f"{row:02d}: {self.line(row)}" for row in range(self.rows))

    def _put(self, ch):
        width = display_width(ch) if self.wide_chars else 1
        if width == 0:
            return
        if self.col >= self.cols:
            self.col = 0
            self._linefeed()
        self.cells[self.row][self.col] = ch
        self.col += 1
        if width == 2 and self.col < self.cols:
            self.cells[self.row][self.col] = " "
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
        if data[index] == "7":
            self.saved_position = (self.row, self.col)
            return index
        if data[index] == "8":
            if self.saved_position is not None:
                self.row, self.col = self.saved_position
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
        elif final == "A":
            amount = nums[0] if nums and nums[0] else 1
            self.row = max(0, self.row - amount)
        elif final == "B":
            amount = nums[0] if nums and nums[0] else 1
            self.row = min(self.rows - 1, self.row + amount)
        elif final == "J":
            mode = nums[0] if nums else 0
            if mode == 2 or not nums:
                self.full_clears += 1
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
                self.scroll_region_set = True
            else:
                self.scroll_top = 0
                self.scroll_bottom = self.rows - 1
                self.scroll_region_set = False
            self.row = 0
            self.col = 0
        return index


def read_available(fd, screen, timeout=0.1, raw=None):
    """Read currently available PTY output into a Screen."""
    end = time.monotonic() + timeout
    out = []
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
        if raw is not None:
            raw.append(chunk)
        text = screen.decoder.decode(chunk)
        screen.raw += text
        out.append(text)
        screen.feed(text)
        if "\x1b[6n" in text:
            os.write(fd, f"\x1b[{screen.row + 1};{screen.col + 1}R".encode())
    return "".join(out)


def wait_for(predicate, fd, screen, label, timeout=8.0):
    """Poll a PTY until predicate returns true or raise with a screen dump."""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        read_available(fd, screen, 0.05)
        if predicate():
            return
    raise AssertionError(f"timed out waiting for {label}\n{screen.dump()}")


def wait_for_bool(predicate, fd, screen, timeout=4.0, raw=None):
    """Poll a PTY and return whether predicate became true."""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        read_available(fd, screen, 0.05, raw=raw)
        if predicate():
            return True
    return False


def spawn_pty(binary, args, env, cwd, rows, cols):
    """Spawn a process attached to a fixed-size PTY."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    proc = subprocess.Popen(
        [binary, *args],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        cwd=cwd,
        close_fds=True,
    )
    os.close(slave)
    return master, proc


def prompt_visible(screen, name):
    """Return whether a dock-style prompt for this binary name is visible."""
    return name in screen.all_text() and "›" in screen.all_text()


def resolve_binary(name, binary=None, profile="debug"):
    """Resolve an explicit, target profile, PATH binary, or exit clearly."""
    candidate = binary or str(Path.cwd() / "target" / profile / name)
    if not os.path.exists(candidate):
        candidate = shutil.which(name) or candidate
    if not os.path.exists(candidate):
        raise SystemExit(f"binary not found: {candidate}")
    return os.path.abspath(candidate)


def run_checked(cmd, cwd, env=None, quiet_stdout=True):
    """Run a setup command while preserving stderr for failure debugging."""
    stdout = subprocess.DEVNULL if quiet_stdout else None
    subprocess.run(cmd, cwd=cwd, env=env, check=True, stdout=stdout)


def write_executable(path, text):
    """Write a UTF-8 executable helper script."""
    path.write_text(text, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
