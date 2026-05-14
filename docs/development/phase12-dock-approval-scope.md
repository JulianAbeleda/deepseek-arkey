# Phase 12 - Dock-Native Approval Scope

## Goal

Allow docked model-decided routing to request approval-gated tools without
stealing stdin from the bottom composer or corrupting the terminal display.

## First Slice

- [x] Implement in DeepSeek first.
- [x] Keep MiniMax unchanged until DeepSeek passes focused tests and PTY smoke.
- [x] Preserve explicit agent mode `yes run` and `yes apply` prompts.
- [x] Add dock-native approval requests for `run_shell` and `propose_patch`.
- [x] Accept approval or denial through the docked composer.
- [x] Keep shell/edit tools denied unless the user types the exact approval phrase.
- [x] Port the validated behavior to MiniMax.
- [x] Remove the stale configurable `deny_phrase` field and keep denial handling
  explicit in the REPL.
- [x] Cover patch denial and approval in the Phase 12 PTY smoke.

## Expected Behavior

When the model requests `run_shell` in docked chat:

```text
agent step 1: run_shell
approval required: run_shell
Type yes run to approve, n to deny.
```

- `n` denies the tool and returns the denial as the tool result.
- `Approve once` approves the shell command once.
- `Approve shell for this root` approves future shell requests for the same
  canonical workspace root during the current docked chat process.
- The dock remains mounted.
- No raw stdin prompt appears outside the dock.

When the model requests `propose_patch` in docked chat:

```text
approval required: propose_patch
Type yes apply to approve, n to deny.
```

- `n` denies the edit.
- `Approve once` approves the exact prepared edit once.
- `Approve writes for this root` approves future write requests for the same
  canonical workspace root during the current docked chat process.
- Write and shell approvals are separate scopes, and approval for one root does
  not carry to another root.

## Out Of Scope

- Pretty approval dialogs.
- Approve-for-session policies.
- Reasoning display.
- Broadening tool capabilities.

## Current Status

Phase 12 first slice is complete and pushed in both repos.

DeepSeek latest commits:

- `afa796a [test] Cover patch dock approvals`
- `59f83ed [cli] Align dock approval denial handling`
- `3812582 [docs] Document Phase 12 approval flow`

MiniMax parity commits:

- `ea09fd0 [test] Cover patch dock approvals`
- `2401abe [cli] Align dock approval denial handling`
- `c95e7d3 [docs] Document Phase 12 approval flow`

Validation last run:

```bash
cargo fmt --check
cargo test --offline
cargo build --offline
python3 scripts/phase11-docked-routing-smoke.py --binary target/debug/deepseek
python3 scripts/phase12-dock-approval-smoke.py --binary target/debug/deepseek
```

## Next Session

- Audit the Phase 12 completion commits as a matched DeepSeek/MiniMax pair.
- Decide whether to extract approval prompt formatting out of `agent.rs` before
  adding more approval types.
- If the audit passes, tag or document Phase 12 as complete.
