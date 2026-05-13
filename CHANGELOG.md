# Changelog

## Phase 17 - Internet Tools

Addendum:

- `/features` now shows which API-backed capabilities are enabled by the
  current shell environment without printing secret values.

Summary:

- Normal chat now prefetches web context for URL and current-info prompts,
  continuing with a warning if web context is unavailable.
- Explicit agent mode can call `web_search` and `fetch_url` as read-only tools.
- Search defaults to Brave via `BRAVE_SEARCH_API_KEY` or `BRAVE_API_KEY`.
- `DEEPSEEK_SEARCH_PROVIDER=tavily` switches search to Tavily via `TAVILY_API_KEY`.
- `fetch_url` is limited to HTTP(S), validates DNS/IP and redirect targets, rejects
  restricted addresses, and caps response size, redirects, and timeout.

Validation:

```bash
cargo fmt --check
cargo test --offline
cargo clippy --offline
```

## Phase 12 - Dock-Native Approval First Slice

Reliability addendum:

- OpenAI-style agent decisions now preserve multiple tool calls and execute them
  in order within one provider step.
- Placeholder final content such as `answer with concrete findings` no longer
  masks real `blocked` or `final_answer` fields.
- Patch failure-mode coverage now tracks ambiguous replacements and changed-file
  races across the provider CLIs.

Summary:

- Docked model-decided routing can now request approval for `run_shell` and
  `propose_patch` through the bottom composer.
- Approval requests render above the dock.
- `n` denies the pending tool request.
- Exact approval phrases approve one tool request:
  - `yes run` for `run_shell`
  - `yes apply` for `propose_patch`
- Explicit agent mode still keeps its existing terminal approval prompts.

Validation:

```bash
cargo fmt --check
cargo test --offline
cargo build --offline
python3 scripts/phase11-docked-routing-smoke.py --binary target/debug/deepseek
python3 scripts/phase12-dock-approval-smoke.py --binary target/debug/deepseek
```

## Phase 11 - Live Validated Docked Routing

Tags:

- `phase11-parity-complete`
- `phase11-live-validated`

Summary:

- Default docked chat now uses the model-decided agent runtime path.
- Tool progress renders above the dock as `agent step N: tool_name`.
- Final answers render above the dock without an `agent task:` stdout handoff.
- `/runtime legacy-routing on|off` toggles the deterministic Phase 10 fallback.
- Docked chat can use read-only workspace tools.
- Shell commands and edits are denied in docked routing until a dock-native
  approval UI is scoped.
- Explicit agent mode still owns the rough `yes run` and `yes apply` approval
  prompts.

Validation:

```bash
cargo fmt --check
cargo test --offline
cargo build --release --offline
python3 scripts/phase11-docked-routing-smoke.py --binary target/release/deepseek
python3 scripts/docked-smoke.py --binary target/release/deepseek --entrypoint default
python3 scripts/docked-smoke.py --binary target/release/deepseek --entrypoint chat
```

Live validation requires `DEEPSEEK_API_KEY` and network access:

```bash
python3 scripts/live-docked-routing-smoke.py --binary target/release/deepseek
```
