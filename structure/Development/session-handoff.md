# Session Handoff

## Stop Point: 2026-05-06 Formatter/Dogfood Prep

DeepSeek and MiniMax are clean, pushed, and ready for real interactive dogfood.

DeepSeek current formatter commits:

- `324c9bb [ui] Extract terminal markdown renderer`
- `aee5aae [ui] Split flattened agent table rows`
- `a3e13b3 [ui] Cover flattened table line splitting`

MiniMax matching formatter commits:

- `2dd72a1 [ui] Extract terminal markdown renderer`
- `3274dcb [ui] Split flattened agent table rows`
- `11ab1f3 [ui] Cover flattened table line splitting`

Latest validation:

- DeepSeek: `cargo fmt --check`, `cargo test --offline` -> `150 passed`
- MiniMax: `cargo fmt --check`, `cargo test --offline` -> `159 passed`
- Kimi accepted the renderer extraction and flattened-table follow-ups with no
  confirmed defects.

Current behavior:

- Agent final answers use centralized complete-text terminal Markdown rendering.
- Agent normalization handles flattened heading bodies and flattened table rows,
  including prose attached after the final row.
- Regular chat remains raw-streamed and intentionally does not use the Markdown
  renderer.

Next move:

- Dogfood installed `deepseek` and `minimax` in the interactive TUI.
- Use repo-analysis prompts that produce headings, bullets, tables, inline code,
  URLs, and follow-up questions.
- If formatting breaks, capture exact prompt, visible output, expected rendering,
  and whether the transcript Markdown was valid or model-flattened.

## Current State

Phase 11 model-decided docked routing, Phase 12 dock-native approvals, and the
post-Phase-12 persistent workspace navigation slice are implemented, audited,
validated, and pushed in DeepSeek and MiniMax.

DeepSeek latest navigation/runtime commit:

- `59eab5a [cli] Persist natural workspace navigation`
- `83db9d4 [docs] Save Phase 12 session handoff`
- `afa796a [test] Cover patch dock approvals`
- `59f83ed [cli] Align dock approval denial handling`

MiniMax matching navigation/runtime commit:

- `0587657 [cli] Persist natural workspace navigation`
- `9f412b0 [docs] Save Phase 12 session handoff`
- `ea09fd0 [test] Cover patch dock approvals`
- `2401abe [cli] Align dock approval denial handling`

## What Changed

- Docked model-decided routing can request approval for `run_shell` and
  `propose_patch`.
- Approval and denial are handled through the bottom composer.
- Exact approval phrases are still required:
  - `yes run` for `run_shell`
  - `yes apply` for `propose_patch`
- Denial is explicit in the REPL with `n`, `no`, or `deny`.
- `ApprovalRequest` no longer exposes a stale configurable `deny_phrase`.
- Phase 12 PTY smoke covers shell denial, shell approval, patch denial, and
  patch approval.
- Natural navigation now persists a session-level selected workspace root:
  `go to`, `navigate into`, `cd into`, `enter`, `open`, and related path/alias
  forms can update `selected_root`.
- Root navigation does not approve shell/edit tools; `agent_root` remains a
  separate approval scope.
- Bad explicit navigation paths print inline `root error:` messages instead of
  exiting the TUI.

## Last Validation

```bash
cargo fmt --check
cargo test --offline
cargo build --offline
python3 scripts/phase11-docked-routing-smoke.py --binary target/debug/deepseek
python3 scripts/phase12-dock-approval-smoke.py --binary target/debug/deepseek
./scripts/persistent-navigation-test.sh
```

MiniMax passed the equivalent commands with `target/debug/minimax`.

External Kimi audit was attempted on 2026-05-04 but failed with a connection
error. External Claude print-mode audit was attempted but produced no output and
was stopped. Local deterministic validation passed in both repos.

Claude audit later passed Phase 12 and persistent workspace navigation in both
repos. Decision: defer extracting approval prompt formatting from `agent.rs`
until a third approval type makes the duplication meaningful.

## Next Session Checklist

- [x] Audit the Phase 12 completion commits in both repos.
- [x] Decide whether to extract approval prompt formatting from `agent.rs` into
      a UI-facing layer before adding more approval types.
- [x] If the audit passes, tag or document Phase 12 as complete.
- [x] Push the persistent workspace navigation commits if they are not already
      on `origin/main`.
- [x] Port MiniMax patch failure-mode tests equivalent to DeepSeek
      `apply_patch_rejects_changed_file` and `rejects_ambiguous_replacement`.
- [ ] Keep DeepSeek and MiniMax parity unless a provider-specific behavior
      requires divergence.
