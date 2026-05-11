#!/usr/bin/env python3
"""Commit-audit preflight smoke for deterministic local git evidence.

This proves natural commit-audit prompts do not rely on the model choosing a
shell tool. The CLI should collect git evidence before the first provider call,
then the provider can audit from that evidence without asking for a pasted diff.
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
            import codecs
            import json
            import re
            import sys

            config = sys.stdin.read()
            match = re.search(r'^data = "(.*)"$', config, re.MULTILINE)
            if not match:
                raise SystemExit("missing curl data config")
            request = json.loads(codecs.decode(match.group(1), "unicode_escape"))
            user_content = "\n".join(
                message.get("content", "")
                for message in request.get("messages", [])
                if message.get("role") == "user"
            )

            if (
                "Original user request:" not in user_content
                or "Resolved commit target:" not in user_content
                or "Local git evidence collected before model review:" not in user_content
                or "git show --stat --patch --find-renames" not in user_content
                or "Change README" not in user_content
            ):
                decision = {
                    "blocked": "missing deterministic commit-audit preflight evidence"
                }
            elif "run_shell" in user_content:
                decision = {"blocked": "preflight should not ask the model to run shell"}
            else:
                decision = {
                    "final_answer": (
                        "## Findings\n\n"
                        "- No blocking findings in the preloaded commit evidence.\n\n"
                        "## Evidence\n\n"
                        "- Reviewed deterministic `git show` evidence for Change README.\n"
                    )
                }

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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None, help="DeepSeek binary to test.")
    parser.add_argument("--name", default="deepseek")
    args = parser.parse_args()

    binary = resolve_binary(args.name, args.binary)

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-commit-audit-preflight-") as tmp:
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
        env["DEEPSEEK_API_KEY"] = "commit-audit-preflight-smoke-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        try:
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "initial dock")

            os.write(master, b"audit commit HEAD\r")
            wait_for(
                lambda: screen.contains("No blocking findings in the preloaded commit evidence"),
                master,
                screen,
                "preflight audit final answer",
            )
            if screen.contains("run_shell requires approval"):
                raise AssertionError(f"commit audit preflight requested shell approval\n{screen.dump()}")
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "dock restored")

            os.write(master, b"/exit\r")
            end = time.monotonic() + 4.0
            while proc.poll() is None and time.monotonic() < end:
                read_available(master, screen, 0.05)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} phase14 commit audit preflight smoke: ok")


if __name__ == "__main__":
    main()
