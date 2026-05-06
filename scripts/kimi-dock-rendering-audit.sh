#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET_COMMIT="${1:-HEAD}"
PARENT_COMMIT="${2:-${TARGET_COMMIT}^}"

section() {
  printf '\n==== %s ====\n' "$1"
}

section "Kimi Audit Prompt"
cat <<'PROMPT'
Audit the DeepSeek dock rendering/navigation change.

Review stance:
- Prioritize correctness bugs, terminal UI regressions, missed edge cases, and missing tests.
- Lead with findings ordered by severity.
- Include file/line references.
- Treat ignored structure/cache files as local documentation only, not commit source.

Primary questions:
1. Does relative navigation work correctly for cd .., cd ., cd ../path, /root ../path, and cd -?
2. Did the dock layout math remain correct after reserving 7 rows and adding blank padding above/below?
3. Does deterministic post-processing render regular chat and agent final answers without relying on prompt wording?
4. Does table rendering handle ragged rows, wide cells, ANSI width, inline code/bold, and malformed non-table pipe text?
5. Do quiet-cache provider paths prevent cache stats from colliding with the raw-mode dock/status area?
6. Are the tests sufficient for the risk introduced here?
PROMPT

section "Commit Context"
git --no-pager log --oneline -5
printf '\nTarget: %s\nParent: %s\n' "$TARGET_COMMIT" "$PARENT_COMMIT"

section "Tracked Diff Stat"
git --no-pager diff --stat "$PARENT_COMMIT" "$TARGET_COMMIT"

section "Tracked Diff Name Status"
git --no-pager diff --name-status "$PARENT_COMMIT" "$TARGET_COMMIT"

section "Ignored Cache State"
git status --short --ignored=matching structure/cache || true
git check-ignore -v structure/cache/repo-cache.md structure/cache/repo-map.md || true

section "Important Source Anchors"
printf '\n-- src/workspace.rs navigation/root helpers --\n'
grep -nE 'update_selected_root_from|clean_navigation_target|trim_navigation_punctuation|looks_like_path_target|relative_navigation_supports' src/workspace.rs || true

printf '\n-- src/repl.rs docked navigation/rendering/quiet cache --\n'
grep -nE 'is_cd_previous_request|parse_navigation_request_from|update_selected_root_from|run_agent_quiet_cache_with_approval_handler|chat_with_delta_quiet_cache|render_terminal_markdown' src/repl.rs || true

printf '\n-- src/input.rs dock/prompt spacing --\n'
grep -nE 'DOCK_RESERVED_ROWS|DOCK_VERTICAL_PADDING_ROWS|print_above|render_dock_lines|dock_reserves_vertical_padding_rows' src/input.rs || true

printf '\n-- src/terminal_markdown.rs table rendering --\n'
grep -nE 'render_table_block|MAX_TABLE_CELL_WIDTH|is_table_row|format_table_row|format_table_rule|wrap_table_cell|display_width|renders_aligned_markdown_tables' src/terminal_markdown.rs || true

printf '\n-- src/provider.rs and src/agent.rs quiet cache helpers --\n'
grep -nE 'chat_with_delta_quiet_cache|run_agent_quiet_cache_with_approval_handler' src/provider.rs src/agent.rs || true

section "Validation Commands"
cat <<'COMMANDS'
Required:
  cargo fmt --check
  cargo test --offline

Optional local smoke after building release binary:
  cargo build --offline --release
  python3 scripts/docked-smoke.py --binary target/release/deepseek
  python3 scripts/docked-smoke.py --binary target/release/deepseek --entrypoint default
  ./scripts/persistent-navigation-test.sh

Optional live provider smoke requires DEEPSEEK_API_KEY and network:
  python3 scripts/live-docked-routing-smoke.py --binary target/release/deepseek
COMMANDS

section "Full Diff"
git --no-pager diff --find-renames "$PARENT_COMMIT" "$TARGET_COMMIT" -- \
  src/agent.rs \
  src/input.rs \
  src/provider.rs \
  src/repl.rs \
  src/terminal_markdown.rs \
  src/workspace.rs
