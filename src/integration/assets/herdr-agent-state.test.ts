import { afterEach, expect, test } from "bun:test";
import { rm } from "node:fs/promises";
import { createServer, type Server } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

const originalEnvironment = {
  HERDR_ENV: process.env.HERDR_ENV,
  HERDR_PANE_ID: process.env.HERDR_PANE_ID,
  HERDR_SOCKET_PATH: process.env.HERDR_SOCKET_PATH,
};

let server: Server | undefined;
let socketPath: string | undefined;

afterEach(async () => {
  await new Promise<void>((resolve, reject) => {
    if (!server) {
      resolve();
      return;
    }
    server.close((error) => (error ? reject(error) : resolve()));
  });
  server = undefined;

  if (socketPath) {
    await rm(socketPath, { force: true });
    socketPath = undefined;
  }

  for (const [name, value] of Object.entries(originalEnvironment)) {
    if (value === undefined) {
      delete process.env[name];
    } else {
      process.env[name] = value;
    }
  }
});

const integrations = [
  { name: "Pi", modulePath: "./pi/herdr-agent-state.ts" },
  { name: "Oh My Pi", modulePath: "./omp/herdr-agent-state.ts" },
] as const;

for (const integration of integrations) {
  test(`${integration.name} reload preserves working state when the agent is active`, async () => {
    const recordingSocketPath = join(
      tmpdir(),
      `herdr-${integration.name.toLowerCase().replaceAll(" ", "-")}-${process.pid}.sock`,
    );
    socketPath = recordingSocketPath;
    await rm(recordingSocketPath, { force: true });

    const requests: unknown[] = [];
    const recordingServer = createServer((socket) => {
      let input = "";
      socket.setEncoding("utf8");
      socket.on("data", (chunk) => {
        input += chunk;
        const newline = input.indexOf("\n");
        if (newline === -1) {
          return;
        }
        requests.push(JSON.parse(input.slice(0, newline)));
        socket.end("{}\n");
      });
    });
    server = recordingServer;
    await new Promise<void>((resolve, reject) => {
      recordingServer.once("error", reject);
      recordingServer.listen(recordingSocketPath, resolve);
    });

    process.env.HERDR_ENV = "1";
    process.env.HERDR_SOCKET_PATH = recordingSocketPath;
    process.env.HERDR_PANE_ID = "test:p1";

    type Handler = (event: unknown, context: unknown) => unknown;
    const handlers = new Map<string, Handler>();
    const pi = {
      on(event: string, handler: Handler) {
        handlers.set(event, handler);
      },
      events: {
        on() {
          return () => {};
        },
      },
    };

    const { default: install } = await import(integration.modulePath);
    install(pi);

    const sessionStart = handlers.get("session_start");
    expect(sessionStart).toBeDefined();
    await sessionStart?.(
      { reason: "reload" },
      {
        hasUI: true,
        isIdle: () => false,
        sessionManager: {
          getSessionFile: () => undefined,
          getSessionId: () => undefined,
        },
      },
    );

    const reportedState = () => {
      for (const request of requests) {
        if (!isRecord(request) || request.method !== "pane.report_agent") {
          continue;
        }
        const params = request.params;
        if (isRecord(params) && typeof params.state === "string") {
          return params.state;
        }
      }
      return undefined;
    };

    const deadline = Date.now() + 1_000;
    while (Date.now() < deadline && reportedState() === undefined) {
      await Bun.sleep(5);
    }

    expect(reportedState()).toBe("working");
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
