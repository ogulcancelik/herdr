# Integration checklist

## New integration touch list

| Area | Files |
|------|--------|
| Hook asset | `src/integration/assets/<agent>/herdr-agent-state.sh` |
| Install/uninstall | `src/integration/mod.rs` |
| API enum | `src/api/schema.rs` — `IntegrationTarget` |
| CLI | `src/cli/integration.rs` |
| Resume plan | `src/agent_resume.rs` |
| Detection | `src/detect/mod.rs` (if needed) |
| Docs | `docs/next/website/src/content/docs/integrations.mdx`, `session-state.mdx` |
| E2E (optional) | `scripts/e2e-<agent>-integration.sh` |

## `install_<agent>` checklist

- [ ] Resolve config dir (env override + default home)
- [ ] **`command -v <exact-cli>`** — error before writing files if missing
- [ ] Write hook script from `include_str!` asset
- [ ] Merge hook into agent config (`hooks.json`, `config.toml`, settings, etc.) idempotently
- [ ] Return human-readable install messages

## Hook script checklist

- [ ] `HERDR_INTEGRATION_ID` / `HERDR_INTEGRATION_VERSION` comments
- [ ] Guard: `HERDR_ENV`, `HERDR_SOCKET_PATH`, `HERDR_PANE_ID`
- [ ] Parse stdin JSON; validate hook event name if agent sends it
- [ ] Extract session id (and path if supported)
- [ ] POST `pane.report_agent_session` to Unix socket
- [ ] Session-only: no `pane.release_agent`, no state payloads

## `agent_resume.rs` checklist

- [ ] `("herdr:<agent>", "<label>", Id|Path)` arm with **exact** CLI argv
- [ ] Test in `planner_builds_resume_argv_for_official_agents`
- [ ] Add to `is_official_agent_source` / reserved source lists if session-only

## Tests in `integration/mod.rs`

- [ ] Hook asset contains `pane.report_agent_session`, correct `source`
- [ ] Install writes hook + updates agent config
- [ ] Idempotent second install
- [ ] Config dir env override (e.g. `CURSOR_CONFIG_DIR`)
- [ ] Install errors when resume CLI missing from PATH
- [ ] Uninstall removes herdr hook entries, preserves unrelated hooks

## Manual sign-off (PR comment)

- [ ] `integration install <agent>` succeeds with CLI present
- [ ] Interactive agent in herdr pane → session id on `agent list`
- [ ] `session.json` persists `agent_session`
- [ ] After stop + reattach → resume uses correct argv
