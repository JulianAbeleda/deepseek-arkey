#!/usr/bin/env bash
set -euo pipefail

cat <<'PURPOSE'
Purpose: debug Kimi-style persistent workspace navigation.

This verifies that natural-language navigation changes the selected workspace
root and keeps it for later turns without approving shell/edit tools.

Expected behavior:
- "go to my env folder" sets the selected root to ~/env.
- /status continues to show that root.
- A follow-up like "fix this repo" routes against the selected root.
- Casual chat like "switch to main branch" and "stay in touch" does not become
  filesystem navigation.
- Bad path navigation reports an inline root error instead of exiting the TUI.

Primary files to inspect on failure:
- src/workspace.rs: parse_navigation_request and navigation_target
- src/repl.rs: inline handling of parse_navigation_request results
- scripts/routing-debug.py: PTY expectations for root persistence
PURPOSE

repo_name="$(basename "$(pwd)")"
case "$repo_name" in
  deepseek)
    model="deepseek-v4-flash"
    ;;
  minimax)
    model="MiniMax-M2.7"
    ;;
  *)
    echo "Run this from the deepseek or minimax repo root." >&2
    exit 2
    ;;
esac

echo "== $repo_name: format check =="
cargo fmt --check

echo "== $repo_name: build debug binary =="
cargo build --offline

echo "== $repo_name: full unit suite =="
cargo test --offline

echo "== $repo_name: focused unit test =="
cargo test --offline workspace::tests::parses_navigation_requests_as_persistent_roots

echo "== $repo_name: persistent navigation PTY smoke =="
python3 scripts/routing-debug.py \
  --binary "target/debug/$repo_name" \
  --name "$repo_name" \
  --model "$model"

echo "== $repo_name: persistent navigation debug complete =="
