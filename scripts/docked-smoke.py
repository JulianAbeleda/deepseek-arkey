#!/usr/bin/env python3
import argparse
import os
import subprocess
import tempfile

from smoke_lib import Screen, read_available, resolve_binary, spawn_pty, wait_for


ROWS = 24
COLS = 80


def dock_contains(screen, text):
    return text in screen.dock_text()


def dock_prompt_visible(screen, name, prompt_fragment):
    dock = screen.dock_text()
    return name in dock and prompt_fragment in dock and "›" in dock


def dock_idle_prompt(screen, name):
    return any(f"{name} [" in line and "›" in line for line in screen.dock_text().splitlines())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek-arkey")
    parser.add_argument("--model", default="deepseek-v4-flash")
    parser.add_argument("--entrypoint", choices=("default", "chat", "switch"), default="chat")
    args = parser.parse_args()

    binary = resolve_binary(args.name, args.binary, profile="release")

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-docked-smoke-") as home:
        env = os.environ.copy()
        env["HOME"] = home
        command = [binary] if args.entrypoint == "default" else [binary, "chat"]
        prompt_fragment = "debug:"
        response_fragment = "debug/manual backend"
        env[f"{args.name.upper()}_FORCE_TTY_SIZE"] = f"{COLS}x{ROWS}"
        env[f"{args.name.upper()}_DEBUG_STREAM_DELAY_MS"] = "10"
        subprocess.run([binary, "debug", "on"], env=env, check=True, stdout=subprocess.DEVNULL)

        master, proc = spawn_pty(command[0], command[1:], env, None, ROWS, COLS)
        screen = Screen()
        try:
            wait_for(lambda: dock_prompt_visible(screen, args.name, prompt_fragment), master, screen, "PromptIdle dock")
            if args.entrypoint == "switch":
                os.write(master, b"/agent\r")
                wait_for(lambda: screen.contains("workspace:") and screen.contains("agent"), master, screen, "inline agent mode")
                os.write(master, b"/chat\r")
                wait_for(lambda: dock_prompt_visible(screen, args.name, "debug:"), master, screen, "chat dock after /chat")
                proc.terminate()
                proc.wait(timeout=2)
                print(f"{args.name} switch docked smoke: ok")
                return
            os.write(master, b"/")
            wait_for(lambda: dock_contains(screen, "/chat"), master, screen, "slash command panel")
            if screen.row != ROWS - 2:
                raise AssertionError(
                    f"slash command cursor left prompt row: row={screen.row}\n{screen.dump()}"
                )
            os.write(master, b"\x03")
            wait_for(lambda: dock_idle_prompt(screen, args.name), master, screen, "clear slash panel")
            os.write(master, b"/sta\t")
            wait_for(lambda: dock_contains(screen, "/status"), master, screen, "slash command completion")
            os.write(master, b"\x03")
            wait_for(lambda: dock_idle_prompt(screen, args.name), master, screen, "clear completed slash command")
            os.write(master, b"one two")
            wait_for(lambda: dock_contains(screen, "one two"), master, screen, "editable draft in dock")
            os.write(master, b"\x1b[1;5D")
            os.write(master, b"X")
            wait_for(lambda: dock_contains(screen, "one Xtwo"), master, screen, "ctrl-left word movement")
            os.write(master, b"\x03")
            wait_for(lambda: dock_idle_prompt(screen, args.name), master, screen, "clear word movement draft")
            os.write(master, b"\x1b[200~line one\nline two\x1b[201~")
            wait_for(
                lambda: dock_contains(screen, "line one") and dock_contains(screen, "line two"),
                master,
                screen,
                "bracketed paste multiline insert",
            )
            os.write(master, b"\r")
            wait_for(
                lambda: screen.contains("Loading ") or screen.contains(response_fragment),
                master,
                screen,
                "Loading row or fast response",
            )
            wait_for(lambda: screen.contains(response_fragment), master, screen, "ResponseRender", timeout=10.0)
            wait_for(lambda: dock_prompt_visible(screen, args.name, prompt_fragment), master, screen, "PromptResume dock")
            if not screen.contains(response_fragment):
                dump = "\n".join(f"{i:02d}: {screen.line(i)}" for i in range(screen.rows))
                raise AssertionError(f"streamed response did not persist after completion\n{dump}")
            proc.terminate()
            proc.wait(timeout=2)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} {args.entrypoint} docked smoke: ok")


if __name__ == "__main__":
    main()
