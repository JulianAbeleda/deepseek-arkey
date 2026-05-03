#!/usr/bin/env python3
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


ROWS = 24
COLS = 80


def display_width(text):
    width = 0
    for ch in text:
        code = ord(ch)
        if ch == "\x1b":
            continue
        if ch < " ":
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
    def __init__(self, rows=ROWS, cols=COLS):
        self.rows = rows
        self.cols = cols
        self.cells = [[" "] * cols for _ in range(rows)]
        self.row = 0
        self.col = 0
        self.scroll_top = 0
        self.scroll_bottom = rows - 1
        self.history = []
        self.raw = ""
        self.pending_escape = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("ignore")

    def feed_bytes(self, chunk):
        text = self.decoder.decode(chunk)
        self.raw += text
        self.feed(text)

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

    def line(self, row):
        return "".join(self.cells[row]).rstrip()

    def lines(self):
        return [self.line(row) for row in range(self.rows)]

    def contains(self, text):
        return text in "\n".join(self.history + self.lines())

    def line_with(self, text):
        for row, line in enumerate(self.lines()):
            if text in line:
                return row, line
        return None, None

    def _put(self, ch):
        width = display_width(ch)
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
            nums.append(int(part))
        if final in ("H", "f"):
            row = nums[0] if len(nums) > 0 and nums[0] else 1
            col = nums[1] if len(nums) > 1 and nums[1] else 1
            self.row = max(0, min(self.rows - 1, row - 1))
            self.col = max(0, min(self.cols - 1, col - 1))
        elif final == "G":
            col = nums[0] if nums and nums[0] else 1
            self.col = max(0, min(self.cols - 1, col - 1))
        elif final == "J":
            if not nums or nums[0] == 2:
                self.cells = [[" "] * self.cols for _ in range(self.rows)]
                self.row = 0
                self.col = 0
        elif final == "K":
            if not nums or nums[0] in (0, 2):
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
        screen.feed_bytes(chunk)
        if "\x1b[6n" in screen.raw:
            os.write(fd, f"\x1b[{screen.row + 1};{screen.col + 1}R".encode())


def wait_for(predicate, fd, screen, label, timeout=3.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        read_available(fd, screen, 0.05)
        if predicate():
            return
    dump = "\n".join(f"{i:02d}: {screen.line(i)}" for i in range(screen.rows))
    raise AssertionError(f"timed out waiting for {label}\n{dump}")


def assert_cursor_after(screen, text):
    row, line = screen.line_with(text)
    if row is None:
        raise AssertionError(f"missing {text!r}")
    expected_col = display_width(line[: line.index(text)]) + display_width(text)
    if screen.row != row or screen.col != expected_col:
        dump = "\n".join(f"{i:02d}: {screen.line(i)}" for i in range(screen.rows))
        raise AssertionError(
            f"cursor mismatch after {text!r}: expected {(row, expected_col)}, "
            f"got {(screen.row, screen.col)}\n{dump}"
        )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek")
    parser.add_argument("--cols", type=int, default=COLS)
    parser.add_argument("--rows", type=int, default=ROWS)
    args = parser.parse_args()

    binary = args.binary or os.path.join(os.getcwd(), "target", "release", args.name)
    if not os.path.exists(binary):
        binary = shutil.which(args.name) or binary
    if not os.path.exists(binary):
        raise SystemExit(f"binary not found: {binary}")
    binary = os.path.abspath(binary)

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-composer-cursor-") as home:
        env = os.environ.copy()
        env["HOME"] = home
        env.pop("NO_COLOR", None)
        env[f"{args.name.upper()}_FORCE_TTY_SIZE"] = f"{args.cols}x{args.rows}"
        subprocess.run([binary, "debug", "on"], env=env, check=True, stdout=subprocess.DEVNULL)

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.cols, 0, 0))
        proc = subprocess.Popen([binary, "chat"], stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True)
        os.close(slave)
        screen = Screen(args.rows, args.cols)
        try:
            wait_for(lambda: screen.contains(f"{args.name} [debug:"), master, screen, "debug prompt")
            if "\x1b[36" not in screen.raw and "\x1b[38;2" not in screen.raw:
                raise AssertionError("prompt did not emit ANSI color sequences")

            os.write(master, "ab界".encode())
            wait_for(lambda: screen.contains("ab界"), master, screen, "wide char input")
            assert_cursor_after(screen, "ab界")

            os.write(master, b"\x03")
            wait_for(lambda: not screen.contains("ab界"), master, screen, "clear wide input")

            os.write(master, b"\x1b[200~line one\nline two\x1b[201~")
            wait_for(lambda: screen.contains("line one") and screen.contains("line two"), master, screen, "multiline paste")
            assert_cursor_after(screen, "line two")

            os.write(master, b"\x03")
            wait_for(lambda: not screen.contains("line two"), master, screen, "clear multiline input")

            long_text = "wrap-" * 18
            os.write(master, long_text.encode())
            wait_for(lambda: screen.contains("wrap-wrap"), master, screen, "wrapped input")
            if not (args.rows - 6 <= screen.row < args.rows and 0 <= screen.col < args.cols):
                raise AssertionError(f"cursor left dock area after wrap: {(screen.row, screen.col)}")

            proc.terminate()
            proc.wait(timeout=2)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} composer cursor smoke: ok")


if __name__ == "__main__":
    main()
