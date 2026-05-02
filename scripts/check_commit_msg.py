#!/usr/bin/env python3
"""Validate DeepSeek commit message prefixes."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ALLOWED_PREFIXES = {
    "cli",
    "provider",
    "ui",
    "runtime",
    "test",
    "docs",
    "control",
    "repo",
}

NORMAL_RE = re.compile(r"^\[(?P<prefix>[a-z]+)\]\s+\S.+$")
NFC_RE = re.compile(r"^\[(?P<prefix>[a-z]+)\]\s+NFC\s+-\s+\S.+$")


def first_non_comment_line(text: str) -> str:
    for line in text.splitlines():
        stripped = line.strip()
        if stripped and not stripped.startswith("#"):
            return stripped
    return ""


def validate_subject(subject: str) -> tuple[bool, str]:
    if not subject:
        return False, "commit message subject is empty"

    nfc_match = NFC_RE.match(subject)
    if nfc_match:
        prefix = nfc_match.group("prefix")
        if prefix not in ALLOWED_PREFIXES:
            return False, f"unknown commit prefix [{prefix}]"
        return True, "ok"

    if "NFC" in subject:
        return False, "NFC commits must use format: [subsystem] NFC - description"

    match = NORMAL_RE.match(subject)
    if not match:
        return False, "commit message must start with [subsystem] prefix"

    prefix = match.group("prefix")
    if prefix not in ALLOWED_PREFIXES:
        return False, f"unknown commit prefix [{prefix}]"
    return True, "ok"


def validate_message_text(text: str) -> tuple[bool, str]:
    return validate_subject(first_non_comment_line(text))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Validate DeepSeek commit message format.")
    parser.add_argument("message_file", help="Path to commit message file.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    message_path = Path(args.message_file)
    ok, detail = validate_message_text(message_path.read_text(encoding="utf-8"))
    if ok:
        return 0
    print(f"Invalid commit message: {detail}", file=sys.stderr)
    print(
        "Expected: [cli|provider|ui|runtime|test|docs|control|repo] description",
        file=sys.stderr,
    )
    print("NFC format: [subsystem] NFC - description", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
