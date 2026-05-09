import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:net";
import test from "node:test";

import {
  HerdrApiError,
  HerdrTimeoutError,
  createHerdrClient,
  resolveHerdrSocketPath,
} from "../dist/index.js";

test("resolves default, environment, and named session socket paths", () => {
  assert.equal(
    resolveHerdrSocketPath({
      env: { XDG_CONFIG_HOME: "/config", HOME: "/home/can" },
    }),
    "/config/herdr/herdr.sock",
  );
  assert.equal(
    resolveHerdrSocketPath({
      env: { HERDR_SOCKET_PATH: "/tmp/herdr.sock", HOME: "/home/can" },
    }),
    "/tmp/herdr.sock",
  );
  assert.equal(
    resolveHerdrSocketPath({
      session: "work",
      env: { HERDR_SOCKET_PATH: "/tmp/ignored.sock", XDG_CONFIG_HOME: "/config" },
    }),
    "/config/herdr/sessions/work/herdr.sock",
  );
  assert.equal(
    resolveHerdrSocketPath({
      env: { HERDR_SESSION: "work", HOME: "/home/can" },
    }),
    "/home/can/.config/herdr/sessions/work/herdr.sock",
  );
});

test("sends newline-delimited requests and returns typed results", async () => {
  await withSocketServer(async ({ socketPath, received, respondOnce }) => {
    respondOnce({
      id: "test:ping",
      result: { type: "pong", version: "0.5.6", protocol: 4 },
    });

    const client = createHerdrClient({ socketPath });
    const result = await client.request("ping", {}, { id: "test:ping" });

    assert.deepEqual(result, { type: "pong", version: "0.5.6", protocol: 4 });
    assert.deepEqual(await received, {
      id: "test:ping",
      method: "ping",
      params: {},
    });
  });
});

test("pane.run maps to pane.send_input with Enter", async () => {
  await withSocketServer(async ({ socketPath, received, respondOnce }) => {
    respondOnce({
      id: "run",
      result: { type: "ok" },
    });

    const client = createHerdrClient({ socketPath });
    await client.pane.run("1-1", "npm run dev", { id: "run" });

    assert.deepEqual(await received, {
      id: "run",
      method: "pane.send_input",
      params: {
        pane_id: "1-1",
        text: "npm run dev",
        keys: ["Enter"],
      },
    });
  });
});

test("rejects API errors with server code", async () => {
  await withSocketServer(async ({ socketPath, respondOnce }) => {
    respondOnce({
      id: "bad-pane",
      error: {
        code: "pane_not_found",
        message: "pane 1-9 not found",
      },
    });

    const client = createHerdrClient({ socketPath });
    await assert.rejects(
      () => client.pane.get("1-9", { id: "bad-pane" }),
      (error) => {
        assert.ok(error instanceof HerdrApiError);
        assert.equal(error.code, "pane_not_found");
        assert.equal(error.response.error.message, "pane 1-9 not found");
        return true;
      },
    );
  });
});

test("streams subscription events after the subscription ack", async () => {
  await withSocketServer(async ({ socketPath, received, respondOnce }) => {
    respondOnce(
      {
        id: "sub",
        result: { type: "subscription_started" },
      },
      {
        event: "pane.agent_status_changed",
        data: {
          pane_id: "1-1",
          workspace_id: "w1",
          agent_status: "done",
          agent: "codex",
        },
      },
    );

    const client = createHerdrClient({ socketPath });
    const subscription = await client.events.subscribe(
      {
        subscriptions: [
          {
            type: "pane.agent_status_changed",
            pane_id: "1-1",
            agent_status: "done",
          },
        ],
      },
      { id: "sub" },
    );

    assert.deepEqual(await received, {
      id: "sub",
      method: "events.subscribe",
      params: {
        subscriptions: [
          {
            type: "pane.agent_status_changed",
            pane_id: "1-1",
            agent_status: "done",
          },
        ],
      },
    });

    assert.deepEqual(subscription.ack.result, { type: "subscription_started" });
    assert.deepEqual(await subscription.nextEvent(), {
      event: "pane.agent_status_changed",
      data: {
        pane_id: "1-1",
        workspace_id: "w1",
        agent_status: "done",
        agent: "codex",
      },
    });
    subscription.close();
  });
});

test("times out while waiting for a response when requested", async () => {
  await withSocketServer(async ({ socketPath }) => {
    const client = createHerdrClient({ socketPath });
    await assert.rejects(
      () => client.request("ping", {}, { id: "slow", timeoutMs: 10 }),
      (error) => {
        assert.ok(error instanceof HerdrTimeoutError);
        return true;
      },
    );
  });
});

async function withSocketServer(run) {
  const dir = await mkdtemp(join(tmpdir(), "herdr-sdk-"));
  const socketPath = join(dir, "herdr.sock");
  const responses = [];
  let receivedResolve;
  const received = new Promise((resolve) => {
    receivedResolve = resolve;
  });

  const server = createServer((socket) => {
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      buffer += chunk;
      const newline = buffer.indexOf("\n");
      if (newline === -1) {
        return;
      }

      const line = buffer.slice(0, newline);
      receivedResolve(JSON.parse(line));

      const response = responses.shift();
      if (!response) {
        return;
      }

      for (const message of response) {
        socket.write(`${JSON.stringify(message)}\n`);
      }
    });
  });

  server.listen(socketPath);
  await once(server, "listening");

  try {
    await run({
      socketPath,
      received,
      respondOnce: (...messages) => responses.push(messages),
    });
  } finally {
    server.close();
    await once(server, "close");
    await rm(dir, { force: true, recursive: true });
  }
}
