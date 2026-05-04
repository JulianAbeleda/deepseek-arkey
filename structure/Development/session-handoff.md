# Session Handoff

## Current State

Phase 11 model-decided docked routing, Phase 12 dock-native approvals, and the
post-Phase-12 persistent workspace navigation slice are implemented and
validated in DeepSeek and MiniMax.

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

## Next Session Checklist

- [ ] Audit the Phase 12 completion commits in both repos.
- [ ] Decide whether to extract approval prompt formatting from `agent.rs` into
      a UI-facing layer before adding more approval types.
- [ ] If the audit passes, tag or document Phase 12 as complete.
- [ ] Push the persistent workspace navigation commits if they are not already
      on `origin/main`.
- [ ] Keep DeepSeek and MiniMax parity unless a provider-specific behavior
      requires divergence.
