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


class Screen:
    def __init__(self, rows=ROWS, cols=COLS):
        self.rows = rows
        self.cols = cols
        self.cells = [[" "] * cols for _ in range(rows)]
        self.row = 0
        self.col = 0
        self.scroll_top = 0
        self.scroll_bottom = rows - 1
        self.decoder = codecs.getincrementaldecoder("utf-8")("ignore")

    def feed(self, data: str):
        index = 0
        while index < len(data):
            ch = data[index]
            if ch == "\x1b":
                index = self._escape(data, index + 1)
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

    def line(self, row: int) -> str:
        return "".join(self.cells[row]).rstrip()

    def contains(self, text: str) -> bool:
        return text in "\n".join(self.line(row) for row in range(self.rows))

    def bottom(self) -> str:
        return self.line(self.rows - 1)

    def above_bottom(self) -> str:
        return self.line(self.rows - 2)

    def _put(self, ch: str):
        if self.col >= self.cols:
            self.col = 0
            self._linefeed()
        self.cells[self.row][self.col] = ch
        self.col += 1

    def _linefeed(self):
        if self.row == self.scroll_bottom:
            del self.cells[self.scroll_top]
            self.cells.insert(self.scroll_bottom, [" "] * self.cols)
        else:
            self.row = min(self.rows - 1, self.row + 1)

    def _escape(self, data: str, index: int) -> int:
        if index >= len(data):
            return index
        if data[index] != "[":
            return index
        index += 1
        start = index
        while index < len(data) and not ("@" <= data[index] <= "~"):
            index += 1
        if index >= len(data):
            return index
        params = data[start:index]
        final = data[index]
        nums = [int(part) if part else 0 for part in params.split(";") if part != "?25"]
        if final == "H" or final == "f":
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
        text = screen.decoder.decode(chunk)
        out.append(text)
        screen.feed(text)
    return "".join(out)


def wait_for(predicate, fd, screen, label, timeout=3.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        read_available(fd, screen, 0.05)
        if predicate():
            return
    dump = "\n".join(f"{i:02d}: {screen.line(i)}" for i in range(screen.rows))
    raise AssertionError(f"timed out waiting for {label}\n{dump}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek")
    parser.add_argument("--model", default="deepseek-v4-flash")
    args = parser.parse_args()

    binary = args.binary or str((os.getcwd() + f"/target/release/{args.name}"))
    if not os.path.exists(binary):
        binary = shutil.which(args.name) or binary
    if not os.path.exists(binary):
        raise SystemExit(f"binary not found: {binary}")

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-docked-smoke-") as home:
        env = os.environ.copy()
        env["HOME"] = home
        env[f"{args.name.upper()}_DEBUG_STREAM_DELAY_MS"] = "10"
        subprocess.run([binary, "debug", "on"], env=env, check=True, stdout=subprocess.DEVNULL)

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        proc = subprocess.Popen([binary, "chat"], stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True)
        os.close(slave)
        screen = Screen()
        try:
            wait_for(lambda: args.name in screen.bottom() and "debug:" in screen.bottom(), master, screen, "PromptIdle dock")
            os.write(master, b"draft")
            wait_for(lambda: "dra" in screen.bottom() and "t" in screen.bottom(), master, screen, "editable draft in dock")
            os.write(master, b"\r")
            wait_for(lambda: screen.contains("context: scanning"), master, screen, "ContextScan row")
            wait_for(lambda: screen.contains("agent --root"), master, screen, "PromptResume after response", timeout=10.0)
            wait_for(lambda: args.name in screen.bottom() and "debug:" in screen.bottom(), master, screen, "PromptResume dock")
            proc.terminate()
            proc.wait(timeout=2)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} docked smoke: ok")


if __name__ == "__main__":
    main()
