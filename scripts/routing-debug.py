#!/usr/bin/env python3
"""Debug chat-to-agent routing through the real interactive CLI.

This is intended for external auditors: run it against a built or installed
binary and paste the PASS/FAIL output into the review.
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
import termios
import time
from pathlib import Path


ROWS = 28
COLS = 100


class Screen:
    def __init__(self, rows=ROWS, cols=COLS):
        self.rows = rows
        self.cols = cols
        self.cells = [[" "] * cols for _ in range(rows)]
        self.history = []
        self.row = 0
        self.col = 0
        self.scroll_top = 0
        self.scroll_bottom = rows - 1
        self.decoder = codecs.getincrementaldecoder("utf-8")("ignore")

    def feed(self, data):
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

    def contains(self, text):
        return text in self.text()

    def text(self):
        return "\n".join(self.history + [self.line(row) for row in range(self.rows)])

    def line(self, row):
        return "".join(self.cells[row]).rstrip()

    def bottom(self):
        return self.line(self.rows - 1)

    def dump(self):
        return "\n".join(f"{row:02d}|{self.line(row)}" for row in range(self.rows))

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
            return index
        if data[index] != "[":
            return index + 1
        index += 1
        start = index
        while index < len(data) and not ("@" <= data[index] <= "~"):
            index += 1
        if index >= len(data):
            return index
        params = data[start:index]
        final = data[index]
        nums = []
        for part in params.split(";"):
            if part == "" or part.startswith("?"):
                continue
            try:
                nums.append(int(part))
            except ValueError:
                pass
        if final in ("H", "f"):
            row = nums[0] if nums and nums[0] else 1
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
        return index + 1


def read_available(fd, screen, timeout=0.05):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        ready, _, _ = select.select([fd], [], [], 0.02)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            return
        if not chunk:
            return
        text = screen.decoder.decode(chunk)
        screen.feed(text)
        if "\x1b[6n" in text:
            os.write(fd, f"\x1b[{screen.row + 1};{screen.col + 1}R".encode())


def wait_for(fd, screen, predicate, label, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        read_available(fd, screen)
        if predicate():
            return
    raise AssertionError(f"timed out waiting for {label}\n{screen.dump()}")


def resolve_binary(name, binary_arg):
    if binary_arg:
        return os.path.abspath(binary_arg)
    candidate = Path.cwd() / "target" / "release" / name
    if candidate.exists():
        return str(candidate)
    found = shutil.which(name)
    if found:
        return found
    raise SystemExit(f"binary not found for {name}; pass --binary")


def make_env(name, home):
    env = os.environ.copy()
    env["HOME"] = str(home)
    env[f"{name.upper()}_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
    env[f"{name.upper()}_DEBUG_STREAM_DELAY_MS"] = "1"
    return env


def enable_debug(binary, env):
    subprocess.run([binary, "debug", "on"], env=env, check=True, stdout=subprocess.DEVNULL)


def spawn_chat(binary, env, cwd):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    proc = subprocess.Popen(
        [binary, "chat"],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        cwd=cwd,
        close_fds=True,
    )
    os.close(slave)
    return master, proc, Screen()


def run_prompt(binary, env, name, cwd, prompt, setup_commands=None):
    master, proc, screen = spawn_chat(binary, env, cwd)
    try:
        wait_for(master, screen, lambda: name in screen.text() and "›" in screen.text(), "initial prompt")
        os.write(master, b"/runtime legacy-routing on\r")
        wait_for(master, screen, lambda: "Routing: legacy-deterministic" in screen.text(), "legacy routing")
        os.write(master, b"/root clear\r")
        wait_for(master, screen, lambda: "root:" in screen.text(), "root clear")
        for command in setup_commands or []:
            os.write(master, (command + "\r").encode())
            wait_for(master, screen, lambda: "root-source: explicit" in screen.text(), command)
        os.write(master, (prompt + "\r").encode())
        wait_for(
            master,
            screen,
            lambda: any(
                marker in screen.text()
                for marker in (
                    "debug/manual backend",
                    "route: agent task",
                    "route: unclear",
                    "root-source: explicit",
                )
            ),
            f"route for {prompt!r}",
            timeout=15.0,
        )
        return screen.text()
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
        os.close(master)


def classify_output(text):
    if "route: agent task" in text:
        return "agent"
    if "route: unclear" in text:
        return "clarify"
    if "debug/manual backend" in text:
        return "chat"
    if "root:" in text and "root-source: explicit" in text:
        return "root"
    return "unknown"


def path_visible(text, path):
    compact_text = "".join(text.split())
    return any(candidate in compact_text for candidate in {str(path), os.path.realpath(path)})


def check_case(binary, env, name, cwd, prompt, expected, root=None, setup_commands=None):
    text = run_prompt(binary, env, name, cwd, prompt, setup_commands=setup_commands)
    actual = classify_output(text)
    ok = actual == expected
    if root is not None:
        ok = ok and path_visible(text, root)
    status = "PASS" if ok else "FAIL"
    detail = f"{prompt!r} -> {actual}"
    if root is not None:
        detail += f", expected root {root}"
    print(f"{status:4} {detail}")
    if not ok:
        print("---- screen ----")
        print(text)
        print("---------------")
    return ok


def run_sequence(binary, env, name, cwd, commands):
    master, proc, screen = spawn_chat(binary, env, cwd)
    try:
        wait_for(master, screen, lambda: name in screen.text() and "›" in screen.text(), "initial prompt")
        for command, predicate, label in commands:
            os.write(master, (command + "\r").encode())
            wait_for(master, screen, lambda: predicate(screen.text()), label, timeout=8.0)
        return screen.text()
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
        os.close(master)


def check_persistent_navigation(binary, env, name, home, env_root, provider_repo):
    commands = [
        (
            "/runtime legacy-routing on",
            lambda text: "Routing: legacy-deterministic" in text,
            "legacy routing enabled",
        ),
        (
            "navigate into my env folder",
            lambda text: "root-source: explicit" in text and path_visible(text, env_root),
            "navigate into env",
        ),
        (
            "/status",
            lambda text: "mode: chat" in text and "root-source: explicit" in text and path_visible(text, env_root),
            "status keeps env root",
        ),
        (
            "fix this repo",
            lambda text: "route: agent task" in text and path_visible(text, env_root),
            "follow-up task uses env root",
        ),
        (
            f"cd into {name}",
            lambda text: "root-source: explicit" in text and path_visible(text, provider_repo),
            "cd into repo alias",
        ),
        (
            f"go inside ~/{provider_repo.relative_to(home)}",
            lambda text: "root-source: explicit" in text and path_visible(text, provider_repo),
            "go inside repo path",
        ),
        (
            "/root clear",
            lambda text: "root: unset" in text,
            "root clear",
        ),
    ]
    text = run_sequence(binary, env, name, home, commands)
    ok = all(predicate(text) for _, predicate, _ in commands)
    status = "PASS" if ok else "FAIL"
    print(f"{status:4} persistent natural navigation keeps selected root")
    if not ok:
        print("---- screen ----")
        print(text)
        print("---------------")
    return ok


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--name", default="deepseek")
    parser.add_argument("--binary")
    parser.add_argument("--model", help="Accepted for parity with smoke wrappers; routing-debug uses the binary default.")
    args = parser.parse_args()

    binary = resolve_binary(args.name, args.binary)
    passed = 0
    failed = 0

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-routing-debug-") as tmp:
        home = Path(tmp) / "home"
        home.mkdir()
        desktop = home / "Desktop"
        downloads = home / "Downloads"
        documents = home / "Documents"
        env_root = home / "env"
        deepseek_repo = env_root / "deepseek"
        minimax_repo = env_root / "minimax"
        workspace = home / "workspace"
        for path in (desktop, downloads, documents, env_root, deepseek_repo, minimax_repo, workspace):
            path.mkdir()
        (workspace / "README.md").write_text("routing debug workspace\n", encoding="utf-8")

        env = make_env(args.name, home)
        enable_debug(binary, env)

        cases = [
            (home, "hi", "chat", None, None),
            (home, "what is a desktop?", "chat", None, None),
            (home, "how do I read files in Rust?", "chat", None, None),
            (home, "can you explain config files?", "chat", None, None),
            (home, "can you explain this repo structure?", "chat", None, None),
            (home, "what is a config file?", "chat", None, None),
            (home, "why is my code broken?", "chat", None, None),
            (home, "read my files on my desktop", "agent", desktop, None),
            (home, "go through downloads", "agent", downloads, None),
            (home, "go through my env folder and tell me what you find there", "agent", env_root, None),
            (home, "go to my env folder", "root", env_root, None),
            (home, "scan desktop", "agent", desktop, None),
            (home, "scan the downloads", "agent", downloads, None),
            (home, "my desktop files are a mess", "agent", desktop, None),
            (home, "read this function", "clarify", None, None),
            (home, "scan the config", "clarify", None, None),
            (home, "the config is broken", "clarify", None, None),
            (home, "my files are a mess", "clarify", None, None),
            (home, "the tests are failing", "clarify", None, None),
            (home, "agent task", "clarify", None, None),
            (workspace, "the config is broken", "agent", workspace, None),
            (workspace, "this repo needs cleanup", "agent", workspace, None),
            (workspace, "the repo is broken", "agent", workspace, None),
            (workspace, "the project is failing", "agent", workspace, None),
            (workspace, "the codebase has errors", "agent", workspace, None),
            (workspace, "the project needs cleanup", "agent", workspace, None),
            (workspace, "review this project", "agent", workspace, None),
            (workspace, "go through the codebase", "agent", workspace, None),
            (workspace, "scan the codebase", "agent", workspace, None),
            (workspace, "my files are a mess", "agent", workspace, None),
            (workspace, "the tests are failing", "agent", workspace, None),
            (workspace, "the app is not working", "agent", workspace, None),
            (home, "go through downloads", "agent", downloads, [f"/root {workspace}"]),
            (home, "fix this repo", "agent", workspace, [f"/root {workspace}"]),
        ]

        for cwd, prompt, expected, root, setup in cases:
            if check_case(binary, env, args.name, cwd, prompt, expected, root, setup):
                passed += 1
            else:
                failed += 1

        provider_repo = deepseek_repo if args.name == "deepseek" else minimax_repo
        if check_persistent_navigation(binary, env, args.name, home, env_root, provider_repo):
            passed += 1
        else:
            failed += 1

    print(f"\nsummary: {passed} passed, {failed} failed")
    raise SystemExit(1 if failed else 0)


if __name__ == "__main__":
    main()
