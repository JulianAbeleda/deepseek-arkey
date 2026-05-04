# Session Handoff

## Repos

- DeepSeek: `/Users/julianabeleda/env/deepseek`
- MiniMax: `/Users/julianabeleda/env/minimax`

## Current State

Phase 11 model-decided docked routing and Phase 12 dock-native approvals are
implemented, validated, and pushed to `origin/main`.

DeepSeek current head:

- `afa796a [test] Cover patch dock approvals`
- `59f83ed [cli] Align dock approval denial handling`
- `3812582 [docs] Document Phase 12 approval flow`

MiniMax matching head:

- `ea09fd0 [test] Cover patch dock approvals`
- `2401abe [cli] Align dock approval denial handling`
- `c95e7d3 [docs] Document Phase 12 approval flow`

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

## Last Validation

DeepSeek:

```bash
cargo fmt --check
cargo test --offline
cargo build --offline
python3 scripts/phase11-docked-routing-smoke.py --binary target/debug/deepseek
python3 scripts/phase12-dock-approval-smoke.py --binary target/debug/deepseek
```

MiniMax:

```bash
cargo fmt --check
cargo test --offline
cargo build --offline
python3 scripts/phase11-docked-routing-smoke.py --binary target/debug/minimax
python3 scripts/phase12-dock-approval-smoke.py --binary target/debug/minimax
```

## Next Session Checklist

- [ ] Audit the Phase 12 completion commits in both repos.
- [ ] Decide whether to extract approval prompt formatting from `agent.rs` into
      a UI-facing layer before adding more approval types.
- [ ] If the audit passes, tag or document Phase 12 as complete.
- [ ] Keep DeepSeek and MiniMax parity unless a provider-specific behavior
      requires divergence.
