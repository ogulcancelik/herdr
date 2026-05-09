import { createConnection, type Socket } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";

export type EmptyParams = Record<string, never>;

export type AgentStatus = "idle" | "working" | "blocked" | "done" | "unknown";
export type PaneAgentState = "idle" | "working" | "blocked" | "unknown";
export type ReadSource = "visible" | "recent" | "recent_unwrapped";
export type ReadFormat = "text" | "ansi";
export type SplitDirection = "right" | "down";
export type IntegrationTarget = "pi" | "claude" | "codex" | "opencode";

export interface WorkspaceTarget {
  workspace_id: string;
}

export interface PaneTarget {
  pane_id: string;
}

export interface TabTarget {
  tab_id: string;
}

export interface WorkspaceCreateParams {
  cwd?: string;
  focus?: boolean;
  label?: string;
}

export interface WorkspaceRenameParams {
  workspace_id: string;
  label: string;
}

export interface TabCreateParams {
  workspace_id?: string;
  cwd?: string;
  focus?: boolean;
  label?: string;
}

export interface TabListParams {
  workspace_id?: string;
}

export interface TabRenameParams {
  tab_id: string;
  label: string;
}

export interface PaneSplitParams {
  workspace_id?: string;
  target_pane_id: string;
  direction: SplitDirection;
  cwd?: string;
  focus?: boolean;
}

export interface PaneListParams {
  workspace_id?: string;
}

export interface PaneSendTextParams {
  pane_id: string;
  text: string;
}

export interface PaneSendKeysParams {
  pane_id: string;
  keys: string[];
}

export interface PaneSendInputParams {
  pane_id: string;
  text?: string;
  keys?: string[];
}

export interface PaneReadParams {
  pane_id: string;
  source: ReadSource;
  lines?: number;
  format?: ReadFormat;
  strip_ansi?: boolean;
}

export interface PaneReportAgentParams {
  pane_id: string;
  source: string;
  agent: string;
  state: PaneAgentState;
  message?: string;
}

export interface PaneClearAgentAuthorityParams {
  pane_id: string;
  source?: string;
}

export interface PaneReleaseAgentParams {
  pane_id: string;
  source: string;
  agent: string;
}

export type OutputMatch =
  | { type: "substring"; value: string }
  | { type: "regex"; value: string };

export type Subscription =
  | { type: "workspace.created" }
  | { type: "workspace.closed" }
  | { type: "workspace.focused" }
  | { type: "tab.created" }
  | { type: "tab.closed" }
  | { type: "tab.focused" }
  | { type: "tab.renamed" }
  | { type: "pane.created" }
  | { type: "pane.closed" }
  | { type: "pane.focused" }
  | { type: "pane.exited" }
  | { type: "pane.agent_detected" }
  | {
      type: "pane.output_matched";
      pane_id: string;
      source: ReadSource;
      lines?: number;
      match: OutputMatch;
      strip_ansi?: boolean;
    }
  | {
      type: "pane.agent_status_changed";
      pane_id: string;
      agent_status?: AgentStatus;
    };

export interface EventsSubscribeParams {
  subscriptions: Subscription[];
}

export type EventMatch =
  | { event: "workspace_created"; workspace_id?: string }
  | { event: "workspace_closed"; workspace_id: string }
  | { event: "workspace_renamed"; workspace_id: string; label?: string }
  | { event: "workspace_focused"; workspace_id: string }
  | { event: "tab_created"; tab_id?: string; workspace_id?: string }
  | { event: "tab_closed"; tab_id: string }
  | { event: "tab_renamed"; tab_id: string; label?: string }
  | { event: "tab_focused"; tab_id: string }
  | { event: "pane_created"; pane_id?: string; workspace_id?: string }
  | { event: "pane_closed"; pane_id: string }
  | { event: "pane_focused"; pane_id: string }
  | { event: "pane_output_changed"; pane_id: string; min_revision?: number }
  | { event: "pane_exited"; pane_id: string }
  | { event: "pane_agent_detected"; pane_id: string; agent?: string }
  | { event: "pane_agent_status_changed"; pane_id: string; agent_status: AgentStatus };

export interface EventsWaitParams {
  match_event: EventMatch;
  timeout_ms?: number;
}

export interface PaneWaitForOutputParams {
  pane_id: string;
  source: ReadSource;
  lines?: number;
  match: OutputMatch;
  timeout_ms?: number;
  strip_ansi?: boolean;
}

export interface IntegrationInstallParams {
  target: IntegrationTarget;
}

export interface IntegrationUninstallParams {
  target: IntegrationTarget;
}

export interface WorkspaceInfo {
  workspace_id: string;
  number: number;
  label: string;
  focused: boolean;
  pane_count: number;
  tab_count: number;
  active_tab_id: string;
  agent_status: AgentStatus;
}

export interface TabInfo {
  tab_id: string;
  workspace_id: string;
  number: number;
  label: string;
  focused: boolean;
  pane_count: number;
  agent_status: AgentStatus;
}

export interface PaneInfo {
  pane_id: string;
  workspace_id: string;
  tab_id: string;
  focused: boolean;
  cwd?: string;
  agent?: string;
  agent_status: AgentStatus;
  revision: number;
}

export interface PaneReadResult {
  pane_id: string;
  workspace_id: string;
  tab_id: string;
  source: ReadSource;
  format: ReadFormat;
  text: string;
  revision: number;
  truncated: boolean;
}

export interface IntegrationInstallResult {
  messages: string[];
}

export interface IntegrationUninstallResult {
  messages: string[];
}

export type ConfigReloadStatus = "applied" | "partial" | "failed";

export interface PongResult {
  type: "pong";
  version: string;
  protocol: number;
}

export interface OkResult {
  type: "ok";
}

export interface WorkspaceInfoResult {
  type: "workspace_info";
  workspace: WorkspaceInfo;
}

export interface WorkspaceCreatedResult {
  type: "workspace_created";
  workspace: WorkspaceInfo;
  tab: TabInfo;
  root_pane: PaneInfo;
}

export interface WorkspaceListResult {
  type: "workspace_list";
  workspaces: WorkspaceInfo[];
}

export interface TabInfoResult {
  type: "tab_info";
  tab: TabInfo;
}

export interface TabCreatedResult {
  type: "tab_created";
  tab: TabInfo;
  root_pane: PaneInfo;
}

export interface TabListResult {
  type: "tab_list";
  tabs: TabInfo[];
}

export interface PaneInfoResult {
  type: "pane_info";
  pane: PaneInfo;
}

export interface PaneListResult {
  type: "pane_list";
  panes: PaneInfo[];
}

export interface PaneReadResultEnvelope {
  type: "pane_read";
  read: PaneReadResult;
}

export interface SubscriptionStartedResult {
  type: "subscription_started";
}

export interface WaitMatchedResult {
  type: "wait_matched";
  event: HerdrLifecycleEvent;
}

export interface OutputMatchedResult {
  type: "output_matched";
  pane_id: string;
  revision: number;
  matched_line?: string | null;
  read: PaneReadResult;
}

export interface IntegrationInstallResultEnvelope {
  type: "integration_install";
  target: IntegrationTarget;
  details: IntegrationInstallResult;
}

export interface IntegrationUninstallResultEnvelope {
  type: "integration_uninstall";
  target: IntegrationTarget;
  details: IntegrationUninstallResult;
}

export interface ConfigReloadResult {
  type: "config_reload";
  status: ConfigReloadStatus;
  diagnostics: string[];
}

export type HerdrMethodParams = {
  ping: EmptyParams;
  "server.stop": EmptyParams;
  "server.reload_config": EmptyParams;
  "workspace.create": WorkspaceCreateParams;
  "workspace.list": EmptyParams;
  "workspace.get": WorkspaceTarget;
  "workspace.focus": WorkspaceTarget;
  "workspace.rename": WorkspaceRenameParams;
  "workspace.close": WorkspaceTarget;
  "tab.create": TabCreateParams;
  "tab.list": TabListParams;
  "tab.get": TabTarget;
  "tab.focus": TabTarget;
  "tab.rename": TabRenameParams;
  "tab.close": TabTarget;
  "pane.split": PaneSplitParams;
  "pane.list": PaneListParams;
  "pane.get": PaneTarget;
  "pane.send_text": PaneSendTextParams;
  "pane.send_keys": PaneSendKeysParams;
  "pane.send_input": PaneSendInputParams;
  "pane.read": PaneReadParams;
  "pane.report_agent": PaneReportAgentParams;
  "pane.clear_agent_authority": PaneClearAgentAuthorityParams;
  "pane.release_agent": PaneReleaseAgentParams;
  "pane.close": PaneTarget;
  "events.subscribe": EventsSubscribeParams;
  "events.wait": EventsWaitParams;
  "pane.wait_for_output": PaneWaitForOutputParams;
  "integration.install": IntegrationInstallParams;
  "integration.uninstall": IntegrationUninstallParams;
};

export type HerdrMethodResults = {
  ping: PongResult;
  "server.stop": OkResult;
  "server.reload_config": ConfigReloadResult;
  "workspace.create": WorkspaceCreatedResult;
  "workspace.list": WorkspaceListResult;
  "workspace.get": WorkspaceInfoResult;
  "workspace.focus": WorkspaceInfoResult;
  "workspace.rename": WorkspaceInfoResult;
  "workspace.close": OkResult;
  "tab.create": TabCreatedResult;
  "tab.list": TabListResult;
  "tab.get": TabInfoResult;
  "tab.focus": TabInfoResult;
  "tab.rename": TabInfoResult;
  "tab.close": OkResult;
  "pane.split": PaneInfoResult;
  "pane.list": PaneListResult;
  "pane.get": PaneInfoResult;
  "pane.send_text": OkResult;
  "pane.send_keys": OkResult;
  "pane.send_input": OkResult;
  "pane.read": PaneReadResultEnvelope;
  "pane.report_agent": OkResult;
  "pane.clear_agent_authority": OkResult;
  "pane.release_agent": OkResult;
  "pane.close": OkResult;
  "events.subscribe": SubscriptionStartedResult;
  "events.wait": WaitMatchedResult;
  "pane.wait_for_output": OutputMatchedResult;
  "integration.install": IntegrationInstallResultEnvelope;
  "integration.uninstall": IntegrationUninstallResultEnvelope;
};

export type HerdrMethod = keyof HerdrMethodParams & keyof HerdrMethodResults;

export interface HerdrRequest<M extends HerdrMethod = HerdrMethod> {
  id: string;
  method: M;
  params: HerdrMethodParams[M];
}

export interface HerdrSuccessResponse<M extends HerdrMethod = HerdrMethod> {
  id: string;
  result: HerdrMethodResults[M];
}

export interface HerdrErrorBody {
  code: string;
  message: string;
}

export interface HerdrErrorResponse {
  id: string;
  error: HerdrErrorBody;
}

export type HerdrResponse<M extends HerdrMethod = HerdrMethod> =
  | HerdrSuccessResponse<M>
  | HerdrErrorResponse;

export type HerdrEventKind =
  | "workspace_created"
  | "workspace_closed"
  | "workspace_renamed"
  | "workspace_focused"
  | "tab_created"
  | "tab_closed"
  | "tab_renamed"
  | "tab_focused"
  | "pane_created"
  | "pane_closed"
  | "pane_focused"
  | "pane_output_changed"
  | "pane_exited"
  | "pane_agent_detected"
  | "pane_agent_status_changed";

export type HerdrLifecycleEvent =
  | {
      event: "workspace_created";
      data: { type: "workspace_created"; workspace: WorkspaceInfo };
    }
  | { event: "workspace_closed"; data: { type: "workspace_closed"; workspace_id: string } }
  | {
      event: "workspace_renamed";
      data: { type: "workspace_renamed"; workspace_id: string; label: string };
    }
  | { event: "workspace_focused"; data: { type: "workspace_focused"; workspace_id: string } }
  | { event: "tab_created"; data: { type: "tab_created"; tab: TabInfo } }
  | {
      event: "tab_closed";
      data: { type: "tab_closed"; tab_id: string; workspace_id: string };
    }
  | {
      event: "tab_renamed";
      data: { type: "tab_renamed"; tab_id: string; workspace_id: string; label: string };
    }
  | {
      event: "tab_focused";
      data: { type: "tab_focused"; tab_id: string; workspace_id: string };
    }
  | { event: "pane_created"; data: { type: "pane_created"; pane: PaneInfo } }
  | {
      event: "pane_closed";
      data: { type: "pane_closed"; pane_id: string; workspace_id: string };
    }
  | {
      event: "pane_focused";
      data: { type: "pane_focused"; pane_id: string; workspace_id: string };
    }
  | {
      event: "pane_output_changed";
      data: { type: "pane_output_changed"; pane_id: string; workspace_id: string; revision: number };
    }
  | {
      event: "pane_exited";
      data: { type: "pane_exited"; pane_id: string; workspace_id: string };
    }
  | {
      event: "pane_agent_detected";
      data: { type: "pane_agent_detected"; pane_id: string; workspace_id: string; agent?: string };
    }
  | {
      event: "pane_agent_status_changed";
      data: {
        type: "pane_agent_status_changed";
        pane_id: string;
        workspace_id: string;
        agent_status: AgentStatus;
      };
    };

export interface PaneOutputMatchedSubscriptionEvent {
  event: "pane.output_matched";
  data: {
    pane_id: string;
    matched_line: string;
    read: PaneReadResult;
  };
}

export interface PaneAgentStatusChangedSubscriptionEvent {
  event: "pane.agent_status_changed";
  data: {
    pane_id: string;
    workspace_id: string;
    agent_status: AgentStatus;
    agent?: string;
  };
}

export type HerdrSubscriptionEvent =
  | PaneOutputMatchedSubscriptionEvent
  | PaneAgentStatusChangedSubscriptionEvent;

export type HerdrStreamEvent = HerdrLifecycleEvent | HerdrSubscriptionEvent;

export interface SocketPathOptions {
  socketPath?: string;
  session?: string;
  env?: Record<string, string | undefined>;
}

export interface HerdrClientOptions extends SocketPathOptions {
  connectTimeoutMs?: number;
  requestIdPrefix?: string;
  responseTimeoutMs?: number;
}

export interface RequestOptions {
  id?: string;
  signal?: AbortSignal;
  timeoutMs?: number;
}

export interface WaitForAgentStatusOptions {
  id?: string;
  signal?: AbortSignal;
  timeoutMs?: number;
  ackTimeoutMs?: number;
}

export class HerdrError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "HerdrError";
  }
}

export class HerdrTimeoutError extends HerdrError {
  constructor(message = "timed out waiting for herdr response") {
    super(message);
    this.name = "HerdrTimeoutError";
  }
}

export class HerdrProtocolError extends HerdrError {
  constructor(message: string) {
    super(message);
    this.name = "HerdrProtocolError";
  }
}

export class HerdrApiError extends HerdrError {
  readonly code: string;
  readonly response: HerdrErrorResponse;

  constructor(response: HerdrErrorResponse) {
    super(`${response.error.code}: ${response.error.message}`);
    this.name = "HerdrApiError";
    this.code = response.error.code;
    this.response = response;
  }
}

const DEFAULT_REQUEST_ID_PREFIX = "herdr-sdk";
const DEFAULT_CONNECT_TIMEOUT_MS = 5_000;
const SESSION_NAME_PATTERN = /^[A-Za-z0-9._-]{1,64}$/;

export function resolveHerdrSocketPath(options: SocketPathOptions = {}): string {
  const env = options.env ?? process.env;

  if (options.socketPath) {
    return options.socketPath;
  }

  if (options.session !== undefined) {
    return socketPathForSession(options.session, env);
  }

  if (env.HERDR_SOCKET_PATH) {
    return env.HERDR_SOCKET_PATH;
  }

  return socketPathForSession(env.HERDR_SESSION, env);
}

export function createHerdrClient(options: HerdrClientOptions = {}): HerdrClient {
  return new HerdrClient(options);
}

export class HerdrClient {
  readonly options: HerdrClientOptions;
  private sequence = 0;

  readonly server = {
    stop: (options?: RequestOptions) => this.request("server.stop", {}, options),
    reloadConfig: (options?: RequestOptions) =>
      this.request("server.reload_config", {}, options),
  };

  readonly workspace = {
    list: (options?: RequestOptions) => this.request("workspace.list", {}, options),
    get: (workspaceId: string, options?: RequestOptions) =>
      this.request("workspace.get", { workspace_id: workspaceId }, options),
    create: (params: WorkspaceCreateParams = {}, options?: RequestOptions) =>
      this.request("workspace.create", params, options),
    focus: (workspaceId: string, options?: RequestOptions) =>
      this.request("workspace.focus", { workspace_id: workspaceId }, options),
    rename: (workspaceId: string, label: string, options?: RequestOptions) =>
      this.request("workspace.rename", { workspace_id: workspaceId, label }, options),
    close: (workspaceId: string, options?: RequestOptions) =>
      this.request("workspace.close", { workspace_id: workspaceId }, options),
  };

  readonly tab = {
    list: (params: TabListParams = {}, options?: RequestOptions) =>
      this.request("tab.list", params, options),
    get: (tabId: string, options?: RequestOptions) =>
      this.request("tab.get", { tab_id: tabId }, options),
    create: (params: TabCreateParams = {}, options?: RequestOptions) =>
      this.request("tab.create", params, options),
    focus: (tabId: string, options?: RequestOptions) =>
      this.request("tab.focus", { tab_id: tabId }, options),
    rename: (tabId: string, label: string, options?: RequestOptions) =>
      this.request("tab.rename", { tab_id: tabId, label }, options),
    close: (tabId: string, options?: RequestOptions) =>
      this.request("tab.close", { tab_id: tabId }, options),
  };

  readonly pane = {
    list: (params: PaneListParams = {}, options?: RequestOptions) =>
      this.request("pane.list", params, options),
    get: (paneId: string, options?: RequestOptions) =>
      this.request("pane.get", { pane_id: paneId }, options),
    read: (params: PaneReadParams, options?: RequestOptions) =>
      this.request("pane.read", params, options),
    split: (params: PaneSplitParams, options?: RequestOptions) =>
      this.request("pane.split", params, options),
    sendText: (paneId: string, text: string, options?: RequestOptions) =>
      this.request("pane.send_text", { pane_id: paneId, text }, options),
    sendKeys: (paneId: string, keys: string[], options?: RequestOptions) =>
      this.request("pane.send_keys", { pane_id: paneId, keys }, options),
    sendInput: (params: PaneSendInputParams, options?: RequestOptions) =>
      this.request("pane.send_input", params, options),
    run: (paneId: string, command: string, options?: RequestOptions) =>
      this.request("pane.send_input", { pane_id: paneId, text: command, keys: ["Enter"] }, options),
    waitForOutput: (params: PaneWaitForOutputParams, options?: RequestOptions) =>
      this.request("pane.wait_for_output", params, options),
    waitForAgentStatus: (
      paneId: string,
      agentStatus: AgentStatus,
      options?: WaitForAgentStatusOptions,
    ) => this.waitForAgentStatus(paneId, agentStatus, options),
    reportAgent: (params: PaneReportAgentParams, options?: RequestOptions) =>
      this.request("pane.report_agent", params, options),
    clearAgentAuthority: (params: PaneClearAgentAuthorityParams, options?: RequestOptions) =>
      this.request("pane.clear_agent_authority", params, options),
    releaseAgent: (params: PaneReleaseAgentParams, options?: RequestOptions) =>
      this.request("pane.release_agent", params, options),
    close: (paneId: string, options?: RequestOptions) =>
      this.request("pane.close", { pane_id: paneId }, options),
  };

  readonly events = {
    subscribe: (params: EventsSubscribeParams, options?: RequestOptions) =>
      this.subscribe(params, options),
    wait: (params: EventsWaitParams, options?: RequestOptions) =>
      this.request("events.wait", params, options),
  };

  readonly integration = {
    install: (target: IntegrationTarget, options?: RequestOptions) =>
      this.request("integration.install", { target }, options),
    uninstall: (target: IntegrationTarget, options?: RequestOptions) =>
      this.request("integration.uninstall", { target }, options),
  };

  constructor(options: HerdrClientOptions = {}) {
    this.options = { ...options };
  }

  get socketPath(): string {
    return resolveHerdrSocketPath(this.options);
  }

  async request<M extends HerdrMethod>(
    method: M,
    params: HerdrMethodParams[M],
    options: RequestOptions = {},
  ): Promise<HerdrMethodResults[M]> {
    const response = await this.requestEnvelope(method, params, options);
    if (isErrorResponse(response)) {
      throw new HerdrApiError(response);
    }
    return response.result;
  }

  async requestEnvelope<M extends HerdrMethod>(
    method: M,
    params: HerdrMethodParams[M],
    options: RequestOptions = {},
  ): Promise<HerdrResponse<M>> {
    const socket = await this.openSocket(options);
    const reader = new LineReader(socket);
    const id = options.id ?? this.nextRequestId(method);
    const request: HerdrRequest<M> = { id, method, params };

    try {
      await writeLine(socket, JSON.stringify(request), options.signal);
      const line = await reader.readLine({
        signal: options.signal,
        timeoutMs: options.timeoutMs ?? this.options.responseTimeoutMs,
      });
      socket.end();

      if (line === null) {
        throw new HerdrProtocolError("herdr closed the socket before sending a response");
      }

      const response = parseJsonLine<HerdrResponse<M>>(line);
      if (!isResponse(response)) {
        throw new HerdrProtocolError("herdr returned an invalid response envelope");
      }
      return response;
    } catch (error) {
      socket.destroy();
      throw error;
    }
  }

  private async subscribe(
    params: EventsSubscribeParams,
    options: RequestOptions = {},
  ): Promise<HerdrSubscription> {
    const socket = await this.openSocket(options);
    const reader = new LineReader(socket);
    const id = options.id ?? this.nextRequestId("events.subscribe");
    const request: HerdrRequest<"events.subscribe"> = {
      id,
      method: "events.subscribe",
      params,
    };

    try {
      await writeLine(socket, JSON.stringify(request), options.signal);
      const line = await reader.readLine({
        signal: options.signal,
        timeoutMs: options.timeoutMs ?? this.options.responseTimeoutMs,
      });

      if (line === null) {
        throw new HerdrProtocolError("herdr closed the subscription before sending an ack");
      }

      const ack = parseJsonLine<HerdrResponse<"events.subscribe">>(line);
      if (!isResponse(ack)) {
        throw new HerdrProtocolError("herdr returned an invalid subscription ack");
      }
      if (isErrorResponse(ack)) {
        throw new HerdrApiError(ack);
      }
      if (ack.result.type !== "subscription_started") {
        throw new HerdrProtocolError(`expected subscription_started ack, got ${ack.result.type}`);
      }

      return new HerdrSubscription(socket, reader, ack);
    } catch (error) {
      socket.destroy();
      throw error;
    }
  }

  private async waitForAgentStatus(
    paneId: string,
    agentStatus: AgentStatus,
    options: WaitForAgentStatusOptions = {},
  ): Promise<PaneAgentStatusChangedSubscriptionEvent> {
    const subscription = await this.subscribe(
      {
        subscriptions: [
          {
            type: "pane.agent_status_changed",
            pane_id: paneId,
            agent_status: agentStatus,
          },
        ],
      },
      {
        id: options.id,
        signal: options.signal,
        timeoutMs: options.ackTimeoutMs,
      },
    );

    try {
      const event = await subscription.nextEvent({
        signal: options.signal,
        timeoutMs: options.timeoutMs,
      });
      if (event === null) {
        throw new HerdrProtocolError("subscription closed before agent status changed");
      }
      if (event.event !== "pane.agent_status_changed") {
        throw new HerdrProtocolError(`expected pane.agent_status_changed event, got ${event.event}`);
      }
      return event;
    } finally {
      subscription.close();
    }
  }

  private async openSocket(options: RequestOptions): Promise<Socket> {
    return connectUnixSocket(this.socketPath, {
      signal: options.signal,
      timeoutMs: this.options.connectTimeoutMs ?? DEFAULT_CONNECT_TIMEOUT_MS,
    });
  }

  private nextRequestId(method: string): string {
    this.sequence += 1;
    const prefix = this.options.requestIdPrefix ?? DEFAULT_REQUEST_ID_PREFIX;
    return `${prefix}:${method}:${this.sequence}`;
  }
}

export class HerdrSubscription implements AsyncIterable<HerdrStreamEvent> {
  readonly ack: HerdrSuccessResponse<"events.subscribe">;
  private closed = false;

  constructor(
    private readonly socket: Socket,
    private readonly reader: LineReader,
    ack: HerdrSuccessResponse<"events.subscribe">,
  ) {
    this.ack = ack;
  }

  async nextEvent(options: Omit<RequestOptions, "id"> = {}): Promise<HerdrStreamEvent | null> {
    if (this.closed) {
      return null;
    }

    const line = await this.reader.readLine(options);
    if (line === null) {
      this.closed = true;
      return null;
    }

    const event = parseJsonLine<HerdrStreamEvent>(line);
    if (!isStreamEvent(event)) {
      throw new HerdrProtocolError("herdr returned an invalid subscription event");
    }
    return event;
  }

  close(): void {
    if (this.closed) {
      return;
    }
    this.closed = true;
    this.socket.end();
    this.socket.destroy();
  }

  [Symbol.asyncIterator](): AsyncIterator<HerdrStreamEvent> {
    return {
      next: async () => {
        const value = await this.nextEvent();
        if (value === null) {
          return { done: true, value: undefined };
        }
        return { done: false, value };
      },
    };
  }
}

interface ConnectOptions {
  signal?: AbortSignal;
  timeoutMs: number;
}

interface ReadLineOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

interface PendingRead {
  resolve: (line: string | null) => void;
  reject: (error: Error) => void;
  timeout?: NodeJS.Timeout;
  signal?: AbortSignal;
  onAbort?: () => void;
}

class LineReader {
  private buffer = "";
  private readonly lines: string[] = [];
  private readonly pending: PendingRead[] = [];
  private ended = false;
  private error: Error | undefined;

  constructor(private readonly socket: Socket) {
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => this.push(chunk));
    socket.once("end", () => this.finish());
    socket.once("close", () => this.finish());
    socket.once("error", (error) => this.fail(error));
  }

  readLine(options: ReadLineOptions = {}): Promise<string | null> {
    const line = this.lines.shift();
    if (line !== undefined) {
      return Promise.resolve(line);
    }
    if (this.error) {
      return Promise.reject(this.error);
    }
    if (this.ended) {
      return Promise.resolve(null);
    }
    if (options.signal?.aborted) {
      return Promise.reject(abortError(options.signal));
    }

    return new Promise<string | null>((resolve, reject) => {
      const pending: PendingRead = { resolve, reject, signal: options.signal };
      if (options.timeoutMs !== undefined) {
        pending.timeout = setTimeout(() => {
          this.removePending(pending);
          reject(new HerdrTimeoutError());
        }, options.timeoutMs);
      }
      if (options.signal) {
        pending.onAbort = () => {
          this.removePending(pending);
          reject(abortError(options.signal));
        };
        options.signal.addEventListener("abort", pending.onAbort, { once: true });
      }
      this.pending.push(pending);
    });
  }

  private push(chunk: string): void {
    this.buffer += chunk;
    for (;;) {
      const newline = this.buffer.indexOf("\n");
      if (newline === -1) {
        break;
      }
      const line = this.buffer.slice(0, newline).replace(/\r$/, "");
      this.buffer = this.buffer.slice(newline + 1);
      this.lines.push(line);
    }
    this.flush();
  }

  private finish(): void {
    if (this.ended) {
      return;
    }
    if (this.buffer.length > 0) {
      this.lines.push(this.buffer);
      this.buffer = "";
    }
    this.ended = true;
    this.flush();
    while (this.pending.length > 0) {
      this.resolvePending(this.pending.shift()!, null);
    }
  }

  private fail(error: Error): void {
    this.error = error;
    while (this.pending.length > 0) {
      this.rejectPending(this.pending.shift()!, error);
    }
  }

  private flush(): void {
    while (this.lines.length > 0 && this.pending.length > 0) {
      this.resolvePending(this.pending.shift()!, this.lines.shift()!);
    }
  }

  private removePending(pending: PendingRead): void {
    const index = this.pending.indexOf(pending);
    if (index >= 0) {
      this.pending.splice(index, 1);
    }
    this.cleanupPending(pending);
  }

  private resolvePending(pending: PendingRead, line: string | null): void {
    this.cleanupPending(pending);
    pending.resolve(line);
  }

  private rejectPending(pending: PendingRead, error: Error): void {
    this.cleanupPending(pending);
    pending.reject(error);
  }

  private cleanupPending(pending: PendingRead): void {
    if (pending.timeout) {
      clearTimeout(pending.timeout);
    }
    if (pending.signal && pending.onAbort) {
      pending.signal.removeEventListener("abort", pending.onAbort);
    }
  }
}

function socketPathForSession(
  session: string | undefined,
  env: Record<string, string | undefined>,
): string {
  if (!session || session === "default") {
    return join(configHome(env), "herdr", "herdr.sock");
  }
  if (!SESSION_NAME_PATTERN.test(session)) {
    throw new HerdrError(
      "session names may contain only ASCII letters, numbers, '.', '_', and '-' and must be 1-64 characters",
    );
  }
  return join(configHome(env), "herdr", "sessions", session, "herdr.sock");
}

function configHome(env: Record<string, string | undefined>): string {
  if (env.XDG_CONFIG_HOME) {
    return env.XDG_CONFIG_HOME;
  }
  const home = env.HOME ?? homedir();
  if (!home) {
    throw new HerdrError("cannot resolve Herdr config path without HOME or XDG_CONFIG_HOME");
  }
  return join(home, ".config");
}

async function connectUnixSocket(path: string, options: ConnectOptions): Promise<Socket> {
  if (options.signal?.aborted) {
    throw abortError(options.signal);
  }

  return new Promise<Socket>((resolve, reject) => {
    const socket = createConnection(path);
    let settled = false;
    let timeout: NodeJS.Timeout | undefined;

    const finish = (callback: () => void) => {
      if (settled) {
        return;
      }
      settled = true;
      if (timeout) {
        clearTimeout(timeout);
      }
      socket.off("connect", onConnect);
      socket.off("error", onError);
      options.signal?.removeEventListener("abort", onAbort);
      callback();
    };

    const onConnect = () => finish(() => resolve(socket));
    const onError = (error: Error) => finish(() => reject(error));
    const onAbort = () => finish(() => {
      const error = abortError(options.signal);
      socket.destroy(error);
      reject(error);
    });

    socket.once("connect", onConnect);
    socket.once("error", onError);
    options.signal?.addEventListener("abort", onAbort, { once: true });

    timeout = setTimeout(() => {
      finish(() => {
        const error = new HerdrTimeoutError(`timed out connecting to herdr socket at ${path}`);
        socket.destroy(error);
        reject(error);
      });
    }, options.timeoutMs);
  });
}

async function writeLine(socket: Socket, line: string, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) {
    throw abortError(signal);
  }

  const payload = `${line}\n`;
  const wrote = socket.write(payload, "utf8");
  if (!wrote) {
    await waitForDrain(socket, signal);
  }
}

function parseJsonLine<T>(line: string): T {
  try {
    return JSON.parse(line) as T;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new HerdrProtocolError(`failed to parse herdr JSON line: ${message}`);
  }
}

function isResponse(value: unknown): value is HerdrResponse {
  if (!isRecord(value) || typeof value.id !== "string") {
    return false;
  }
  return isRecord(value.result) || isRecord(value.error);
}

function isErrorResponse(value: HerdrResponse): value is HerdrErrorResponse {
  return "error" in value;
}

function isStreamEvent(value: unknown): value is HerdrStreamEvent {
  return isRecord(value) && typeof value.event === "string" && isRecord(value.data);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

async function waitForDrain(socket: Socket, signal: AbortSignal | undefined): Promise<void> {
  if (signal?.aborted) {
    throw abortError(signal);
  }

  await new Promise<void>((resolve, reject) => {
    let settled = false;

    const cleanup = () => {
      socket.off("drain", onDrain);
      socket.off("error", onError);
      signal?.removeEventListener("abort", onAbort);
    };

    const finish = (callback: () => void) => {
      if (settled) {
        return;
      }
      settled = true;
      cleanup();
      callback();
    };

    const onDrain = () => finish(resolve);
    const onError = (error: Error) => finish(() => reject(error));
    const onAbort = () => finish(() => reject(abortError(signal)));

    socket.once("drain", onDrain);
    socket.once("error", onError);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function abortError(signal: AbortSignal | undefined): Error {
  const reason = signal?.reason;
  if (reason instanceof Error) {
    return reason;
  }
  return new HerdrError(reason === undefined ? "operation aborted" : String(reason));
}

export default HerdrClient;
