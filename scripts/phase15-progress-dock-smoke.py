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
    repl = repo_root / "src" / "repl" / "chat.rs"
    repl_stream = repo_root / "src" / "repl" / "chat_support" / "stream.rs"
    input_composer = repo_root / "src" / "input" / "composer.rs"

    progress_calls = numbered_lines(repl, "progress_dock(&context_scan_status")
    stale_calls = numbered_lines(repl, "status_above(&context_scan_status")
    progress_def = numbered_lines(input_composer, "pub fn progress_dock")
    progress_clear = numbered_lines(input_composer, "self.progress_rows.clear();")
    paced_final = numbered_lines(repl_stream, "send_rendered_markdown_stream")
    eager_final = numbered_lines(repl_stream, "TurnEvent::Delta(render_terminal_markdown")

    if not progress_calls:
        raise AssertionError("missing progress_dock context-scan call in src/repl/chat.rs")
    if stale_calls:
        details = "\n".join(f"{line}: {text}" for line, text in stale_calls)
        raise AssertionError(f"stale status_above context-scan progress path found:\n{details}")
    if not progress_def:
        raise AssertionError("missing DockedComposer::progress_dock in src/input/composer.rs")
    if not progress_clear:
        raise AssertionError("missing progress_rows clear path in src/input/composer.rs")
    if not paced_final:
        raise AssertionError("missing paced final markdown stream path in src/repl/chat_support/stream.rs")
    if eager_final:
        details = "\n".join(f"{line}: {text}" for line, text in eager_final)
        raise AssertionError(f"eager rendered markdown delta path found:\n{details}")

    print("source_contract=PASS")
    for line, text in progress_calls:
        print(f"progress_render=repl/chat.rs:{line}: {text}")
    for line, text in progress_def:
        print(f"progress_api=input/composer.rs:{line}: {text}")
    for line, text in progress_clear:
        print(f"progress_clear=input/composer.rs:{line}: {text}")
    for line, text in paced_final:
        print(f"paced_final_stream=repl/chat_support/stream.rs:{line}: {text}")


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
                decision = {
                    "final_answer": "## Listing Result\n\nfiles listed successfully\n\n- first streamed line\n\n- second streamed line\n\n- final stream marker"
                }
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


def write_slow_second_call_fake_curl(directory):
    path = Path(directory) / "curl"
    path.write_text(
        textwrap.dedent(
            r"""
            #!/usr/bin/env python3
            import json
            import sys
            import time

            config = sys.stdin.read()

            if "Tool result for step 1" in config:
                time.sleep(4.0)
                decision = {"final_answer": "files listed after slow continuation"}
            else:
                time.sleep(1.0)
                decision = {
                    "thought": "listing files as requested",
                    "tool": {"name": "list_files", "arguments": {"path": "."}},
                }

            print(json.dumps({
                "choices": [{"message": {"content": json.dumps(decision)}}]
            }))
            """
        ).lstrip(),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def write_long_answer_fake_curl(directory):
    path = Path(directory) / "curl"
    lines = ["## Long Stream Budget"]
    lines.extend(f"- budget line {index}" for index in range(220))
    lines.append("final budget marker")
    response = "\n".join(lines)
    path.write_text(
        textwrap.dedent(
            f"""
            #!/usr/bin/env python3
            import json

            print(json.dumps({{
                "choices": [{{"message": {{"content": {json.dumps(response)}}}}}]
            }}))
            """
        ).lstrip(),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def visible_row_with(screen, text):
    for row, line in enumerate(screen.lines()):
        if text in line:
            return row
    return None


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
        env["DEEPSEEK_RENDERED_STREAM_DELAY_MS"] = "60"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        saw_loading_in_dock = False
        saw_tool_step_in_dock = False
        saw_partial_final_stream = False
        active_loading_row = None
        active_tool_step_row = None
        active_prompt_row = None
        active_help_row = None

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
                    active_loading_row = visible_row_with(screen, "Loading ")
                if "agent step 1: list_files" in dock:
                    saw_tool_step_in_dock = True
                    active_tool_step_row = visible_row_with(screen, "agent step 1: list_files")
                if saw_loading_in_dock or saw_tool_step_in_dock:
                    active_prompt_row = visible_row_with(screen, f"{name} [")
                    active_help_row = visible_row_with(screen, "Enter send")
                if saw_loading_in_dock and saw_tool_step_in_dock:
                    break
                if "files listed successfully" in screen.all_text():
                    break

            stream_deadline = time.monotonic() + 8.0
            while time.monotonic() < stream_deadline:
                read_available(master, screen, 0.02)
                all_text = screen.all_text()
                if "Listing Result" in all_text and "final stream marker" not in all_text:
                    saw_partial_final_stream = True
                    break
                if "final stream marker" in all_text:
                    break

            wait_for(
                lambda: "final stream marker" in screen.all_text(),
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
            print(f"saw_partial_final_stream={saw_partial_final_stream}")
            print(f"active_loading_row={active_loading_row}")
            print(f"active_tool_step_row={active_tool_step_row}")
            print(f"active_prompt_row={active_prompt_row}")
            print(f"active_help_row={active_help_row}")
            print(f"final_has_loading={final_has_loading}")
            print(f"final_has_tool_step={final_has_tool_step}")

            failures = []
            if not saw_loading_in_dock:
                failures.append("Loading never appeared in dock_text() during active turn")
            if not saw_tool_step_in_dock:
                failures.append("agent step 1: list_files never appeared in dock_text()")
            if not saw_partial_final_stream:
                failures.append("final markdown payload did not appear in paced chunks")
            if active_loading_row is not None and active_prompt_row is not None and (
                active_loading_row >= active_prompt_row
            ):
                failures.append("Loading row did not render above the prompt row")
            if active_tool_step_row is not None and active_prompt_row is not None and (
                active_tool_step_row >= active_prompt_row
            ):
                failures.append("agent step row did not render above the prompt row")
            if active_help_row is not None and active_prompt_row is not None and (
                active_help_row <= active_prompt_row
            ):
                failures.append("help row did not render below the prompt row")
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


def run_long_stream_budget_smoke(binary, name):
    with tempfile.TemporaryDirectory(prefix=f"{name}-long-stream-budget-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        fake_bin = tmp_path / "bin"
        home.mkdir()
        workspace.mkdir()
        fake_bin.mkdir()
        write_long_answer_fake_curl(fake_bin)

        env = os.environ.copy()
        env["HOME"] = str(home)
        env["DEEPSEEK_API_KEY"] = "long-stream-budget-smoke-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["DEEPSEEK_RENDERED_STREAM_DELAY_MS"] = "60"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        first_seen_at = None
        final_seen_at = None
        saw_partial_stream = False

        try:
            wait_for(
                lambda: prompt_visible(screen, name),
                master,
                screen,
                "initial dock prompt",
                timeout=10.0,
            )

            os.write(master, b"hello\r")
            deadline = time.monotonic() + 6.0
            while time.monotonic() < deadline:
                read_available(master, screen, 0.01)
                all_text = screen.all_text()
                now = time.monotonic()
                if first_seen_at is None and "Long Stream Budget" in all_text:
                    first_seen_at = now
                if first_seen_at is not None and "final budget marker" not in all_text:
                    saw_partial_stream = True
                if "final budget marker" in all_text:
                    final_seen_at = now
                    break

            wait_for(
                lambda: "final budget marker" in screen.all_text(),
                master,
                screen,
                "long final answer",
                timeout=6.0,
            )
            if final_seen_at is None:
                final_seen_at = time.monotonic()

            stream_elapsed = (
                final_seen_at - first_seen_at if first_seen_at is not None else None
            )
            print(f"saw_long_partial_stream={saw_partial_stream}")
            elapsed_label = f"{stream_elapsed:.3f}s" if stream_elapsed is not None else "none"
            print(f"long_stream_elapsed={elapsed_label}")

            if first_seen_at is None:
                raise AssertionError("long answer never started rendering\n" + screen.dump())
            if not saw_partial_stream:
                raise AssertionError("long answer did not stream partially\n" + screen.dump())
            if stream_elapsed is None or stream_elapsed > 3.0:
                raise AssertionError(
                    "long answer fake stream exceeded capped budget\n" + screen.dump()
                )
        finally:
            if proc.poll() is None:
                try:
                    os.write(master, b"/exit\r")
                    time.sleep(0.3)
                except OSError:
                    pass
                proc.terminate()
            os.close(master)


def run_progress_timer_smoke(binary, name):
    with tempfile.TemporaryDirectory(prefix=f"{name}-progress-timer-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        workspace = tmp_path / "workspace"
        fake_bin = tmp_path / "bin"
        home.mkdir()
        workspace.mkdir()
        fake_bin.mkdir()
        (workspace / "hello.txt").write_text("hello\n", encoding="utf-8")
        write_slow_second_call_fake_curl(fake_bin)

        env = os.environ.copy()
        env["HOME"] = str(home)
        env["DEEPSEEK_API_KEY"] = "progress-timer-smoke-key"
        env["DEEPSEEK_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"

        master, proc = spawn_pty(binary, ["chat"], env, str(workspace), ROWS, COLS)
        screen = Screen(ROWS, COLS)
        loading_values = []
        saw_tool_step = False

        try:
            wait_for(
                lambda: prompt_visible(screen, name),
                master,
                screen,
                "initial dock prompt",
                timeout=10.0,
            )

            os.write(master, b"list files\r")
            deadline = time.monotonic() + 8.0
            while time.monotonic() < deadline:
                read_available(master, screen, 0.1)
                dock = screen.dock_text()
                if "agent step 1: list_files" in dock:
                    saw_tool_step = True
                if saw_tool_step:
                    for line in screen.lines():
                        if "Loading " in line:
                            value = line.strip()
                            if not loading_values or loading_values[-1] != value:
                                loading_values.append(value)
                if "files listed after slow continuation" in screen.all_text():
                    break

            wait_for(
                lambda: "files listed after slow continuation" in screen.all_text(),
                master,
                screen,
                "slow continuation final answer",
                timeout=10.0,
            )

            print(f"saw_tool_step_before_slow_wait={saw_tool_step}")
            print("loading_values_after_tool_step=" + ",".join(loading_values))
            if not saw_tool_step:
                raise AssertionError("tool step never appeared before slow continuation")
            if len(set(loading_values)) < 3:
                raise AssertionError(
                    "Loading timer did not continue ticking after tool step\n" + screen.dump()
                )
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
    run_long_stream_budget_smoke(binary, args.name)
    run_progress_timer_smoke(binary, args.name)

    print(f"{args.name} phase15 progress dock smoke: ok")


if __name__ == "__main__":
    main()
