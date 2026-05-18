---
title: Agent skill
description: A concise guide for coding agents running inside Herdr.
---

This page teaches coding agents how to use Herdr while they are running inside a Herdr pane.

Use Herdr to organize parallel work instead of stacking everything in one terminal.

## Core model

Herdr has four levels:

| Level | Meaning |
| --- | --- |
| Workspace | A project or task context. |
| Tab | A view inside a workspace. |
| Pane | A terminal area inside a tab. |
| Agent | A detected coding agent running in a pane. |

Use workspace and tab actions for layout. Use pane actions for commands and output.

## Read the current layout

```bash
herdr pane list
```

Use the pane IDs returned by Herdr for later commands.

## Create a helper pane

Split a pane and run a command:

```bash
herdr pane split 1-1 --direction right
herdr pane run 1-2 "just test"
```

Prefer `pane run` over sending raw keys when you want to submit a full command line.

## Read output

```bash
herdr pane read 1-2 --source recent --lines 80
```

Use `recent-unwrapped` when reading logs or matching long lines.

```bash
herdr pane read 1-2 --source recent-unwrapped --lines 120
```

## Wait for agents

Wait for another agent to finish or become blocked:

```bash
herdr wait agent-status 1-3 --status done
herdr wait agent-status 1-3 --status blocked
```

Use agent waits for recognized coding agents. Use normal output matching for ordinary commands, servers, and tests.

## Preserve focus

Do not steal visible focus unless the user asked for it. Create panes, submit commands, and read output without changing the user’s active view when possible.

## Use clear pane aliases

When your tool supports aliases, use names like `server`, `tests`, `reviewer`, or `docs`. This makes later pane operations easier to audit.

## Good patterns

Run long work in a separate pane:

```bash
herdr pane split 1-1 --direction right
herdr pane run 1-2 "just check"
```

Watch a server until it is ready:

```bash
herdr pane read 1-2 --source recent-unwrapped --lines 80
```

Ask a reviewer agent to inspect a change:

```bash
herdr pane split 1-1 --direction right
herdr pane run 1-2 "pi --model <review-model>"
```

## Avoid these patterns

Do not pile every command into the current pane.

Do not use raw key injection for command submission when `pane run` is available.

Do not wait for `agent-status` on a normal process that is not a coding agent.

Do not change implementation direction without telling the user.
