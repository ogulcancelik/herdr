# integrations

herdr works without any hook or plugin setup.

out of the box, it detects supported agents automatically by combining foreground process detection with screen heuristics. that is enough to give you workspace awareness with zero configuration.

when an agent exposes hooks or plugins, the more robust path is to forward semantic state to herdr over the local socket api. the built-in integrations in this document do exactly that.

if you want to inspect the exact files herdr installs, they are versioned in this repo:

- [pi extension](./src/integration/assets/pi/herdr-agent-state.ts)
- [claude code hook](./src/integration/assets/claude/herdr-agent-state.sh)
- [codex hook](./src/integration/assets/codex/herdr-agent-state.sh)
- [opencode plugin](./src/integration/assets/opencode/herdr-agent-state.js)

## how herdr uses integrations

herdr uses a hybrid model:

- **process detection** owns pane identity, liveness, and "the process is gone"
- **agent integrations** report semantic state like `working`, `blocked`, and `idle` over the local socket api when the tool exposes those events
- **screen heuristics** remain the fallback for gaps, unsupported tools, or incomplete hook surfaces

that means hooks/plugins do **not** become the source of truth for pane ownership. they enrich state reporting; they do not replace process detection.

## install commands

```bash
herdr integration install pi
herdr integration install claude
herdr integration install codex
herdr integration install opencode
```

## uninstall commands

```bash
herdr integration uninstall pi
herdr integration uninstall claude
herdr integration uninstall codex
herdr integration uninstall opencode
```

## pi

install:

```bash
herdr integration install pi
```

this writes the bundled pi extension to:

```text
~/.pi/agent/extensions/herdr-agent-state.ts
```

pi is the cleanest integration. it already has an authoritative hook model, so herdr can get direct state reports over the socket api without guessing as much from the terminal.

bundled source: [`src/integration/assets/pi/herdr-agent-state.ts`](./src/integration/assets/pi/herdr-agent-state.ts)

uninstall:

```bash
herdr integration uninstall pi
```

this removes:

```text
~/.pi/agent/extensions/herdr-agent-state.ts
```

## claude code

install:

```bash
herdr integration install claude
```

this:

- writes the hook script to `~/.claude/hooks/herdr-agent-state.sh`
- updates `~/.claude/settings.json`

bundled source: [`src/integration/assets/claude/herdr-agent-state.sh`](./src/integration/assets/claude/herdr-agent-state.sh)

current hook mapping:

- `UserPromptSubmit` → `working`
- `PreToolUse` → `working`
- `PermissionRequest` → `blocked`
- `Stop` → `idle`
- `SessionEnd` → `release`

notes:

- claude's current hook surface improves state reporting, but it is not a perfect permission lifecycle.
- when a permission prompt is canceled, claude does not currently give herdr a clean hook event that always resolves the pane out of `blocked` immediately.
- that is acceptable in herdr's model: process detection still owns liveness, and heuristics remain the fallback for unresolved edges.

uninstall:

```bash
herdr integration uninstall claude
```

this:

- removes `~/.claude/hooks/herdr-agent-state.sh`
- removes herdr-owned hook entries from `~/.claude/settings.json`

## codex

install:

```bash
herdr integration install codex
```

this:

- writes the hook script to `~/.codex/herdr-agent-state.sh`
- updates `~/.codex/hooks.json`
- ensures `codex_hooks = true` in `~/.codex/config.toml`

bundled source: [`src/integration/assets/codex/herdr-agent-state.sh`](./src/integration/assets/codex/herdr-agent-state.sh)

current hook mapping:

- `SessionStart` → `idle`
- `UserPromptSubmit` → `working`
- `PreToolUse` → `working`
- `Stop` → `idle`

notes:

- codex does **not** currently expose a permission-specific hook like claude or opencode, so `blocked` still depends on herdr's normal heuristics.
- codex currently renders hook lifecycle messages in its own tui, for example `Running SessionStart hook` and `SessionStart hook (completed)`.
- that noise is an upstream codex limitation, not a herdr-specific issue.
- codex has a `suppressOutput` field in its hook output schema, but it is currently not effective for suppressing those tui lifecycle lines.

uninstall:

```bash
herdr integration uninstall codex
```

this:

- removes `~/.codex/herdr-agent-state.sh`
- removes herdr-owned hook entries from `~/.codex/hooks.json`
- intentionally leaves `~/.codex/config.toml` alone

that last point is deliberate: herdr does **not** try to guess whether `codex_hooks = true` is still needed for some other codex hook setup.

## opencode

install:

```bash
herdr integration install opencode
```

this writes the bundled plugin to:

```text
~/.config/opencode/plugins/herdr-agent-state.js
```

bundled source: [`src/integration/assets/opencode/herdr-agent-state.js`](./src/integration/assets/opencode/herdr-agent-state.js)

current plugin mapping:

- `permission.asked` → `blocked`
- `permission.replied: once|always` → `working`
- `permission.replied: reject` → `idle`
- `question.asked` → `blocked`
- `question.replied` → `working`
- `question.rejected` → `idle`
- `session.status: busy|retry` → `working`
- `session.status: idle` → `idle`
- `session.idle` → `idle`

notes:

- opencode has the richest event surface of the currently supported integrations.
- herdr intentionally does **not** guess that `session.deleted` means process exit. process detection still owns liveness and pane identity.

uninstall:

```bash
herdr integration uninstall opencode
```

this removes:

```text
~/.config/opencode/plugins/herdr-agent-state.js
```

## known limitations

- these integrations only activate inside herdr-managed panes.
- if an agent has an incomplete hook surface, herdr falls back to process detection and screen heuristics rather than inventing lease or ttl behavior.
- codex currently shows hook lifecycle chatter in its own tui until upstream adds a real silent mode.

## troubleshooting

if an install command succeeds but you do not see improved state reporting:

1. make sure you launched the agent inside a herdr pane
2. restart the agent session so it picks up the new hook/plugin config
3. verify the expected config file was written to the path above
4. remember that unsupported transitions still fall back to herdr's built-in heuristics
