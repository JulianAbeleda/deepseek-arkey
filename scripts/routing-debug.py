#!/usr/bin/env python3
"""Debug chat-to-agent routing through the real interactive CLI.

This is intended for external auditors: run it against a built or installed
binary and paste the PASS/FAIL output into the review.
"""
import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from smoke_lib import Screen, read_available, spawn_pty, wait_for as wait_for_common


ROWS = 28
COLS = 100


def wait_for(fd, screen, predicate, label, timeout=5.0):
    wait_for_common(predicate, fd, screen, label, timeout=timeout)


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
    master, proc = spawn_pty(binary, ["chat"], env, cwd, ROWS, COLS)
    return master, proc, Screen(ROWS, COLS)


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
        route_markers = [
            "debug/manual backend",
            "debug/manual agent backend",
            "route: agent task",
            "route: unclear",
        ]
        if not setup_commands:
            route_markers.append("root-source: explicit")
        wait_for(
            master,
            screen,
            lambda: any(marker in screen.text() for marker in route_markers),
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
    if "debug/manual agent backend" in text:
        return "agent"
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


def check_direct_agent_stays_in_chat_memory(binary, env, name, home, workspace):
    commands = [
        (
            "/runtime legacy-routing on",
            lambda text: "Routing: legacy-deterministic" in text,
            "legacy routing enabled",
        ),
        (
            f"/root {workspace}",
            lambda text: "root-source: explicit" in text and path_visible(text, workspace),
            "root set",
        ),
        (
            "/agent analyze this repo structure",
            lambda text: "debug/manual agent backend" in text and path_visible(text, workspace),
            "direct agent runs in dock",
        ),
        (
            "/status",
            lambda text: "chat-turns: 1" in text and "mode: chat" in text,
            "agent result enters chat memory",
        ),
        (
            "does that make sense",
            lambda text: "debug/manual backend" in text,
            "follow-up stays chat",
        ),
    ]
    text = run_sequence(binary, env, name, home, commands)
    ok = all(predicate(text) for _, predicate, _ in commands) and "returning to chat" not in text
    status = "PASS" if ok else "FAIL"
    print(f"{status:4} direct /agent result stays in docked chat memory")
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
            (home, "analyze this repo structure", "agent", workspace, [f"/root {workspace}"]),
            (home, "tell me the main modules", "agent", workspace, [f"/root {workspace}"]),
            (home, "list files", "agent", workspace, [f"/root {workspace}"]),
            (home, "scan src", "agent", workspace, [f"/root {workspace}"]),
            (home, "read Cargo.toml", "agent", workspace, [f"/root {workspace}"]),
            (home, "what is this repo trying to do", "agent", workspace, [f"/root {workspace}"]),
            (home, "does that make sense", "chat", None, [f"/root {workspace}"]),
            (home, "does that align with Kimi", "chat", None, [f"/root {workspace}"]),
            (home, "switch to main branch", "chat", None, [f"/root {workspace}"]),
            (home, "stay in touch", "chat", None, [f"/root {workspace}"]),
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
        if check_direct_agent_stays_in_chat_memory(binary, env, args.name, home, workspace):
            passed += 1
        else:
            failed += 1

    print(f"\nsummary: {passed} passed, {failed} failed")
    raise SystemExit(1 if failed else 0)


if __name__ == "__main__":
    main()
