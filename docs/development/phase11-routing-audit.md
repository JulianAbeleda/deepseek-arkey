# Phase 11 Routing Parity Audit

Date: 2026-05-03

## Scope

Audited commits:

- DeepSeek `26e914e` `[runtime] Enable model-decided routing baseline`
- DeepSeek `cffb7a7` `[test] Add docked routing smoke`
- DeepSeek `909d20a` `[runtime] Deny docked approval-gated tools`
- MiniMax `6550171` `[runtime] Enable model-decided routing baseline`

Parity marker:

- DeepSeek `phase11-parity-complete` -> `909d20a`
- MiniMax `phase11-parity-complete` -> `6550171`

## Summary

The repos are functionally equivalent for Phase 11 routing behavior. MiniMax
contains the equivalent of the three DeepSeek commits in one squashed commit.
The only meaningful difference is commit granularity, not behavior.

Do not rewrite MiniMax history just to mirror DeepSeek commit boundaries.

## Commit Assessment

| Commit | Prefix | Self-contained | Test coverage | Grade |
| --- | --- | --- | --- | --- |
| DeepSeek `26e914e` | Should likely have been `[ui]` or `[repl]`, not `[runtime]` | Intermediate bug: docked routing used `ApprovalMode::Interactive` | Unit tests only | B |
| DeepSeek `cffb7a7` | Correct: `[test]` | Self-contained smoke coverage | Happy path read-tool smoke, missing approval-gated path | B+ |
| DeepSeek `909d20a` | Should likely have been `[agent]` or `[ui]`, not `[runtime]` | Self-contained fix for `26e914e` | Adds docked `run_shell` denial smoke | A |
| MiniMax `6550171` | `[runtime]` is broad but acceptable for the squash | Self-contained squash of all DeepSeek Phase 11 behavior | Includes denial coverage | A- |

## Findings

### Prefix Inconsistency

Earlier routing commits used `[ui]`, such as `[ui] Route env folder tasks` and
`[ui] Accept short agent route confirmation`. The Phase 11 commits use
`[runtime]` even though much of the behavioral change is in `repl.rs`.

This is a minor commit-discipline issue. It does not affect runtime behavior.

### DeepSeek Intermediate Approval Bug

DeepSeek `26e914e` routed docked chat through the agent loop with
`ApprovalMode::Interactive`. In raw-mode docked chat, stdin is owned by the
bottom composer. A shell or patch approval prompt from the background worker
could hang or corrupt the display.

DeepSeek `909d20a` fixes this by using deny mode for approval-gated tools in
docked model-decided routing.

### MiniMax Squash Avoids The Intermediate State

MiniMax `6550171` includes the baseline routing, smoke coverage, and deny-mode
approval behavior together. MiniMax therefore never has the intermediate buggy
state present between DeepSeek `26e914e` and `909d20a`.

### Tool-Step Callback Is Now Justified

Earlier audit guidance suggested reducing extra agent API surface. Phase 11
docked rendering now needs a tool-step callback so the UI can render lines such
as `agent step 1: list_files` without stdout handoff.

That makes the callback mechanism justified in both repos.

## Bottom Line

No critical or high issues were found. The Phase 11 routing baseline, docked
smoke coverage, and docked denial behavior are equivalent across both repos.

Future cleanup should be behavioral or maintainability-driven, not history
rewriting. Reasonable follow-ups include:

- align `task_root` naming across repos if it reduces reader friction
- normalize agent API shape only if it removes real duplication
- keep provider-specific behavior, such as MiniMax thinking-block stripping,
  provider-local
