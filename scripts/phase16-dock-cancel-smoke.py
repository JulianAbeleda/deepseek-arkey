#!/usr/bin/env python3
"""Phase 16 dock cancellation smoke."""

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


def numbered_lines(path, needle):
    lines = []
    for index, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if needle in line:
            lines.append((index, line.strip()))
    return lines


def assert_source_contract(repo_root):
    input_terminal = repo_root / "src" / "input" / "support" / "terminal.rs"
    input_composer = repo_root / "src" / "input" / "composer.rs"
    repl = repo_root / "src" / "repl" / "chat.rs"
    provider = repo_root / "src" / "provider.rs"

    required = [
        (input_terminal, "PushKeyboardEnhancementFlags"),
        (input_terminal, "PopKeyboardEnhancementFlags"),
        (input_composer, "KeyCode::Esc => Ok(Some(InputAction::Cancel))"),
        (repl, "turn.cancel.cancel();"),
        (provider, "child.kill()"),
    ]
    missing = []
    for path, needle in required:
        if not numbered_lines(path, needle):
            missing.append(f"{path.name}: {needle}")
    if missing:
        raise AssertionError("missing cancellation source contract:\n" + "\n".join(missing))


def write_slow_fake_curl(directory, marker):
    path = Path(directory) / "curl"
    path.write_text(
        textwrap.dedent(
            f"""
            #!/usr/bin/env python3
            import json
            import pathlib
            import time

            time.sleep(4.0)
            pathlib.Path({json.dumps(str(marker))}).write_text("survived\\n", encoding="utf-8")
            print(json.dumps({{
                "choices": [{{"message": {{"content": "final marker after cancel"}}}}]
            }}))
            """
        ).lstrip(),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def run_cancel_smoke(binary, name):
    with tempfile.TemporaryDirectory(prefix=f"{name}-dock-cancel-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        fake_bin = tmp_path / "bin"
        marker = tmp_path / "provider-survived"
        home.mkdir()
        workspace.mkdir()
        fake_bin.mkdir()
        write_slow_fake_curl(fake_bin, marker)

        env = os.environ.copy()
        env["HOME"] = str(home)
        env["DEEPSEEK_API_KEY"] = "dock-cancel-smoke-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        try:
            wait_for(
                lambda: prompt_visible(screen, name),
                master,
                screen,
                "initial dock prompt",
                timeout=10.0,
            )

            os.write(master, b"hello\r")
            wait_for(
                lambda: "Loading " in screen.dock_text(),
                master,
                screen,
                "loading before cancellation",
                timeout=2.0,
            )
            os.write(master, b"\x1b")
            wait_for(
                lambda: "cancelled current response" in screen.all_text(),
                master,
                screen,
                "cancelled transcript status",
                timeout=3.0,
            )
            wait_for(
                lambda: prompt_visible(screen, name),
                master,
                screen,
                "dock prompt after cancellation",
                timeout=3.0,
            )
            time.sleep(4.5)
            read_available(master, screen, 0.2)

            all_text = screen.all_text()
            failures = []
            if marker.exists():
                failures.append("fake provider survived cancellation")
            if "final marker after cancel" in all_text:
                failures.append("cancelled provider answer rendered")
            if "Loading " in all_text:
                failures.append("Loading persisted after cancellation")
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
    parser.add_argument("--name", default="deepseek-arkey")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[1]
    binary = resolve_binary(args.name, args.binary)
    assert_source_contract(repo_root)
    run_cancel_smoke(binary, args.name)
    print(f"{args.name} phase16 dock cancel smoke: ok")


if __name__ == "__main__":
    main()
