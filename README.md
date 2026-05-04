# DeepSeek

Standalone Rust CLI for using DeepSeek from the terminal.

Do not store API keys in this folder. Put secrets in your shell environment and expose them as environment variables.

## Install

```bash
cargo build --release
cp target/release/deepseek ~/.local/bin/deepseek
```

Make sure `~/.local/bin` is on `PATH`.

## Configuration

- Secret env var: `DEEPSEEK_API_KEY`
- API style: OpenAI-compatible chat completions
- Base URL: `https://api.deepseek.com`
- Chat endpoint: `/chat/completions`
- Default model: `deepseek-v4-flash`

The API key is read from the environment and is never written to the session file.

## Commands

```bash
deepseek login
deepseek -p "prompt"
deepseek --stream -p "prompt"
deepseek --no-session -p "prompt"
deepseek chat
deepseek chat -p "prompt"
deepseek --agent
deepseek agent --root . "Inspect README.md and summarize the CLI."
deepseek agent --root . transcript latest
deepseek
deepseek session start [name]
deepseek session status
deepseek session end
```

One-off prompt mode prints only the assistant response to stdout. Errors are printed to stderr and exit non-zero.
Use `--stream` to print response deltas as they arrive. Cache-token stats are printed to stderr when the provider returns them.

Default interactive mode is the bottom-docked chat shell. It is the primary surface for open-ended questions and workspace-aware tool use.

Chat mode:

```text
$ deepseek
deepseek [deepseek-v4-flash] › look at this code
<response>
deepseek [deepseek-v4-flash] › /model deepseek-v4-pro
model set: deepseek-v4-pro
deepseek [deepseek-v4-pro] › /end
```

The explicit `chat` command starts the same docked chat shell:

```text
$ deepseek chat
deepseek [deepseek-v4-flash] › what do you think about this design?
<response>
```

The CLI keeps context only during the active ephemeral session and deletes that context when the session ends. The active state path is `~/.local/state/provider-cli/deepseek/active-session.json`, with fallback behavior for environments where the home state path cannot be determined.

Use `/model` inside the interactive shell to show supported model IDs, and `/model <id>` to switch the active session model. Use `/root <path>` to choose the workspace root for tool-capable chat, `/root` to show it, and `/root clear` to return to cwd-based root detection. Use `/runtime legacy-routing on` to temporarily restore deterministic Phase 10 route confirmation, and `/runtime legacy-routing off` to return to model-decided routing. Use `/agent` or `--agent` when you want explicit workspace-agent execution, and `/chat` to return to the docked chat shell. One-off calls can also switch models with `--model <id>`. Current DeepSeek API model IDs are `deepseek-v4-flash` and `deepseek-v4-pro`; legacy aliases `deepseek-chat` and `deepseek-reasoner` retire on 2026-07-24.
Prompts that reference paths outside the selected root ask for clarification instead of routing directly to tool execution. Docked chat can use read-only workspace tools. Shell commands and edits require dock-native approval through the composer with exact phrases such as `yes run` or `yes apply`.

Agent mode is explicit and runs a bounded local tool loop with workspace-scoped tools, transcript logging, and approval gates for shell commands and exact text edits:

```bash
deepseek --agent
deepseek agent --root . --max-steps 8 "Inspect README.md and report the default model."
deepseek agent --root . transcript latest
```

Agent transcripts are written under `.deepseek/agent-transcripts/` inside the selected root. The latest transcript command prints the newest JSON transcript to stdout and its path to stderr.

## Development

```bash
cargo fmt --check
cargo check
cargo test --offline
python3 scripts/docked-smoke.py --binary target/release/deepseek
python3 scripts/docked-smoke.py --binary target/release/deepseek --entrypoint default
python3 scripts/agent-startup-smoke.py --binary target/release/deepseek
python3 scripts/phase10-scope-probe.py --binary target/release/deepseek --name deepseek --model deepseek-v4-flash
python3 scripts/phase11-docked-routing-smoke.py --binary target/release/deepseek
python3 scripts/phase12-dock-approval-smoke.py --binary target/release/deepseek
```

Live provider smoke requires `DEEPSEEK_API_KEY` and network access:

```bash
python3 scripts/live-docked-routing-smoke.py --binary target/release/deepseek
```
