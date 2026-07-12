import { afterEach, expect, test } from "bun:test";
import { readFile, rm } from "node:fs/promises";
import { createServer, type Server } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

const originalEnvironment = {
  HERDR_ENV: process.env.HERDR_ENV,
  HERDR_PANE_ID: process.env.HERDR_PANE_ID,
  HERDR_SOCKET_PATH: process.env.HERDR_SOCKET_PATH,
  HERDR_BIN_PATH: process.env.HERDR_BIN_PATH,
  HERDR_OMP_REPORTER_SCRIPT: process.env.HERDR_OMP_REPORTER_SCRIPT,
  HERDR_OMP_TEST_REPORTS: process.env.HERDR_OMP_TEST_REPORTS,
  HERDR_OMP_TEST_DELAY_METHOD: process.env.HERDR_OMP_TEST_DELAY_METHOD,
  HERDR_OMP_TEST_DELAY_MS: process.env.HERDR_OMP_TEST_DELAY_MS,
  HERDR_OMP_TEST_FAIL_METHOD: process.env.HERDR_OMP_TEST_FAIL_METHOD,
};

let server: Server | undefined;
let socketPath: string | undefined;
let importCounter = 0;
let reporterPath: string | undefined;

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

  if (reporterPath) {
    await rm(reporterPath, { force: true });
    reporterPath = undefined;
  }

  for (const [name, value] of Object.entries(originalEnvironment)) {
    if (value === undefined) {
      delete process.env[name];
    } else {
      process.env[name] = value;
    }
  }
});

const socketIntegrations = [{ name: "Pi", modulePath: "./pi/herdr-agent-state.ts" }] as const;

function importFresh(modulePath: string) {
  importCounter += 1;
  return import(`${modulePath}?test=${importCounter}`);
}

type Handler = (event: unknown, context: unknown) => unknown;

function createExtensionHarness() {
  const handlers = new Map<string, Handler>();
  return {
    handlers,
    pi: {
      on(event: string, handler: Handler) {
        handlers.set(event, handler);
      },
      events: {
        on() {
          return () => {};
        },
      },
    },
  };
}

function configureIntegrationEnvironment(recordingSocketPath: string) {
  process.env.HERDR_ENV = "1";
  process.env.HERDR_SOCKET_PATH = recordingSocketPath;
  process.env.HERDR_PANE_ID = "test:p1";
}

async function configureOmpReporter(name: string): Promise<void> {
  reporterPath = join(tmpdir(), `herdr-omp-${name}-${process.pid}.jsonl`);
  await rm(reporterPath, { force: true });
  process.env.HERDR_ENV = "1";
  process.env.HERDR_SOCKET_PATH = "test-only";
  process.env.HERDR_PANE_ID = "test:p1";
  process.env.HERDR_BIN_PATH = process.execPath;
  process.env.HERDR_OMP_REPORTER_SCRIPT = join(import.meta.dir, "omp", "reporter-fixture.ts");
  process.env.HERDR_OMP_TEST_REPORTS = reporterPath;
}

async function readOmpReports(): Promise<string[][]> {
  if (!reporterPath) {
    return [];
  }
  try {
    const content = await readFile(reporterPath, "utf8");
    return content
      .split(/\r?\n/)
      .filter((line) => line.length > 0)
      .map((line) => JSON.parse(line));
  } catch (error) {
    if (isRecord(error) && error.code === "ENOENT") {
      return [];
    }
    throw error;
  }
}

async function startRecordingServer(name: string): Promise<unknown[]> {
  const recordingSocketPath = join(tmpdir(), `herdr-${name}-${process.pid}.sock`);
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
  configureIntegrationEnvironment(recordingSocketPath);
  return requests;
}

for (const integration of socketIntegrations) {
  test(`${integration.name} reload preserves working state when the agent is active`, async () => {
    const requests = await startRecordingServer(
      integration.name.toLowerCase().replaceAll(" ", "-"),
    );
    const { handlers, pi } = createExtensionHarness();

    const { default: install } = await importFresh(integration.modulePath);
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

test("Pi reports the session replacement source", async () => {
  const requests = await startRecordingServer("pi-session-source");
  const { handlers, pi } = createExtensionHarness();

  const { default: install } = await importFresh("./pi/herdr-agent-state.ts");
  install(pi);

  const sessionStart = handlers.get("session_start");
  expect(sessionStart).toBeDefined();
  await sessionStart?.(
    { reason: "new" },
    {
      hasUI: true,
      isIdle: () => true,
      sessionManager: {
        getSessionFile: () => "/tmp/pi-new.jsonl",
        getSessionId: () => "pi-new",
      },
    },
  );

  const reportedSession = () =>
    requests.find((request) => isRecord(request) && request.method === "pane.report_agent_session");
  const deadline = Date.now() + 1_000;
  while (Date.now() < deadline && reportedSession() === undefined) {
    await Bun.sleep(5);
  }

  const request = reportedSession();
  expect(request).toBeDefined();
  expect(isRecord(request) && isRecord(request.params) ? request.params.session_start_source : null)
    .toBe("new");
});

test("Pi waits for a replacement session report before publishing state", async () => {
  const recordingSocketPath = join(tmpdir(), `herdr-pi-session-order-${process.pid}.sock`);
  socketPath = recordingSocketPath;
  await rm(recordingSocketPath, { force: true });

  const requests: unknown[] = [];
  let acknowledgeSessionReport: (() => void) | undefined;
  const recordingServer = createServer((socket) => {
    let input = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      input += chunk;
      const newline = input.indexOf("\n");
      if (newline === -1) {
        return;
      }
      const request = JSON.parse(input.slice(0, newline));
      requests.push(request);
      if (isRecord(request) && request.method === "pane.report_agent_session") {
        acknowledgeSessionReport = () => socket.end("{}\n");
        return;
      }
      socket.end("{}\n");
    });
  });
  server = recordingServer;
  await new Promise<void>((resolve, reject) => {
    recordingServer.once("error", reject);
    recordingServer.listen(recordingSocketPath, resolve);
  });

  configureIntegrationEnvironment(recordingSocketPath);
  const { handlers, pi } = createExtensionHarness();
  const { default: install } = await importFresh("./pi/herdr-agent-state.ts");
  install(pi);

  const sessionStart = handlers.get("session_start");
  expect(sessionStart).toBeDefined();
  const sessionStartResult = sessionStart?.(
    { reason: "new" },
    {
      hasUI: true,
      isIdle: () => false,
      sessionManager: {
        getSessionFile: () => "/tmp/pi-new.jsonl",
        getSessionId: () => "pi-new",
      },
    },
  );

  const deadline = Date.now() + 1_000;
  while (Date.now() < deadline && acknowledgeSessionReport === undefined) {
    await Bun.sleep(5);
  }
  expect(acknowledgeSessionReport).toBeDefined();
  expect(
    requests.some((request) => isRecord(request) && request.method === "pane.report_agent"),
  ).toBe(false);

  acknowledgeSessionReport?.();
  await sessionStartResult;

  const stateDeadline = Date.now() + 1_000;
  while (
    Date.now() < stateDeadline &&
    !requests.some((request) => isRecord(request) && request.method === "pane.report_agent")
  ) {
    await Bun.sleep(5);
  }
  expect(requests.map((request) => (isRecord(request) ? request.method : undefined))).toEqual([
    "pane.report_agent_session",
    "pane.report_agent",
  ]);
});

test("Pi retries working state after an unanswered socket attempt", async () => {
  const recordingSocketPath = join(tmpdir(), `herdr-pi-retry-${process.pid}.sock`);
  socketPath = recordingSocketPath;
  await rm(recordingSocketPath, { force: true });

  let connectionCount = 0;
  const attemptedRequests: unknown[] = [];
  const deliveredRequests: unknown[] = [];
  const recordingServer = createServer((socket) => {
    connectionCount += 1;
    const connectionNumber = connectionCount;
    let input = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      input += chunk;
      const newline = input.indexOf("\n");
      if (newline === -1) {
        return;
      }
      const request = JSON.parse(input.slice(0, newline));
      attemptedRequests.push(request);
      if (connectionNumber === 1) {
        return;
      }
      deliveredRequests.push(request);
      socket.end("{}\n");
    });
  });
  server = recordingServer;
  await new Promise<void>((resolve, reject) => {
    recordingServer.once("error", reject);
    recordingServer.listen(recordingSocketPath, resolve);
  });

  configureIntegrationEnvironment(recordingSocketPath);
  const { handlers, pi } = createExtensionHarness();

  const { default: install } = await importFresh("./pi/herdr-agent-state.ts");
  install(pi);

  const sessionStart = handlers.get("session_start");
  expect(sessionStart).toBeDefined();
  await sessionStart?.(
    { reason: "startup" },
    {
      hasUI: true,
      isIdle: () => false,
      sessionManager: {
        getSessionFile: () => undefined,
        getSessionId: () => undefined,
      },
    },
  );

  const reportedWorking = () =>
    deliveredRequests.some((request) => {
      if (!isRecord(request) || request.method !== "pane.report_agent") {
        return false;
      }
      const params = request.params;
      return isRecord(params) && params.state === "working";
    });

  const deadline = Date.now() + 2_500;
  while (Date.now() < deadline && !reportedWorking()) {
    await Bun.sleep(5);
  }

  expect(connectionCount).toBeGreaterThanOrEqual(2);
  expect(attemptedRequests.length).toBeGreaterThanOrEqual(2);
  expect(attemptedRequests[1]).toEqual(attemptedRequests[0]);
  expect(reportedWorking()).toBe(true);
});


test("OMP accepts a root context without hasUI and preserves a Windows session path", async () => {
  await configureOmpReporter("missing-has-ui");
  const { handlers, pi } = createExtensionHarness();
  const { default: install } = await importFresh("./omp/herdr-agent-state.ts");
  install(pi);

  const windowsSessionPath = String.raw`C:\Users\tester\.omp\sessions\session.jsonl`;
  const beforeAgentStart = handlers.get("before_agent_start");
  expect(beforeAgentStart).toBeDefined();
  await beforeAgentStart?.(
    { prompt: "test" },
    {
      isIdle: () => false,
      sessionManager: {
        getSessionFile: () => windowsSessionPath,
        getSessionId: () => "session-id",
      },
    },
  );

  const deadline = Date.now() + 1_000;
  let reports = await readOmpReports();
  while (Date.now() < deadline && reports.length < 2) {
    await Bun.sleep(5);
    reports = await readOmpReports();
  }
  expect(reports.map((args) => args.slice(0, 2))).toEqual([
    ["pane", "report-agent-session"],
    ["pane", "report-agent"],
  ]);
  expect(reports[0]).toContain(windowsSessionPath);
  expect(reports[1]).toContain("working");
});

test("OMP rejects an explicitly non-UI context", async () => {
  await configureOmpReporter("explicit-no-ui");
  const { handlers, pi } = createExtensionHarness();
  const { default: install } = await importFresh("./omp/herdr-agent-state.ts");
  install(pi);

  await handlers.get("before_agent_start")?.(
    { prompt: "test" },
    {
      hasUI: false,
      sessionManager: {
        getSessionFile: () => undefined,
        getSessionId: () => "ignored",
      },
    },
  );
  await Bun.sleep(50);
  expect(await readOmpReports()).toEqual([]);
});

test("OMP reports a replacement session before its state", async () => {
  await configureOmpReporter("session-order");
  process.env.HERDR_OMP_TEST_DELAY_METHOD = "pane report-agent-session";
  process.env.HERDR_OMP_TEST_DELAY_MS = "150";
  const { handlers, pi } = createExtensionHarness();
  const { default: install } = await importFresh("./omp/herdr-agent-state.ts");
  install(pi);

  const sessionStart = handlers.get("session_start");
  expect(sessionStart).toBeDefined();
  const result = sessionStart?.(
    { reason: "new" },
    {
      hasUI: true,
      isIdle: () => false,
      sessionManager: {
        getSessionFile: () => String.raw`C:\sessions\new.jsonl`,
        getSessionId: () => "new-session",
      },
    },
  );
  await Bun.sleep(50);
  expect(await readOmpReports()).toEqual([]);
  await result;

  const deadline = Date.now() + 1_000;
  let reports = await readOmpReports();
  while (Date.now() < deadline && reports.length < 2) {
    await Bun.sleep(5);
    reports = await readOmpReports();
  }
  expect(reports.map((args) => args.slice(0, 2))).toEqual([
    ["pane", "report-agent-session"],
    ["pane", "report-agent"],
  ]);
});

test("OMP preserves working to idle edges while a report is slow", async () => {
  await configureOmpReporter("state-fifo");
  process.env.HERDR_OMP_TEST_DELAY_METHOD = "pane report-agent";
  process.env.HERDR_OMP_TEST_DELAY_MS = "500";
  const { handlers, pi } = createExtensionHarness();
  const { default: install } = await importFresh("./omp/herdr-agent-state.ts");
  install(pi);

  const context = {
    hasUI: true,
    isIdle: () => false,
    sessionManager: {
      getSessionFile: () => "/tmp/session.jsonl",
      getSessionId: () => "session-id",
    },
  };
  await handlers.get("before_agent_start")?.({ prompt: "test" }, context);
  handlers.get("agent_end")?.({ messages: [] }, context);

  const deadline = Date.now() + 2_000;
  let stateReports: string[][] = [];
  while (Date.now() < deadline && stateReports.length < 2) {
    await Bun.sleep(10);
    stateReports = (await readOmpReports()).filter(
      (args) => args[0] === "pane" && args[1] === "report-agent",
    );
  }
  expect(stateReports.map((args) => args[args.indexOf("--state") + 1])).toEqual([
    "working",
    "idle",
  ]);
});

test("OMP retries failed state reports and deduplicates only after acknowledgment", async () => {
  await configureOmpReporter("state-retry");
  process.env.HERDR_OMP_TEST_FAIL_METHOD = "pane report-agent";
  const { handlers, pi } = createExtensionHarness();
  const { default: install } = await importFresh("./omp/herdr-agent-state.ts");
  install(pi);

  const context = {
    hasUI: true,
    isIdle: () => false,
    sessionManager: {
      getSessionFile: () => "/tmp/retry-session.jsonl",
      getSessionId: () => "retry-session",
    },
  };
  await handlers.get("before_agent_start")?.({ prompt: "test" }, context);

  const failedDeadline = Date.now() + 1_500;
  let stateReports: string[][] = [];
  while (Date.now() < failedDeadline && stateReports.length < 3) {
    await Bun.sleep(10);
    stateReports = (await readOmpReports()).filter(
      (args) => args[0] === "pane" && args[1] === "report-agent",
    );
  }
  expect(stateReports).toHaveLength(3);

  delete process.env.HERDR_OMP_TEST_FAIL_METHOD;
  const recoveredDeadline = Date.now() + 1_000;
  while (Date.now() < recoveredDeadline && stateReports.length < 4) {
    await Bun.sleep(10);
    stateReports = (await readOmpReports()).filter(
      (args) => args[0] === "pane" && args[1] === "report-agent",
    );
  }
  expect(stateReports).toHaveLength(4);

  await handlers.get("agent_start")?.({}, context);
  await Bun.sleep(100);
  stateReports = (await readOmpReports()).filter(
    (args) => args[0] === "pane" && args[1] === "report-agent",
  );
  expect(stateReports).toHaveLength(4);
});
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
