#!/usr/bin/env python3
"""Phase 11 docked routing smoke.

This validates the Kimi-style baseline path without a live provider:

- natural-language prompts in a safe workspace go through the agent loop
- final answers render above the existing dock
- loading and tool steps render inside the dock while work is active
- loading and tool steps do not persist after the final answer
- no Phase 10 `agent task:` stdout handoff or route-confirmation block appears
"""

import argparse
import json
import os
import stat
import tempfile
import textwrap
import time
from pathlib import Path

from smoke_lib import Screen, prompt_visible, read_available, resolve_binary, spawn_pty, wait_for


ROWS = 24
COLS = 100


def write_fake_curl(directory):
    path = Path(directory) / "curl"
    path.write_text(
        textwrap.dedent(
            r"""
            #!/usr/bin/env python3
            import json
            import sys

            config = sys.stdin.read()
            if "inspect shell denial gate" in config and "Tool result for step" in config:
                decision = {"final_answer": "shell denied as expected"}
            elif "Tool result for step" in config:
                decision = {"final_answer": "desktop scan complete"}
            elif "inspect shell denial gate" in config:
                decision = {
                    "thought": "request shell to verify dock denies it",
                    "tool": {
                        "name": "run_shell",
                        "arguments": {
                            "command": "pwd",
                            "cwd": ".",
                            "reason": "approval gate smoke",
                        },
                    },
                }
            elif "scan my desktop" in config:
                decision = {
                    "thought": "inspect desktop entries",
                    "tool": {"name": "list_files", "arguments": {"path": "."}},
                }
            else:
                decision = {"final_answer": "hi docked agent ok"}

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
    args = parser.parse_args()

    binary = resolve_binary(args.name, args.binary)

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-phase11-") as tmp:
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

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        try:
            wait_for(
                lambda: prompt_visible(screen, args.name),
                master,
                screen,
                "initial dock",
            )

            os.write(master, b"hi\r")
            wait_for(
                lambda: screen.contains("context: scanning")
                or screen.contains("hi docked agent ok"),
                master,
                screen,
                "hi scan row or fast final answer",
            )
            wait_for(
                lambda: screen.contains("hi docked agent ok"),
                master,
                screen,
                "hi final answer",
            )
            wait_for(
                lambda: prompt_visible(screen, args.name),
                master,
                screen,
                "dock after hi",
            )
            assert_not_legacy_handoff(screen)

            os.write(master, b"scan my desktop\r")
            wait_for(
                lambda: screen.contains("agent step 1: list_files"),
                master,
                screen,
                "tool step render",
            )
            wait_for(
                lambda: screen.contains("desktop scan complete"),
                master,
                screen,
                "scan final answer",
            )
            wait_for(
                lambda: prompt_visible(screen, args.name),
                master,
                screen,
                "dock after scan",
            )
            if screen.contains("Loading "):
                raise AssertionError(f"loading status persisted after final answer\n{screen.dump()}")
            if screen.contains("agent step 1: list_files"):
                raise AssertionError(f"tool step persisted after final answer\n{screen.dump()}")
            assert_not_legacy_handoff(screen)

            os.write(master, b"inspect shell denial gate\r")
            wait_for(
                lambda: screen.contains("agent step 1: run_shell"),
                master,
                screen,
                "shell tool step render",
            )
            wait_for(
                lambda: screen.contains("run_shell requires approval"),
                master,
                screen,
                "shell approval request",
            )
            os.write(master, b"n\r")
            wait_for(
                lambda: screen.contains("shell denied as expected"),
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

            os.write(master, b"/exit\r")
            end = time.monotonic() + 4.0
            while proc.poll() is None and time.monotonic() < end:
                read_available(master, screen, 0.05)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} phase11 docked routing smoke: ok")


if __name__ == "__main__":
    main()
