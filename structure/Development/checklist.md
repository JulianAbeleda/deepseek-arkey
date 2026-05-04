# DeepSeek Development Checklist

Working checklist for the standalone Rust DeepSeek CLI.

## Phase 0: Scope And Boundary

- [x] Define standalone CLI shape.
- [x] Confirm this is not an adapter for another runtime.
- [x] Confirm secret boundary: `DEEPSEEK_API_KEY` only, no committed keys.
- [x] Scope ephemeral session continuity.
- [x] Decide Rust from the start.

## Phase 1: Rust Scaffold

- [x] Add `Cargo.toml`.
- [x] Add `src/main.rs`.
- [x] Add `src/cli.rs` for command parsing.
- [x] Add `src/provider.rs` for DeepSeek API calls.
- [x] Add `src/session.rs` for active ephemeral session state.
- [x] Add `src/safety.rs` for redaction, caps, and atomic writes.
- [x] Add baseline tests.
- [x] Verify `cargo test`.

## Phase 2: Auth And Provider Call

- [x] Add `deepseek login`.
- [x] Validate missing `DEEPSEEK_API_KEY` produces a clear stderr error.
- [x] Validate API key with a small API request.
- [x] Implement one-off prompt mode: `deepseek -p "prompt"`.
- [x] Parse DeepSeek chat-completions response.
- [x] Print only assistant text to stdout.
- [x] Send diagnostics and API errors to stderr.
- [x] Exit non-zero on auth, request, or response-shape failure.

## Phase 3: Interactive Shell

- [x] Add default interactive mode when running `deepseek`.
- [x] Render Arkey-style prompt label: `deepseek [model] ›`.
- [x] Submit one user message per line.
- [x] Print assistant response after each turn.
- [x] Add `exit` / `quit` handling.
- [x] Add `session end` handling inside the shell.

## Phase 4: Ephemeral Session Context

- [x] Add `session start [name]`.
- [x] Add `session status`.
- [x] Add `session end`.
- [x] Store active session outside the project folder.
- [x] Store only user/assistant messages, model, and timestamps.
- [x] Ensure session files never contain `DEEPSEEK_API_KEY`.
- [x] Replay recent session history on active-session turns.
- [x] Cap history by turns and approximate characters.
- [x] Delete active session file on `session end`.
- [x] Add `--no-session` for stateless one-off calls.

## Phase 5: Install And Use

- [x] Build release binary.
- [x] Install as global `deepseek`.
- [x] Document install path.
- [x] Verify `deepseek login`.
- [x] Verify `deepseek -p "Say exactly: DEEPSEEK_OK"`.
- [x] Verify interactive continuity.
- [x] Verify `session end` deletes context.

## Phase 6: Later Enhancements

- [x] Add optional `--model`.
- [x] Add optional `--temperature`.
- [x] Add `/model` slash command for active model switching.
- [x] Add cache stats to stderr if useful.
- [x] Add streaming only after the basic shell is stable.
- [x] Warn on malformed streaming events instead of silently dropping them.

## Phase 7: Level 3 Agent Loop

- [x] Write the local agent-loop design note, using Goose, `rmcp`, Rig, and `genai` as references only.
- [x] Add an explicit `agent "<task>"` command or `--agent "<task>"` mode without changing normal chat behavior.
- [x] Add bounded planning/execution state: max steps, max output chars, done/blocked stop conditions, and transcript logging.
- [x] Define the internal tool-call schema for model-requested local actions.
- [x] Implement read-only tools first: list files, read files, search files, and inspect a bounded tree.
- [x] Enforce workspace-root restrictions and secret redaction on every tool result.
- [x] Add approval-gated `run_shell` requests with command display before execution.
- [x] Add approval-gated write/edit requests only after read-only and shell paths are stable.
- [x] Add tests for tool schema parsing, root restriction, tool error observations, transcript path, and ignored folders.
- [x] Decide whether to extract a shared agent core after one provider implementation is stable. Decision: defer extraction and keep this repo's runtime provider-local and independent.

## Validation Notes

- 2026-05-01: `deepseek login` succeeded after sourcing `~/.zsh_secrets`.
- 2026-05-01: `deepseek --no-session -p "Say exactly: DEEPSEEK_OK"` returned exactly `DEEPSEEK_OK`.
- 2026-05-01: `deepseek login` initially failed because the 16-token health-check budget was exhausted by reasoning output; fixed by raising the login check budget to 128 tokens.
- 2026-05-01: `deepseek --no-session --model deepseek-v4-flash -p "Say exactly: DEEPSEEK_OK"` returned exactly `DEEPSEEK_OK` with the installed binary.
- 2026-05-01: piped interactive continuity check remembered `BLUE` across turns and `/end` deleted `~/.local/state/provider-cli/deepseek/active-session.json`.
- 2026-05-01: `deepseek --no-session --stream -p "Count from 1 to 3, one number per line."` streamed successfully.
- 2026-05-01: `/model` is handled locally and no longer falls through to the provider as a prompt.
- 2026-05-01: Default and `/model` help updated to current DeepSeek V4 IDs: `deepseek-v4-flash` and `deepseek-v4-pro`; legacy `deepseek-chat` / `deepseek-reasoner` documented as retiring on 2026-07-24.
- 2026-05-01: `deepseek agent --root /Users/julianabeleda/env/deepseek --max-steps 4 Inspect README.md and answer with the default model documented there.` inspected the repo read-only and returned `deepseek-v4-flash`.
- 2026-05-01: Agent protocol uses nested `thought` / `tool` / `final_answer` / `blocked` JSON, local transcripts, and resilient tool error observations.
- 2026-05-01: `run_shell` added as an approval-gated agent tool; non-interactive runs deny by default.
- 2026-05-01: `propose_patch` added as an approval-gated exact single-file text replacement tool; non-interactive runs deny by default.
- 2026-05-01: Agent runtime pattern documented: keep a provider-local `src/agent.rs`; apply safety lessons independently without cross-provider code or doc references.

## Phase 8: Agent UX And Docs

- [x] Add README examples for `agent`.
- [x] Add `agent transcript latest` inspection command.
- [x] Improve approval prompts with command/edit blocks and stronger confirmation text.
- [x] Add live smoke commands for read-only, denied shell, and denied patch paths.

## Phase 8.5: Debug Runtime And Docked Composer Parity

- [x] Add local debug/manual runtime controls: `deepseek debug ...`, `/debug`, and `/runtime`.
- [x] Persist provider/debug runtime state outside the repo.
- [x] Keep normal chat separate from agent file access; debug response points file work to `agent --root`.
- [x] Add true token streaming above the composer for provider responses.
- [x] Make scanner/status row transient instead of permanent scrollback.
- [x] Fix repeated transient status row clearing.
- [x] Replace fragile cursor-column streaming with fake-rendered wrapped output.
- [x] Reserve the bottom terminal row in raw TTY mode and render the composer there.
- [x] Add PTY docked smoke covering PromptIdle, ContextScan, ResponseRender, and PromptResume.
- [x] Rebuild and install refreshed binary at `~/.local/bin/deepseek`.
- [x] Push public-main to `origin/main` through commit `75c3207`.

Validation notes:

- 2026-05-01: `python3 scripts/docked-smoke.py --binary target/release/deepseek` passed.
- 2026-05-01: Installed binary refreshed at `~/.local/bin/deepseek`.
- 2026-05-01: Public branch pushed to `origin/main` with debug runtime, transient scanner, fake-rendered streaming, reserved dock row, and PTY smoke.

## Phase 9: Superseded Workspace Agent Experiment

- [x] Scope Codex/Claude-style workspace agent mode.
- [x] Extract explicit plain chat mode so current behavior remains available.
- [x] Experiment with `deepseek` starting directly in interactive workspace-agent mode.
- [x] Keep `deepseek -p "prompt"` as plain one-shot chat for scripting stability.
- [x] Add `/chat` and `/agent` mode switches inside the interactive shell.
- [x] Show workspace root and approval policy at agent startup.
- [x] Superseded by Phase 10: bare `deepseek` now opens docked chat; agent execution is explicit or confirmed.

Validation notes:

- 2026-05-02: First pass added `deepseek chat`, `--chat`, `--agent`, experimental direct-agent startup, `/chat` and `/agent` switches, and explicit workspace/approval startup text.
- 2026-05-02: `cargo fmt --check`, `cargo test`, `cargo build`, `cargo build --release`, `python3 scripts/docked-smoke.py --binary target/debug/deepseek`, `python3 scripts/docked-smoke.py --binary target/release/deepseek`, `printf '/exit\n' | target/debug/deepseek --agent`, `printf '/exit\n' | target/debug/deepseek chat`, and installed binary startup smoke passed.
- 2026-05-02: Installed binary refreshed at `~/.local/bin/deepseek`.
- 2026-05-02: Phase 10 intentionally replaced this direct-agent startup direction with one docked chat surface plus routed task execution.

## Phase 10: Unified Chat Surface And Intent Routing

- Scope: `structure/Development/unified-intent-routing-scope.md`
- [x] Supersede the Phase 9 direct-agent assumption with one default docked chat surface.
- [x] Make bare `deepseek` boot the same bottom-docked chat UI as `deepseek chat`.
- [x] Keep agent mode as an explicit inline execution path, not a second bottom-dock UI.
- [x] Preserve `deepseek --agent` and `deepseek agent --root <path> "<task>"` for explicit bounded task execution.
- [x] Add intent routing inside the main chat shell: open-ended questions stay in chat; bounded task requests route to agent/task execution or ask for confirmation.
- [x] Treat the main chat UI as the collapse target when merging chat and agent behavior.
- [x] Show ContextScan progress above the chat composer for routed task work.
- [x] Require an explicit workspace root or confirmation before agent work when launched from `$HOME`.
- [x] Add `/root <path>` in chat so routed tasks can choose an explicit workspace without launching from that directory.
- [x] Show selected chat root in `/status` and route task confirmations.
- [x] Block routed task execution when prompt path references escape the selected `/root`.
- [x] Add smoke coverage proving bare default startup lands in the chat dock, not the inline agent prompt.
- [x] Add smoke coverage for `/agent` -> inline agent and `/chat` -> restored chat dock.
- [x] Update README, next-session context, and old structure docs that still describe direct workspace-agent startup.

Validation notes:

- 2026-05-02: Product direction changed from "direct agent shell" to "one default chat shell with intent-routed task execution." Agent mode should do work inline; the persistent bottom composer belongs to chat.
- 2026-05-02: First Phase 10 slice changed bare `deepseek` to docked chat, kept `--agent` as inline agent, added default dock smoke coverage, and updated README/handoff docs.
- 2026-05-02: Intent routing is implemented. Confirmed `y` tasks now run once through the agent runtime and return to the docked chat shell.
- 2026-05-02: PTY smoke/probe stability improved with `DEEPSEEK_FORCE_TTY_SIZE=80x24`; phase probe A4 treats debug-backend non-JSON agent responses as N/A.
- 2026-05-02: `/root <path>` is implemented for docked chat. `/status` reports `root-source`, routed task confirmations use the explicit root, and `/root clear` restores `$HOME` clarify safety.
- 2026-05-02: Routed task prompts that reference `../outside.md` or absolute paths outside the selected `/root` now clarify instead of routing to agent.
- 2026-05-02: Outside-root clarification now includes `Suggested root:` and a concrete `/root <parent>` command so the user can intentionally switch workspace and retry.
- 2026-05-02: ContextScan status now repaints one row in place above the composer, caps the timer at `1.0s`, and waits one second before streaming so Phase 2 is visible without scrolling log lines.

## Recent Completed Work: 2026-05-03

- [x] Keep normal provider mode as the default; debug backend remains explicit via debug/runtime controls.
- [x] Add direct `/agent <task>` execution so the user can launch a bounded agent task from chat without waiting on a pending confirmation prompt.
- [x] Prevent bare `agent task` with no pending task from falling through to model chat.
- [x] Expand declarative task routing for file/workspace prompts, including `read`, `scan`, `inspect`, scoped problem statements, and natural filesystem locations.
- [x] Add session/root approval ergonomics so routed tasks can accept short `y` / `n` confirmations instead of only `yes agent`.
- [x] Fix audit gap: help text documents `/agent <task>`.
- [x] Add natural root inference for `env folder`, `env directory`, and `my env`.
- [x] Rebuild and reinstall the local binary at `~/.local/bin/deepseek`.
- [x] Push recent public-main commits:
  - `1fbd04a [ui] Handle direct agent task command`
  - `63e9f81 [ui] Expand declarative task routing`
  - `f0239a7 [ui] Cover routing audit gaps`
  - `783b13b [ui] Accept short agent route confirmation`
  - `5699a02 [ui] Route env folder tasks`
- [x] Consolidate Kimi-style routing direction into `structure/Development/kimi-routing-context.md`.
- [x] Remove the broad root-level `SCOPE.md` draft.

Validation notes:

- 2026-05-03: Routing smoke expanded through 33 cases and passed against debug and installed binaries.
- 2026-05-03: `scan my desktop` routes to agent with `~/Desktop` as root and accepts `y`.
- 2026-05-03: `go to my env folder and tell me what you find there` routes to agent with `~/env` as root.
- 2026-05-03: `hi` and open-ended questions remain normal chat when deterministic routing is active.

## Phase 11: Kimi-Style Docked Agent Direction

- Source of truth: `structure/Development/kimi-routing-context.md`
- [x] Consolidate the Kimi-style design direction into one local handoff note.
- [x] Remove the broad root-level `SCOPE.md` draft from the working tree.
- [x] Add async docked agent execution so tool steps and final answers render through the dock instead of stdout handoff.
- [x] Make model-decided routing the default baseline.
- [x] Add a `legacy-routing` runtime flag, default off.
- [x] When `legacy-routing` is on, preserve deterministic Phase 10 routing behavior.
- [x] When `legacy-routing` is off, send prompts through the tool-capable agent path and let the model return `final_answer` or tool calls.
- [x] Keep current shell/edit approval gates (`yes run`, `yes apply`) in explicit agent mode.
- [x] Deny shell/edit tools in docked model-decided routing until a dock-native approval UI is scoped.
- [x] Add PTY smoke proving `hi` and `scan my desktop` complete inside the dock without `agent task:` stdout handoff.
- [x] Do not add reasoning display or the Kimi-style approval dialog in this slice.

Validation notes:

- 2026-05-03: Direction set: Kimi-style model-decided tool use is the destination, but the next implementation slice is limited to async docked agent execution plus an opt-in `kimi-routing` flag.
- 2026-05-03: DeepSeek should be implemented and proven first; MiniMax should only be ported after DeepSeek passes focused tests and PTY smoke.
- 2026-05-03: Implemented model-decided routing as the default baseline with `/runtime legacy-routing on|off` as the deterministic fallback.
- 2026-05-03: Added `scripts/phase11-docked-routing-smoke.py`; it proves `hi` and `scan my desktop` render through the dock with a fake provider and no `agent task:` stdout handoff.
- 2026-05-03: Docked model-decided routing now uses deny mode for shell/edit tools. Explicit agent mode still owns `yes run` and `yes apply` prompts.
- 2026-05-03: Release validation passed: `cargo build --release --offline`, unit tests, Phase 11 docked routing smoke, and default/chat docked smokes.

## Phase 12: Dock-Native Approval First Slice

- Scope: `docs/phase12-dock-approval-scope.md`
- Handoff: `structure/Development/session-handoff.md`
- [x] Implement dock-native approval requests in DeepSeek first.
- [x] Preserve explicit agent mode `yes run` and `yes apply` prompts.
- [x] Add dock-native approval requests for `run_shell` and `propose_patch`.
- [x] Accept approval and denial through the bottom composer.
- [x] Keep approval exact and one-shot:
  - `yes run` approves one `run_shell`.
  - `yes apply` approves one `propose_patch`.
- [x] Keep denial explicit in the REPL with `n`, `no`, or `deny`.
- [x] Remove the stale configurable `deny_phrase` field from `ApprovalRequest`.
- [x] Add unit tests for patch approval request creation and approved patch application.
- [x] Add Phase 12 PTY smoke coverage for shell denial, shell approval, patch denial, and patch approval.
- [x] Port the validated Phase 12 behavior to MiniMax.
- [x] Push DeepSeek and MiniMax parity commits to `origin/main`.
- [ ] Audit Phase 12 completion commits as a matched DeepSeek/MiniMax pair.
- [ ] Decide whether to extract approval prompt formatting from `agent.rs` before adding more approval types.
- [ ] If audit passes, tag or document Phase 12 as complete.

Validation notes:

- 2026-05-03: DeepSeek Phase 12 pushed through:
  - `afa796a [test] Cover patch dock approvals`
  - `59f83ed [cli] Align dock approval denial handling`
  - `3812582 [docs] Document Phase 12 approval flow`
- 2026-05-03: MiniMax parity pushed through:
  - `ea09fd0 [test] Cover patch dock approvals`
  - `2401abe [cli] Align dock approval denial handling`
  - `c95e7d3 [docs] Document Phase 12 approval flow`
- 2026-05-03: Validation passed: `cargo fmt --check`, `cargo test --offline`, `cargo build --offline`, Phase 11 docked routing smoke, and Phase 12 dock approval smoke.

## Post-Phase 12: Persistent Workspace Navigation

- [x] Add session-level `selected_root` persistence separate from approval-scoped `agent_root`.
- [x] Make natural navigation update the selected workspace root without approving shell/edit tools.
- [x] Support Kimi-style navigation phrases:
  - `go to my env folder`
  - `navigate into my env folder`
  - `cd into deepseek`
  - `enter the deepseek repo`
  - `open the minimax repo`
  - `go inside ~/env/deepseek`
- [x] Keep task phrases such as `go through downloads` routed as tasks, not root navigation.
- [x] Keep casual chat such as `switch to main branch`, `stay in touch`, and `open a ticket` from changing roots.
- [x] Print inline `root error:` messages for bad explicit navigation paths instead of exiting the TUI.
- [x] Add `scripts/persistent-navigation-test.sh` as a focused debugger for this behavior.
- [x] Expand `scripts/routing-debug.py` to cover persistent navigation and selected-root reuse.
- [x] Port matching behavior and debugger coverage to MiniMax.

Validation notes:

- 2026-05-04: DeepSeek `./scripts/persistent-navigation-test.sh` passed:
  - `cargo fmt --check`
  - `cargo build --offline`
  - `cargo test --offline` with 80/80 tests passing
  - focused navigation unit test
  - routing/debug PTY smoke with 35/35 cases passing
- 2026-05-04: MiniMax `./scripts/persistent-navigation-test.sh` passed with 87/87 tests and 35/35 PTY cases.
- 2026-05-04: External Kimi audit was attempted but failed with a connection error; external Claude print-mode audit was attempted but did not return output and was stopped. Local verification passed.

Phase 8 smoke commands:

```bash
deepseek agent --root /Users/julianabeleda/env/deepseek --max-steps 4 "Inspect README.md and answer with the default model documented there."
deepseek agent --root /Users/julianabeleda/env/deepseek --max-steps 4 "Request run_shell with command 'pwd', cwd '.', reason 'denied smoke', then report the result."
deepseek agent --root /Users/julianabeleda/env/deepseek --max-steps 4 "Request propose_patch on README.md replacing 'Standalone Rust CLI' with 'Standalone Rust CLI', reason 'denied smoke', then report the result."
deepseek agent --root /Users/julianabeleda/env/deepseek transcript latest
```
