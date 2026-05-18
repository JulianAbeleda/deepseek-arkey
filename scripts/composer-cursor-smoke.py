#!/usr/bin/env python3
import argparse
import os
import subprocess
import tempfile

from smoke_lib import Screen, display_width, resolve_binary, spawn_pty, wait_for


ROWS = 24
COLS = 80


def assert_cursor_after(screen, text):
    row, line = screen.line_with(text)
    if row is None:
        raise AssertionError(f"missing {text!r}")
    expected_col = display_width(line[: line.index(text)]) + display_width(text)
    if screen.row != row or screen.col != expected_col:
        dump = "\n".join(f"{i:02d}: {screen.line(i)}" for i in range(screen.rows))
        raise AssertionError(
            f"cursor mismatch after {text!r}: expected {(row, expected_col)}, "
            f"got {(screen.row, screen.col)}\n{dump}"
        )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek-arkey")
    parser.add_argument("--cols", type=int, default=COLS)
    parser.add_argument("--rows", type=int, default=ROWS)
    args = parser.parse_args()

    binary = resolve_binary(args.name, args.binary, profile="release")

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-composer-cursor-") as home:
        env = os.environ.copy()
        env["HOME"] = home
        env.pop("NO_COLOR", None)
        env[f"{args.name.upper()}_FORCE_TTY_SIZE"] = f"{args.cols}x{args.rows}"
        subprocess.run([binary, "debug", "on"], env=env, check=True, stdout=subprocess.DEVNULL)

        master, proc = spawn_pty(binary, ["chat"], env, None, args.rows, args.cols)
        screen = Screen(args.rows, args.cols, wide_chars=True)
        try:
            wait_for(lambda: screen.contains(f"{args.name} [debug:"), master, screen, "debug prompt")
            if "\x1b[36" not in screen.raw and "\x1b[38;2" not in screen.raw:
                raise AssertionError("prompt did not emit ANSI color sequences")

            os.write(master, "ab界".encode())
            wait_for(lambda: screen.contains("ab界"), master, screen, "wide char input")
            assert_cursor_after(screen, "ab界")

            os.write(master, b"\x03")
            wait_for(lambda: not screen.contains("ab界"), master, screen, "clear wide input")

            os.write(master, b"\x1b[200~line one\nline two\x1b[201~")
            paste_marker = "[pasted context - 17 chars]"
            wait_for(lambda: screen.contains(paste_marker), master, screen, "multiline paste marker")
            assert_cursor_after(screen, paste_marker)

            os.write(master, b"\x03")
            wait_for(lambda: not screen.contains(paste_marker), master, screen, "clear multiline input")

            long_text = "wrap-" * 18
            os.write(master, long_text.encode())
            wait_for(lambda: screen.contains("wrap-wrap"), master, screen, "wrapped input")
            if not (args.rows - 6 <= screen.row < args.rows and 0 <= screen.col < args.cols):
                raise AssertionError(f"cursor left dock area after wrap: {(screen.row, screen.col)}")

            proc.terminate()
            proc.wait(timeout=2)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} composer cursor smoke: ok")


if __name__ == "__main__":
    main()
