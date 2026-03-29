# herdr socket api

herdr exposes a local unix socket API for scripts, tools, and coding agents that want to control a running herdr instance or subscribe to pane/workspace events.

this is the low-level integration surface.
a CLI wrapper on top of it is planned, but the socket API is the foundation.

## transport

- transport: unix domain socket
- encoding: newline-delimited JSON
- request/response: one JSON request per line, one JSON response per line
- subscriptions: send `events.subscribe`, receive an ack, then keep the same connection open for pushed events

socket path resolution:

1. `HERDR_SOCKET_PATH`
2. `$XDG_RUNTIME_DIR/herdr.sock`
3. `$XDG_CONFIG_HOME/herdr/herdr.sock`
4. `$HOME/.config/herdr/herdr.sock`
5. `/tmp/herdr.sock`

## request shape

all requests use this envelope:

```json
{
  "id": "req_1",
  "method": "ping",
  "params": {}
}
```

success responses:

```json
{
  "id": "req_1",
  "result": {
    "type": "pong",
    "version": "0.1.2"
  }
}
```

error responses:

```json
{
  "id": "req_1",
  "error": {
    "code": "pane_not_found",
    "message": "pane 1-99 not found"
  }
}
```

## ids

workspace ids look like:

- `1`
- `2`

pane ids look like:

- `1-1`
- `1-2`
- `2-1`

that means:
- first number = current workspace number
- second number = current pane number within that workspace

these are compact public ids for the current live session. if a workspace or pane is closed, numbering compacts.

## core request methods

currently useful methods include:

### basic
- `ping`

### workspace
- `workspace.list`
- `workspace.get`
- `workspace.create`
- `workspace.focus`
- `workspace.rename`
- `workspace.close`

### pane
- `pane.list`
- `pane.get`
- `pane.read`
- `pane.send_text`
- `pane.send_keys`
- `pane.split`
- `pane.close`

### waits / events
- `pane.wait_for_output`
- `events.subscribe`

## example: create a workspace

```json
{
  "id": "req_create",
  "method": "workspace.create",
  "params": {
    "cwd": "/home/can/Projects/herdr",
    "focus": true
  }
}
```

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
      "agent_state": "unknown"
    }
  }
}
```

## example: read pane output

```json
{
  "id": "req_read",
  "method": "pane.read",
  "params": {
    "pane_id": "1-1",
    "source": "recent",
    "lines": 80
  }
}
```

`source` can be:
- `visible`
- `recent`

## example: send text and press enter

low-level input is intentionally explicit:

```json
{
  "id": "req_send_text",
  "method": "pane.send_text",
  "params": {
    "pane_id": "1-1",
    "text": "bun run dev"
  }
}
```

then:

```json
{
  "id": "req_send_keys",
  "method": "pane.send_keys",
  "params": {
    "pane_id": "1-1",
    "keys": ["Enter"]
  }
}
```

this is kept separate on purpose. sending text is not always the same thing as submitting it.

a future CLI wrapper will likely offer a more ergonomic `pane run` style command on top of this.

## example: one-shot wait for output

```json
{
  "id": "req_wait",
  "method": "pane.wait_for_output",
  "params": {
    "pane_id": "1-1",
    "source": "recent",
    "lines": 200,
    "match": { "type": "substring", "value": "ready" },
    "timeout_ms": 30000
  }
}
```

regex matching is also supported:

```json
{
  "type": "regex",
  "value": "server.*ready"
}
```

## subscriptions

`events.subscribe` is the long-lived pubsub entrypoint.

you send a subscribe request once, get an ack on the same connection, and then keep reading newline-delimited JSON events from that same socket.

### subscription ack

```json
{
  "id": "sub_1",
  "result": {
    "type": "subscription_started"
  }
}
```

## supported subscriptions

### lifecycle / base events
- `workspace.created`
- `workspace.closed`
- `workspace.focused`
- `pane.created`
- `pane.closed`
- `pane.focused`
- `pane.exited`
- `pane.agent_detected`
- `pane.agent_state_changed`

### parameterized event
- `pane.output_matched`

## example: subscribe to lifecycle events

```json
{
  "id": "sub_life",
  "method": "events.subscribe",
  "params": {
    "subscriptions": [
      { "type": "workspace.created" },
      { "type": "workspace.focused" },
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
      "agent_state": "unknown"
    }
  }
}
```

## example: subscribe to output matches and agent state changes

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
      "source": "recent",
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

## behavior notes

- `pane.output_matched` emits when a subscription transitions into a matching state. it does not repeatedly spam the same visible match on every poll.
- closing the socket connection ends the subscription.
- there is no separate transport for events.
- the same herdr process can serve regular request/response calls and long-lived subscription connections at the same time.

## intended layering

recommended architecture:
- socket api = foundational integration protocol
- `herdr ...` commands = ergonomic wrapper for humans and coding agents

current wrapper commands include:
- `herdr workspace list`
- `herdr workspace create ...`
- `herdr workspace get ...`
- `herdr workspace focus ...`
- `herdr workspace rename ...`
- `herdr workspace close ...`
- `herdr pane list ...`
- `herdr pane get ...`
- `herdr pane read ...`
- `herdr pane split ...`
- `herdr pane close ...`
- `herdr pane send-text ...`
- `herdr pane send-keys ...`
- `herdr pane run ...`
- `herdr wait output ...`
- `herdr wait agent-state ...`

those commands sit on top of this socket surface rather than replacing it.

for convenience, the CLI accepts pane ids in either raw socket form (`1-1`) or short human form (`1-1`).
