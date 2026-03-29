# herdr — agent skill

you are running inside herdr, a terminal workspace manager. herdr gives you workspaces (tabs) and panes (splits) — each pane is a full terminal with its own shell or agent. you can control all of it from the CLI.

this means you can:
- see what other agents/panes are doing
- split panes and run commands in them
- start servers, watch logs, run tests — in separate panes
- wait for specific output before continuing
- wait for another agent to finish
- spawn new agent instances

the `herdr` binary is available in your PATH. all commands talk to the running herdr instance over a local unix socket.

## concepts

**workspaces** are like tabs. each workspace has one or more panes. you switch between workspaces in the sidebar.

**panes** are terminal splits inside a workspace. each pane runs its own process — a shell, an agent, a server, anything.

**agent state** is detected automatically by herdr. each pane can be:
- `idle` — agent finished, user has seen it
- `busy` — agent is working
- `waiting` — agent needs user input
- `unknown` — no recognized agent, or just a shell

**ids** — workspace ids look like `1`, `2`. pane ids look like `1-1`, `1-2`, `2-1`. these are the public ids you should use.

## discover yourself

find out which pane you're in and what else is running:

```bash
herdr pane list
```

```json
{"id":"cli:pane:list","result":{"panes":[
  {"agent":"claude","agent_state":"waiting","cwd":"/home/user/project","focused":true,"pane_id":"1-2","workspace_id":"1"},
  {"agent":"pi","agent_state":"idle","cwd":"/home/user/project","focused":false,"pane_id":"1-1","workspace_id":"1"}
],"type":"pane_list"}}
```

the focused pane is yours. other panes are your neighbors.

```bash
herdr workspace list
```

```json
{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[
  {"workspace_id":"1","number":1,"label":"project","focused":true,"pane_count":2,"agent_state":"waiting"}
]}}
```

## read another pane

see what's on another pane's screen:

```bash
herdr pane read 1-1 --source recent --lines 50
```

`--source visible` shows the current viewport. `--source recent` shows recent scrollback (default).

## split a pane and run a command

split your pane to the right and run a dev server:

```bash
herdr pane split 1-2 --direction right --no-focus
```

this returns the new pane's id. then run a command in it:

```bash
herdr pane run 1-3 "npm run dev"
```

`pane run` sends the text and presses Enter. use `--no-focus` on split to keep focus on your pane.

split downward instead:

```bash
herdr pane split 1-2 --direction down --no-focus
```

## wait for output

block until specific text appears in a pane. useful for waiting on servers, builds, tests:

```bash
herdr wait output 1-3 --match "ready on port 3000" --timeout 30000
```

with regex:

```bash
herdr wait output 1-3 --match "server.*ready" --regex --timeout 30000
```

timeout is in milliseconds. if it times out, exit code is 1.

## wait for an agent to finish

block until another agent reaches a specific state:

```bash
herdr wait agent-state 1-1 --state idle --timeout 60000
```

useful when you need another agent to finish before you proceed.

## send text or keys to a pane

send text without pressing Enter:

```bash
herdr pane send-text 1-1 "hello from claude"
```

press Enter (or other keys):

```bash
herdr pane send-keys 1-1 Enter
```

`pane run` combines both — sends text then Enter:

```bash
herdr pane run 1-1 "echo hello"
```

## workspace management

create a new workspace:

```bash
herdr workspace create --cwd /path/to/project
```

```bash
herdr workspace create --no-focus
```

focus a workspace:

```bash
herdr workspace focus 2
```

rename:

```bash
herdr workspace rename 1 "api server"
```

close:

```bash
herdr workspace close 2
```

## close a pane

```bash
herdr pane close 1-3
```

## recipes

### run a server and watch for ready

```bash
# split a pane for the server
herdr pane split 1-2 --direction right --no-focus
# assume new pane is 1-3 from the response

# start the server
herdr pane run 1-3 "npm run dev"

# wait until it's ready
herdr wait output 1-3 --match "ready" --timeout 30000

# now read the output to confirm
herdr pane read p_1_7 --source recent --lines 20
```

### run tests in a separate pane and check results

```bash
herdr pane split 1-2 --direction down --no-focus
herdr pane run 1-3 "cargo test"
herdr wait output 1-3 --match "test result" --timeout 60000
herdr pane read p_1_7 --source recent --lines 30
```

### check what another agent is working on

```bash
herdr pane list
herdr pane read 1-1 --source recent --lines 80
```

### spawn a new agent and give it a task

```bash
herdr pane split 1-2 --direction right --no-focus
herdr pane run 1-3 "claude"
# wait for it to start
herdr wait output 1-3 --match ">" --timeout 15000
# send it a task
herdr pane run 1-3 "review the test coverage in src/api/"
```

### coordinate with another agent

```bash
# wait for the other agent to finish its current work
herdr wait agent-state 1-1 --state idle --timeout 120000

# read what it produced
herdr pane read 1-1 --source recent --lines 100
```

## notes

- all commands return JSON. parse the response to get pane ids from splits, workspace ids from creates, etc.
- `pane run` = `pane send-text` + `pane send-keys Enter`. use the separate commands when you need finer control.
- pane ids from split responses are the source of truth — don't guess pane ids, read them from the response.
- `--no-focus` on split keeps your terminal focused. without it, herdr focuses the new pane.
- if you're running inside herdr, the `HERDR_ENV` environment variable is set to `1`. ids are compact and can change when panes/workspaces are closed.
