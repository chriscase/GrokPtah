import { parseCapabilitySet, type CapabilitySet } from "./capabilities";

export type GrokPtahTool = {
  name: string;
  description?: string;
  inputSchema: Record<string, unknown>;
};

export type GrokPtahCallResult = {
  structuredContent?: unknown;
  content?: unknown;
  isError?: boolean;
  error?: GrokPtahSafeError;
  raw: unknown;
};

/** Share-safe MCP error data; privileged diagnostics are intentionally dropped. */
export type GrokPtahSafeError = {
  code: string;
  message: string;
  requestId?: string;
};

export class GrokPtahRemoteError extends Error {
  readonly code: string;
  readonly requestId?: string;

  constructor(error: GrokPtahSafeError) {
    super(error.message);
    this.name = "GrokPtahRemoteError";
    this.code = error.code;
    this.requestId = error.requestId;
  }
}

export type GrokPtahRunScope = {
  sessionId: string;
  workspace: string;
  runId: string;
};

export type GrokPtahEventNotification = {
  kind: "event";
  sseId: number;
  sessionId: string;
  workspace: string;
  runId: string;
  seq: number;
  ts: string;
  update: unknown;
};

export type GrokPtahRecoveryNotification = {
  kind: "recovery";
  sseId: number | null;
  sessionId: string;
  workspace: string;
  runId: string;
  afterSeq: number;
  reason: string;
  pollTool: string;
};

export type GrokPtahRunNotification =
  | GrokPtahEventNotification
  | GrokPtahRecoveryNotification;

export type GrokPtahClientOptions = {
  baseUrl: string;
  token: string;
  clientName?: string;
  clientVersion?: string;
  fetcher?: typeof fetch;
};

/**
 * Minimal, Tauri-free client for the authenticated GrokPtah MCP surface.
 *
 * This is intentionally transport-only: callers still own user auth,
 * workspace/run scope, approval UX, and the policy that decides which tools
 * may be called. It is suitable for a trusted desktop adapter or server-side
 * broker, not for placing a desktop bearer token in a browser bundle.
 */
export class GrokPtahClient {
  private readonly mcpUrl: string;
  private readonly token: string;
  private readonly clientName: string;
  private readonly clientVersion: string;
  private readonly fetcher: typeof fetch;
  private nextId = 1;
  private sessionId: string | null = null;
  private protocolVersion: string | null = null;
  private capabilitySet: CapabilitySet | null = null;

  constructor(options: GrokPtahClientOptions) {
    const baseUrl = options.baseUrl.replace(/\/$/, "");
    this.mcpUrl = baseUrl.endsWith("/mcp") ? baseUrl : `${baseUrl}/mcp`;
    this.token = options.token;
    this.clientName = options.clientName ?? "grokptah-client";
    this.clientVersion = options.clientVersion ?? "0.1.0";
    this.fetcher = options.fetcher ?? globalThis.fetch.bind(globalThis);
  }

  get isInitialized(): boolean {
    return this.sessionId !== null && this.protocolVersion !== null;
  }

  get capabilities(): CapabilitySet | null {
    return this.capabilitySet;
  }

  get transportSessionId(): string | null {
    return this.sessionId;
  }

  async initialize(): Promise<unknown> {
    this.resetHandshake();
    try {
      const result = await this.rpc("initialize", {
        protocolVersion: "2025-03-26",
        capabilities: { tools: {} },
        clientInfo: { name: this.clientName, version: this.clientVersion },
      });
      if (!isRecord(result) || typeof result.protocolVersion !== "string") {
        throw new Error("GrokPtah initialize response has no protocolVersion");
      }
      this.protocolVersion = result.protocolVersion;
      const serverInfo = isRecord(result.serverInfo) ? result.serverInfo : null;
      this.capabilitySet = parseCapabilitySet(serverInfo?.capabilityContract);
      if (!this.capabilitySet) {
        throw new Error("GrokPtah initialize response has no valid capability contract");
      }
      await this.notify("notifications/initialized", {});
      return result;
    } catch (error) {
      this.resetHandshake();
      throw error;
    }
  }

  async listTools(): Promise<GrokPtahTool[]> {
    this.requireInitialized();
    const result = await this.rpc("tools/list", {});
    if (!isRecord(result) || !Array.isArray(result.tools)) {
      throw new Error("GrokPtah tools/list response is malformed");
    }
    return result.tools.map((tool) => {
      if (!isRecord(tool) || typeof tool.name !== "string" || !isRecord(tool.inputSchema)) {
        throw new Error("GrokPtah tools/list contains a malformed tool");
      }
      return {
        name: tool.name,
        description: typeof tool.description === "string" ? tool.description : undefined,
        inputSchema: tool.inputSchema,
      };
    });
  }

  async callTool(name: string, arguments_: Record<string, unknown>): Promise<GrokPtahCallResult> {
    this.requireInitialized();
    const result = await this.rpc("tools/call", { name, arguments: arguments_ });
    if (!isRecord(result)) {
      return { raw: result };
    }
    return {
      structuredContent: result.structuredContent,
      content: result.content,
      isError: result.isError === true,
      error: parseSafeError(result.error),
      raw: result,
    };
  }

  /**
   * Replay and follow one exact run scope over the bounded SSE channel.
   *
   * Event sequence numbers are checked for strict monotonicity. A recovery
   * notification is yielded and then the stream ends; callers must poll the
   * named tool before reconnecting, as required by the wire contract.
   */
  async *streamRunEvents(
    scope: GrokPtahRunScope,
    afterSeq?: number,
  ): AsyncGenerator<GrokPtahRunNotification> {
    this.requireInitialized();
    const url = new URL(this.mcpUrl);
    url.searchParams.set("session_id", scope.sessionId);
    url.searchParams.set("workspace", scope.workspace);
    url.searchParams.set("run_id", scope.runId);
    const headers = this.headers();
    headers.Accept = "text/event-stream";
    if (afterSeq !== undefined) headers["Last-Event-ID"] = String(afterSeq);
    const response = await this.fetcher(url, {
      method: "GET",
      headers,
    });
    if (!response.ok) {
      throw new Error(`GrokPtah event stream failed with HTTP ${response.status}`);
    }
    const contentType = response.headers.get("content-type") ?? "";
    if (!contentType.startsWith("text/event-stream")) {
      throw new Error(`GrokPtah event stream returned content type ${contentType || "<missing>"}`);
    }
    if (!response.body) throw new Error("GrokPtah event stream has no body");
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    const maxFrameBytes = 512 * 1024;
    let buffer = "";
    let lastSeq = afterSeq ?? 0;
    try {
      for (;;) {
        const chunk = await reader.read();
        buffer += decoder.decode(chunk.value ?? new Uint8Array(), { stream: !chunk.done });
        if (new TextEncoder().encode(buffer).byteLength > maxFrameBytes) {
          throw new Error("GrokPtah event stream frame exceeds 512 KiB");
        }
        for (;;) {
          const delimiter = findSseDelimiter(buffer);
          if (!delimiter) break;
          const frame = buffer.slice(0, delimiter.end);
          buffer = buffer.slice(delimiter.end + delimiter.length);
          const notification = parseSseNotification(frame);
          if (!notification) continue;
          if (notification.kind === "event") {
            if (
              notification.sessionId !== scope.sessionId ||
              notification.workspace !== scope.workspace ||
              notification.runId !== scope.runId
            ) {
              throw new Error("GrokPtah event stream scope does not match requested run");
            }
            if (notification.seq <= lastSeq) {
              throw new Error("GrokPtah event sequence is not strictly increasing");
            }
            lastSeq = notification.seq;
          } else if (notification.kind === "recovery") {
            if (
              notification.sessionId !== scope.sessionId ||
              notification.workspace !== scope.workspace ||
              notification.runId !== scope.runId
            ) {
              throw new Error("GrokPtah recovery scope does not match requested run");
            }
            yield notification;
            return;
          }
          yield notification;
        }
        if (chunk.done) {
          buffer += decoder.decode();
          if (buffer.trim().length > 0) {
            throw new Error("GrokPtah event stream ended with a partial SSE frame");
          }
          return;
        }
      }
    } finally {
      await reader.cancel().catch(() => undefined);
    }
  }

  async close(): Promise<void> {
    if (!this.sessionId) return;
    const response = await this.fetcher(this.mcpUrl, {
      method: "DELETE",
      headers: this.headers(),
    });
    if (!response.ok && response.status !== 204) {
      throw new Error(`GrokPtah session close failed with HTTP ${response.status}`);
    }
    this.sessionId = null;
    this.protocolVersion = null;
    this.capabilitySet = null;
  }

  private resetHandshake(): void {
    this.sessionId = null;
    this.protocolVersion = null;
    this.capabilitySet = null;
  }

  private requireInitialized(): void {
    if (!this.isInitialized) {
      throw new Error("GrokPtah client is not initialized");
    }
  }

  private headers(): Record<string, string> {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.token}`,
      Accept: "application/json, text/event-stream",
      "Content-Type": "application/json",
      "MCP-Protocol-Version": this.protocolVersion ?? "2025-03-26",
    };
    if (this.sessionId) headers["mcp-session-id"] = this.sessionId;
    return headers;
  }

  private async notify(method: string, params: Record<string, unknown>): Promise<void> {
    const response = await this.fetcher(this.mcpUrl, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({ jsonrpc: "2.0", method, params }),
    });
    if (!response.ok && response.status !== 202) {
      throw new Error(`GrokPtah notification failed with HTTP ${response.status}`);
    }
  }

  private async rpc(method: string, params: Record<string, unknown>): Promise<unknown> {
    const id = this.nextId++;
    const response = await this.fetcher(this.mcpUrl, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
    });
    const text = await response.text();
    let body: unknown = {};
    try {
      body = text ? JSON.parse(text) : {};
    } catch {
      if (!response.ok) {
        throw new Error(`GrokPtah MCP request failed with HTTP ${response.status}`);
      }
      throw new Error("GrokPtah MCP response is not valid JSON");
    }
    if (isRecord(body) && body.error) {
      throw new GrokPtahRemoteError(
        parseSafeError(body.error) ?? {
          code: "remote_error",
          message: "GrokPtah MCP request failed",
        },
      );
    }
    if (!isRecord(body) || body.jsonrpc !== "2.0" || body.id !== id) {
      throw new Error("GrokPtah MCP response correlation is invalid");
    }
    if (!response.ok) {
      throw new Error(`GrokPtah MCP request failed with HTTP ${response.status}`);
    }
    if (isRecord(body) && "result" in body) {
      const sessionId = response.headers.get("mcp-session-id");
      if (sessionId && this.sessionId && sessionId !== this.sessionId) {
        throw new Error("GrokPtah MCP response changed the transport session");
      }
      if (sessionId) this.sessionId = sessionId;
      return body.result;
    }
    throw new Error("GrokPtah MCP response has no result");
  }
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function findSseDelimiter(value: string): { end: number; length: number } | null {
  const crlf = value.indexOf("\r\n\r\n");
  const lf = value.indexOf("\n\n");
  if (crlf < 0 && lf < 0) return null;
  if (crlf >= 0 && (lf < 0 || crlf < lf)) return { end: crlf, length: 4 };
  return { end: lf, length: 2 };
}

function parseSseNotification(frame: string): GrokPtahRunNotification | null {
  let sseId: number | null = null;
  let data = "";
  for (const rawLine of frame.split("\n")) {
    const line = rawLine.endsWith("\r") ? rawLine.slice(0, -1) : rawLine;
    if (!line || line.startsWith(":")) continue;
    const separator = line.indexOf(":");
    const field = separator < 0 ? line : line.slice(0, separator);
    const value = separator < 0 ? "" : line.slice(separator + 1).replace(/^ /, "");
    if (field === "id") {
      const parsed = Number(value);
      if (!Number.isSafeInteger(parsed) || parsed < 0) {
        throw new Error("GrokPtah SSE id is not a non-negative integer");
      }
      sseId = parsed;
    } else if (field === "data") {
      data += `${data ? "\n" : ""}${value}`;
    }
  }
  if (!data) return null;
  const body: unknown = JSON.parse(data);
  if (!isRecord(body) || body.jsonrpc !== "2.0" || typeof body.method !== "string") {
    throw new Error("GrokPtah SSE frame is not a JSON-RPC notification");
  }
  const params = isRecord(body.params) ? body.params : {};
  if (body.method === "notifications/ptah_event") {
    if (
      typeof sseId !== "number" ||
      typeof params.sessionId !== "string" ||
      typeof params.workspace !== "string" ||
      typeof params.runId !== "string" ||
      typeof params.seq !== "number" ||
      typeof params.ts !== "string" ||
      !("update" in params)
    ) {
      throw new Error("GrokPtah event notification is malformed");
    }
    if (sseId !== params.seq) throw new Error("GrokPtah SSE id does not match event sequence");
    return {
      kind: "event",
      sseId,
      sessionId: params.sessionId,
      workspace: params.workspace,
      runId: params.runId,
      seq: params.seq,
      ts: params.ts,
      update: params.update,
    };
  }
  if (body.method === "notifications/ptah_recovery") {
    if (
      typeof params.sessionId !== "string" ||
      typeof params.workspace !== "string" ||
      typeof params.runId !== "string" ||
      typeof params.afterSeq !== "number" ||
      typeof params.reason !== "string" ||
      typeof params.pollTool !== "string"
    ) {
      throw new Error("GrokPtah recovery notification is malformed");
    }
    return {
      kind: "recovery",
      sseId,
      sessionId: params.sessionId,
      workspace: params.workspace,
      runId: params.runId,
      afterSeq: params.afterSeq,
      reason: params.reason,
      pollTool: params.pollTool,
    };
  }
  throw new Error(`GrokPtah SSE notification method is unsupported: ${body.method}`);
}

function parseSafeError(value: unknown): GrokPtahSafeError | undefined {
  if (!isRecord(value)) return undefined;
  const data = isRecord(value.data) ? value.data : {};
  const code = typeof data.code === "string" ? data.code : "remote_error";
  const rawMessage = typeof value.message === "string" ? value.message : "GrokPtah request failed";
  return {
    code: code.slice(0, 128),
    message: rawMessage.slice(0, 512),
    ...(typeof data.requestId === "string" ? { requestId: data.requestId.slice(0, 256) } : {}),
  };
}
