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

const MAX_BROKER_PROMPT_BYTES = 1_048_576;
const MAX_BROKER_ROUNDS = 24;
const MAX_BROKER_PROMPT_BOUND_BYTES = 4 * 1_048_576;

const BROKER_AVAILABILITIES: ReadonlySet<GrokPtahBrokerCapability["availability"]> = new Set([
  "available",
  "gated",
  "unavailable",
]);

/** The exact review evidence a broker must bind before issuing approval. */
export type GrokPtahBrokerChangedFile = {
  /** Repository-relative path from the authoritative review receipt. */
  path: string;
  /** Bounded human-readable summary from the authoritative review receipt. */
  summary: string;
};

export type GrokPtahBrokerApprovalRequest = {
  sourceFingerprint: string;
  finalFingerprint: string;
  changedFiles: GrokPtahBrokerChangedFile[];
  ttlMs?: number;
};

/** Opaque, short-lived approval returned by the trusted broker. */
export type GrokPtahBrokerApproval = {
  approvalId: string;
  bindingId: string;
  brokerRunId: string;
  sourceFingerprint: string;
  finalFingerprint: string;
  changedFiles: GrokPtahBrokerChangedFile[];
  expiresAt: string;
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

function boundedString(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.length > 0 && new TextEncoder().encode(value).byteLength <= maxBytes;
}

function hasOnlyKeys(value: Record<string, unknown>, keys: ReadonlySet<string>): boolean {
  return Object.keys(value).every((key) => keys.has(key));
}

/** Parse a broker binding without trusting a response's structural shape. */
export function parseBrokerBinding(value: unknown): GrokPtahBrokerBinding | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (!hasOnlyKeys(record, new Set(["bindingId", "contract", "expiresAt", "capabilities"]))) return null;
  if (
    !boundedString(record.bindingId, 256) ||
    !boundedString(record.contract, 128) ||
    !boundedString(record.expiresAt, 128) ||
    !Array.isArray(record.capabilities) ||
    record.capabilities.length > 64
  ) return null;
  const capabilities: GrokPtahBrokerCapability[] = [];
  for (const value of record.capabilities) {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
    const capability = value as Record<string, unknown>;
    if (!hasOnlyKeys(capability, new Set(["id", "availability"]))) return null;
    if (
      !boundedString(capability.id, 128) ||
      typeof capability.availability !== "string" ||
      !BROKER_AVAILABILITIES.has(capability.availability as GrokPtahBrokerCapability["availability"])
    ) return null;
    capabilities.push({
      id: capability.id,
      availability: capability.availability as GrokPtahBrokerCapability["availability"],
    });
  }
  const ids = capabilities.map(({ id }) => id);
  if (new Set(ids).size !== ids.length) return null;
  return {
    bindingId: record.bindingId,
    contract: record.contract,
    expiresAt: record.expiresAt,
    capabilities,
  };
}

/** Parse an opaque broker run envelope without trusting a response's shape. */
export function parseBrokerRun(value: unknown): GrokPtahBrokerRun | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (
    !hasOnlyKeys(record, new Set(["brokerRunId", "bindingId"])) ||
    !boundedString(record.brokerRunId, 256) ||
    !boundedString(record.bindingId, 256)
  ) return null;
  return { brokerRunId: record.brokerRunId, bindingId: record.bindingId };
}

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
  /** Broker-issued CSRF token required for every mutating request. */
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
    const csrfToken = options.csrfToken?.trim();
    this.csrfToken = csrfToken || undefined;
  }

  async createBinding(
    investigationId: string,
    workspace: string,
    requestedCapabilities: string[],
    idempotencyKey: string,
  ): Promise<GrokPtahBrokerBinding> {
    return this.requestValidated("/bindings", parseBrokerBinding, {
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

  async submitRun(
    bindingId: string,
    request: GrokPtahBrokerRunRequest,
    idempotencyKey: string,
  ): Promise<GrokPtahBrokerRun> {
    validateRunRequest(request);
    return this.requestValidated(`/bindings/${segment(bindingId)}/runs`, parseBrokerRun, {
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

  /**
   * Ask the broker to create a short-lived approval for the exact review
   * evidence shown to the user. This does not promote anything by itself.
   */
  async approveRun<T = GrokPtahBrokerApproval>(
    bindingId: string,
    brokerRunId: string,
    request: GrokPtahBrokerApprovalRequest,
    idempotencyKey: string,
  ): Promise<T> {
    validateApprovalRequest(request);
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/approve`, {
      method: "POST",
      idempotencyKey,
      body: request,
    });
  }

  /**
   * Consume a broker-issued approval. The broker remains responsible for
   * checking expiry, fingerprints, capability policy, and the desktop human
   * gate before asking GrokPtah to promote.
   */
  async promoteRun<T = unknown>(
    bindingId: string,
    brokerRunId: string,
    approvalId: string,
    idempotencyKey: string,
  ): Promise<T> {
    return this.request<T>(`${this.runPath(bindingId, brokerRunId)}/promote`, {
      method: "POST",
      idempotencyKey,
      body: { approvalId },
    });
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
    validateBoundedText(prompt, "Queue prompt");
    if (priority !== undefined && typeof priority !== "boolean") {
      throw new GrokPtahBrokerError(0, "invalid_request", "Queue priority must be boolean");
    }
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
    validateBoundedText(text, "Steer text");
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
        if (new TextEncoder().encode(buffer).byteLength > maxFrameBytes) {
          throw new Error("Broker SSE frame exceeds 512 KiB");
        }
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
    const method = options.method ?? "GET";
    if (method === "POST") {
      if (!this.csrfToken) {
        throw new GrokPtahBrokerError(
          0,
          "csrf_required",
          "A broker-issued CSRF token is required for mutating requests",
        );
      }
      if (!options.idempotencyKey?.trim()) {
        throw new GrokPtahBrokerError(
          0,
          "idempotency_required",
          "A non-empty idempotency key is required for mutating requests",
        );
      }
    }
    if (options.body !== undefined) headers["Content-Type"] = "application/json";
    if (options.idempotencyKey) headers["Idempotency-Key"] = options.idempotencyKey.trim();
    if (method === "POST" && this.csrfToken) headers["X-CSRF-Token"] = this.csrfToken;
    const response = await this.fetcher(`${this.apiUrl}${path}`, {
      method,
      headers,
      credentials: this.credentials,
      ...(options.body === undefined ? {} : { body: JSON.stringify(options.body) }),
    });
    if (!response.ok) await throwBrokerError(response);
    const text = await response.text();
    if (!text) return undefined as T;
    return JSON.parse(text) as T;
  }

  private async requestValidated<T>(
    path: string,
    parser: (value: unknown) => T | null,
    options: {
      method?: "GET" | "POST";
      body?: unknown;
      idempotencyKey?: string;
    },
  ): Promise<T> {
    const value = await this.request<unknown>(path, options);
    const parsed = parser(value);
    if (parsed === null) {
      throw new GrokPtahBrokerError(0, "invalid_response", "Broker response shape is invalid");
    }
    return parsed;
  }
}

function segment(value: string): string {
  if (!value.trim()) throw new Error("Broker identifier must not be empty");
  return encodeURIComponent(value);
}

function validateApprovalRequest(request: GrokPtahBrokerApprovalRequest): void {
  if (
    typeof request.sourceFingerprint !== "string" ||
    typeof request.finalFingerprint !== "string" ||
    !request.sourceFingerprint.trim() ||
    !request.finalFingerprint.trim()
  ) {
    throw new GrokPtahBrokerError(
      0,
      "invalid_request",
      "Approval fingerprints must not be empty",
    );
  }
  if (!Array.isArray(request.changedFiles)) {
    throw new GrokPtahBrokerError(
      0,
      "invalid_request",
      "Approval changed files must be an array",
    );
  }
  if (
    request.ttlMs !== undefined &&
    (!Number.isSafeInteger(request.ttlMs) || request.ttlMs < 1 || request.ttlMs > 900_000)
  ) {
    throw new GrokPtahBrokerError(
      0,
      "invalid_request",
      "Approval ttlMs must be an integer between 1 and 900000",
    );
  }
  for (const file of request.changedFiles) {
    if (
      file === null ||
      typeof file !== "object" ||
      typeof file.path !== "string" ||
      typeof file.summary !== "string" ||
      !file.path.trim() ||
      file.path.startsWith("/") ||
      file.path.includes("..") ||
      new TextEncoder().encode(file.summary).byteLength > 512
    ) {
      throw new GrokPtahBrokerError(
        0,
        "invalid_request",
        "Approval changed files must be bounded repository-relative summaries",
      );
    }
  }
}

function validateRunRequest(request: GrokPtahBrokerRunRequest): void {
  if (request === null || typeof request !== "object" || Array.isArray(request)) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Run request must be an object");
  }
  const value = request as Record<string, unknown>;
  if (!hasOnlyKeys(value, new Set(["prompt", "executionMode", "bounds", "allowQueue"]))) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Run request contains unknown fields");
  }
  validateBoundedText(value.prompt, "Run prompt");
  if (
    value.executionMode !== undefined &&
    value.executionMode !== "shared" &&
    value.executionMode !== "isolated_worktree"
  ) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Run execution mode is invalid");
  }
  if (value.allowQueue !== undefined && typeof value.allowQueue !== "boolean") {
    throw new GrokPtahBrokerError(0, "invalid_request", "Run allowQueue must be boolean");
  }
  if (value.bounds === undefined) return;
  if (typeof value.bounds !== "object" || value.bounds === null || Array.isArray(value.bounds)) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Run bounds must be an object");
  }
  const bounds = value.bounds as Record<string, unknown>;
  if (!hasOnlyKeys(bounds, new Set(["maxRounds", "maxDurationMs", "maxPromptBytes"]))) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Run bounds contain unknown fields");
  }
  const positiveFields: Array<[string, unknown]> = [
    ["maxRounds", bounds.maxRounds],
    ["maxDurationMs", bounds.maxDurationMs],
    ["maxPromptBytes", bounds.maxPromptBytes],
  ];
  for (const [name, field] of positiveFields) {
    if (
      field !== undefined &&
      (typeof field !== "number" || !Number.isSafeInteger(field) || field <= 0)
    ) {
      throw new GrokPtahBrokerError(0, "invalid_request", `Run ${name} must be a positive safe integer`);
    }
  }
  if (typeof bounds.maxRounds === "number" && bounds.maxRounds > MAX_BROKER_ROUNDS) {
    throw new GrokPtahBrokerError(0, "invalid_request", `Run maxRounds must be at most ${MAX_BROKER_ROUNDS}`);
  }
  if (
    typeof bounds.maxPromptBytes === "number" &&
    bounds.maxPromptBytes > MAX_BROKER_PROMPT_BOUND_BYTES
  ) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Run maxPromptBytes exceeds the broker ceiling");
  }
}

function validateBoundedText(value: unknown, label: string): asserts value is string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new GrokPtahBrokerError(0, "invalid_request", `${label} must not be empty`);
  }
  if (new TextEncoder().encode(value).byteLength > MAX_BROKER_PROMPT_BYTES) {
    throw new GrokPtahBrokerError(0, "invalid_request", `${label} exceeds the broker byte ceiling`);
  }
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
    typeof body.code === "string" ? body.code.slice(0, 128) : "http_error",
    typeof body.message === "string"
      ? body.message.slice(0, 512)
      : `Broker request failed with HTTP ${response.status}`,
    typeof body.requestId === "string" ? body.requestId.slice(0, 256) : undefined,
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
    if (!isRelativeRoute(body.pollRoute)) {
      throw new Error("Broker recovery route must be relative to the broker origin");
    }
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

function isRelativeRoute(route: string): boolean {
  if (!route.startsWith("/") || route.startsWith("//")) return false;
  try {
    return new URL(route, "http://grokptah.invalid").origin === "http://grokptah.invalid";
  } catch {
    return false;
  }
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
