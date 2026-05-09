# @herdr/sdk

TypeScript SDK for controlling a running Herdr instance over the same newline-delimited JSON Unix socket used by the CLI.

```ts
import { createHerdrClient } from "@herdr/sdk";

const herdr = createHerdrClient({ session: "work" });

const workspace = await herdr.workspace.create({
  cwd: "/path/to/project",
  label: "api",
  focus: true,
});

await herdr.pane.run(workspace.root_pane.pane_id, "npm run dev");

const match = await herdr.pane.waitForOutput({
  pane_id: workspace.root_pane.pane_id,
  source: "recent",
  match: { type: "substring", value: "ready" },
  timeout_ms: 30_000,
});

console.log(match.matched_line);
```

## Connection

Herdr exposes a local Unix domain socket. The SDK resolves the socket path in this order:

1. `socketPath` option
2. `session` option, mapped to `$XDG_CONFIG_HOME/herdr/sessions/<name>/herdr.sock` or `$HOME/.config/herdr/sessions/<name>/herdr.sock`
3. `HERDR_SOCKET_PATH`
4. `HERDR_SESSION`, using the same named-session path
5. default path at `$XDG_CONFIG_HOME/herdr/herdr.sock` or `$HOME/.config/herdr/herdr.sock`

```ts
const defaultClient = createHerdrClient();
const namedSession = createHerdrClient({ session: "work" });
const explicitSocket = createHerdrClient({ socketPath: "/tmp/herdr.sock" });
```

## Raw Requests

Use `request()` when you want the exact socket method. Method names, params, and result types are linked at compile time.

```ts
const read = await herdr.request("pane.read", {
  pane_id: "1-1",
  source: "recent",
  lines: 80,
});

read.type; // "pane_read"
read.read.text;
```

API errors reject with `HerdrApiError` and expose the server error code and message.

## Convenience Helpers

The helper groups mirror Herdr concepts:

- `herdr.server.stop()` and `herdr.server.reloadConfig()`
- `herdr.workspace.list|get|create|focus|rename|close`
- `herdr.tab.list|get|create|focus|rename|close`
- `herdr.pane.list|get|read|split|sendText|sendKeys|sendInput|run|waitForOutput|waitForAgentStatus|reportAgent|clearAgentAuthority|releaseAgent|close`
- `herdr.events.subscribe|wait`
- `herdr.integration.install|uninstall`

`pane.run()` is an SDK convenience matching the CLI wrapper: it sends `pane.send_input` with the command text and `keys: ["Enter"]`.

## Subscriptions

`events.subscribe` keeps a socket open after the ack and yields pushed events as an async iterator.

```ts
const subscription = await herdr.events.subscribe({
  subscriptions: [
    {
      type: "pane.agent_status_changed",
      pane_id: "1-1",
      agent_status: "done",
    },
  ],
});

try {
  for await (const event of subscription) {
    console.log(event);
    break;
  }
} finally {
  subscription.close();
}
```

For the common CLI-style wait, use:

```ts
const event = await herdr.pane.waitForAgentStatus("1-1", "done", {
  timeoutMs: 60_000,
});
```
