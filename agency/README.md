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

# 5. inside herdr, spin up the agency
agency/bin/herdr-agency up

# 6. forward requests to the running agency (also how external tools hand off)
agency/bin/herdr-agency submit "add a login API and a login form"
```

`init`, `validate`, `roster`, and `plan` run without a herdr server. `up`,
`submit`, `status`, and `down` talk to a running herdr via the `herdr` CLI.

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

## How external tools hand off work

`herdr-agency submit "<request>"` is the stable contract. Claude Code, Codex, or
any tool can shell out to it (or, later, call an MCP `submit_task` tool) to hand a
request to the running agency. The orchestrator picks it up from the inbox and
distributes it — the agency behaves like an always-on team you can delegate to.

## Status

This is the foundation slice: profile/manifest format, the `herdr-agency` control
tool, example templates, and the orchestrator skill. Roadmap (see
`.local/prd/agency.md`): inbox-driven auto-distribution, an MCP server for
external forwarding, and promotion into a native `herdr agency` subcommand.
