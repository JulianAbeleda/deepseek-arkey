# Commit Discipline

DeepSeek commits must use PKOS-style bracketed subsystem prefixes:

- `[cli]` - command parsing, session flow, interactive shell behavior, installable binary behavior
- `[provider]` - DeepSeek API integration, model IDs, streaming/parser behavior, provider response handling
- `[ui]` - prompt, banner, help, status, color, and terminal display formatting
- `[runtime]` - local provider/debug runtime state and backend controls
- `[test]` - tests, smoke scripts, and verification fixtures
- `[docs]` - README, contributor docs, checklists, handoffs, and non-runtime documentation
- `[control]` - Purpose/control-plane role assignments and delegation routing

Examples:

```text
[cli] Default to workspace agent mode
[ui] Reserve bottom dock row in raw mode
[test] Add docked composer PTY smoke
```

Use `NFC` after the prefix for non-functional changes:

```text
[cli] NFC - extract session helper
```

CI validates this format for pull request titles and pushed commit subjects.

The tracked hook in `.githooks/commit-msg` is optional local feedback. Enable it with:

```bash
git config core.hooksPath .githooks
```
