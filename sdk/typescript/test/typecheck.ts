import { createHerdrClient, type HerdrApiError, type OutputMatchedResult } from "../src/index.js";

const herdr = createHerdrClient({ socketPath: "/tmp/herdr.sock" });

async function compileTypedCalls() {
  const pong = await herdr.request("ping", {});
  pong.protocol satisfies number;

  const read = await herdr.request("pane.read", {
    pane_id: "1-1",
    source: "recent",
    lines: 80,
  });
  read.read.text satisfies string;

  const output = await herdr.pane.waitForOutput({
    pane_id: "1-1",
    source: "recent",
    match: { type: "substring", value: "ready" },
    timeout_ms: 30_000,
  });
  output satisfies OutputMatchedResult;

  const done = await herdr.pane.waitForAgentStatus("1-1", "done", {
    timeoutMs: 60_000,
  });
  done.data.agent_status satisfies "idle" | "working" | "blocked" | "done" | "unknown";

  try {
    await herdr.workspace.rename("1", "api");
  } catch (error) {
    const maybeHerdrError = error as HerdrApiError;
    maybeHerdrError.code satisfies string;
  }
}

void compileTypedCalls;
