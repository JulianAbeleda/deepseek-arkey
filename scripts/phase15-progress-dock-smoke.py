#!/usr/bin/env python3
"""Phase 15 progress dock smoke.

This catches the regression where Loading/tool-step progress either disappears
during active work or leaks into the final answer/scrollback. It also checks
the current source contract so audit reports cannot pass while describing the
old status_above progress mechanism.
"""

import argparse
import json
import os
import stat
import tempfile
import textwrap
import time
from pathlib import Path

from smoke_lib import (
    Screen,
    prompt_visible,
    read_available,
    resolve_binary,
    spawn_pty,
    wait_for,
)


ROWS = 24
COLS = 100


def numbered_lines(path, needle):
    lines = []
    for index, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if needle in line:
            lines.append((index, line.strip()))
    return lines


def assert_source_contract(repo_root):
    repl = repo_root / "src" / "repl.rs"
    input_rs = repo_root / "src" / "input.rs"

    progress_calls = numbered_lines(repl, "progress_dock(&context_scan_status")
    stale_calls = numbered_lines(repl, "status_above(&context_scan_status")
    progress_def = numbered_lines(input_rs, "pub fn progress_dock")
    progress_clear = numbered_lines(input_rs, "self.progress_rows.clear();")

    if not progress_calls:
        raise AssertionError("missing progress_dock context-scan call in src/repl.rs")
    if stale_calls:
        details = "\n".join(f"{line}: {text}" for line, text in stale_calls)
        raise AssertionError(f"stale status_above context-scan progress path found:\n{details}")
    if not progress_def:
        raise AssertionError("missing DockedComposer::progress_dock in src/input.rs")
    if not progress_clear:
        raise AssertionError("missing progress_rows clear path in src/input.rs")

    print("source_contract=PASS")
    for line, text in progress_calls:
        print(f"progress_render=repl.rs:{line}: {text}")
    for line, text in progress_def:
        print(f"progress_api=input.rs:{line}: {text}")
    for line, text in progress_clear:
        print(f"progress_clear=input.rs:{line}: {text}")


def write_slow_fake_curl(directory):
    path = Path(directory) / "curl"
    path.write_text(
        textwrap.dedent(
            r"""
            #!/usr/bin/env python3
            import json
            import sys
            import time

            config = sys.stdin.read()

            if "Tool result for step" in config:
                time.sleep(1.5)
                decision = {"final_answer": "files listed successfully"}
            else:
                time.sleep(1.5)
                decision = {
                    "thought": "listing files as requested",
                    "tool": {"name": "list_files", "arguments": {"path": "."}},
                }

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


def run_progress_smoke(binary, name):
    with tempfile.TemporaryDirectory(prefix=f"{name}-progress-dock-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        fake_bin = tmp_path / "bin"
        home.mkdir()
        workspace.mkdir()
        fake_bin.mkdir()
        (workspace / "hello.txt").write_text("hello\n", encoding="utf-8")
        write_slow_fake_curl(fake_bin)

        env = os.environ.copy()
        env["HOME"] = str(home)
        env["DEEPSEEK_API_KEY"] = "progress-dock-smoke-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        saw_loading_in_dock = False
        saw_tool_step_in_dock = False

        try:
            wait_for(
                lambda: prompt_visible(screen, name),
                master,
                screen,
                "initial dock prompt",
                timeout=10.0,
            )

            os.write(master, b"list files\r")

            deadline = time.monotonic() + 6.0
            while time.monotonic() < deadline:
                read_available(master, screen, 0.1)
                dock = screen.dock_text()
                if "Loading " in dock:
                    saw_loading_in_dock = True
                if "agent step 1: list_files" in dock:
                    saw_tool_step_in_dock = True
                if saw_loading_in_dock and saw_tool_step_in_dock:
                    break
                if "files listed successfully" in screen.all_text():
                    break

            wait_for(
                lambda: "files listed successfully" in screen.all_text(),
                master,
                screen,
                "final answer",
                timeout=10.0,
            )
            wait_for(
                lambda: prompt_visible(screen, name),
                master,
                screen,
                "dock prompt after final answer",
                timeout=6.0,
            )
            read_available(master, screen, 0.3)

            all_text = screen.all_text()
            final_has_loading = "Loading " in all_text
            final_has_tool_step = "agent step 1: list_files" in all_text

            print(f"saw_loading_in_dock={saw_loading_in_dock}")
            print(f"saw_tool_step_in_dock={saw_tool_step_in_dock}")
            print(f"final_has_loading={final_has_loading}")
            print(f"final_has_tool_step={final_has_tool_step}")

            failures = []
            if not saw_loading_in_dock:
                failures.append("Loading never appeared in dock_text() during active turn")
            if not saw_tool_step_in_dock:
                failures.append("agent step 1: list_files never appeared in dock_text()")
            if final_has_loading:
                failures.append("'Loading ' persisted in all_text() after final answer")
            if final_has_tool_step:
                failures.append("'agent step 1: list_files' persisted in all_text()")
            if failures:
                raise AssertionError("\n".join(failures) + "\n" + screen.dump())
        finally:
            if proc.poll() is None:
                try:
                    os.write(master, b"/exit\r")
                    time.sleep(0.3)
                except OSError:
                    pass
                proc.terminate()
            os.close(master)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None, help="DeepSeek binary to test.")
    parser.add_argument("--name", default="deepseek")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[1]
    binary = resolve_binary(args.name, args.binary)

    assert_source_contract(repo_root)
    run_progress_smoke(binary, args.name)

    print(f"{args.name} phase15 progress dock smoke: ok")


if __name__ == "__main__":
    main()
