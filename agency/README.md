# herdr agency

Define an **agency** of AI agents as markdown profiles + skills, then run it on
top of [herdr](../README.md). A single **orchestrator** (manager) agent knows the
whole roster, spins specialist agents into herdr panes per task, distributes
incoming requests, and integrates the results — like a company routing work to
consultants.

It's built entirely on herdr's existing primitives (`herdr agent start`,
`herdr pane run`, `herdr wait`, `herdr agent read`), so an agent is "just a
markdown profile + a model + skills," and the orchestrator is "just an agent with
the `agency-orchestrator` skill."

## Quick start

```bash
# 1. scaffold an agency in the current project
agency/bin/herdr-agency init

# 2. edit agents and the manifest
#    .herdr/agency/agency.toml
#    .herdr/agency/agents/*.md

# 3. check it compiles
agency/bin/herdr-agency validate
agency/bin/herdr-agency roster

# 4. preview routing for a request (offline, no server needed)
agency/bin/herdr-agency plan "add a login API and a login form"

# 5. inside herdr, spin up the agency with an auto-dispatcher pane
agency/bin/herdr-agency up --watch

# 6. forward requests to the running agency (also how external tools hand off)
agency/bin/herdr-agency submit "add a login API and a login form"
```

`init`, `validate`, `roster`, and `plan` run without a herdr server. `up`,
`submit`, `watch`, `status`, and `down` talk to a running herdr via the `herdr`
CLI. `mcp` runs an MCP server for external tools.

## Automatic distribution

Every request — from `submit`, the MCP server, or a file appended to the inbox —
lands in `inbox.jsonl`. A single cursor-guarded **dispatcher** forwards each new
request to the orchestrator pane exactly once, so distribution is automatic and
idempotent no matter who enqueued it.

`herdr-agency up --watch` starts the dispatcher in its own herdr pane. You can
also run it standalone:

```bash
agency/bin/herdr-agency watch          # poll the inbox and dispatch forever
agency/bin/herdr-agency watch --once   # drain pending once (handy in CI/cron)
```

## Defining agents

Each agent is a markdown file with frontmatter and a system-prompt body:

```markdown
---
name: backend
role: "Implements server-side features and APIs."
model: opus
complexity: high          # low | medium | high  (routing + cost)
command: claude           # any agent CLI; codex, etc.
args: ["--model", "opus"]
skills: [code-review]
tags: [backend, api, rust]
---
You are the backend engineer of the agency...
```

The user picks each agent's **model** and **complexity**, so the orchestrator can
route cheap tasks to cheap agents and escalate only when needed. See
[`SPEC.md`](./SPEC.md) for the full format.

## How external tools see and use the agency (MCP)

`herdr-agency mcp` runs an MCP (Model Context Protocol) stdio server, so agentic
frameworks like Claude Code and Codex can **discover the running agency and
forward requests to it** — like a company hiring an outside team. The server
exposes three tools:

- `submit_task(request)` — delegate a request to the agency.
- `agency_roster()` — see every agent's role, model, complexity, and tags.
- `agency_status()` — see the agency's runtime and agent statuses.

Register it with Claude Code (`.mcp.json` in your project):

```json
{
  "mcpServers": {
    "agency": {
      "command": "/abs/path/to/agency/bin/herdr-agency",
      "args": ["--dir", "/abs/path/to/.herdr/agency", "mcp"]
    }
  }
}
```

Codex registers MCP servers the same way (`command` + `args`). Once registered,
the framework lists the agency's tools and can call `submit_task` to hand off
work; the dispatcher routes it to the orchestrator automatically.

`herdr-agency submit "<request>"` remains the equivalent plain-CLI contract for
tools that prefer to shell out.

## Status

Working: profile/manifest format, the `herdr-agency` control tool, example
templates, the orchestrator skill, **automatic inbox-driven distribution**
(`watch` / `up --watch`), and an **MCP server** for external discovery and
forwarding. Roadmap (see `.local/prd/agency.md`): per-worker worktree isolation,
richer routing strategies, and promotion into a native `herdr agency` subcommand.
