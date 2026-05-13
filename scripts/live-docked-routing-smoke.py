#!/usr/bin/env python3
"""Live docked routing smoke.

Requires DEEPSEEK_API_KEY in the environment and network access. This is not a
CI-default test; it validates the real provider through the docked chat path.
"""

import argparse
import os
import tempfile
from pathlib import Path

from smoke_lib import Screen, prompt_visible, resolve_binary, spawn_pty, wait_for


ROWS = 24
COLS = 100
ENV_KEY = "DEEPSEEK_API_KEY"
FORCE_TTY_ENV = "DEEPSEEK_FORCE_TTY_SIZE"


def assert_not_legacy_handoff(screen):
    text = screen.all_text()
    forbidden = ["agent task:", "route: agent task", "Run this as an agent task?"]
    found = [item for item in forbidden if item in text]
    if found:
        raise AssertionError(f"legacy handoff rendered: {found}\n{screen.dump()}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek-arkey")
    parser.add_argument("--expect", default="DEEPSEEK_DOCKED_LIVE_OK")
    parser.add_argument("--timeout", type=float, default=45.0)
    args = parser.parse_args()

    if not os.environ.get(ENV_KEY):
        raise SystemExit(f"{ENV_KEY} is not set")

    binary = resolve_binary(args.name, args.binary, profile="release")

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-live-docked-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        home.mkdir()
        workspace.mkdir()
        env = os.environ.copy()
        env["HOME"] = str(home)
        env[FORCE_TTY_ENV] = f"{COLS}x{ROWS}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        try:
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "initial dock")
            prompt = (
                "Do not use tools. Return exactly one JSON object and no prose: "
                f'{{"final_answer":"{args.expect}"}}\r'
            )
            os.write(master, prompt.encode())
            wait_for(lambda: screen.contains("context: scanning"), master, screen, "scan row")
            wait_for(
                lambda: screen.contains(args.expect),
                master,
                screen,
                "live provider response",
                timeout=args.timeout,
            )
            wait_for(lambda: prompt_visible(screen, args.name), master, screen, "dock restored")
            assert_not_legacy_handoff(screen)
            proc.terminate()
            proc.wait(timeout=3)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} live docked routing smoke: ok")


if __name__ == "__main__":
    main()
