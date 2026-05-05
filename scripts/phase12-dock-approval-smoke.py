#!/usr/bin/env python3
"""Phase 12 dock-native approval smoke.

This validates the Kimi-style baseline path without a live provider:

- approval-gated tools request approval above the dock
- denial and approval are submitted through the bottom composer
- no raw stdin approval prompt appears
"""

import argparse
import codecs
import fcntl
import json
import os
import pty
import select
import shutil
import stat
import struct
import subprocess
import tempfile
import textwrap
import time
import termios
from pathlib import Path


ROWS = 24
COLS = 100


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

    def contains(self, text):
        return text in self.all_text()

    def all_text(self):
        return "\n".join(self.history + [self.line(row) for row in range(self.rows)])

    def line(self, row):
        return "".join(self.cells[row]).rstrip()

    def bottom(self):
        return self.line(self.rows - 1)

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
        if data[index] == "7":
            return index
        if data[index] == "8":
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


def wait_for(predicate, fd, screen, label, timeout=6.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        read_available(fd, screen, 0.05)
        if predicate():
            return
    raise AssertionError(f"timed out waiting for {label}\n{screen.dump()}")


def write_fake_curl(directory):
    path = Path(directory) / "curl"
    path.write_text(
        textwrap.dedent(
            r"""
            #!/usr/bin/env python3
            import json
            import sys

            config = sys.stdin.read()
            if "deny shell command" in config and "Tool result for step" in config:
                decision = {"final_answer": "shell denied through dock"}
            elif "approve shell command" in config and "Tool result for step" in config:
                decision = {"final_answer": "shell approved through dock"}
            elif "deny patch edit" in config and "Tool result for step" in config:
                decision = {"final_answer": "patch denied through dock"}
            elif "approve patch edit" in config and "Tool result for step" in config:
                decision = {"final_answer": "patch approved through dock"}
            elif "Tool result for step" in config:
                decision = {"final_answer": "unexpected tool result"}
            elif "deny shell command" in config:
                decision = {
                    "thought": "request shell and expect user denial",
                    "tool": {
                        "name": "run_shell",
                        "arguments": {
                            "command": "printf DENIED_SHOULD_NOT_RUN",
                            "cwd": ".",
                            "reason": "phase12 denial smoke",
                        },
                    },
                }
            elif "approve shell command" in config:
                decision = {
                    "thought": "request shell and expect user approval",
                    "tool": {
                        "name": "run_shell",
                        "arguments": {
                            "command": "printf PHASE12_APPROVED",
                            "cwd": ".",
                            "reason": "phase12 approval smoke",
                        },
                    },
                }
            elif "deny patch edit" in config:
                decision = {
                    "thought": "request patch and expect user denial",
                    "tool": {
                        "name": "propose_patch",
                        "arguments": {
                            "path": "README.md",
                            "find": "workspace smoke",
                            "replace": "workspace smoke denied",
                            "reason": "phase12 patch denial smoke",
                        },
                    },
                }
            elif "approve patch edit" in config:
                decision = {
                    "thought": "request patch and expect user approval",
                    "tool": {
                        "name": "propose_patch",
                        "arguments": {
                            "path": "README.md",
                            "find": "workspace smoke",
                            "replace": "workspace smoke approved",
                            "reason": "phase12 patch approval smoke",
                        },
                    },
                }
            else:
                decision = {"final_answer": "phase12 ready"}

            if "no-buffer" in config:
                print("data: " + json.dumps({
                    "choices": [{"delta": {"content": json.dumps(decision)}}]
                }))
                print("data: [DONE]")
                raise SystemExit(0)

            print(json.dumps({
                "choices": [{"message": {"content": json.dumps(decision)}}]
            }))
            """
        ).lstrip(),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def spawn(binary, args, env, cwd):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
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


def assert_not_legacy_handoff(screen):
    text = screen.all_text()
    forbidden = ["agent task:", "route: agent task", "Run this as an agent task?"]
    found = [item for item in forbidden if item in text]
    if found:
        raise AssertionError(f"legacy handoff rendered: {found}\n{screen.dump()}")


def prompt_visible(screen, name):
    return name in screen.all_text() and "›" in screen.all_text()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek")
    args = parser.parse_args()

    binary = args.binary or str(Path.cwd() / "target" / "debug" / args.name)
    if not os.path.exists(binary):
        binary = shutil.which(args.name) or binary
    if not os.path.exists(binary):
        raise SystemExit(f"binary not found: {binary}")
    binary = os.path.abspath(binary)

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-phase12-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        desktop = home / "Desktop"
        fake_bin = tmp_path / "bin"
        home.mkdir()
        workspace.mkdir()
        desktop.mkdir()
        fake_bin.mkdir()
        (workspace / "README.md").write_text("workspace smoke\n", encoding="utf-8")
        (desktop / "note.txt").write_text("desktop smoke\n", encoding="utf-8")
        write_fake_curl(fake_bin)

        env = os.environ.copy()
        env["HOME"] = str(home)
        env["DEEPSEEK_API_KEY"] = "phase11-smoke-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn(binary, ["chat"], env, str(workspace))
        screen = Screen()
        try:
            wait_for(
                lambda: prompt_visible(screen, args.name),
                master,
                screen,
                "initial dock",
            )

            os.write(master, b"deny shell command\r")
            wait_for(
                lambda: screen.contains("agent step 1: run_shell"),
                master,
                screen,
                "shell tool step render",
            )
            wait_for(
                lambda: screen.contains("approval required: run_shell"),
                master,
                screen,
                "shell approval request",
            )
            os.write(master, b"n\r")
            wait_for(
                lambda: screen.contains("approval: denied run_shell"),
                master,
                screen,
                "shell denial accepted",
            )
            wait_for(
                lambda: screen.contains("shell denied through dock"),
                master,
                screen,
                "shell denial final answer",
            )
            wait_for(
                lambda: prompt_visible(screen, args.name),
                master,
                screen,
                "dock after shell denial",
            )
            if screen.contains("agent requests shell execution"):
                raise AssertionError(f"docked worker prompted for shell approval\n{screen.dump()}")
            assert_not_legacy_handoff(screen)

            os.write(master, b"approve shell command\r")
            wait_for(
                lambda: screen.contains("reason: phase12 approval smoke"),
                master,
                screen,
                "shell approval request",
            )
            os.write(master, b"yes run\r")
            wait_for(
                lambda: screen.contains("approval: approved run_shell"),
                master,
                screen,
                "shell approval accepted",
            )
            wait_for(
                lambda: screen.contains("shell approved through dock"),
                master,
                screen,
                "shell approval final answer",
            )
            assert_not_legacy_handoff(screen)

            os.write(master, b"deny patch edit\r")
            wait_for(
                lambda: screen.contains("agent step 1: propose_patch"),
                master,
                screen,
                "patch tool step render",
            )
            wait_for(
                lambda: screen.contains("reason: phase12 patch denial smoke"),
                master,
                screen,
                "patch denial request",
            )
            os.write(master, b"n\r")
            wait_for(
                lambda: screen.contains("approval: denied propose_patch"),
                master,
                screen,
                "patch denial accepted",
            )
            wait_for(
                lambda: screen.contains("patch denied through dock"),
                master,
                screen,
                "patch denial final answer",
            )
            readme = (workspace / "README.md").read_text(encoding="utf-8")
            if readme != "workspace smoke\n":
                raise AssertionError(f"denied patch modified README.md: {readme!r}")
            assert_not_legacy_handoff(screen)

            os.write(master, b"approve patch edit\r")
            wait_for(
                lambda: screen.contains("reason: phase12 patch approval smoke"),
                master,
                screen,
                "patch approval request",
            )
            os.write(master, b"yes apply\r")
            wait_for(
                lambda: screen.contains("approval: approved propose_patch"),
                master,
                screen,
                "patch approval accepted",
            )
            wait_for(
                lambda: screen.contains("patch approved through dock"),
                master,
                screen,
                "patch approval final answer",
            )
            readme = (workspace / "README.md").read_text(encoding="utf-8")
            if readme != "workspace smoke approved\n":
                raise AssertionError(f"approved patch did not update README.md: {readme!r}")
            assert_not_legacy_handoff(screen)

            os.write(master, b"/exit\r")
            proc.wait(timeout=2)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} phase12 dock approval smoke: ok")


if __name__ == "__main__":
    main()
