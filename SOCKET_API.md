# herdr socket api

herdr exposes a local unix socket api for scripts, tools, and coding agents that want to control a running herdr instance or subscribe to events.

if you are teaching an agent that is already running inside herdr, start with [`SKILL.md`](./SKILL.md). use this document when you want the direct protocol, or when you want the cli wrapper reference for the commands that sit on top of it.

## choose your integration layer

there are three practical ways to integrate with herdr:

- **agent skill** — [`SKILL.md`](./SKILL.md). best when an agent inside herdr just needs to learn the workflow quickly.
- **cli wrappers** — `herdr workspace ...`, `herdr tab ...`, `herdr pane ...`, `herdr wait ...`. best for shell scripts and simple orchestration.
- **raw socket api** — best when you want direct request/response control or long-lived event subscriptions.

these layers are intentionally stacked on top of the same control surface.

important difference: `pane.run` and `wait agent-state` are **cli conveniences**, not raw socket methods.

## transport

- transport: unix domain socket
- encoding: newline-delimited json
- request/response: send one json request per line, read one json response per line
- subscriptions: send `events.subscribe`, receive an ack, then keep the same connection open and continue reading pushed events

socket path resolution order:

1. `HERDR_SOCKET_PATH`
2. `$XDG_RUNTIME_DIR/herdr.sock`
3. `$XDG_CONFIG_HOME/herdr/herdr.sock`
4. `$HOME/.config/herdr/herdr.sock`
5. `/tmp/herdr.sock`

## request and response envelopes

all socket requests use this envelope:

```json
{
  "id": "req_1",
  "method": "ping",
  "params": {}
}
```

successful responses look like:

```json
{
  "id": "req_1",
  "result": {
    "type": "pong",
    "version": "0.1.2"
  }
}
```

errors look like:

```json
{
  "id": "req_1",
  "error": {
    "code": "pane_not_found",
    "message": "pane 1-99 not found"
  }
}
```

## ids and numbering

workspace ids are opaque, stable ids like:

- `w64e95948145ed1`
- `w64e95948146a82`

pane ids are workspace-scoped and stable across workspace reorder:

- `w64e95948145ed1-1`
- `w64e95948145ed1-2`
- `w64e95948146a82-1`

that means:

- workspace id = stable workspace identity
- pane number = compact pane number within that workspace

workspace ids are durable for the life of the workspace and survive display reordering. pane numbers are still compact public numbers, so if a pane closes, higher pane numbers in that same workspace compact down.

tabs are first-class socket api objects now.

- tab ids look like `w64e95948145ed1:1`, `w64e95948145ed1:2`
- workspace id = stable workspace identity
- tab number = tab number within that workspace
- pane ids still stay workspace-scoped like `w64e95948145ed1-2` rather than becoming `workspace-tab-pane` triples

for backward compatibility, requests also accept the older positional forms like `1`, `1:2`, and `1-2` as shorthand for the current session order. responses use the stable ids.

## core objects

`workspace_info` responses contain objects like:

```json
{
  "workspace_id": "w64e95948145ed1",
  "number": 1,
  "label": "herdr",
  "focused": true,
  "pane_count": 1,
  "tab_count": 1,
  "active_tab_id": "w64e95948145ed1:1",
  "agent_state": "unknown"
}
```

`tab_info` responses contain objects like:

```json
{
  "tab_id": "w64e95948145ed1:1",
  "workspace_id": "w64e95948145ed1",
  "number": 1,
  "label": "1",
  "focused": true,
  "pane_count": 1,
  "agent_state": "unknown"
}
```

`pane_info` responses contain objects like:

```json
{
  "pane_id": "w64e95948145ed1-1",
  "workspace_id": "w64e95948145ed1",
  "tab_id": "w64e95948145ed1:1",
  "focused": true,
  "cwd": "/home/can/Projects/herdr",
  "agent": "pi",
  "agent_state": "working",
  "revision": 0
}
```

`pane_read` responses contain objects like:

```json
{
  "pane_id": "w64e95948145ed1-1",
  "workspace_id": "w64e95948145ed1",
  "tab_id": "w64e95948145ed1:1",
  "source": "recent",
  "text": "...",
  "revision": 0,
  "truncated": false
}
```

agent states are:

- `idle`
- `working`
- `blocked`
- `unknown`

## methods at a glance

| method | purpose | success result type |
|---|---|---|
| `ping` | health check / version | `pong` |
| `workspace.list` | list workspaces | `workspace_list` |
| `workspace.get` | inspect one workspace | `workspace_info` |
| `workspace.create` | create a workspace | `workspace_info` |
| `workspace.focus` | focus a workspace | `workspace_info` |
| `workspace.rename` | rename a workspace | `workspace_info` |
| `workspace.close` | close a workspace | `ok` |
| `tab.list` | list tabs, optionally filtered by workspace | `tab_list` |
| `tab.get` | inspect one tab | `tab_info` |
| `tab.create` | create a tab in a workspace | `tab_info` |
| `tab.focus` | focus a tab | `tab_info` |
| `tab.rename` | rename a tab | `tab_info` |
| `tab.close` | close a tab | `ok` |
| `pane.list` | list panes, optionally filtered by workspace | `pane_list` |
| `pane.get` | inspect one pane | `pane_info` |
| `pane.read` | read pane output | `pane_read` |
| `pane.split` | split a pane and create a sibling pane | `pane_info` |
| `pane.send_text` | send literal text without Enter | `ok` |
| `pane.send_keys` | send keypresses like `Enter` | `ok` |
| `pane.close` | close a pane | `ok` |
| `pane.wait_for_output` | one-shot blocking wait for text | `output_matched` |
| `events.subscribe` | start a long-lived subscription stream | `subscription_started` ack |

## workspace methods

### `workspace.list`

request:

```json
{
  "id": "req_list",
  "method": "workspace.list",
  "params": {}
}
```

returns `workspace_list` with zero or more workspace objects.

### `workspace.get`

params:

```json
{
  "workspace_id": "1"
}
```

returns `workspace_info` for one workspace.

### `workspace.create`

params:

```json
{
  "cwd": "/home/can/Projects/herdr",
  "focus": true
}
```

notes:

- `cwd` is optional
- if `cwd` is omitted, herdr uses its current working directory and falls back to `/` if needed
- `focus` is optional in raw socket requests and defaults to `false`
- the cli wrapper is more ergonomic here: `herdr workspace create` focuses by default unless you pass `--no-focus`

example response:

```json
{
  "id": "req_create",
  "result": {
    "type": "workspace_info",
    "workspace": {
      "workspace_id": "1",
      "number": 1,
      "label": "herdr",
      "focused": true,
      "pane_count": 1,
      "tab_count": 1,
      "active_tab_id": "1:1",
      "agent_state": "unknown"
    }
  }
}
```

### `workspace.focus`

params:

```json
{
  "workspace_id": "1"
}
```

returns the focused workspace as `workspace_info`.

### `workspace.rename`

params:

```json
{
  "workspace_id": "1",
  "label": "api"
}
```

returns updated `workspace_info`.

### `workspace.close`

params:

```json
{
  "workspace_id": "1"
}
```

returns:

```json
{
  "id": "req_close",
  "result": {
    "type": "ok"
  }
}
```

## tab methods

### `tab.list`

request with no filter:

```json
{
  "id": "req_tabs",
  "method": "tab.list",
  "params": {}
}
```

request filtered to one workspace:

```json
{
  "id": "req_tabs_ws",
  "method": "tab.list",
  "params": {
    "workspace_id": "1"
  }
}
```

returns `tab_list`.

### `tab.get`

params:

```json
{
  "tab_id": "1:2"
}
```

returns `tab_info`.

### `tab.create`

params:

```json
{
  "workspace_id": "1",
  "cwd": "/home/can/Projects/herdr",
  "focus": true
}
```

notes:

- `workspace_id` is optional and defaults to the active workspace
- `cwd` is optional; if omitted, herdr uses the focused pane cwd in that workspace when available
- `focus` is optional in raw socket requests and defaults to `false`
- the cli wrapper focuses by default unless you pass `--no-focus`

returns `tab_info` for the new tab.

### `tab.focus`

params:

```json
{
  "tab_id": "1:2"
}
```

returns focused `tab_info`.

### `tab.rename`

params:

```json
{
  "tab_id": "1:2",
  "label": "logs"
}
```

returns updated `tab_info`.

### `tab.close`

params:

```json
{
  "tab_id": "1:2"
}
```

returns `ok`. the last tab in a workspace cannot be closed.

## pane methods

### `pane.list`

request with no filter:

```json
{
  "id": "req_panes",
  "method": "pane.list",
  "params": {}
}
```

request filtered to one workspace:

```json
{
  "id": "req_panes_ws",
  "method": "pane.list",
  "params": {
    "workspace_id": "1"
  }
}
```

returns `pane_list`.

### `pane.get`

params:

```json
{
  "pane_id": "1-1"
}
```

returns `pane_info`.

### `pane.read`

params:

```json
{
  "pane_id": "1-1",
  "source": "recent",
  "lines": 80,
  "strip_ansi": true
}
```

notes:

- `source` is required and must be `visible` or `recent`
- `lines` is optional
- current implementation defaults to `80` lines when `lines` is omitted and caps reads at `1000`
- `strip_ansi` defaults to `true`

`source` meanings:

- `visible` — current viewport
- `recent` — recent scrollback text

example response:

```json
{
  "id": "req_read",
  "result": {
    "type": "pane_read",
    "read": {
      "pane_id": "1-1",
      "workspace_id": "1",
      "tab_id": "1:1",
      "source": "recent",
      "text": "...",
      "revision": 0,
      "truncated": false
    }
  }
}
```

### `pane.split`

params:

```json
{
  "target_pane_id": "1-1",
  "direction": "right",
  "focus": true
}
```

notes:

- `direction` must be `right` or `down`
- `cwd` is optional
- `focus` is optional in raw socket requests and defaults to `false`
- the cli wrapper is more ergonomic here too: `herdr pane split ...` focuses by default unless you pass `--no-focus`

returns `pane_info` for the new pane.

### `pane.send_text`

params:

```json
{
  "pane_id": "1-1",
  "text": "bun run dev"
}
```

this sends literal text only. it does **not** press Enter.

### `pane.send_keys`

params:

```json
{
  "pane_id": "1-1",
  "keys": ["Enter"]
}
```

use this after `pane.send_text` when you want to submit a command.

### `pane.close`

params:

```json
{
  "pane_id": "1-2"
}
```

returns `ok`.

## waits

### `pane.wait_for_output`

this is the direct socket-side one-shot blocking wait.

params:

```json
{
  "pane_id": "1-1",
  "source": "recent",
  "lines": 200,
  "match": { "type": "substring", "value": "ready" },
  "timeout_ms": 30000,
  "strip_ansi": true
}
```

matcher forms:

```json
{ "type": "substring", "value": "ready" }
```

```json
{ "type": "regex", "value": "server.*ready" }
```

notes:

- `source` must be `visible`, `recent`, or `recent_unwrapped`
- `lines` is optional
- `timeout_ms` is optional
- `strip_ansi` defaults to `true`
- for `source = "recent"`, output matching uses unwrapped recent terminal text so soft wraps do not break matches
- `source = "recent_unwrapped"` is also available on `pane.read` when you want to inspect the same unwrapped transcript directly
- on success you get `output_matched`
- on timeout you get an error response with code `timeout`

example success response:

```json
{
  "id": "req_wait",
  "result": {
    "type": "output_matched",
    "pane_id": "1-1",
    "revision": 0,
    "matched_line": "server ready",
    "read": {
      "pane_id": "1-1",
      "workspace_id": "1",
      "tab_id": "1:1",
      "source": "recent_unwrapped",
      "text": "...server ready...",
      "revision": 0,
      "truncated": false
    }
  }
}
```

## subscriptions

`events.subscribe` is the long-lived pubsub entrypoint.

you send a subscribe request once, get an ack on the same connection, and then keep reading newline-delimited json events from that same socket.

### subscription ack

```json
{
  "id": "sub_1",
  "result": {
    "type": "subscription_started"
  }
}
```

### supported subscriptions

base lifecycle subscriptions:

- `workspace.created`
- `workspace.closed`
- `workspace.focused`
- `tab.created`
- `tab.closed`
- `tab.focused`
- `tab.renamed`
- `pane.created`
- `pane.closed`
- `pane.focused`
- `pane.exited`
- `pane.agent_detected`

parameterized subscriptions:

- `pane.output_matched`
- `pane.agent_state_changed`

### event naming rule

this part matters because the pushed event names are **not all shaped the same**.

- when you subscribe to a **base lifecycle event**, the pushed `event` value uses snake_case with underscores:
  - subscribe with `workspace.created`
  - receive `workspace_created`
- when you subscribe to a **parameterized subscription**, the pushed `event` value keeps the dotted name:
  - subscribe with `pane.output_matched`
  - receive `pane.output_matched`

examples below show both forms.

### example: subscribe to lifecycle events

request:

```json
{
  "id": "sub_life",
  "method": "events.subscribe",
  "params": {
    "subscriptions": [
      { "type": "workspace.created" },
      { "type": "workspace.focused" },
      { "type": "tab.created" },
      { "type": "tab.focused" },
      { "type": "tab.renamed" },
      { "type": "tab.closed" },
      { "type": "pane.created" },
      { "type": "pane.focused" },
      { "type": "pane.agent_detected" },
      { "type": "pane.closed" },
      { "type": "workspace.closed" }
    ]
  }
}
```

example pushed event:

```json
{
  "event": "workspace_created",
  "data": {
    "workspace": {
      "workspace_id": "1",
      "number": 1,
      "label": "herdr",
      "focused": true,
      "pane_count": 1,
      "tab_count": 1,
      "active_tab_id": "1:1",
      "agent_state": "unknown"
    }
  }
}
```

### example: subscribe to output matches and agent state changes

request:

```json
{
  "id": "sub_1",
  "method": "events.subscribe",
  "params": {
    "subscriptions": [
      {
        "type": "pane.output_matched",
        "pane_id": "1-1",
        "source": "recent",
        "lines": 200,
        "match": { "type": "substring", "value": "ready" }
      },
      {
        "type": "pane.agent_state_changed",
        "pane_id": "1-1",
        "state": "idle"
      }
    ]
  }
}
```

notes:

- `pane.output_matched` supports `source`, optional `lines`, matcher config, and optional `strip_ansi`
- `pane.agent_state_changed` accepts an optional `state` filter; if omitted, any state transition for that pane can match

example pushed `pane.output_matched` event:

```json
{
  "event": "pane.output_matched",
  "data": {
    "pane_id": "1-1",
    "matched_line": "server ready",
    "read": {
      "pane_id": "1-1",
      "workspace_id": "1",
      "tab_id": "1:1",
      "source": "recent_unwrapped",
      "text": "...server ready...",
      "revision": 0,
      "truncated": false
    }
  }
}
```

example pushed `pane.agent_state_changed` event:

```json
{
  "event": "pane.agent_state_changed",
  "data": {
    "pane_id": "1-1",
    "workspace_id": "1",
    "state": "idle",
    "agent": "pi"
  }
}
```

## cli wrappers

these commands talk to the same local socket surface and are usually the easiest starting point for shell scripts and coding agents.

### command groups

workspace commands:

```text
herdr workspace list
herdr workspace create [--cwd PATH] [--no-focus]
herdr workspace get <workspace_id>
herdr workspace focus <workspace_id>
herdr workspace rename <workspace_id> <label>
herdr workspace close <workspace_id>
```

tab commands:

```text
herdr tab list [--workspace <workspace_id>]
herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--no-focus]
herdr tab get <tab_id>
herdr tab focus <tab_id>
herdr tab rename <tab_id> <label>
herdr tab close <tab_id>
```

pane commands:

```text
herdr pane list [--workspace <workspace_id>]
herdr pane get <pane_id>
herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--raw]
herdr pane split <pane_id> --direction right|down [--cwd PATH] [--no-focus]
herdr pane close <pane_id>
herdr pane send-text <pane_id> <text>
herdr pane send-keys <pane_id> <key> [key ...]
herdr pane run <pane_id> <command>
```

wait commands:

```text
herdr wait output <pane_id> --match <text> [--source visible|recent|recent-unwrapped] [--lines N] [--timeout MS] [--regex] [--raw]
herdr wait agent-state <pane_id> --state <idle|working|blocked|unknown> [--timeout MS]
```

### cli behavior notes

- `workspace create` focuses by default; pass `--no-focus` to keep focus where it is
- `tab create` focuses by default; pass `--no-focus` to keep focus where it is
- `pane split` focuses the new pane by default; pass `--no-focus` to keep focus on the original pane
- `pane read` prints **text**, not json
- `pane read --source recent-unwrapped` returns recent terminal text with soft wraps joined back together
- `pane send-text`, `pane send-keys`, and `pane run` print nothing on success
- list/get/create/split/wait commands print json on success
- `pane run` is a convenience wrapper for `pane send-text` with a trailing carriage return (`\r`)
- `wait agent-state` is a cli convenience built on top of event subscriptions
- `--raw` disables ansi stripping for `pane read` and `wait output`
- `wait output --source recent` matches against unwrapped recent terminal text by default, so pane width and soft wrapping do not break matches

### cli examples

create a workspace, split a pane, run a server, and wait for readiness:

```bash
herdr workspace create --cwd /path/to/project
herdr pane split 1-1 --direction right --no-focus
herdr pane run 1-2 "npm run dev"
herdr wait output 1-2 --match "ready" --timeout 30000
```

wait for another agent to finish:

```bash
herdr wait agent-state 1-1 --state idle --timeout 60000
```

inspect another pane's output:

```bash
herdr pane read 1-1 --source recent --lines 80
```

## behavior notes and gotchas

- `pane.send_text` sends literal text only. if you want to execute a command, follow it with `pane.send_keys` and `Enter`, or use cli `pane run`, which appends a trailing carriage return (`\r`).
- `pane.read` and `pane.wait_for_output` strip ansi by default.
- `pane.output_matched` subscriptions fire on transitions into a matching state; they do not repeatedly spam the same still-visible match on every poll.
- closing the socket connection ends the subscription.
- there is no separate event transport.
- the same herdr process can serve regular request/response calls and long-lived subscription connections at the same time.
