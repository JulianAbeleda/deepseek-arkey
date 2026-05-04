# Phase 12 - Dock-Native Approval Scope

## Goal

Allow docked model-decided routing to request approval-gated tools without
stealing stdin from the bottom composer or corrupting the terminal display.

## First Slice

- Implement in DeepSeek first.
- Keep MiniMax unchanged until DeepSeek passes focused tests and PTY smoke.
- Preserve explicit agent mode `yes run` and `yes apply` prompts.
- Add dock-native approval requests for `run_shell` and `propose_patch`.
- Accept approval or denial through the docked composer.
- Keep shell/edit tools denied unless the user types the exact approval phrase.

## Expected Behavior

When the model requests `run_shell` in docked chat:

```text
agent step 1: run_shell
approval required: run_shell
Type yes run to approve, n to deny.
```

- `n` denies the tool and returns the denial as the tool result.
- `yes run` approves the shell command once.
- The dock remains mounted.
- No raw stdin prompt appears outside the dock.

When the model requests `propose_patch` in docked chat:

```text
approval required: propose_patch
Type yes apply to approve, n to deny.
```

- `n` denies the edit.
- `yes apply` approves the exact prepared edit once.

## Out Of Scope

- Pretty approval dialogs.
- Approve-for-session policies.
- MiniMax implementation before DeepSeek proof.
- Reasoning display.
- Broadening tool capabilities.
