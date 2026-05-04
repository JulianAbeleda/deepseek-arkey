# Changelog

## Phase 12 - Dock-Native Approval First Slice

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
