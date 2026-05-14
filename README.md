# DeepSeek Arkey

Standalone Rust CLI for using DeepSeek from the terminal, packaged as
`deepseek-arkey`.

## What This Is

DeepSeek is an AI model provider. This project is an independent terminal client
for talking to DeepSeek models from a local machine.

A CLI, or command-line interface, is a tool you run from a terminal with commands
like `deepseek-arkey -p "prompt"`. A TUI, or terminal user interface, is still terminal
based, but it gives the session an interactive interface instead of only printing
one command result at a time.

The goal of this project is to make DeepSeek feel closer to tools like Codex and
Claude Code: a focused terminal workflow with chat, local project context,
workspace tools, approval gates, and a docked prompt. At the time this was built,
DeepSeek did not have an official CLI in that style. There were unofficial
options, including a full-screen TUI, but they did not match the smaller docked
workflow this project was aiming for.

This is also a public development artifact. Julian built the project through an
iterative prompt-driven workflow: using AI assistants to plan, implement, audit,
debug, and refine the CLI over many small commits.

Do not store API keys in this folder. Put secrets in your shell environment and expose them as environment variables.

## Quick Start

With Homebrew:

```bash
brew install JulianAbeleda/tap/deepseek-arkey
export DEEPSEEK_API_KEY="your_deepseek_api_key"
deepseek-arkey login
deepseek-arkey
```

From source:

```bash
cargo build --release
cp target/release/deepseek-arkey ~/.local/bin/deepseek-arkey
cp target/release/deepseek ~/.local/bin/deepseek
export DEEPSEEK_API_KEY="your_deepseek_api_key"
deepseek-arkey login
```

For source installs, make sure `~/.local/bin` is on `PATH`. The `deepseek`
binary is kept as a compatibility alias.

For zsh persistence:

```bash
echo 'export DEEPSEEK_API_KEY="your_deepseek_api_key"' >> ~/.zsh_secrets
source ~/.zshrc
deepseek-arkey login
```

## Configuration

- Secret env var: `DEEPSEEK_API_KEY`
- API style: OpenAI-compatible chat completions
- Base URL: `https://api.deepseek.com`
- Chat endpoint: `/chat/completions`
- Default model: `deepseek-v4-flash`

The API key is read from the environment and is never written to the session file.

Internet search is opt-in by provider key:

- Search provider: `DEEPSEEK_SEARCH_PROVIDER=brave|tavily` (defaults to `brave`)
- Brave key: `BRAVE_SEARCH_API_KEY` (`BRAVE_API_KEY` is accepted as an alias)
- Tavily key: `TAVILY_API_KEY`
- Runtime switch: `/features toggle` persists the selected search provider
  (`brave` or `tavily`) without storing secrets.

Normal chat prefetches web context for URL and current-info prompts, but continues with a warning if web context is unavailable. Agent mode exposes two read-only web tools: `web_search` and `fetch_url`; explicit web tool calls return errors when the selected provider is missing its key or a fetch fails.

## Commands

```bash
deepseek-arkey login
deepseek-arkey -p "prompt"
deepseek-arkey --stream -p "prompt"
deepseek-arkey --no-session -p "prompt"
deepseek-arkey chat
deepseek-arkey chat -p "prompt"
deepseek-arkey --agent
deepseek-arkey agent --root . "Inspect README.md and summarize the CLI."
deepseek-arkey agent --root . transcript latest
deepseek-arkey
deepseek-arkey session start [name]
deepseek-arkey session status
deepseek-arkey session end
```

One-off prompt mode prints only the assistant response to stdout. Errors are printed to stderr and exit non-zero.
Use `--stream` to print response deltas as they arrive. Cache-token stats are printed to stderr when the provider returns them.

Default interactive mode is the bottom-docked chat shell. It is the primary surface for open-ended questions and workspace-aware tool use.

Chat mode:

```text
$ deepseek-arkey
deepseek-arkey [deepseek-v4-flash] › look at this code
<response>
deepseek-arkey [deepseek-v4-flash] › /model deepseek-v4-pro
model set: deepseek-v4-pro
deepseek-arkey [deepseek-v4-pro] › /end
```

The explicit `chat` command starts the same docked chat shell:

```text
$ deepseek-arkey chat
deepseek-arkey [deepseek-v4-flash] › what do you think about this design?
<response>
```

The CLI keeps context only during the active ephemeral session and deletes that context when the session ends. The active state path is `~/.local/state/provider-cli/deepseek-arkey/active-session.json`, with fallback behavior for environments where the home state path cannot be determined.

Use `/model` inside the interactive shell to show supported model IDs, and `/model <id>` to switch the active session model. Use `/features` to show which API-backed capabilities are enabled by the current shell environment, and `/features toggle` to persistently switch web search between Brave and Tavily. Use `/root <path>` to choose the workspace root for tool-capable chat, `/root` to show it, and `/root clear` to return to cwd-based root detection. Use `/runtime legacy-routing on` to temporarily restore deterministic Phase 10 route confirmation, and `/runtime legacy-routing off` to return to model-decided routing. Use `/agent` or `--agent` when you want explicit workspace-agent execution, and `/chat` to return to the docked chat shell. One-off calls can also switch models with `--model <id>`. Current DeepSeek API model IDs are `deepseek-v4-flash` and `deepseek-v4-pro`; legacy aliases `deepseek-chat` and `deepseek-reasoner` retire on 2026-07-24.
Prompts that reference paths outside the selected root ask for clarification instead of routing directly to tool execution. Docked chat can use read-only workspace tools and deterministic web prefetch for URL/current-info prompts. Shell commands and edits require dock-native approval through the composer. Approvals can be one-shot or scoped to the current workspace root, with separate scopes for shell commands and file writes.

Agent mode is explicit and runs a bounded local tool loop with workspace-scoped tools, read-only web tools, transcript logging, and approval gates for shell commands and exact text edits:

```bash
deepseek-arkey --agent
deepseek-arkey agent --root . --max-steps 8 "Inspect README.md and report the default model."
deepseek-arkey agent --root . transcript latest
```

Agent transcripts are written under `.deepseek-arkey/agent-transcripts/` inside the selected root. The latest transcript command prints the newest JSON transcript to stdout and its path to stderr.

## Development

```bash
cargo fmt --check
cargo check
cargo test --offline
python3 scripts/docked-smoke.py --binary target/release/deepseek-arkey
python3 scripts/docked-smoke.py --binary target/release/deepseek-arkey --entrypoint default
python3 scripts/agent-startup-smoke.py --binary target/release/deepseek-arkey
python3 scripts/phase10-scope-probe.py --binary target/release/deepseek-arkey --name deepseek-arkey --model deepseek-v4-flash
python3 scripts/phase11-docked-routing-smoke.py --binary target/release/deepseek-arkey
python3 scripts/phase12-dock-approval-smoke.py --binary target/release/deepseek-arkey
```

Live provider smoke requires `DEEPSEEK_API_KEY` and network access:

```bash
python3 scripts/live-docked-routing-smoke.py --binary target/release/deepseek-arkey
```

## License

MIT. See [LICENSE](LICENSE).
