#!/usr/bin/env python3
import argparse
import fcntl
import os
import pty
import shutil
import struct
import subprocess
import tempfile
import termios
import time


ROWS = 24
COLS = 100


def read_until(fd, expected, timeout=3.0):
    end = time.monotonic() + timeout
    output = b""
    while time.monotonic() < end:
        try:
            chunk = os.read(fd, 4096)
        except BlockingIOError:
            chunk = b""
        except OSError:
            break
        if chunk:
            output += chunk
            text = output.decode("utf-8", "ignore")
            if expected in text:
                return text
        time.sleep(0.02)
    text = output.decode("utf-8", "ignore")
    raise AssertionError(f"timed out waiting for {expected!r}\n{text}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--name", default="deepseek-arkey")
    parser.add_argument("--model", default="deepseek-v4-flash")
    args = parser.parse_args()

    binary = args.binary or str((os.getcwd() + f"/target/release/{args.name}"))
    if not os.path.exists(binary):
        binary = shutil.which(args.name) or binary
    if not os.path.exists(binary):
        raise SystemExit(f"binary not found: {binary}")

    with tempfile.TemporaryDirectory(prefix=f"{args.name}-agent-startup-smoke-") as home:
        env = os.environ.copy()
        env["HOME"] = home
        master, slave = pty.openpty()
        os.set_blocking(master, False)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        proc = subprocess.Popen([binary, "--agent"], stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True)
        os.close(slave)
        try:
            text = read_until(master, f"{args.name} [{args.model}] agent")
            assert "workspace:" in text
            assert "read tools on" in text
            assert "yes apply" in text
            assert "yes run" in text
            os.write(master, b"/status\r")
            status = read_until(master, "mode: agent")
            assert "root:" in status
            os.write(master, b"/exit\r")
            proc.wait(timeout=2)
        finally:
            if proc.poll() is None:
                proc.terminate()
            os.close(master)

    print(f"{args.name} agent startup smoke: ok")


if __name__ == "__main__":
    main()
