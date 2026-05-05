#!/usr/bin/env bash
set -euo pipefail

DEEPSEEK_REPO="${DEEPSEEK_REPO:-/Users/julianabeleda/env/deepseek}"
MINIMAX_REPO="${MINIMAX_REPO:-/Users/julianabeleda/env/minimax}"
DOGFOOD_ROOT="${DOGFOOD_ROOT:-/Users/julianabeleda/env/pkos_v0.2}"

if ! command -v claude >/dev/null 2>&1; then
  echo "error: claude is not on PATH" >&2
  exit 1
fi

for path in "$DEEPSEEK_REPO" "$MINIMAX_REPO" "$DOGFOOD_ROOT"; do
  if [[ ! -d "$path" ]]; then
    echo "error: missing directory: $path" >&2
    exit 1
  fi
done

claude \
  --permission-mode plan \
  --add-dir "$DEEPSEEK_REPO" \
  --add-dir "$MINIMAX_REPO" \
  --add-dir "$DOGFOOD_ROOT" \
  -p "$(cat <<'PROMPT'
You are doing the first read-only dogfood readiness pass for two paired Rust CLIs:

- /Users/julianabeleda/env/deepseek
- /Users/julianabeleda/env/minimax

Dogfood target:
- /Users/julianabeleda/env/pkos_v0.2

Do not edit files. Do not commit. Do not push. This is an audit pass only.

Context:
- These CLIs are considered ready for early read-only dogfooding if repo-analysis and navigation tasks do not crash on malformed model decisions.
- Recent reliability work added one corrective retry for invalid decision JSON and one corrective retry for non-actionable JSON.
- Transcript summaries are available through:
  - /Users/julianabeleda/.local/bin/deepseek agent --root /Users/julianabeleda/env/pkos_v0.2 transcript latest --summary
  - /Users/julianabeleda/.local/bin/minimax agent --root /Users/julianabeleda/env/pkos_v0.2 transcript latest --summary

First-pass goals:
1. Inspect both repos and confirm the latest dogfood/reliability commits are present.
2. Review the agent runtime retry path for correctness, especially:
   - invalid JSON retry
   - no-action retry
   - transcript write on failure
   - redaction/capping of raw snippets
   - no change to write-tool approval behavior
3. Review transcript summary behavior for usefulness after failed or weird runs.
4. Run only non-mutating checks if useful, such as:
   - git status --short --branch
   - git log --oneline -5
   - cargo test --offline
   - installed transcript summary commands above
5. Optionally run one live read-only smoke per provider only if credentials and network are already available:
   - /Users/julianabeleda/.local/bin/deepseek agent --root /Users/julianabeleda/env/pkos_v0.2 "analyze this repo"
   - /Users/julianabeleda/.local/bin/minimax agent --root /Users/julianabeleda/env/pkos_v0.2 "analyze this repo"

Report format:
- Verdict: READY / READY WITH CAVEATS / NOT READY
- Blocking findings, if any, with file references
- Non-blocking findings
- Dogfood scope that is safe now
- Dogfood scope to avoid for now
- Exact commands you ran and their pass/fail result
- Latest transcript summary observations for DeepSeek and MiniMax

Be strict about reliability. Do not suggest broad refactors unless they block dogfooding.
PROMPT
)"
