/**
 * Browser-safe ContextDesk broker client.
 *
 * This client deliberately has no token option and never talks to GrokPtah's
 * MCP endpoint. It uses the browser's authenticated broker session and opaque
 * binding/run ids defined by docs/WEB_BROKER_PROTOCOL.md.
 */

export type GrokPtahBrokerCapability = {
  id: string;
  availability: "available" | "gated" | "unavailable";
};

export type GrokPtahBrokerBinding = {
  bindingId: string;
  contract: string;
  expiresAt: string;
  capabilities: GrokPtahBrokerCapability[];
};

export type GrokPtahBrokerRun = {
  brokerRunId: string;
  bindingId: string;
};

export type GrokPtahBrokerRunRequest = {
  prompt: string;
  executionMode?: "shared" | "isolated_worktree";
  bounds?: {
    maxRounds?: number;
    maxDurationMs?: number;
    maxPromptBytes?: number;
  };
  allowQueue?: boolean;
};

export type GrokPtahBrokerEvent = {
  kind: "event";
  brokerRunId: string;
  seq: number;
  ts: string;
  update: unknown;
};

export type GrokPtahBrokerRecovery = {
  kind: "recovery";
  brokerRunId: string;
  afterSeq: number;
  reason: string;
  pollRoute: string;
};

export type GrokPtahBrokerNotification = GrokPtahBrokerEvent | GrokPtahBrokerRecovery;

export class GrokPtahBrokerError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId?: string;

  constructor(status: number, code: string, message: string, requestId?: string) {
    super(message);
    this.name = "GrokPtahBrokerError";
    this.status = status;
    this.code = code;
    this.requestId = requestId;
  }
}

export type GrokPtahBrokerClientOptions = {
  /** The ContextDesk origin or the `/api/grokptah/v1` route root. */
  baseUrl: string;
  fetcher?: typeof fetch;
  credentials?: RequestCredentials;
  /** Optional broker-issued CSRF token; never a GrokPtah bearer token. */
  csrfToken?: string;
};

/**
 * Minimal browser-facing client for a trusted ContextDesk broker.
 *
 * The broker, not this class, owns GrokPtah credentials, exact workspace
 * paths, policy, and promotion/Computer Use authority.
 */
export class GrokPtahBrokerClient {
  private readonly apiUrl: string;
  private readonly fetcher: typeof fetch;
  private readonly credentials: RequestCredentials;
  private readonly csrfToken: string | undefined;

  constructor(options: GrokPtahBrokerClientOptions) {
    const baseUrl = options.baseUrl.replace(/\/$/, "");
    this.apiUrl = baseUrl.endsWith("/api/grokptah/v1")
      ? baseUrl
      : `${baseUrl}/api/grokptah/v1`;
    this.fetcher = options.fetcher ?? globalThis.fetch.bind(globalThis);
    this.credentials = options.credentials ?? "include";
    this.csrfToken = options.csrfToken;
  }

  async createBinding(
    investigationId: string,
    workspace: string,
    requestedCapabilities: string[],
    idempotencyKey: string,
  ): Promise<GrokPtahBrokerBinding> {
    return this.request<GrokPtahBrokerBinding>("/bindings", {
      method: "POST",
      idempotencyKey,
      body: { investigationId, workspace, requestedCapabilities },
    });
  }

  async listSessions<T = unknown>(bindingId: string): Promise<T> {
    return this.request<T>(`/bindings/${segment(bindingId)}/sessions`);
  }

  async getCapacity<T = unknown>(bindingId: string): Promise<T> {
    return this.request<T>(`/bindings/${segment(bindingId)}/capacity`);
  }

  async submitRun<T = GrokPtahBrokerRun>(
    bindingId: string,
    request: GrokPtahBrokerRunRequest,
    idempotencyKey: string,
  ): Promise<T> {
    return this.request<T>(`/bindings/${segment(bindingId)}/runs`, {
      method: "POST",
      idempotencyKey,
      body: request,
    });
  }

  async getRun<T = unknown>(bindingId: string, brokerRunId: string): Promise<T> {
    return this.request<T>(this.runPath(bindingId, brokerRunId));
  }

  async getProgress<T = unknown>(bindingId: string, brokerRunId: string): Promise<T> {
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/progress`);
  }

  async getChanges<T = unknown>(bindingId: string, brokerRunId: string): Promise<T> {
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/changes`);
  }

  async getTests<T = unknown>(bindingId: string, brokerRunId: string): Promise<T> {
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/tests`);
  }

  async getHandoff<T = unknown>(bindingId: string, brokerRunId: string): Promise<T> {
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/handoff`);
  }

  async getReview<T = unknown>(bindingId: string, brokerRunId: string): Promise<T> {
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/review`);
  }

  async cancelRun<T = unknown>(
    bindingId: string,
    brokerRunId: string,
    idempotencyKey: string,
  ): Promise<T> {
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/cancel`, {
      method: "POST",
      idempotencyKey,
    });
  }

  async queuePrompt<T = unknown>(
    bindingId: string,
    prompt: string,
    idempotencyKey: string,
    priority?: boolean,
  ): Promise<T> {
    return this.request<T>(`/bindings/${segment(bindingId)}/queue`, {
      method: "POST",
      idempotencyKey,
      body: { prompt, ...(priority === undefined ? {} : { priority }) },
    });
  }

  async steer<T = unknown>(
    bindingId: string,
    text: string,
    idempotencyKey: string,
  ): Promise<T> {
    return this.request<T>(`/bindings/${segment(bindingId)}/steer`, {
      method: "POST",
      idempotencyKey,
      body: { text },
    });
  }

  /**
   * Follow the broker's redacted SSE stream. A recovery frame ends the stream;
   * callers must poll the advertised route before reconnecting.
   */
  async *streamEvents(
    bindingId: string,
    brokerRunId: string,
    afterSeq?: number,
  ): AsyncGenerator<GrokPtahBrokerNotification> {
    const url = new URL(
      `${this.apiUrl}${this.runPath(bindingId, brokerRunId)}/events`,
      globalThis.location?.origin ?? "http://localhost",
    );
    if (afterSeq !== undefined) url.searchParams.set("afterSeq", String(afterSeq));
    const headers: Record<string, string> = { Accept: "text/event-stream" };
    if (afterSeq !== undefined) headers["Last-Event-ID"] = String(afterSeq);
    const response = await this.fetcher(url, {
      method: "GET",
      headers,
      credentials: this.credentials,
    });
    if (!response.ok) await throwBrokerError(response);
    if (!(response.headers.get("content-type") ?? "").startsWith("text/event-stream")) {
      throw new GrokPtahBrokerError(response.status, "invalid_response", "Broker event stream is not SSE");
    }
    if (!response.body) {
      throw new GrokPtahBrokerError(response.status, "invalid_response", "Broker event stream has no body");
    }
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    const maxFrameBytes = 512 * 1024;
    let buffer = "";
    let lastSeq = afterSeq ?? 0;
    try {
      for (;;) {
        const chunk = await reader.read();
        buffer += decoder.decode(chunk.value ?? new Uint8Array(), { stream: !chunk.done });
        if (buffer.length > maxFrameBytes) throw new Error("Broker SSE frame exceeds 512 KiB");
        for (;;) {
          const delimiter = findDelimiter(buffer);
          if (!delimiter) break;
          const frame = buffer.slice(0, delimiter.end);
          buffer = buffer.slice(delimiter.end + delimiter.length);
          const notification = parseNotification(frame, brokerRunId);
          if (!notification) continue;
          if (notification.kind === "event") {
            if (notification.seq <= lastSeq) throw new Error("Broker event sequence is not increasing");
            lastSeq = notification.seq;
          } else {
            yield notification;
            return;
          }
          yield notification;
        }
        if (chunk.done) {
          buffer += decoder.decode();
          if (buffer.trim()) throw new Error("Broker SSE ended with a partial frame");
          return;
        }
      }
    } finally {
      await reader.cancel().catch(() => undefined);
    }
  }

  private runPath(bindingId: string, brokerRunId: string): string {
    return `/bindings/${segment(bindingId)}/runs/${segment(brokerRunId)}`;
  }

  private async request<T>(
    path: string,
    options: {
      method?: "GET" | "POST";
      body?: unknown;
      idempotencyKey?: string;
    } = {},
  ): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    if (options.body !== undefined) headers["Content-Type"] = "application/json";
    if (options.idempotencyKey) headers["Idempotency-Key"] = options.idempotencyKey;
    if (options.method === "POST" && this.csrfToken) headers["X-CSRF-Token"] = this.csrfToken;
    const response = await this.fetcher(`${this.apiUrl}${path}`, {
      method: options.method ?? "GET",
      headers,
      credentials: this.credentials,
      ...(options.body === undefined ? {} : { body: JSON.stringify(options.body) }),
    });
    if (!response.ok) await throwBrokerError(response);
    const text = await response.text();
    if (!text) return undefined as T;
    return JSON.parse(text) as T;
  }
}

function segment(value: string): string {
  if (!value.trim()) throw new Error("Broker identifier must not be empty");
  return encodeURIComponent(value);
}

async function throwBrokerError(response: Response): Promise<never> {
  let body: Record<string, unknown> = {};
  try {
    const parsed: unknown = await response.json();
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
      body = parsed as Record<string, unknown>;
    }
  } catch {
    // Preserve the stable HTTP status even when a proxy emits no JSON body.
  }
  throw new GrokPtahBrokerError(
    response.status,
    typeof body.code === "string" ? body.code : "http_error",
    typeof body.message === "string" ? body.message : `Broker request failed with HTTP ${response.status}`,
    typeof body.requestId === "string" ? body.requestId : undefined,
  );
}

function findDelimiter(value: string): { end: number; length: number } | null {
  const crlf = value.indexOf("\r\n\r\n");
  const lf = value.indexOf("\n\n");
  if (crlf < 0 && lf < 0) return null;
  if (crlf >= 0 && (lf < 0 || crlf < lf)) return { end: crlf, length: 4 };
  return { end: lf, length: 2 };
}

function parseNotification(frame: string, brokerRunId: string): GrokPtahBrokerNotification | null {
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
      if (!Number.isSafeInteger(parsed) || parsed < 0) throw new Error("Broker SSE id is invalid");
      sseId = parsed;
    } else if (field === "data") {
      data += `${data ? "\n" : ""}${value}`;
    }
  }
  if (!data) return null;
  const body: unknown = JSON.parse(data);
  if (!isRecord(body) || body.brokerRunId !== brokerRunId || typeof body.kind !== "string") {
    throw new Error("Broker SSE scope is invalid");
  }
  if (body.kind === "event") {
    if (
      typeof sseId !== "number" ||
      typeof body.seq !== "number" ||
      sseId !== body.seq ||
      typeof body.ts !== "string" ||
      !("update" in body)
    ) {
      throw new Error("Broker event notification is malformed");
    }
    return {
      kind: "event",
      brokerRunId,
      seq: body.seq,
      ts: body.ts,
      update: body.update,
    };
  }
  if (
    body.kind === "recovery" &&
    typeof body.afterSeq === "number" &&
    typeof body.reason === "string" &&
    typeof body.pollRoute === "string"
  ) {
    return {
      kind: "recovery",
      brokerRunId,
      afterSeq: body.afterSeq,
      reason: body.reason,
      pollRoute: body.pollRoute,
    };
  }
  throw new Error("Broker SSE notification kind is unknown");
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
