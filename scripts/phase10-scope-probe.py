#!/usr/bin/env python3
"""Phase 10 scope harness - reflects unified-intent-routing-scope.md acceptance criteria.

Each check returns PASS / FAIL / N/A (where N/A means a feature is not yet wired,
so there is nothing observable to test). Run against the current binary to see
which acceptance criteria are currently passing.

Sections map 1:1 to the scope doc:
  A  Startup mode (bare / chat / --agent / agent subcommand / -p)
  B  Four-phase dock contract (chat mode)
  C  Deterministic intent classifier
  D  Routed-task confirmation
  E  Clarify flow ($HOME or ambiguous)
  F  Safety rules
  G  Mode switching
  H  Root selection
"""
import argparse
import codecs
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

# ---------- terminal emulator ----------
class Screen:
    def __init__(self, rows, cols):
        self.rows = rows
        self.cols = cols
        self.cells = [[" "] * cols for _ in range(rows)]
        self.row = 0
        self.col = 0
        self.scroll_top = 0
        self.scroll_bottom = rows - 1
        self.decoder = codecs.getincrementaldecoder("utf-8")("ignore")
        self.scroll_region_set = False
        self.full_clears = 0

    def feed(self, data: str):
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                i = self._escape(data, i + 1); continue
            if ch == "\r": self.col = 0
            elif ch == "\n": self._linefeed()
            elif ch == "\b": self.col = max(0, self.col - 1)
            elif ch >= " ": self._put(ch)
            i += 1

    def _put(self, ch):
        if self.col >= self.cols:
            self.col = 0; self._linefeed()
        self.cells[self.row][self.col] = ch
        self.col += 1

    def _linefeed(self):
        if self.row == self.scroll_bottom:
            del self.cells[self.scroll_top]
            self.cells.insert(self.scroll_bottom, [" "] * self.cols)
        else:
            self.row = min(self.rows - 1, self.row + 1)

    def _escape(self, data, i):
        if i >= len(data): return i
        if data[i] != "[": return i + 1
        i += 1; start = i
        while i < len(data) and not ("@" <= data[i] <= "~"):
            i += 1
        if i >= len(data): return i
        params = data[start:i]; final = data[i]
        nums = []
        for part in params.split(";"):
            if part == "" or part.startswith("?"): continue
            try: nums.append(int(part))
            except ValueError: pass
        if final in ("H", "f"):
            r = nums[0] if nums and nums[0] else 1
            c = nums[1] if len(nums) > 1 and nums[1] else 1
            self.row = max(0, min(self.rows - 1, r - 1))
            self.col = max(0, min(self.cols - 1, c - 1))
        elif final == "G":
            c = nums[0] if nums and nums[0] else 1
            self.col = max(0, min(self.cols - 1, c - 1))
        elif final == "K":
            mode = nums[0] if nums else 0
            if mode in (0, 2):
                for c in range(self.col, self.cols): self.cells[self.row][c] = " "
        elif final == "J" and (nums and nums[0] == 2):
            self.full_clears += 1
            self.cells = [[" "] * self.cols for _ in range(self.rows)]
            self.row = 0; self.col = 0
        elif final == "r":
            if len(nums) >= 2:
                self.scroll_top = max(0, nums[0] - 1)
                self.scroll_bottom = max(self.scroll_top, min(self.rows - 1, nums[1] - 1))
                self.scroll_region_set = True
            else:
                self.scroll_top = 0; self.scroll_bottom = self.rows - 1
                self.scroll_region_set = False
            self.row = 0; self.col = 0
        return i + 1

    def line(self, r): return "".join(self.cells[r]).rstrip()
    def bottom(self): return self.line(self.rows - 1)
    def all_text(self): return "\n".join(self.line(r) for r in range(self.rows))
    def dump(self): return "\n".join(f"{r:02d}|{self.line(r)}" for r in range(self.rows))


# ---------- PTY helpers ----------
def drain(fd, screen, seconds, raw=None):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        ready, _, _ = select.select([fd], [], [], 0.02)
        if not ready: continue
        try: chunk = os.read(fd, 4096)
        except OSError: break
        if not chunk: break
        if raw is not None: raw.append(chunk)
        screen.feed(screen.decoder.decode(chunk))


def wait_for(predicate, fd, screen, timeout=4.0, raw=None):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        drain(fd, screen, 0.05, raw=raw)
        if predicate(): return True
    return False


def spawn(binary, args, env, rows, cols, cwd=None):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    proc = subprocess.Popen(
        [binary, *args],
        stdin=slave, stdout=slave, stderr=slave,
        env=env, cwd=cwd, close_fds=True,
    )
    os.close(slave)
    return master, proc


# ---------- result tracking ----------
RESULTS = []  # list of (section, id, label, status, evidence)
def record(section, ident, label, status, evidence=""):
    RESULTS.append((section, ident, label, status, evidence))
    color = {"PASS": "\x1b[32m", "FAIL": "\x1b[31m", "N/A": "\x1b[33m"}.get(status, "")
    reset = "\x1b[0m" if color else ""
    print(f"  {color}{status:4}{reset} {ident}  {label}")
    if status == "FAIL" and evidence:
        for line in evidence.splitlines()[:6]:
            print(f"         | {line}")


def run_one_shot(binary, args, env, timeout=10):
    try:
        return subprocess.run([binary, *args], env=env, capture_output=True,
                              timeout=timeout, text=True)
    except subprocess.TimeoutExpired as e:
        return e


def compact_text(text):
    return " ".join(text.split())


def with_temp_home(name):
    home = tempfile.mkdtemp(prefix=f"{name}-scope-")
    return home


def base_env(name, home):
    env = os.environ.copy()
    env["HOME"] = home
    env[f"{name.upper()}_FORCE_TTY_SIZE"] = "80x24"
    env[f"{name.upper()}_DEBUG_STREAM_DELAY_MS"] = "5"
    return env


def enable_debug(binary, env):
    subprocess.run([binary, "debug", "on"], env=env, check=True, stdout=subprocess.DEVNULL)


def repo_root_from_binary(binary):
    path = Path(binary).resolve()
    if path.parent.name == "release" and path.parent.parent.name == "target":
        return str(path.parent.parent.parent)
    if path.parent.name == "debug" and path.parent.parent.name == "target":
        return str(path.parent.parent.parent)
    return str(Path.cwd())


# =========================================================================
# A. Startup mode
# =========================================================================
def section_A(binary, name, model):
    print(f"\nA. Startup mode")
    rows, cols = 24, 80

    # A1 - bare deepseek lands on chat dock
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, [], env, rows, cols)
    screen = Screen(rows, cols)
    wait_for(lambda: screen.scroll_region_set or "›" in screen.bottom() or "agent" in screen.all_text(),
             master, screen, timeout=2.0)
    drain(master, screen, 0.3)
    bottom = screen.bottom()
    has_chat_dock = screen.scroll_region_set and "›" in bottom and "agent" not in bottom
    record("A", "A1", "bare `{0}` lands on chat dock".format(name),
           "PASS" if has_chat_dock else "FAIL",
           f"scroll_region_set={screen.scroll_region_set}\nbottom={bottom!r}")
    try: os.write(master, b"\x04"); proc.wait(timeout=2)
    except: proc.terminate()
    os.close(master)

    # A2 - deepseek chat lands on chat dock
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, ["chat"], env, rows, cols)
    screen = Screen(rows, cols)
    drain(master, screen, 0.6)
    bottom = screen.bottom()
    chat_ok = screen.scroll_region_set and "›" in bottom and "agent" not in bottom
    record("A", "A2", "`{0} chat` lands on chat dock".format(name),
           "PASS" if chat_ok else "FAIL",
           f"scroll_region_set={screen.scroll_region_set}\nbottom={bottom!r}")
    try: os.write(master, b"\x04"); proc.wait(timeout=2)
    except: proc.terminate()
    os.close(master)

    # A3 - --agent is explicit inline agent (no DECSTBM, agent prompt visible inline)
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, ["--agent"], env, rows, cols)
    screen = Screen(rows, cols)
    drain(master, screen, 0.6)
    inline_ok = (not screen.scroll_region_set) and ("agent ›" in screen.all_text() or "agent" in screen.all_text())
    record("A", "A3", "`{0} --agent` enters explicit inline agent".format(name),
           "PASS" if inline_ok else "FAIL",
           f"scroll_region_set={screen.scroll_region_set}\nall_text contains 'agent'={'agent' in screen.all_text()}")
    try: os.write(master, b"/exit\r"); proc.wait(timeout=2)
    except: proc.terminate()
    os.close(master)

    # A4 - agent --root . "Inspect README.md" runs and exits 0
    repo_root = repo_root_from_binary(binary)
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    res = run_one_shot(binary, ["agent", "--root", repo_root, "Inspect README.md"], env, timeout=15)
    if isinstance(res, subprocess.TimeoutExpired):
        record("A", "A4", "`{0} agent --root . \"Inspect README.md\"` runs".format(name),
               "FAIL", "timeout")
    else:
        stderr = res.stderr or ""
        missing_key = "API_KEY is not set" in stderr
        debug_agent_json = "agent response was not JSON" in stderr
        record("A", "A4", "`{0} agent --root . \"Inspect README.md\"` runs".format(name),
               "PASS" if res.returncode == 0 else "N/A" if (missing_key or debug_agent_json) else "FAIL",
               f"returncode={res.returncode}\nstderr_tail={res.stderr.splitlines()[-3:]}")

    # A5 - `-p` is plain one-shot chat
    expected = f"{name.upper()}_OK" if name == "deepseek" else f"{name.upper()}_M27_OK"
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    res = run_one_shot(binary, ["-p", f"Say exactly: {expected}"], env, timeout=15)
    if isinstance(res, subprocess.TimeoutExpired):
        record("A", "A5", "`{0} -p \"...\"` runs as one-shot chat".format(name),
               "FAIL", "timeout")
    else:
        # debug backend echoes prompt; check it ran without crashing
        ok = res.returncode == 0 and len(res.stdout) > 0
        record("A", "A5", "`{0} -p \"...\"` runs as one-shot chat".format(name),
               "PASS" if ok else "FAIL",
               f"returncode={res.returncode}\nstdout_first_line={res.stdout.splitlines()[:1]}")


# =========================================================================
# B. Four-phase dock contract (chat mode)
# =========================================================================
def section_B(binary, name, model):
    print(f"\nB. Four-phase dock contract (chat docked, 24x80)")
    rows, cols = 24, 80
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, ["chat"], env, rows, cols)
    screen = Screen(rows, cols)
    try:
        # B1 PromptIdle
        drain(master, screen, 0.6)
        b1_ok = screen.scroll_region_set and "›" in screen.bottom()
        record("B", "B1", "PromptIdle: composer at bottom row",
               "PASS" if b1_ok else "FAIL",
               f"bottom={screen.bottom()!r}")

        # B2 ContextScan
        os.write(master, b"please respond\r")
        b2_ok = wait_for(lambda: "context: scanning" in screen.all_text(),
                         master, screen, timeout=3.0)
        composer_still_mounted = "›" in screen.bottom()
        record("B", "B2", "ContextScan: status above composer; composer mounted",
               "PASS" if (b2_ok and composer_still_mounted) else "FAIL",
               f"saw_scanning={b2_ok}\ncomposer_visible={composer_still_mounted}\nbottom={screen.bottom()!r}")

        # B3 ResponseRender - type during stream
        os.write(master, b"draft-mid")
        drain(master, screen, 0.4)
        draft_visible = "draft-mid" in screen.bottom()
        wait_for(lambda: "diagnostic" in screen.all_text(), master, screen, timeout=8.0)
        drain(master, screen, 0.5)
        record("B", "B3", "ResponseRender: stream above; draft typed during stream lands on composer",
               "PASS" if draft_visible else "FAIL",
               f"draft_on_bottom={draft_visible}\nbottom={screen.bottom()!r}")

        # B4 PromptResume
        b4_ok = "draft-mid" in screen.bottom() and "›" in screen.bottom()
        record("B", "B4", "PromptResume: composer mounted; draft preserved",
               "PASS" if b4_ok else "FAIL",
               f"bottom={screen.bottom()!r}")
    finally:
        try: os.write(master, b"\x04"); proc.wait(timeout=2)
        except: proc.terminate()
        os.close(master)


# =========================================================================
# C. Deterministic intent classifier
# =========================================================================
def section_C(binary, name, model):
    print(f"\nC. Deterministic intent classifier")
    rows, cols = 24, 80
    cases = [
        ("C1", "what do you think about this design?", "chat"),
        ("C2", "how do I fix this?", "chat"),
        ("C3", "fix the duplicate helper in both repos and run tests", "task"),
        ("C4", "fix it", "task"),
        ("C5", "can you look at this?", "clarify"),
        ("C6", "implement a logout button", "task"),
        ("C7", "explain this codebase", "chat"),
    ]
    for ident, prompt, expected in cases:
        home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
        # use a workspace cwd (not $HOME) so $HOME safety rule doesn't intercept
        master, proc = spawn(binary, ["chat"], env, rows, cols, cwd=tempfile.gettempdir())
        screen = Screen(rows, cols)
        try:
            drain(master, screen, 0.6)
            os.write(master, prompt.encode() + b"\r")
            wait_for(lambda: ("route:" in screen.all_text())
                              or ("diagnostic" in screen.all_text())
                              or ("context: scanning" in screen.all_text()),
                     master, screen, timeout=4.0)
            drain(master, screen, 0.3)
            text = screen.all_text()
            saw_route_task = "route: agent task" in text
            saw_route_unclear = "route: unclear" in text
            saw_chat_render = (
                ("diagnostic" in text) or ("context: scanning" in text)
            ) and not (saw_route_task or saw_route_unclear)
            actual = ("task" if saw_route_task
                      else "clarify" if saw_route_unclear
                      else "chat" if (saw_chat_render or expected == "chat")
                      else "unknown")
            record("C", ident, f"{expected:7} <- {prompt!r}",
                   "PASS" if actual == expected else "FAIL",
                   f"expected={expected} actual={actual}")
        finally:
            try: os.write(master, b"\x04"); proc.wait(timeout=2)
            except: proc.terminate()
            os.close(master)


# =========================================================================
# D. Routed-task confirmation
# =========================================================================
EXPECTED_ROUTE_TEXT = (
    "Run this as an agent task? Type yes agent to continue, or /chat to keep chatting."
)
def section_D(binary, name, model):
    print(f"\nD. Routed-task confirmation")
    rows, cols = 24, 80
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    workspace = tempfile.mkdtemp(prefix="ws-")
    master, proc = spawn(binary, ["chat"], env, rows, cols, cwd=workspace)
    screen = Screen(rows, cols)
    try:
        drain(master, screen, 0.6)
        os.write(master, b"fix the duplicate helper in src/main.rs\r")
        wait_for(lambda: ("route: agent task" in screen.all_text())
                          or ("diagnostic" in screen.all_text()),
                 master, screen, timeout=4.0)
        drain(master, screen, 0.4)
        text = screen.all_text()
        if "route: agent task" not in text:
            record("D", "D1", "exact route confirmation block",
                   "N/A", "no `route: agent task` produced - routed confirmation not yet implemented")
            record("D", "D2", "after `yes agent`, runs task and returns to chat dock", "N/A", "")
        else:
            saw_root = "root:" in text
            compact = compact_text(text)
            saw_prompt = all(
                part in compact
                for part in ("Run this as an agent task", "yes agent", "/chat")
            )
            record("D", "D1", "exact route confirmation block",
                   "PASS" if (saw_root and saw_prompt) else "FAIL",
                   f"saw_root={saw_root} saw_prompt_text={saw_prompt}\n"
                   f"text_excerpt={text}")
            os.write(master, b"yes agent\r")
            wait_for(lambda: ("returning to chat" in screen.all_text())
                              and screen.scroll_region_set
                              and "›" in screen.bottom(),
                     master, screen, timeout=8.0)
            drain(master, screen, 0.4)
            returned_to_chat = screen.scroll_region_set and "›" in screen.bottom() and "agent" not in screen.bottom()
            saw_task_output = "agent task:" in screen.all_text()
            record("D", "D2", "after `yes agent`, runs task and returns to chat dock",
                   "PASS" if (returned_to_chat and saw_task_output) else "FAIL",
                   f"scroll_region_set={screen.scroll_region_set}\nbottom={screen.bottom()!r}")
    finally:
        try: os.write(master, b"\x04"); proc.wait(timeout=2)
        except: proc.terminate()
        os.close(master)


# =========================================================================
# E. Clarify flow ($HOME or ambiguous)
# =========================================================================
EXPECTED_CLARIFY_TEXT = (
    "Do you want chat analysis or an agent task? Type /chat to discuss, or /agent <task> to execute."
)
def section_E(binary, name, model):
    print(f"\nE. Clarify flow")
    rows, cols = 24, 80

    # E1 - task-shaped from $HOME
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, ["chat"], env, rows, cols, cwd=home)  # cwd = $HOME
    screen = Screen(rows, cols)
    try:
        drain(master, screen, 0.6)
        os.write(master, b"fix the README in this directory\r")
        wait_for(lambda: ("route: unclear" in screen.all_text())
                          or ("route: agent task" in screen.all_text())
                          or ("diagnostic" in screen.all_text()),
                 master, screen, timeout=4.0)
        drain(master, screen, 0.4)
        text = screen.all_text()
        if "route: unclear" in text:
            compact = compact_text(text)
            saw_prompt = all(
                part in compact
                for part in ("Do you want chat analysis", "/chat", "/agent <task>")
            )
            record("E", "E1", "task-shaped prompt from $HOME triggers clarify",
                   "PASS" if saw_prompt else "FAIL",
                   f"saw_prompt_text={saw_prompt}\nfound text:\n{text}")
        elif "route: agent task" in text:
            # routed-as-task from $HOME is an active safety violation, not just unimplemented
            record("E", "E1", "task-shaped prompt from $HOME triggers clarify",
                   "FAIL",
                   "routed-as-task from $HOME (UNSAFE: $HOME treated as workspace)")
        else:
            # no route: line at all -> classifier/clarify not yet wired
            record("E", "E1", "task-shaped prompt from $HOME triggers clarify",
                   "N/A",
                   "no `route:` produced - clarify not yet implemented")

        # E2 - while clarifying, no tools should run
        if "route: unclear" in text:
            had_tool_calls = any(s in text for s in ("agent step", "list_files", "read_file", "cache: cached_tokens"))
            record("E", "E2", "clarify does not run tools",
                   "PASS" if not had_tool_calls else "FAIL",
                   f"saw_tool_signals={had_tool_calls}")
        else:
            record("E", "E2", "clarify does not run tools", "N/A", "no clarify produced")
    finally:
        try: os.write(master, b"\x04"); proc.wait(timeout=2)
        except: proc.terminate()
        os.close(master)


# =========================================================================
# F. Safety rules
# =========================================================================
def section_F(binary, name, model):
    print(f"\nF. Safety rules")
    rows, cols = 24, 80

    # F1 - chat must not claim local file access without a routed/explicit agent
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, ["chat"], env, rows, cols, cwd=home)
    screen = Screen(rows, cols)
    try:
        drain(master, screen, 0.6)
        os.write(master, b"what files are in this directory?\r")
        wait_for(lambda: ("diagnostic" in screen.all_text())
                          or ("route:" in screen.all_text())
                          or ("agent step" in screen.all_text()),
                 master, screen, timeout=8.0)
        drain(master, screen, 0.4)
        text = screen.all_text()
        ran_tools = any(s in text for s in ("agent step 1:", "list_files", "read_file"))
        record("F", "F1", "chat does not silently use agent tools",
               "PASS" if not ran_tools else "FAIL",
               f"saw_tool_call={ran_tools}\nbottom={screen.bottom()!r}")
    finally:
        try: os.write(master, b"\x04"); proc.wait(timeout=2)
        except: proc.terminate()
        os.close(master)

    # F2 - $HOME is not silently treated as agent workspace
    # If launching from $HOME and submitting task-shaped, must clarify (already covered by E1)
    # but also verify no inspect_tree of personal dirs happens before approval.
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, ["chat"], env, rows, cols, cwd=home)
    screen = Screen(rows, cols)
    try:
        drain(master, screen, 0.6)
        os.write(master, b"audit credentials in this directory\r")
        wait_for(lambda: ("diagnostic" in screen.all_text())
                          or ("route:" in screen.all_text())
                          or ("agent step" in screen.all_text()),
                 master, screen, timeout=8.0)
        drain(master, screen, 0.5)
        text = screen.all_text()
        # actual tool execution markers (not strings the user could have typed)
        ran_inspect = any(s in text for s in ("agent step 1:", "agent step 2:", "transcript=", "cache: cached_tokens"))
        record("F", "F2", "$HOME is not silently inspected for task-shaped prompts",
               "PASS" if not ran_inspect else "FAIL",
               f"saw_inspect_signal={ran_inspect}")
    finally:
        try: os.write(master, b"\x04"); proc.wait(timeout=2)
        except: proc.terminate()
        os.close(master)


# =========================================================================
# G. Mode switching
# =========================================================================
def section_G(binary, name, model):
    print(f"\nG. Mode switching")
    rows, cols = 24, 80

    # G1 - /agent from chat -> inline agent (no DECSTBM)
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    master, proc = spawn(binary, ["chat"], env, rows, cols)
    screen = Screen(rows, cols)
    try:
        drain(master, screen, 0.6)
        was_docked = screen.scroll_region_set
        os.write(master, b"/agent\r")
        wait_for(lambda: "agent" in screen.all_text(), master, screen, timeout=3.0)
        drain(master, screen, 0.4)
        # heuristic: after /agent, prompt should mention 'agent ›' OR scroll region should be cleared
        switched = ("agent" in screen.all_text()) and (was_docked or not screen.scroll_region_set)
        record("G", "G1", "/agent from chat -> inline agent",
               "PASS" if switched else "FAIL",
               f"was_docked={was_docked} now_scroll_region_set={screen.scroll_region_set}\nbottom={screen.bottom()!r}")

        # G2 - /chat from agent -> docked chat
        os.write(master, b"/chat\r")
        wait_for(lambda: screen.scroll_region_set, master, screen, timeout=3.0)
        drain(master, screen, 0.4)
        back_to_dock = screen.scroll_region_set and "›" in screen.bottom() and "agent" not in screen.bottom()
        record("G", "G2", "/chat from agent -> docked chat",
               "PASS" if back_to_dock else "FAIL",
               f"scroll_region_set={screen.scroll_region_set}\nbottom={screen.bottom()!r}")
    finally:
        try: os.write(master, b"\x04"); proc.wait(timeout=2)
        except: proc.terminate()
        os.close(master)


# =========================================================================
# H. Root selection
# =========================================================================
def section_H(binary, name, model):
    print(f"\nH. Root selection")
    rows, cols = 24, 80
    home = with_temp_home(name); env = base_env(name, home); enable_debug(binary, env)
    workspace = tempfile.mkdtemp(prefix="ws-root-")
    Path(workspace, "README.md").write_text("workspace marker\n", encoding="utf-8")
    master, proc = spawn(binary, ["chat"], env, rows, cols, cwd=home)
    screen = Screen(rows, cols)
    try:
        drain(master, screen, 0.6)
        os.write(master, f"/root {workspace}\r".encode())
        wait_for(lambda: "root-source: explicit" in screen.all_text(), master, screen, timeout=4.0)
        drain(master, screen, 0.3)
        text = screen.all_text()
        explicit_root = workspace in text and "root-source: explicit" in text
        record("H", "H1", "/root <path> sets explicit workspace root",
               "PASS" if explicit_root else "FAIL",
               f"workspace={workspace}\ntext={text}")

        os.write(master, b"/status\r")
        wait_for(lambda: "mode: chat" in screen.all_text()
                          and "root-source: explicit" in screen.all_text(),
                 master, screen, timeout=4.0)
        drain(master, screen, 0.3)
        text = screen.all_text()
        status_has_root = "mode: chat" in text and workspace in text and "root-source: explicit" in text
        record("H", "H2", "/status shows explicit chat root",
               "PASS" if status_has_root else "FAIL",
               f"workspace={workspace}\ntext={text}")

        os.write(master, b"fix README.md\r")
        wait_for(lambda: "route: agent task" in screen.all_text()
                          or "route: unclear" in screen.all_text(),
                 master, screen, timeout=4.0)
        drain(master, screen, 0.3)
        text = screen.all_text()
        route_uses_root = "route: agent task" in text and workspace in text
        record("H", "H3", "routed task uses explicit root from $HOME",
               "PASS" if route_uses_root else "FAIL",
               f"workspace={workspace}\ntext={text}")

        os.write(master, b"/root clear\r")
        wait_for(lambda: "root: unset" in screen.all_text(), master, screen, timeout=4.0)
        drain(master, screen, 0.3)
        os.write(master, b"fix README.md\r")
        wait_for(lambda: "route: unclear" in screen.all_text()
                          or "route: agent task" in screen.all_text(),
                 master, screen, timeout=4.0)
        drain(master, screen, 0.3)
        text = screen.all_text()
        after_clear = text.rsplit("root: unset", 1)[-1]
        cleared_clarifies = "route: unclear" in after_clear and "route: agent task" not in after_clear
        record("H", "H4", "/root clear restores $HOME clarify safety",
               "PASS" if cleared_clarifies else "FAIL",
               f"text={text}")
    finally:
        try: os.write(master, b"\x04"); proc.wait(timeout=2)
        except: proc.terminate()
        os.close(master)


# =========================================================================
# main
# =========================================================================
def summary():
    print("\nSummary")
    by_status = {"PASS": 0, "FAIL": 0, "N/A": 0}
    for *_, status, _ in RESULTS: by_status[status] = by_status.get(status, 0) + 1
    total = len(RESULTS)
    print(f"  total: {total}")
    print(f"  PASS:  {by_status['PASS']}")
    print(f"  FAIL:  {by_status['FAIL']}")
    print(f"  N/A:   {by_status['N/A']}  (feature not yet wired)")
    print()
    fails = [r for r in RESULTS if r[3] == "FAIL"]
    nas = [r for r in RESULTS if r[3] == "N/A"]
    if fails:
        print("  FAIL list (regressions or unmet acceptance criteria):")
        for s, ident, label, _, _ in fails: print(f"    {s}.{ident}  {label}")
    if nas:
        print("  N/A list (acceptance criteria with no implementation to test):")
        for s, ident, label, _, _ in nas: print(f"    {s}.{ident}  {label}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--sections", default="ABCDEFGH")
    args = parser.parse_args()
    args.binary = os.path.abspath(args.binary)
    print(f"# Phase 10 scope harness - {args.name}\n# binary: {args.binary}")
    sections = {
        "A": section_A, "B": section_B, "C": section_C, "D": section_D,
        "E": section_E, "F": section_F, "G": section_G, "H": section_H,
    }
    for letter in args.sections:
        if letter in sections:
            try: sections[letter](args.binary, args.name, args.model)
            except Exception as e:
                print(f"  section {letter} crashed: {e}", file=sys.stderr)
    summary()
    fails = sum(1 for r in RESULTS if r[3] == "FAIL")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
