// installed by herdr
// managed by herdr; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// HERDR_INTEGRATION_ID=omp
// HERDR_INTEGRATION_VERSION=9
// @ts-nocheck

import { spawn } from "node:child_process";
import { posix, win32 } from "node:path";

const HERDR_ENV = process.env.HERDR_ENV;
const socketPath = process.env.HERDR_SOCKET_PATH;
const paneId = process.env.HERDR_PANE_ID;
const source = "herdr:omp";

function enabled() {
  return HERDR_ENV === "1" && !!socketPath && !!paneId;
}

let requestQueue = Promise.resolve(true);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function appendCliFlag(args: string[], flag: string, value: unknown): void {
  if (typeof value === "string" && value.length > 0) {
    args.push(flag, value);
  } else if (typeof value === "number") {
    args.push(flag, String(value));
  }
}

function requestCliArgs(request: unknown): string[] | undefined {
  if (!isRecord(request) || !isRecord(request.params)) {
    return undefined;
  }
  const params = request.params;
  if (typeof params.pane_id !== "string") {
    return undefined;
  }

  if (request.method === "pane.report_agent") {
    const args = ["pane", "report-agent", params.pane_id];
    appendCliFlag(args, "--source", params.source);
    appendCliFlag(args, "--agent", params.agent);
    appendCliFlag(args, "--state", params.state);
    appendCliFlag(args, "--message", params.message);
    appendCliFlag(args, "--seq", params.seq);
    appendCliFlag(args, "--agent-session-id", params.agent_session_id);
    appendCliFlag(args, "--agent-session-path", params.agent_session_path);
    return args;
  }
  if (request.method === "pane.report_agent_session") {
    const args = ["pane", "report-agent-session", params.pane_id];
    appendCliFlag(args, "--source", params.source);
    appendCliFlag(args, "--agent", params.agent);
    appendCliFlag(args, "--seq", params.seq);
    appendCliFlag(args, "--agent-session-id", params.agent_session_id);
    appendCliFlag(args, "--agent-session-path", params.agent_session_path);
    appendCliFlag(args, "--session-start-source", params.session_start_source);
    return args;
  }
  if (request.method === "pane.release_agent") {
    const args = ["pane", "release-agent", params.pane_id];
    appendCliFlag(args, "--source", params.source);
    appendCliFlag(args, "--agent", params.agent);
    appendCliFlag(args, "--seq", params.seq);
    return args;
  }
  return undefined;
}

function sendRequestNow(request: unknown): Promise<boolean> {
  if (!enabled()) {
    return Promise.resolve(true);
  }
  const args = requestCliArgs(request);
  if (!args) {
    return Promise.resolve(false);
  }

  const { promise, resolve } = Promise.withResolvers<boolean>();
  const reporterScript = process.env.HERDR_OMP_REPORTER_SCRIPT;
  const reporterArgs = reporterScript ? [reporterScript, ...args] : args;
  const child = spawn(process.env.HERDR_BIN_PATH || "herdr", reporterArgs, {
    stdio: "ignore",
    windowsHide: true,
  });
  let settled = false;
  let timeout: NodeJS.Timeout | undefined;
  const finish = (delivered: boolean) => {
    if (settled) return;
    settled = true;
    if (timeout) {
      clearTimeout(timeout);
    }
    resolve(delivered);
  };
  child.on("error", () => finish(false));
  child.on("exit", (code) => finish(code === 0));
  timeout = setTimeout(() => {
    child.kill();
    finish(false);
  }, 2_000);
  timeout.unref?.();
  return promise;
}

function sendRequest(request: unknown): Promise<boolean> {
  const queued = requestQueue.then(
    () => sendRequestNow(request),
    () => sendRequestNow(request),
  );
  requestQueue = queued;
  return queued;
}

type AgentState = "working" | "blocked" | "idle";

type QueuedState = {
  state: AgentState;
  message?: string;
  seq: number;
  attempts: number;
};

const idleDebounceMs = parseDurationEnv("HERDR_OMP_IDLE_DEBOUNCE_MS", 250);
const retryGraceMs = parseDurationEnv("HERDR_OMP_RETRY_GRACE_MS", 2500);
const retryableErrorPattern =
  /overloaded|provider.?returned.?error|rate.?limit|too many requests|429|500|502|503|504|service.?unavailable|server.?error|internal.?error|network.?error|connection.?error|connection.?refused|connection.?lost|websocket.?closed|websocket.?error|other side closed|fetch failed|upstream.?connect|reset before headers|socket hang up|ended without|http2 request did not get a response|timed? out|timeout|terminated|retry delay/i;
let reportSeq = Date.now() * 1000;
let currentAgentSessionId: string | undefined;
let currentAgentSessionPath: string | undefined;

function nextReportSeq(): number {
  reportSeq += 1;
  return reportSeq;
}

function updateSessionRef(ctx: unknown): void {
  const sessionManager = isRecord(ctx) && isRecord(ctx.sessionManager) ? ctx.sessionManager : undefined;
  try {
    const file =
      sessionManager && typeof sessionManager.getSessionFile === "function"
        ? sessionManager.getSessionFile()
        : undefined;
    currentAgentSessionPath =
      typeof file === "string" && (posix.isAbsolute(file) || win32.isAbsolute(file))
        ? file
        : undefined;
  } catch {
    currentAgentSessionPath = undefined;
  }

  try {
    const id =
      sessionManager && typeof sessionManager.getSessionId === "function"
        ? sessionManager.getSessionId()
        : undefined;
    currentAgentSessionId = typeof id === "string" && id.length > 0 ? id : undefined;
  } catch {
    currentAgentSessionId = undefined;
  }
}

function withSessionRef(params: Record<string, unknown>): Record<string, unknown> {
  if (currentAgentSessionPath) {
    return { ...params, agent_session_path: currentAgentSessionPath };
  }
  if (currentAgentSessionId) {
    return { ...params, agent_session_id: currentAgentSessionId };
  }
  return params;
}

function parseDurationEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) {
    return fallback;
  }
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed < 0) {
    return fallback;
  }
  return parsed;
}

function currentSessionRef(): Record<string, unknown> | undefined {
  if (currentAgentSessionPath) {
    return { agent_session_path: currentAgentSessionPath };
  }
  if (currentAgentSessionId) {
    return { agent_session_id: currentAgentSessionId };
  }
  return undefined;
}

function reportSession(sessionStartSource = "startup"): Promise<boolean> {
  const sessionRef = currentSessionRef();
  if (!sessionRef) {
    return Promise.resolve(true);
  }

  return sendRequest({
    id: `${source}:session:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "pane.report_agent_session",
    params: {
      pane_id: paneId,
      source,
      agent: "omp",
      seq: nextReportSeq(),
      session_start_source: sessionStartSource,
      ...sessionRef,
    },
  });
}

function sendState(state: AgentState, message?: string, seq = nextReportSeq()): Promise<boolean> {
  return sendRequest({
    id: `${source}:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "pane.report_agent",
    params: withSessionRef({
      pane_id: paneId,
      source,
      agent: "omp",
      state,
      message,
      seq,
    }),
  });
}

function releaseAgent(): Promise<boolean> {
  return sendRequest({
    id: `${source}:release:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "pane.release_agent",
    params: {
      pane_id: paneId,
      source,
      agent: "omp",
      seq: nextReportSeq(),
    },
  });
}

function shouldReleaseOnSessionShutdown(event: unknown): boolean {
  // OMP tears down and rebinds extension runtimes for internal lifecycle actions
  // such as /reload, /new, /resume, and /fork. Those do not mean the pane's
  // agent process has exited, and releasing hook authority there can suppress
  // legitimate reports from the replacement runtime. Only a user/process quit
  // should release Herdr's full-lifecycle authority.
  return isRecord(event) && event.reason === "quit";
}

let sendInFlight = false;
const queuedStates: QueuedState[] = [];
let acknowledgedState: AgentState | undefined;
let acknowledgedMessage: string | undefined;

function queueState(state: AgentState, message?: string, force = false): void {
  const pending = queuedStates.at(-1);
  if (
    !force &&
    ((pending && pending.state === state && pending.message === message) ||
      (!pending && acknowledgedState === state && acknowledgedMessage === message))
  ) {
    return;
  }
  queuedStates.push({ state, message, seq: nextReportSeq(), attempts: 0 });
  if (!sendInFlight) {
    void drainStateQueue();
  }
}

async function drainStateQueue(): Promise<void> {
  if (sendInFlight) {
    return;
  }

  sendInFlight = true;
  try {
    while (queuedStates.length > 0) {
      const next = queuedStates[0];
      if (await sendState(next.state, next.message, next.seq)) {
        queuedStates.shift();
        acknowledgedState = next.state;
        acknowledgedMessage = next.message;
        continue;
      }

      next.attempts += 1;
      const retryDelay = Promise.withResolvers<void>();
      const backoffMs = Math.min(5_000, 100 * 2 ** Math.min(next.attempts - 1, 6));
      const timeout = setTimeout(retryDelay.resolve, backoffMs);
      timeout.unref?.();
      await retryDelay.promise;
    }
  } finally {
    sendInFlight = false;
    if (queuedStates.length > 0) {
      void drainStateQueue();
    }
  }
}

function lastAssistantMessage(messages: unknown[]): Record<string, unknown> | undefined {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (isRecord(message) && message.role === "assistant") {
      return message;
    }
  }
  return undefined;
}

function retryableErrorMessage(event: unknown): string | undefined {
  const messages = isRecord(event) && Array.isArray(event.messages) ? event.messages : [];
  const assistant = lastAssistantMessage(messages);
  if (!assistant || assistant.stopReason !== "error") {
    return undefined;
  }

  const errorMessage = String(assistant.errorMessage ?? "");
  if (!retryableErrorPattern.test(errorMessage)) {
    return undefined;
  }
  return errorMessage || "retryable provider error";
}

function askBlockedMessage(args: unknown): string {
  const questions = isRecord(args) && Array.isArray(args.questions) ? args.questions : [];
  const firstQuestion = questions.find(
    (question: unknown) => isRecord(question) && typeof question.question === "string",
  );
  return isRecord(firstQuestion) && typeof firstQuestion.question === "string"
    ? firstQuestion.question
    : "waiting for user input";
}

export default function (pi) {
  if (!enabled()) {
    return;
  }

  let agentActive = false;
  let retryHoldActive = false;
  let failureBlocked = false;
  let failureMessage: string | undefined;
  let blockedCount = 0;
  let blockedMessage: string | undefined;
  // Delivery deduplication is based on acknowledged state at module scope.
  let idleTimer: NodeJS.Timeout | undefined;
  let retryTimer: NodeJS.Timeout | undefined;
  let rootSession = false;

  function clearTimer(timer: NodeJS.Timeout | undefined) {
    if (timer) {
      clearTimeout(timer);
    }
  }

  function clearPendingTimers() {
    clearTimer(idleTimer);
    clearTimer(retryTimer);
    idleTimer = undefined;
    retryTimer = undefined;
  }

  function clearFailureState() {
    retryHoldActive = false;
    failureBlocked = false;
    failureMessage = undefined;
  }

  function desiredState() {
    if (blockedCount > 0) {
      return { state: "blocked" as const, message: blockedMessage };
    }
    if (failureBlocked) {
      return { state: "blocked" as const, message: failureMessage };
    }
    if (agentActive || retryHoldActive) {
      return { state: "working" as const, message: undefined };
    }
    return { state: "idle" as const, message: undefined };
  }

  function publishState(force = false) {
    const next = desiredState();
    queueState(next.state, next.message, force);
  }

  function scheduleIdle() {
    clearPendingTimers();
    clearFailureState();
    idleTimer = setTimeout(() => {
      idleTimer = undefined;
      publishState();
    }, idleDebounceMs);
    idleTimer.unref?.();
  }

  function holdForRetry(message: string) {
    clearPendingTimers();
    retryHoldActive = true;
    failureBlocked = false;
    failureMessage = message;
    publishState();

    retryTimer = setTimeout(() => {
      retryTimer = undefined;
      retryHoldActive = false;
      failureBlocked = true;
      publishState();
    }, retryGraceMs);
    retryTimer.unref?.();
  }

  function activateRootSession(ctx: unknown): boolean {
    if (!isRecord(ctx) || ctx.hasUI === false || !isRecord(ctx.sessionManager)) {
      return false;
    }
    rootSession = true;
    updateSessionRef(ctx);
    return true;
  }

  function resetSessionState() {
    clearPendingTimers();
    clearFailureState();
    agentActive = false;
    blockedCount = 0;
    blockedMessage = undefined;
  }

  function activateBlocked(message: string | undefined) {
    clearPendingTimers();
    blockedCount += 1;
    blockedMessage = message;
    publishState();
  }

  function deactivateBlocked() {
    blockedCount = Math.max(0, blockedCount - 1);
    if (blockedCount === 0) {
      blockedMessage = undefined;
    }
    publishState();
  }

  pi.events.on("herdr:blocked", (data) => {
    if (!rootSession) {
      return;
    }
    if (!data?.active) {
      deactivateBlocked();
      return;
    }

    activateBlocked(data.label);
  });

  pi.on("before_agent_start", async (_event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    updateSessionRef(ctx);
    if (!(await reportSession())) {
      return;
    }
    clearPendingTimers();
    clearFailureState();
    agentActive = true;
    publishState();
  });

  pi.on("session_start", async (event, ctx) => {
    if (!activateRootSession(ctx)) {
      return;
    }
    const sessionStartSource = isRecord(event) && typeof event.reason === "string" ? event.reason : "startup";
    if (!(await reportSession(sessionStartSource))) {
      return;
    }
    const contextIsIdle =
      isRecord(ctx) && typeof ctx.isIdle === "function" ? ctx.isIdle() : undefined;
    // A pre-start hook can arrive before a late session_start with stale idle context.
    agentActive = agentActive || contextIsIdle === false;
    publishState(true);
  });

  pi.on("session_switch", async (event, ctx) => {
    if (!activateRootSession(ctx)) {
      return;
    }
    resetSessionState();
    const sessionStartSource = isRecord(event) && typeof event.reason === "string" ? event.reason : "resume";
    if (!(await reportSession(sessionStartSource))) {
      return;
    }
    publishState(true);
  });

  pi.on("agent_start", async (_event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    updateSessionRef(ctx);
    if (!(await reportSession())) {
      return;
    }
    clearPendingTimers();
    clearFailureState();
    agentActive = true;
    publishState();
  });

  pi.on("tool_approval_requested", (event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    const label = event?.reason || `${event?.toolName || "Tool"} approval`;
    activateBlocked(label);
  });

  pi.on("tool_approval_resolved", (_event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    deactivateBlocked();
  });

  pi.on("tool_execution_start", (event, ctx) => {
    if (event?.toolName !== "ask") {
      return;
    }
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    activateBlocked(askBlockedMessage(event.args));
  });

  pi.on("tool_execution_end", (event, ctx) => {
    if (event?.toolName !== "ask") {
      return;
    }
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    deactivateBlocked();
  });

  pi.on("agent_end", (event) => {
    if (!rootSession) {
      return;
    }
    if (!agentActive) {
      // OMP can emit duplicate/late end events while auto-retry is already
      // holding the pane in Working. Do not let an unqualified duplicate end
      // cancel the retry hold and publish a false Idle.
      return;
    }

    agentActive = false;

    const retryableMessage = retryableErrorMessage(event);
    if (retryableMessage) {
      holdForRetry(retryableMessage);
      return;
    }

    scheduleIdle();
  });

  pi.on("session_shutdown", async (event) => {
    if (!rootSession) {
      return;
    }
    clearPendingTimers();
    if (shouldReleaseOnSessionShutdown(event)) {
      await releaseAgent();
    }
  });
}
