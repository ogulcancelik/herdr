---
name: agency-orchestrator
description: "Run a herdr-powered agency. Read the roster, spin worker agents into panes per task, hand them work, wait for results, and integrate. Use when you are the orchestrator/manager agent of an agency (HERDR_ENV=1)."
---

# agency-orchestrator

You are the orchestrator of a herdr agency. Your job is to turn incoming
requests into work distributed across specialist agents, each running in its own
herdr pane, then integrate their results into one answer.

Confirm `HERDR_ENV=1` before using herdr commands. If it is not set, you are not
inside a herdr pane — stop and say so.

## 1. Know your roster

Your roster is compiled to `roster.json` in the agency directory. Read it:

```bash
cat .herdr/agency/roster.json
```

Each agent has: `name`, `role`, `complexity` (low/medium/high), `argv` (the
exact command herdr should launch, e.g. `["claude","--model","sonnet"]`),
`skills`, and `tags`. Route by matching the task to `tags`/`role`, and prefer the
lowest-complexity agent that can do the job (cost discipline). Escalate to a
high-complexity agent only when the task needs it.

## 2. Receive requests

Requests arrive two ways:

- A human or external tool runs `herdr-agency submit "<request>"`, which appends
  a JSON line to `.herdr/agency/inbox.jsonl` and nudges this pane.
- You are asked directly in this pane.

Drain new inbox lines you have not handled yet:

```bash
tail -n +1 .herdr/agency/inbox.jsonl
```

## 3. Spin an agent into a pane

Split a pane and launch the agent's argv. Keep focus on yourself with
`--no-focus` so you stay in control:

```bash
NEW_PANE=$(herdr pane split --current --direction right --no-focus \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["pane"]["pane_id"])')
herdr pane run "$NEW_PANE" "claude --model sonnet"
herdr wait output "$NEW_PANE" --match ">" --timeout 20000
```

You can also use the higher-level launcher, which creates the pane and starts the
agent in one step:

```bash
herdr agent start backend --split right --no-focus -- claude --model opus
```

Give each worker a crisp brief, including its role from the roster and exactly
what you need back:

```bash
herdr pane run "$NEW_PANE" "You are the backend engineer. Implement X. Report a summary when done."
```

## 4. Run tasks in parallel

Independent tasks should run at the same time — spin each agent into its own pane
before waiting on any of them. Track the pane id for each.

## 5. Wait and collect

Wait for each worker to finish, then read its output:

```bash
herdr wait agent-status "$NEW_PANE" --status idle --timeout 600000
herdr agent read "$NEW_PANE" --source recent --lines 120
```

`idle`/`done` mean the agent stopped working. If an agent goes `blocked`, it
needs input — read the pane, decide, and `herdr pane run` the answer, or escalate
to the human.

## 6. Integrate and report

Combine the workers' results into a single answer. Resolve conflicts. Tell the
user who did what. Close panes you no longer need with `herdr pane close <id>`.

## Routing heuristics

- Match request keywords to agent `tags` first, then `role`.
- Prefer `complexity: low` → `medium` → `high`; only escalate when warranted.
- Batch independent work across agents; serialize only true dependencies.
- One agent per pane. Re-read ids from `pane list` — they compact when panes close.

## Cost and model choice

Each agent's `argv` already encodes its model. Honor it. When a task is simple,
route to a low-complexity (cheaper model) agent even if a high-complexity agent
could also do it.
