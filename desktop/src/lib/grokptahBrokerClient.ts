/**
 * Browser-safe ContextDesk broker client.
 *
 * This client deliberately has no token option and never talks to GrokPtah's
 * MCP endpoint. It uses the browser's authenticated broker session and opaque
 * binding/run ids defined by docs/WEB_BROKER_PROTOCOL.md.
 */

import {
  parseExternalWorkerArtifact,
  parseExternalWorkerFollowUpRequest,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerRecord,
  parseExternalWorkerRunRecord,
  type ExternalWorkerArtifact,
  type ExternalWorkerFollowUpRequest,
  type ExternalWorkerLaunchRequest,
  type ExternalWorkerLaunchResult,
  type ExternalWorkerRecord,
  type ExternalWorkerRunRecord,
} from "./externalWorker";

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

export type GrokPtahBrokerRunState =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "limit_reached";

/** Redacted, browser-safe status projection for a bound run. */
export type GrokPtahBrokerRunProjection = {
  brokerRunId: string;
  bindingId: string;
  state: GrokPtahBrokerRunState;
  promptPreview: string;
  createdAt: string;
  updatedAt: string;
  progress?: {
    round: number;
    maxRounds: number;
    lastTool?: string | null;
    detail: string;
    updatedAt: string;
  } | null;
  terminalResult?: string | null;
  errorCode?: string | null;
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
const MAX_BROKER_SSE_REASON_BYTES = 256;
const MAX_BROKER_SSE_ROUTE_BYTES = 2_048;
const MAX_BROKER_EVENT_TYPE_BYTES = 64;
const MAX_BROKER_EVENT_DETAIL_BYTES = 2_048;
const MAX_BROKER_ID_BYTES = 256;
const MAX_BROKER_IDEMPOTENCY_BYTES = 256;
const MAX_BROKER_CAPABILITIES = 64;
const MAX_BROKER_CAPABILITY_ID_BYTES = 128;
const MAX_BROKER_CHANGED_FILES = 256;
const MAX_BROKER_FINGERPRINT_BYTES = 256;
const MAX_BROKER_PATH_BYTES = 512;
const MAX_BROKER_DIFF_BYTES = 1 * 1_048_576;
const MAX_BROKER_JSON_RESPONSE_BYTES = 4 * 1_048_576;
const MAX_BROKER_ERROR_RESPONSE_BYTES = 64 * 1_024;
const MAX_BROKER_CSRF_BYTES = 256;
const BROKER_CAPABILITY_ID = /^[a-z][a-z0-9]*(\.[a-z][a-z0-9_]*)+$/;
const BROKER_RUN_STATES: ReadonlySet<GrokPtahBrokerRunState> = new Set([
  "queued",
  "running",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "limit_reached",
]);
const BROKER_EVENT_UPDATE_KEYS = new Set([
  "type",
  "detail",
  "round",
  "maxRounds",
  "lastTool",
  "state",
  "terminalResult",
  "errorCode",
  "updatedAt",
]);
const BROKER_EVENT_TYPE = /^[a-z][a-z0-9_.-]{0,63}$/;
const PRIVILEGED_TEXT_NEEDLE = /(?:\/(?:users|private|var|tmp|home|volumes)\/|https?:\/\/|(?:^|[\s=:])(authorization|bearer|api[_ -]?key|xai_api_key|grokptah_home|clipboard|private[_ -]?key|secret(?:[_ -]?key)?)(?:[\s=:]|$))/i;

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

/** Redacted review evidence that is safe for a browser approval screen. */
export type GrokPtahBrokerReviewProjection = {
  changedFiles: GrokPtahBrokerChangedFile[];
  diff: string;
  diffTruncated: boolean;
  fingerprint: string;
};

export type GrokPtahBrokerEvent = {
  kind: "event";
  brokerRunId: string;
  seq: number;
  ts: string;
  update: GrokPtahBrokerEventUpdate;
};

/** Browser-safe, bounded event data; raw command output never crosses this boundary. */
export type GrokPtahBrokerEventUpdate = {
  type: string;
  detail?: string;
  round?: number;
  maxRounds?: number;
  lastTool?: string | null;
  state?: GrokPtahBrokerRunState;
  terminalResult?: string | null;
  errorCode?: string | null;
  updatedAt?: string;
};

export type GrokPtahBrokerRecovery = {
  kind: "recovery";
  brokerRunId: string;
  afterSeq: number;
  reason: string;
  pollRoute: string;
};

export type GrokPtahBrokerNotification = GrokPtahBrokerEvent | GrokPtahBrokerRecovery;

function parseChangedFiles(value: unknown): GrokPtahBrokerChangedFile[] | null {
  if (!Array.isArray(value) || value.length > MAX_BROKER_CHANGED_FILES) return null;
  const changedFiles: GrokPtahBrokerChangedFile[] = [];
  for (const file of value) {
    if (
      typeof file !== "object" ||
      file === null ||
      Array.isArray(file) ||
      !hasOnlyKeys(file as Record<string, unknown>, new Set(["path", "summary"]))
    ) {
      return null;
    }
    const record = file as Record<string, unknown>;
    if (
      typeof record.path !== "string" ||
      typeof record.summary !== "string" ||
      !boundedString(record.path, MAX_BROKER_PATH_BYTES) ||
      !boundedString(record.summary, 512) ||
      record.path.startsWith("/") ||
      record.path.includes("\\") ||
      record.path.includes("..")
    ) {
      return null;
    }
    changedFiles.push({ path: record.path, summary: record.summary });
  }
  return changedFiles;
}

function boundedString(value: unknown, maxBytes: number): value is string {
  return (
    typeof value === "string" &&
    value.trim().length > 0 &&
    new TextEncoder().encode(value).byteLength <= maxBytes
  );
}

function safePublicString(value: unknown, maxBytes: number): value is string {
  return boundedString(value, maxBytes) && !PRIVILEGED_TEXT_NEEDLE.test(value);
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
      !BROKER_CAPABILITY_ID.test(capability.id) ||
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

/** Parse an approval envelope without trusting broker response contents. */
export function parseBrokerApproval(value: unknown): GrokPtahBrokerApproval | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (
    !hasOnlyKeys(
      record,
      new Set([
        "approvalId",
        "bindingId",
        "brokerRunId",
        "sourceFingerprint",
        "finalFingerprint",
        "changedFiles",
        "expiresAt",
      ]),
    ) ||
    !boundedString(record.approvalId, MAX_BROKER_ID_BYTES) ||
    !boundedString(record.bindingId, MAX_BROKER_ID_BYTES) ||
    !boundedString(record.brokerRunId, MAX_BROKER_ID_BYTES) ||
    !boundedString(record.sourceFingerprint, MAX_BROKER_FINGERPRINT_BYTES) ||
    !boundedString(record.finalFingerprint, MAX_BROKER_FINGERPRINT_BYTES) ||
    !boundedString(record.expiresAt, 128)
  ) {
    return null;
  }
  const changedFiles = parseChangedFiles(record.changedFiles);
  if (changedFiles === null) return null;
  return {
    approvalId: record.approvalId,
    bindingId: record.bindingId,
    brokerRunId: record.brokerRunId,
    sourceFingerprint: record.sourceFingerprint,
    finalFingerprint: record.finalFingerprint,
    changedFiles,
    expiresAt: record.expiresAt,
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

function parseRunProgress(value: unknown): GrokPtahBrokerRunProjection["progress"] | null | undefined {
  if (value === null || value === undefined) return value;
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  const record = value as Record<string, unknown>;
  if (!hasOnlyKeys(record, new Set(["round", "maxRounds", "lastTool", "detail", "updatedAt"]))) {
    return undefined;
  }
  if (
    typeof record.round !== "number" ||
    !Number.isSafeInteger(record.round) ||
    record.round < 0 ||
    typeof record.maxRounds !== "number" ||
    !Number.isSafeInteger(record.maxRounds) ||
    record.maxRounds < 1 ||
    record.maxRounds > MAX_BROKER_ROUNDS ||
    record.round > record.maxRounds ||
    !safePublicString(record.detail, 2_048) ||
    !safePublicString(record.updatedAt, 128)
  ) {
    return undefined;
  }
  if (
    record.lastTool !== undefined &&
    record.lastTool !== null &&
    !safePublicString(record.lastTool, 256)
  ) {
    return undefined;
  }
  return {
    round: record.round,
    maxRounds: record.maxRounds,
    lastTool: record.lastTool as string | null | undefined,
    detail: record.detail,
    updatedAt: record.updatedAt,
  };
}

/** Parse the minimal redacted run projection exposed to browser consumers. */
export function parseBrokerRunProjection(value: unknown): GrokPtahBrokerRunProjection | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (
    !hasOnlyKeys(
      record,
      new Set([
        "brokerRunId",
        "bindingId",
        "state",
        "promptPreview",
        "createdAt",
        "updatedAt",
        "progress",
        "terminalResult",
        "errorCode",
      ]),
    ) ||
    !boundedString(record.brokerRunId, MAX_BROKER_ID_BYTES) ||
    !boundedString(record.bindingId, MAX_BROKER_ID_BYTES) ||
    typeof record.state !== "string" ||
    !BROKER_RUN_STATES.has(record.state as GrokPtahBrokerRunState) ||
    !safePublicString(record.promptPreview, 512) ||
    !safePublicString(record.createdAt, 128) ||
    !safePublicString(record.updatedAt, 128)
  ) {
    return null;
  }
  const progress = parseRunProgress(record.progress);
  if (record.progress !== undefined && progress === undefined) return null;
  for (const [key, maxBytes] of [["terminalResult", 512], ["errorCode", 128]] as const) {
    const field = record[key];
    if (field !== undefined && field !== null && !safePublicString(field, maxBytes)) return null;
  }
  return {
    brokerRunId: record.brokerRunId,
    bindingId: record.bindingId,
    state: record.state as GrokPtahBrokerRunState,
    promptPreview: record.promptPreview,
    createdAt: record.createdAt,
    updatedAt: record.updatedAt,
    ...(record.progress === undefined ? {} : { progress }),
    ...(record.terminalResult === undefined ? {} : { terminalResult: record.terminalResult as string | null }),
    ...(record.errorCode === undefined ? {} : { errorCode: record.errorCode as string | null }),
  };
}

/** Parse review evidence without exposing unbounded diffs or privileged fields. */
export function parseBrokerReviewProjection(value: unknown): GrokPtahBrokerReviewProjection | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (
    !hasOnlyKeys(record, new Set(["changedFiles", "diff", "diffTruncated", "fingerprint"])) ||
    typeof record.diff !== "string" ||
    new TextEncoder().encode(record.diff).byteLength > MAX_BROKER_DIFF_BYTES ||
    typeof record.diffTruncated !== "boolean" ||
    !boundedString(record.fingerprint, MAX_BROKER_FINGERPRINT_BYTES)
  ) {
    return null;
  }
  const changedFiles = parseChangedFiles(record.changedFiles);
  if (changedFiles === null) return null;
  return {
    changedFiles,
    diff: record.diff,
    diffTruncated: record.diffTruncated,
    fingerprint: record.fingerprint,
  };
}

/** Parse one browser-safe, bounded event update before exposing it to a UI. */
export function parseBrokerEventUpdate(value: unknown): GrokPtahBrokerEventUpdate | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (!hasOnlyKeys(record, BROKER_EVENT_UPDATE_KEYS)) return null;
  if (
    !safePublicString(record.type, MAX_BROKER_EVENT_TYPE_BYTES) ||
    !BROKER_EVENT_TYPE.test(record.type)
  ) return null;
  for (const [key, maxBytes] of [
    ["detail", MAX_BROKER_EVENT_DETAIL_BYTES],
    ["updatedAt", 128],
  ] as const) {
    const field = record[key];
    if (field !== undefined && !safePublicString(field, maxBytes)) return null;
  }
  for (const [key, maxBytes] of [
    ["lastTool", 256],
    ["terminalResult", 512],
    ["errorCode", 128],
  ] as const) {
    const field = record[key];
    if (field !== undefined && field !== null && !safePublicString(field, maxBytes)) return null;
  }
  for (const key of ["round", "maxRounds"] as const) {
    const field = record[key];
    if (
      field !== undefined &&
      (typeof field !== "number" || !Number.isSafeInteger(field) || field < 0 || field > MAX_BROKER_ROUNDS)
    ) return null;
  }
  if (
    typeof record.round === "number" &&
    typeof record.maxRounds === "number" &&
    record.round > record.maxRounds
  ) return null;
  if (
    record.state !== undefined &&
    (typeof record.state !== "string" || !BROKER_RUN_STATES.has(record.state as GrokPtahBrokerRunState))
  ) return null;
  return {
    type: record.type,
    ...(record.detail === undefined ? {} : { detail: record.detail as string }),
    ...(record.round === undefined ? {} : { round: record.round as number }),
    ...(record.maxRounds === undefined ? {} : { maxRounds: record.maxRounds as number }),
    ...(record.lastTool === undefined ? {} : { lastTool: record.lastTool as string | null }),
    ...(record.state === undefined ? {} : { state: record.state as GrokPtahBrokerRunState }),
    ...(record.terminalResult === undefined ? {} : { terminalResult: record.terminalResult as string | null }),
    ...(record.errorCode === undefined ? {} : { errorCode: record.errorCode as string | null }),
    ...(record.updatedAt === undefined ? {} : { updatedAt: record.updatedAt as string }),
  };
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
    if (csrfToken && new TextEncoder().encode(csrfToken).byteLength > MAX_BROKER_CSRF_BYTES) {
      throw new GrokPtahBrokerError(0, "invalid_request", "Broker CSRF token exceeds the byte ceiling");
    }
    this.csrfToken = csrfToken || undefined;
  }

  async createBinding(
    investigationId: string,
    workspace: string,
    requestedCapabilities: string[],
    idempotencyKey: string,
  ): Promise<GrokPtahBrokerBinding> {
    validateBindingRequest(investigationId, workspace, requestedCapabilities);
    return this.requestValidated("/bindings", parseBrokerBinding, {
      method: "POST",
      idempotencyKey,
      body: { investigationId, workspace, requestedCapabilities: [...requestedCapabilities] },
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
    const run = await this.requestValidated(`/bindings/${segment(bindingId)}/runs`, parseBrokerRun, {
      method: "POST",
      idempotencyKey,
      body: request,
    });
    if (run.bindingId !== bindingId) {
      throw new GrokPtahBrokerError(0, "invalid_response", "Broker run binding does not match the request");
    }
    return run;
  }

  /** Launch an isolated external coding worker through the trusted broker. */
  async launchExternalWorker(
    bindingId: string,
    request: ExternalWorkerLaunchRequest,
    idempotencyKey: string,
  ): Promise<ExternalWorkerLaunchResult> {
    if (parseExternalWorkerLaunchRequest(request) === null) {
      throw new GrokPtahBrokerError(0, "invalid_request", "External worker launch request is invalid");
    }
    if (request.requestId !== idempotencyKey) {
      throw new GrokPtahBrokerError(0, "invalid_request", "External worker requestId must match Idempotency-Key");
    }
    const result = await this.requestValidated(
      `/bindings/${segment(bindingId)}/external-workers`,
      parseExternalWorkerLaunchResult,
      { method: "POST", idempotencyKey, body: request },
    );
    if (
      result.worker.externalAgentId !== result.run.externalAgentId ||
      result.worker.repository !== request.repository ||
      result.worker.startingRef !== request.startingRef ||
      result.run.stream !== "unsupported" ||
      result.run.lastSeq !== null
    ) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker launch does not match the request");
    }
    return result;
  }

  /** Queue a bounded follow-up run on an existing external worker. */
  async followUpExternalWorker(
    bindingId: string,
    externalAgentId: string,
    request: ExternalWorkerFollowUpRequest,
    idempotencyKey: string,
  ): Promise<ExternalWorkerRunRecord> {
    if (parseExternalWorkerFollowUpRequest(request) === null) {
      throw new GrokPtahBrokerError(0, "invalid_request", "External worker follow-up request is invalid");
    }
    if (request.requestId !== idempotencyKey) {
      throw new GrokPtahBrokerError(0, "invalid_request", "External worker requestId must match Idempotency-Key");
    }
    const worker = await this.getExternalWorker(bindingId, externalAgentId);
    if (["unknown", "failed", "cancelled", "archived"].includes(worker.state)) {
      throw new GrokPtahBrokerError(0, "invalid_request", "External worker is not eligible for follow-up");
    }
    const run = await this.requestValidated(
      `/bindings/${segment(bindingId)}/external-workers/${segment(externalAgentId)}/runs`,
      parseExternalWorkerRunRecord,
      { method: "POST", idempotencyKey, body: request },
    );
    if (run.externalAgentId !== externalAgentId || run.stream !== "unsupported" || run.lastSeq !== null) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker follow-up does not match the request");
    }
    return run;
  }

  /** Read a redacted external-worker identity from the broker. */
  async getExternalWorker(bindingId: string, externalAgentId: string): Promise<ExternalWorkerRecord> {
    const worker = await this.requestValidated(
      `/bindings/${segment(bindingId)}/external-workers/${segment(externalAgentId)}`,
      parseExternalWorkerRecord,
      {},
    );
    if (worker.externalAgentId !== externalAgentId) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker identity does not match the request");
    }
    return worker;
  }

  /** Read the redacted current run projection for an external worker. */
  async getExternalWorkerRun(
    bindingId: string,
    externalAgentId: string,
    externalRunId: string,
  ): Promise<ExternalWorkerRunRecord> {
    const run = await this.requestValidated(
      `/bindings/${segment(bindingId)}/external-workers/${segment(externalAgentId)}/runs/${segment(externalRunId)}`,
      parseExternalWorkerRunRecord,
      {},
    );
    if (
      run.externalAgentId !== externalAgentId ||
      run.externalRunId !== externalRunId ||
      run.stream !== "unsupported" ||
      run.lastSeq !== null
    ) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker run identity does not match the request");
    }
    return run;
  }

  /** Read bounded relative artifacts returned by an external worker. */
  async getExternalWorkerArtifacts(
    bindingId: string,
    externalAgentId: string,
    externalRunId: string,
  ): Promise<ExternalWorkerArtifact[]> {
    const value = await this.request<unknown>(
      `/bindings/${segment(bindingId)}/external-workers/${segment(externalAgentId)}/runs/${segment(externalRunId)}/artifacts`,
    );
    if (!Array.isArray(value)) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker artifacts response is invalid");
    }
    const artifacts = value.map(parseExternalWorkerArtifact);
    if (artifacts.some((artifact) => artifact === null)) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker artifacts response is invalid");
    }
    const parsed = artifacts as ExternalWorkerArtifact[];
    if (parsed.some((artifact) => artifact.runId !== externalRunId)) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker artifact is not attributed to the requested run");
    }
    return parsed;
  }

  /** Cancel an external worker run; cancellation remains terminal. */
  async cancelExternalWorker(
    bindingId: string,
    externalAgentId: string,
    externalRunId: string,
    idempotencyKey: string,
  ): Promise<ExternalWorkerRunRecord> {
    const result = await this.requestValidated(
      `/bindings/${segment(bindingId)}/external-workers/${segment(externalAgentId)}/runs/${segment(externalRunId)}/cancel`,
      parseExternalWorkerRunRecord,
      { method: "POST", idempotencyKey },
    );
    if (
      result.state !== "cancelled" ||
      result.externalAgentId !== externalAgentId ||
      result.externalRunId !== externalRunId ||
      result.stream !== "unsupported" ||
      result.lastSeq !== null
    ) {
      throw new GrokPtahBrokerError(0, "invalid_response", "External worker cancellation was not terminal");
    }
    return result;
  }

  async getRun<T = unknown>(bindingId: string, brokerRunId: string): Promise<T> {
    return this.request<T>(this.runPath(bindingId, brokerRunId));
  }

  /**
   * Fetch the strict redacted run projection. Consumers that need an
   * evidence-bearing status view should prefer this over the legacy generic
   * `getRun<T>` method.
   */
  async getRunProjection(
    bindingId: string,
    brokerRunId: string,
  ): Promise<GrokPtahBrokerRunProjection> {
    const projection = await this.requestValidated(
      this.runPath(bindingId, brokerRunId),
      parseBrokerRunProjection,
      {},
    );
    if (projection.bindingId !== bindingId || projection.brokerRunId !== brokerRunId) {
      throw new GrokPtahBrokerError(0, "invalid_response", "Broker run projection scope does not match the request");
    }
    return projection;
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

  /** Fetch bounded review evidence suitable for a browser approval surface. */
  async getReviewProjection(
    bindingId: string,
    brokerRunId: string,
  ): Promise<GrokPtahBrokerReviewProjection> {
    return this.requestValidated(
      `${this.runPath(bindingId, brokerRunId)}/review`,
      parseBrokerReviewProjection,
      {},
    );
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
    const approval = await this.requestValidated(
      `${this.runPath(bindingId, brokerRunId)}/approve`,
      parseBrokerApproval,
      {
        method: "POST",
        idempotencyKey,
        body: request,
      },
    );
    if (approval.bindingId !== bindingId || approval.brokerRunId !== brokerRunId) {
      throw new GrokPtahBrokerError(0, "invalid_response", "Broker approval scope does not match the request");
    }
    return approval as T;
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
    validateOpaqueText(approvalId, "Approval id", MAX_BROKER_ID_BYTES);
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
    let sequenceInitialized = afterSeq !== undefined;
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
            if (sequenceInitialized && notification.seq !== lastSeq + 1) {
              throw new Error("Broker event sequence has a gap or is not increasing");
            }
            lastSeq = notification.seq;
            sequenceInitialized = true;
          } else {
            if (notification.afterSeq < (sequenceInitialized ? lastSeq : afterSeq ?? 0)) {
              throw new Error("Broker recovery sequence is behind the observed cursor");
            }
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
      if (new TextEncoder().encode(options.idempotencyKey).byteLength > MAX_BROKER_IDEMPOTENCY_BYTES) {
        throw new GrokPtahBrokerError(
          0,
          "invalid_request",
          "Idempotency key exceeds the broker byte ceiling",
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
    if (new TextEncoder().encode(text).byteLength > MAX_BROKER_JSON_RESPONSE_BYTES) {
      throw new GrokPtahBrokerError(
        response.status,
        "invalid_response",
        "Broker JSON response exceeds the byte ceiling",
      );
    }
    if (!text) return undefined as T;
    try {
      return JSON.parse(text) as T;
    } catch {
      throw new GrokPtahBrokerError(
        response.status,
        "invalid_response",
        "Broker response was not valid JSON",
      );
    }
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
  if (!boundedString(value, MAX_BROKER_ID_BYTES)) {
    throw new GrokPtahBrokerError(
      0,
      "invalid_request",
      "Broker identifier must be a bounded non-empty string",
    );
  }
  return encodeURIComponent(value);
}

function validateBindingRequest(
  investigationId: unknown,
  workspace: unknown,
  requestedCapabilities: unknown,
): asserts investigationId is string {
  validateOpaqueText(investigationId, "Investigation id", MAX_BROKER_ID_BYTES);
  if (!boundedString(workspace, MAX_BROKER_ID_BYTES)) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Workspace alias must be bounded and non-empty");
  }
  if (workspace.includes("/") || workspace.includes("\\") || workspace.includes("..")) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Workspace must be an opaque alias, not a path");
  }
  if (!Array.isArray(requestedCapabilities) || requestedCapabilities.length > MAX_BROKER_CAPABILITIES) {
    throw new GrokPtahBrokerError(0, "invalid_request", "Requested capabilities are invalid or too numerous");
  }
  const ids = new Set<string>();
  for (const capability of requestedCapabilities) {
    if (
      !boundedString(capability, MAX_BROKER_CAPABILITY_ID_BYTES) ||
      !BROKER_CAPABILITY_ID.test(capability) ||
      ids.has(capability)
    ) {
      throw new GrokPtahBrokerError(0, "invalid_request", "Requested capabilities must be unique stable ids");
    }
    ids.add(capability);
  }
}

function validateOpaqueText(value: unknown, label: string, maxBytes: number): asserts value is string {
  if (!boundedString(value, maxBytes)) {
    throw new GrokPtahBrokerError(0, "invalid_request", `${label} must be bounded and non-empty`);
  }
}

function validateApprovalRequest(request: GrokPtahBrokerApprovalRequest): void {
  if (
    typeof request.sourceFingerprint !== "string" ||
    typeof request.finalFingerprint !== "string" ||
    !request.sourceFingerprint.trim() ||
    !request.finalFingerprint.trim() ||
    !boundedString(request.sourceFingerprint, MAX_BROKER_FINGERPRINT_BYTES) ||
    !boundedString(request.finalFingerprint, MAX_BROKER_FINGERPRINT_BYTES)
  ) {
    throw new GrokPtahBrokerError(
      0,
      "invalid_request",
      "Approval fingerprints must not be empty",
    );
  }
  if (!Array.isArray(request.changedFiles) || request.changedFiles.length > MAX_BROKER_CHANGED_FILES) {
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
      !hasOnlyKeys(file as Record<string, unknown>, new Set(["path", "summary"])) ||
      typeof file.path !== "string" ||
      typeof file.summary !== "string" ||
      !file.path.trim() ||
      file.path.startsWith("/") ||
      file.path.includes("\\") ||
      file.path.includes("..") ||
      new TextEncoder().encode(file.path).byteLength > MAX_BROKER_PATH_BYTES ||
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
    const text = await response.text();
    if (new TextEncoder().encode(text).byteLength <= MAX_BROKER_ERROR_RESPONSE_BYTES) {
      const parsed: unknown = JSON.parse(text);
      if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
        body = parsed as Record<string, unknown>;
      }
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
    const update = parseBrokerEventUpdate(body.update);
    if (
      typeof sseId !== "number" ||
      !Number.isSafeInteger(body.seq) ||
      body.seq < 1 ||
      sseId !== body.seq ||
      !safePublicString(body.ts, 128) ||
      update === null
    ) {
      throw new Error("Broker event notification is malformed");
    }
    return {
      kind: "event",
      brokerRunId,
      seq: body.seq,
      ts: body.ts,
      update,
    };
  }
  if (
    body.kind === "recovery" &&
    Number.isSafeInteger(body.afterSeq) &&
    body.afterSeq >= 0 &&
    boundedString(body.reason, MAX_BROKER_SSE_REASON_BYTES) &&
    boundedString(body.pollRoute, MAX_BROKER_SSE_ROUTE_BYTES)
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
