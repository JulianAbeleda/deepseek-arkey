#!/usr/bin/env python3
"""Phase 12 dock-native approval smoke.

This validates the Kimi-style baseline path without a live provider:

- approval-gated tools request approval above the dock
- denial and approval are submitted through the bottom composer
- no raw stdin approval prompt appears
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
                decision = {"final_answer": "shell denied through dock"}
            elif "inspect shell approval gate" in config and "Tool result for step" in config:
                decision = {"final_answer": "shell approved through dock"}
            elif "inspect patch denial gate" in config and "Tool result for step" in config:
                decision = {"final_answer": "patch denied through dock"}
            elif "inspect patch approval gate" in config and "Tool result for step" in config:
                decision = {"final_answer": "patch approved through dock"}
            elif "Tool result for step" in config:
                decision = {"final_answer": "unexpected tool result"}
            elif "inspect shell denial gate" in config:
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
            elif "inspect shell approval gate" in config:
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
            elif "inspect patch denial gate" in config:
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
            elif "inspect patch approval gate" in config:
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

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        try:
            wait_for(
                lambda: prompt_visible(screen, args.name),
                master,
                screen,
                "initial dock",
            )

            os.write(master, b"inspect shell denial gate\r")
            wait_for(
                lambda: screen.contains("run_shell requires approval"),
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

            os.write(master, b"inspect shell approval gate\r")
            wait_for(
                lambda: screen.contains("run_shell requires approval")
                and screen.contains("command: printf PHASE12_APPROVED"),
                master,
                screen,
                "shell approval request",
            )
            os.write(master, b"1\r")
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

            os.write(master, b"inspect patch denial gate\r")
            wait_for(
                lambda: screen.contains("propose_patch requires approval")
                and screen.contains("path: README.md"),
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

            os.write(master, b"inspect patch approval gate\r")
            wait_for(
                lambda: screen.contains("propose_patch requires approval")
                and screen.contains("path: README.md"),
                master,
                screen,
                "patch approval request",
            )
            os.write(master, b"1\r")
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
            end = time.monotonic() + 4.0
            while proc.poll() is None and time.monotonic() < end:
                read_available(master, screen, 0.05)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} phase12 dock approval smoke: ok")


if __name__ == "__main__":
    main()
