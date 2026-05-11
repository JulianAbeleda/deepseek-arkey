#!/usr/bin/env python3
"""Claude-facing debugger for commit-audit approval flow.

This is a focused local smoke for the feature path Claude is auditing:

- a repo-audit prompt routes through docked agent mode
- the agent requests an approval-gated shell command
- the dock approval modal appears
- approving the modal lets the shell command run
- the agent returns an audit-style final answer

It uses a fake `curl` provider and a temporary git repo, so it does not require
network access or a DeepSeek API key.
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


def wait_for(predicate, fd, screen, label, timeout=8.0):
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

            if "Tool result for step" in config:
                decision = {
                    "final_answer": (
                        "## Findings\n\n"
                        "- No blocking findings in the audited commit.\n\n"
                        "## Evidence\n\n"
                        "- Ran `git show --stat --patch HEAD --` through approved shell access.\n"
                    )
                }
            elif "audit commit HEAD" in config:
                decision = {
                    "thought": "inspect local commit before auditing",
                    "tool": {
                        "name": "run_shell",
                        "arguments": {
                            "command": "git show --stat --patch HEAD --",
                            "cwd": ".",
                            "reason": "inspect the target commit for audit",
                        },
                    },
                }
            else:
                decision = {"final_answer": "claude audit debugger ready"}

            print(json.dumps({
                "choices": [{"message": {"content": json.dumps(decision)}}]
            }))
            """
        ).lstrip(),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def run(cmd, cwd, env=None):
    subprocess.run(cmd, cwd=cwd, env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def make_repo(path):
    path.mkdir()
    run(["git", "init"], path)
    run(["git", "config", "user.email", "claude-audit@example.test"], path)
    run(["git", "config", "user.name", "Claude Audit Smoke"], path)
    (path / "README.md").write_text("# Audit Smoke\n\nInitial content.\n", encoding="utf-8")
    run(["git", "add", "README.md"], path)
    run(["git", "commit", "-m", "Initial commit"], path)
    (path / "README.md").write_text("# Audit Smoke\n\nChanged content.\n", encoding="utf-8")
    run(["git", "add", "README.md"], path)
    run(["git", "commit", "-m", "Change README"], path)


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


def prompt_visible(screen, name):
    return name in screen.all_text() and "›" in screen.all_text()


def assert_not_legacy_handoff(screen):
    text = screen.all_text()
    forbidden = ["agent task:", "route: agent task", "Run this as an agent task?"]
    found = [item for item in forbidden if item in text]
    if found:
        raise AssertionError(f"legacy route handoff rendered: {found}\n{screen.dump()}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None, help="DeepSeek binary to test.")
    parser.add_argument("--name", default="deepseek")
    args = parser.parse_args()

    binary = args.binary or str(Path.cwd() / "target" / "debug" / args.name)
    if not os.path.exists(binary):
        binary = shutil.which(args.name) or binary
    if not os.path.exists(binary):
        raise SystemExit(f"binary not found: {binary}")
    binary = os.path.abspath(binary)

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-claude-audit-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        fake_bin = tmp_path / "bin"
        home.mkdir()
        fake_bin.mkdir()
        make_repo(workspace)
        write_fake_curl(fake_bin)

        env = os.environ.copy()
        env["HOME"] = str(home)
        env["DEEPSEEK_API_KEY"] = "claude-audit-debug-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn(binary, ["chat"], env, str(workspace))
        screen = Screen()
        try:
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "initial dock")

            os.write(master, b"audit commit HEAD\r")
            wait_for(
                lambda: screen.contains("agent step 1: run_shell"),
                master,
                screen,
                "commit audit shell tool step",
            )
            wait_for(
                lambda: screen.contains("run_shell requires approval"),
                master,
                screen,
                "dock approval modal",
            )
            wait_for(
                lambda: screen.contains("command: git show --stat --patch HEAD --"),
                master,
                screen,
                "git show command preview",
            )
            os.write(master, b"1\r")
            wait_for(
                lambda: screen.contains("approval: approved run_shell"),
                master,
                screen,
                "approval persisted in transcript",
            )
            wait_for(
                lambda: screen.contains("No blocking findings in the audited commit"),
                master,
                screen,
                "audit final answer",
            )
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "dock restored")
            assert_not_legacy_handoff(screen)

            os.write(master, b"/exit\r")
            end = time.monotonic() + 4.0
            while proc.poll() is None and time.monotonic() < end:
                read_available(master, screen, 0.05)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} claude audit approval debug: ok")


if __name__ == "__main__":
    main()
