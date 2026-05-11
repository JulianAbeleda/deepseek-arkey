#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

LIVE=0
FULL=0
BINARY="target/release/deepseek"

usage() {
  cat <<'USAGE'
Usage: scripts/kimi-deepseek-smoke.sh [--live] [--full] [--binary PATH]

Runs a Kimi-oriented DeepSeek smoke test.

Default checks are local/offline:
  - cargo fmt --check
  - cargo test --offline
  - cargo build --offline --release
  - docked TTY smoke against target/release/deepseek
  - persistent navigation smoke

Options:
  --live         Also run live provider docked routing smoke. Requires DEEPSEEK_API_KEY and network.
  --full         Also run extra docked routing/approval smokes when the release binary is built.
  --binary PATH  Smoke a specific DeepSeek binary after the build/test phase.
USAGE
}

while (($#)); do
  case "$1" in
    --live)
      LIVE=1
      shift
      ;;
    --full)
      FULL=1
      shift
      ;;
    --binary)
      BINARY="${2:?--binary requires a path}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

section() {
  printf '\n==== %s ====\n' "$1"
}

run() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

section "Kimi Smoke Prompt"
cat <<'PROMPT'
Smoke-test DeepSeek after the dock rendering/navigation changes.

Pass criteria:
- Source formats cleanly.
- Unit suite passes.
- Release binary builds.
- Docked TTY prompt renders and accepts input.
- Slash completion still works.
- Persistent navigation still works.
- Optional live provider smoke does not show cache/status collision in docked mode.

On failure:
- Report the failing command.
- Include the exact assertion or screen dump.
- Inspect src/input.rs, src/repl.rs, src/workspace.rs, src/terminal_markdown.rs, src/provider.rs, and src/agent.rs first.
PROMPT

section "Repo Context"
run git --no-pager log --oneline -5
run git status --short

section "Format"
run cargo fmt --check

section "Unit Tests"
run cargo test --offline

section "Release Build"
run cargo build --offline --release

if [[ ! -x "$BINARY" ]]; then
  echo "binary not executable: $BINARY" >&2
  exit 1
fi

section "Docked TTY Smoke"
run python3 scripts/docked-smoke.py --binary "$BINARY" --entrypoint default
run python3 scripts/docked-smoke.py --binary "$BINARY" --entrypoint chat

section "Commit Audit Approval Smoke"
run python3 scripts/phase13-commit-audit-approval-smoke.py --binary "$BINARY"
run python3 scripts/phase14-commit-audit-preflight-smoke.py --binary "$BINARY"

section "Persistent Navigation Smoke"
run ./scripts/persistent-navigation-test.sh

if ((FULL)); then
  section "Full Local Smokes"
  run python3 scripts/agent-startup-smoke.py --binary "$BINARY"
  run python3 scripts/phase10-scope-probe.py --binary "$BINARY" --name deepseek --model deepseek-v4-flash
  run python3 scripts/phase11-docked-routing-smoke.py --binary "$BINARY"
  run python3 scripts/phase12-dock-approval-smoke.py --binary "$BINARY"
fi

if ((LIVE)); then
  section "Live Provider Smoke"
  if [[ -z "${DEEPSEEK_API_KEY:-}" ]]; then
    echo "DEEPSEEK_API_KEY is required for --live" >&2
    exit 1
  fi
  run python3 scripts/live-docked-routing-smoke.py --binary "$BINARY"
else
  section "Live Provider Smoke"
  echo "skipped; pass --live with DEEPSEEK_API_KEY set to run it"
fi

section "Result"
echo "kimi deepseek smoke: ok"
