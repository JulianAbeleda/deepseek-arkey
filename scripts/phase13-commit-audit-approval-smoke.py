#!/usr/bin/env python3
"""Commit-audit approval smoke for docked agent shell access.

This focused local smoke proves the feature path used for commit audits:

- a repo-audit prompt routes through docked agent mode
- the agent requests an approval-gated shell command
- the dock approval modal appears
- approving the modal lets the shell command run
- the agent returns an audit-style final answer

It uses a fake `curl` provider and a temporary git repo, so it does not require
network access or a DeepSeek API key. The audit response below is a stubbed
contract check, not a real model-quality assertion; update it if the audit
answer contract changes.
"""

import argparse
import json
import os
import tempfile
import textwrap
import time
from pathlib import Path

from smoke_lib import (
    Screen,
    prompt_visible,
    read_available,
    resolve_binary,
    run_checked,
    spawn_pty,
    wait_for,
    write_executable,
)


ROWS = 24
COLS = 100


def write_fake_curl(directory):
    path = Path(directory) / "curl"
    write_executable(
        path,
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
                decision = {"final_answer": "commit audit approval smoke ready"}

            print(json.dumps({
                "choices": [{"message": {"content": json.dumps(decision)}}]
            }))
            """
        ).lstrip(),
    )


def make_repo(path):
    path.mkdir()
    run_checked(["git", "init"], path)
    run_checked(["git", "config", "user.email", "audit-smoke@example.test"], path)
    run_checked(["git", "config", "user.name", "Audit Smoke"], path)
    (path / "README.md").write_text("# Audit Smoke\n\nInitial content.\n", encoding="utf-8")
    run_checked(["git", "add", "README.md"], path)
    run_checked(["git", "commit", "-m", "Initial commit"], path)
    (path / "README.md").write_text("# Audit Smoke\n\nChanged content.\n", encoding="utf-8")
    run_checked(["git", "add", "README.md"], path)
    run_checked(["git", "commit", "-m", "Change README"], path)


def assert_not_legacy_handoff(screen):
    # These are the deprecated Phase 10 route-confirmation handoff strings.
    # Model-decided docked routing should keep the task inside the dock instead.
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

    binary = resolve_binary(args.name, args.binary)

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-commit-audit-") as tmp:
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
        env["DEEPSEEK_API_KEY"] = "commit-audit-smoke-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
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

    print(f"{args.name} phase13 commit audit approval smoke: ok")


if __name__ == "__main__":
    main()
