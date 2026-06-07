---
name: herdr-add-agent-integration
description: Add a new built-in agent integration to herdr (install hook, session report, resume plan, docs, tests). Use when implementing herdr integration install/uninstall for a new CLI agent, mirroring Codex/Droid/Cursor patterns, or when a contributor asks how to wire pane.report_agent_session + agent resume for a new tool.
---

# Add a Herdr Agent Integration

Use inside the **herdr** repository when adding a first-party integration such as `herdr integration install <agent>`.

Read `references/integration-checklist.md` for the file-by-file checklist. This skill captures workflow and traps learned from the Cursor integration (#506).

## Decide the integration shape first

Ask (or infer) from the target agent:

| Question | Why it matters |
|----------|----------------|
| Session id only, or also on-disk session path? | `pane.report_agent_session` params + `AgentSessionRefKind` |
| Does the agent expose a dedicated resume CLI? | `agent_resume.rs` argv (e.g. `cursor-agent --resume <id>`, not generic `agent`) |
| Where do hooks/config live? | Install path (`~/.codex`, `~/.cursor`, etc.) and env override (`CODEX_HOME`, `CURSOR_CONFIG_DIR`) |
| Session-only or also runtime state? | Herdr prefers **session-only** for many agents — hook reports id; **screen detection** owns working/idle/blocked |

Default to **session-only** unless the agent has no reliable screen signals.

## Implementation workflow

### 1. Hook asset

Create `src/integration/assets/<agent>/herdr-agent-state.sh` (or plugin/hook shape the agent expects).

Hook must:

- Exit quietly unless `HERDR_ENV=1`, `HERDR_SOCKET_PATH`, and `HERDR_PANE_ID` are set (herdr sets these on pane spawn via `apply_pane_env`).
- Read hook JSON from stdin into a temp file (Cursor/Codex pattern).
- Call `pane.report_agent_session` over the Unix socket with:
  - `source`: `herdr:<agent>` (added to reserved native sources if session-only)
  - `agent`: stable label matching detect/resume code
  - `agent_session_id` and/or `agent_session_path`
- **Not** call `pane.release_agent` or push runtime state for session-only integrations.

`HERDR_PANE_ID` format is `p_{pane_raw_u32}` (e.g. `p_1`), not the public `w…-1` pane id.

### 2. Rust wiring (minimum set)

1. **`src/api/schema.rs`** — `IntegrationTarget::<Agent>` enum variant + serde names.
2. **`src/integration/mod.rs`**
   - Asset constants + `install_<agent>` / `uninstall_<agent>`
   - Register in `install_target`, `uninstall_target`, status helpers
   - **`install_<agent>` must fail clearly** if the resume CLI binary is missing from `PATH` (before writing hooks).
   - **`cursor_command_names()`-style helper** — list only the **specific** binary name(s), not generic aliases (`agent`, etc.).
   - Tests: hook content assertions, idempotent install, config dir env override, missing-binary error.
3. **`src/agent_resume.rs`** — `plan()` match arm: official source + resume argv (use the real CLI name).
4. **`src/cli/integration.rs`** — CLI target string mapping.
5. **`src/detect/mod.rs`** — map process name → `Agent::<Variant>` if screen detection should recognize it.
6. **`src/agent_resume.rs` / persist** — if session-only, ensure source is in `is_reserved_native_state_source` (hook-reported session wins over screen for restore metadata).

Copy an existing sibling integration (**Codex** for TOML/hooks, **Cursor** for `hooks.json` + `sessionStart`, **Droid** for similar hook shape).

### 3. Docs

Update `docs/next/website/src/content/docs/integrations.mdx` and `session-state.mdx` (or current next-release doc paths):

- Install/uninstall commands
- Exact resume command herdr runs
- Session-only note if applicable
- Required binary on PATH

### 4. Tests

- **Unit:** resume planner argv (`agent_resume::tests`), install/uninstall under temp config dir.
- **Manual / script:** `scripts/e2e-<agent>-integration.sh` — see Cursor script for named-session server bootstrap pattern.

Run:

```bash
cargo test <agent> --locked
cargo test planner_builds_resume_argv_for_official_agents -- --exact
```

## Manual E2E (maintainer-grade)

1. `herdr integration install <agent>`
2. Start **interactive** agent in a pane: `herdr agent start foo --split down --focus -- <exact-cli>`
3. Confirm `herdr agent list` shows `agent_session` with `source=herdr:<agent>`.
4. `herdr server stop`, then `herdr` (TUI attach — resume spawn is deferred until a client attaches).
5. Confirm process argv is the **specific** resume command (e.g. `cursor-agent --resume <id>`).

## Known traps (Cursor learnings)

| Trap | Detail |
|------|--------|
| Print/headless mode skips hooks | `cursor-agent -p` does **not** fire `sessionStart` hooks; interactive CLI or IDE session needed for hook-driven report. |
| Generic binary names | Do not use `agent` for restore/availability — collisions with other tools. |
| Named sessions | API subcommands need a running server: `herdr --session name server` or attach flow; `--session` alone does not spawn. |
| Pane id in hooks | API expects `HERDR_PANE_ID=p_<raw>`; public `w…-1` ids are for CLI display. |
| Workspace cleanup | Ephemeral `-p` agents exit quickly; report session **while the pane is alive** or persistence may miss the window. |
| Resume without TUI | Headless server restores **metadata**; `cursor-agent --resume` is sent when a **foreground client** attaches. |

## Optional: E2E script pattern

`scripts/e2e-cursor-integration.sh` demonstrates:

- Isolated `CURSOR_CONFIG_DIR` + named session
- `ensure_server` helper
- Install → agent start → session report → `session.json` check → stop/restart → restored `agent list`

Adapt for new agents; document if hook cannot be exercised in CI (print mode, API keys, etc.).

## Longer-term: plugin-shaped integrations

Today each integration is compiled into herdr core. A plugin model (manifest + hook assets + resume metadata loaded from user config dir) would let community ship agents without a full core PR per tool — worth a Discussion before the N+1th integration.
