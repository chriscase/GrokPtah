/**
 * Tauri-free external-worker contracts for desktop and War Room consumers.
 *
 * These values mirror `grokptah-agent-sdk::external_worker` and the
 * `grokptah-external-worker.v1` schema. The parser is intentionally strict:
 * browser code sees only redacted provider projections, never credentials,
 * host paths, raw tool output, or a provider's private response shape.
 */

export const EXTERNAL_WORKER_CONTRACT = "grokptah.external-workers.v1" as const;

export type ExternalWorkerProvider =
  | "cursor_cloud"
  | "claude_code_cloud"
  | "local_worker"
  | "custom";

export type ExternalWorkerState =
  | "provisioning"
  | "ready"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "archived"
  | "unknown";

export type ExternalWorkerLaunchRequest = {
  requestId: string;
  provider: ExternalWorkerProvider;
  providerId?: string;
  repository: string;
  startingRef: string;
  prompt: string;
  model?: string;
  executionMode: "isolated";
  autoCreatePr: boolean;
  bounds?: {
    maxPromptBytes?: number;
    maxRounds?: number;
    maxDurationMs?: number;
  };
};

export type ExternalWorkerFollowUpRequest = {
  requestId: string;
  prompt: string;
  bounds?: {
    maxPromptBytes?: number;
    maxRounds?: number;
    maxDurationMs?: number;
  };
};

export type ExternalWorkerRecord = {
  provider: ExternalWorkerProvider;
  providerId?: string;
  externalAgentId: string;
  repository: string;
  startingRef: string;
  state: ExternalWorkerState;
  branch?: string;
  workerUrl?: string;
  createdAt: string;
  updatedAt: string;
};

export type ExternalWorkerRunRecord = {
  externalAgentId: string;
  externalRunId: string;
  state: ExternalWorkerState;
  lastSeq: number;
  terminalResult?: string;
  createdAt: string;
  updatedAt: string;
};

export type ExternalWorkerEvent = {
  seq: number;
  ts: string;
  kind: string;
  detail: string;
};

export type ExternalWorkerArtifact = {
  path: string;
  digest: string;
  sizeBytes?: number;
};

export type ExternalWorkerLaunchResult = {
  worker: ExternalWorkerRecord;
  run: ExternalWorkerRunRecord;
};

export type ExternalWorkerNotification =
  | { type: "event"; event: ExternalWorkerEvent }
  | { type: "recovery"; afterSeq: number; reason: string; pollRoute: string };

export type ExternalWorkerMonitorState = {
  lastSeq: number;
  events: ExternalWorkerEvent[];
  recoveryRequired: boolean;
};

const MAX_ID_BYTES = 256;
const MAX_REF_BYTES = 512;
const MAX_PROMPT_BYTES = 1_048_576;
const MAX_DETAIL_BYTES = 4_096;
const MAX_EVENTS = 256;
const PROVIDERS = new Set<ExternalWorkerProvider>([
  "cursor_cloud",
  "claude_code_cloud",
  "local_worker",
  "custom",
]);
const STATES = new Set<ExternalWorkerState>([
  "provisioning",
  "ready",
  "running",
  "completed",
  "failed",
  "cancelled",
  "archived",
  "unknown",
]);
const PRIVILEGED_TEXT = /(?:\/(?:users|private|var|tmp|home|volumes)\/|(?:[a-z]:\\users\\|\\\\)|https?:\/\/|(?:^|[\s=:])(authorization|bearer|api[_ -]?key|xai_api_key|grokptah_home|clipboard|private[_ -]?key|password|cookie|session[_ -]?token|secret(?:[_ -]?key)?)(?:[\s=:]|$))/i;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOnlyKeys(value: Record<string, unknown>, keys: ReadonlySet<string>): boolean {
  return Object.keys(value).every((key) => keys.has(key));
}

function boundedString(value: unknown, maxBytes: number, rejectPrivileged = false): value is string {
  return (
    typeof value === "string" &&
    value.trim().length > 0 &&
    new TextEncoder().encode(value).byteLength <= maxBytes &&
    (!rejectPrivileged || !PRIVILEGED_TEXT.test(value))
  );
}

function safeWorkerUrl(value: unknown, provider?: ExternalWorkerProvider): value is string {
  if (!boundedString(value, MAX_REF_BYTES) || !value.startsWith("https://")) return false;
  try {
    const parsed = new URL(value);
    if (
      parsed.protocol !== "https:" ||
      !parsed.hostname ||
      parsed.username ||
      parsed.password ||
      parsed.search ||
      parsed.hash ||
      /[\u0000-\u001f\u007f]/.test(value)
    ) return false;
    if (provider === "cursor_cloud") {
      const host = parsed.hostname.toLowerCase();
      if (host !== "cursor.com" && !host.endsWith(".cursor.com")) return false;
    }
    return true;
  } catch {
    return false;
  }
}

function relativeRef(value: unknown): value is string {
  return (
    boundedString(value, MAX_REF_BYTES) &&
    !value.startsWith("/") &&
    !value.includes("\\") &&
    !value.split("/").some((segment) => segment === "..") &&
    !/[\u0000-\u001f\u007f]/.test(value)
  );
}

function sameOriginRoute(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.startsWith("/") &&
    !value.startsWith("//") &&
    value.length <= MAX_REF_BYTES &&
    !value.includes("\\") &&
    !value.includes("?") &&
    !value.includes("#") &&
    !value.split("/").some((segment) => segment === "..") &&
    !/[\u0000-\u001f\u007f]/.test(value)
  );
}

function identity(value: unknown): value is string {
  return boundedString(value, MAX_ID_BYTES) && !/[\u0000-\u001f\u007f]/.test(value);
}

function validBounds(value: unknown): value is NonNullable<ExternalWorkerLaunchRequest["bounds"]> {
  if (!isRecord(value)) return false;
  if (!hasOnlyKeys(value, new Set(["maxPromptBytes", "maxRounds", "maxDurationMs"]))) return false;
  const positiveInteger = (candidate: unknown): candidate is number =>
    typeof candidate === "number" && Number.isInteger(candidate) && candidate > 0;
  return (
    (value.maxPromptBytes === undefined || positiveInteger(value.maxPromptBytes)) &&
    (value.maxRounds === undefined || (positiveInteger(value.maxRounds) && value.maxRounds <= 24)) &&
    (value.maxDurationMs === undefined || positiveInteger(value.maxDurationMs))
  );
}

/** Parse and validate a worker launch request before sending it to a broker. */
export function parseExternalWorkerLaunchRequest(value: unknown): ExternalWorkerLaunchRequest | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set([
    "requestId", "provider", "providerId", "repository", "startingRef", "prompt", "model",
    "executionMode", "autoCreatePr", "bounds",
  ]))) return null;
  if (
    !identity(value.requestId) ||
    typeof value.provider !== "string" ||
    !PROVIDERS.has(value.provider as ExternalWorkerProvider) ||
    (value.providerId !== undefined && !identity(value.providerId)) ||
    !relativeRef(value.repository) ||
    !relativeRef(value.startingRef) ||
    !boundedString(value.prompt, MAX_PROMPT_BYTES) ||
    (value.model !== undefined && !identity(value.model)) ||
    value.executionMode !== "isolated" ||
    value.autoCreatePr !== false ||
    (value.bounds !== undefined && !validBounds(value.bounds)) ||
    (value.provider === "custom" && value.providerId === undefined)
  ) return null;
  return value as ExternalWorkerLaunchRequest;
}

/** Parse a bounded follow-up prompt before sending it to a broker. */
export function parseExternalWorkerFollowUpRequest(value: unknown): ExternalWorkerFollowUpRequest | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set(["requestId", "prompt", "bounds"]))) return null;
  if (
    !identity(value.requestId) ||
    !boundedString(value.prompt, MAX_PROMPT_BYTES) ||
    (value.bounds !== undefined && !validBounds(value.bounds))
  ) return null;
  return value as ExternalWorkerFollowUpRequest;
}

/** Parse a redacted external-worker identity returned by a trusted broker. */
export function parseExternalWorkerRecord(value: unknown): ExternalWorkerRecord | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set([
    "provider", "providerId", "externalAgentId", "repository", "startingRef", "state",
    "branch", "workerUrl", "createdAt", "updatedAt",
  ]))) return null;
  if (
    typeof value.provider !== "string" ||
    !PROVIDERS.has(value.provider as ExternalWorkerProvider) ||
    (value.providerId !== undefined && !identity(value.providerId)) ||
    !identity(value.externalAgentId) ||
    !relativeRef(value.repository) ||
    !relativeRef(value.startingRef) ||
    typeof value.state !== "string" ||
    !STATES.has(value.state as ExternalWorkerState) ||
    (value.branch !== undefined && !relativeRef(value.branch)) ||
    (value.workerUrl !== undefined && !safeWorkerUrl(value.workerUrl, value.provider as ExternalWorkerProvider)) ||
    !boundedString(value.createdAt, 128) ||
    !boundedString(value.updatedAt, 128) ||
    (value.provider === "custom" && value.providerId === undefined)
  ) return null;
  return value as ExternalWorkerRecord;
}

/** Parse a redacted provider run projection. */
export function parseExternalWorkerRunRecord(value: unknown): ExternalWorkerRunRecord | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set([
    "externalAgentId", "externalRunId", "state", "lastSeq", "terminalResult", "createdAt", "updatedAt",
  ]))) return null;
  if (
    !identity(value.externalAgentId) ||
    !identity(value.externalRunId) ||
    typeof value.state !== "string" ||
    !STATES.has(value.state as ExternalWorkerState) ||
    !Number.isInteger(value.lastSeq) ||
    (value.lastSeq as number) < 0 ||
    (value.terminalResult !== undefined && !boundedString(value.terminalResult, MAX_DETAIL_BYTES, true)) ||
    !boundedString(value.createdAt, 128) ||
    !boundedString(value.updatedAt, 128)
  ) return null;
  return value as ExternalWorkerRunRecord;
}

/** Parse the initial worker/run envelope returned by a launch request. */
export function parseExternalWorkerLaunchResult(value: unknown): ExternalWorkerLaunchResult | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set(["worker", "run"]))) return null;
  const worker = parseExternalWorkerRecord(value.worker);
  const run = parseExternalWorkerRunRecord(value.run);
  if (!worker || !run || worker.externalAgentId !== run.externalAgentId) return null;
  return { worker, run };
}

/** Parse a redacted event without exposing provider tool output. */
export function parseExternalWorkerEvent(value: unknown): ExternalWorkerEvent | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set(["seq", "ts", "kind", "detail"]))) return null;
  if (
    !Number.isInteger(value.seq) ||
    (value.seq as number) < 0 ||
    !boundedString(value.ts, 128) ||
    !boundedString(value.kind, 128) ||
    !boundedString(value.detail, MAX_DETAIL_BYTES, true)
  ) return null;
  return value as ExternalWorkerEvent;
}

/** Parse a stream event or explicit cursor-recovery notification. */
export function parseExternalWorkerNotification(value: unknown): ExternalWorkerNotification | null {
  if (!isRecord(value) || typeof value.type !== "string") return null;
  if (value.type === "event") {
    if (!hasOnlyKeys(value, new Set(["type", "event"]))) return null;
    const event = parseExternalWorkerEvent(value.event);
    return event ? { type: "event", event } : null;
  }
  if (
    value.type !== "recovery" ||
    !hasOnlyKeys(value, new Set(["type", "afterSeq", "reason", "pollRoute"])) ||
    !Number.isInteger(value.afterSeq) ||
    (value.afterSeq as number) < 0 ||
    !boundedString(value.reason, 256, true) ||
    !sameOriginRoute(value.pollRoute)
  ) return null;
  return { type: "recovery", afterSeq: value.afterSeq as number, reason: value.reason, pollRoute: value.pollRoute };
}

/** Parse a bounded, relative artifact reference. */
export function parseExternalWorkerArtifact(value: unknown): ExternalWorkerArtifact | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set(["path", "digest", "sizeBytes"]))) return null;
  if (
    !relativeRef(value.path) ||
    !identity(value.digest) ||
    (value.sizeBytes !== undefined && (!Number.isInteger(value.sizeBytes) || (value.sizeBytes as number) < 0))
  ) return null;
  return value as ExternalWorkerArtifact;
}

/** Create an empty monitor state for one external run. */
export function createExternalWorkerMonitor(): ExternalWorkerMonitorState {
  return { lastSeq: -1, events: [], recoveryRequired: false };
}

/**
 * Apply one broker notification while enforcing monotonic cursors.
 *
 * `null` means the notification was malformed or stale. A sequence gap does
 * not guess at completion; it marks recovery as required until the caller
 * polls the authoritative route and starts a new contiguous window.
 */
export function applyExternalWorkerNotification(
  state: ExternalWorkerMonitorState,
  notification: ExternalWorkerNotification,
): ExternalWorkerMonitorState | null {
  if (notification.type === "recovery") {
    if (notification.afterSeq < state.lastSeq) return null;
    return { ...state, recoveryRequired: true };
  }
  const { event } = notification;
  if (event.seq <= state.lastSeq) return null;
  if (event.seq !== state.lastSeq + 1) {
    return { ...state, recoveryRequired: true };
  }
  const events = [...state.events, event].slice(-MAX_EVENTS);
  return { lastSeq: event.seq, events, recoveryRequired: false };
}

// ---------------------------------------------------------------------------
// Production authority projections.
//
// A browser consumer never mints authority: it receives a host-minted
// admission and echoes it back on the matching mutation. These parsers exist
// so a malformed, stale, or over-scoped ticket is refused before it is used,
// and so a receipt that somehow carries privileged text never renders.
// ---------------------------------------------------------------------------

export type ExternalWorkerMutation = "launch" | "follow_up" | "cancel";

export type ExternalWorkerReceiptState = "claimed" | "accepted" | "rejected" | "uncertain";

export type ExternalWorkerScope = {
  principalId: string;
  sessionId: string;
  workspace: string;
  runId: string;
};

export type ExternalWorkerTarget = {
  externalAgentId: string;
  externalRunId?: string;
};

export type ExternalWorkerAdmission = {
  contract: typeof EXTERNAL_WORKER_CONTRACT;
  admissionId: string;
  nonce: string;
  requestId: string;
  scope: ExternalWorkerScope;
  mutation: ExternalWorkerMutation;
  provider: ExternalWorkerProvider;
  providerId?: string;
  capabilityRevision: number;
  issuedAtMs: number;
  expiresAtMs: number;
  payloadDigest: string;
  target?: ExternalWorkerTarget;
};

export type ExternalWorkerReceipt = {
  contract: typeof EXTERNAL_WORKER_CONTRACT;
  requestId: string;
  admissionId: string;
  mutation: ExternalWorkerMutation;
  scope: ExternalWorkerScope;
  provider: ExternalWorkerProvider;
  providerId?: string;
  providerRequestId: string;
  attempt: number;
  state: ExternalWorkerReceiptState;
  target?: ExternalWorkerTarget;
  payloadDigest: string;
  reason: string;
  createdAtMs: number;
  updatedAtMs: number;
};

export type ExternalWorkerCapabilityStatus = {
  provider: ExternalWorkerProvider;
  providerId?: string;
  registered: boolean;
  reachable: boolean;
  versionCompatible: boolean;
  policyAllowed: boolean;
  capabilityRevision: number;
  reason?: string;
};

/** Host ceiling on a minted admission lifetime, mirroring the Rust contract. */
export const MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS = 15 * 60 * 1_000;
const MAX_REASON_BYTES = 512;
const SHA256_DIGEST = /^sha256:[0-9a-f]{64}$/;
const MUTATIONS = new Set<ExternalWorkerMutation>(["launch", "follow_up", "cancel"]);
const RECEIPT_STATES = new Set<ExternalWorkerReceiptState>([
  "claimed",
  "accepted",
  "rejected",
  "uncertain",
]);
// A workspace alias is an identity, never a place on the host filesystem.
const HOST_PATH = /^(?:\/|\\\\|[a-z]:[\\/])|\\|(?:^|\/)\.\.(?:\/|$)/i;

function nonNegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
}

function parseScope(value: unknown): ExternalWorkerScope | null {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, new Set(["principalId", "sessionId", "workspace", "runId"]))
  ) return null;
  if (
    !identity(value.principalId) ||
    !identity(value.sessionId) ||
    !identity(value.runId) ||
    !identity(value.workspace) ||
    HOST_PATH.test(value.workspace)
  ) return null;
  return value as ExternalWorkerScope;
}

function parseTarget(value: unknown): ExternalWorkerTarget | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set(["externalAgentId", "externalRunId"]))) return null;
  if (
    !identity(value.externalAgentId) ||
    (value.externalRunId !== undefined && !identity(value.externalRunId))
  ) return null;
  return value as ExternalWorkerTarget;
}

function redactedReason(value: unknown): value is string {
  return (
    boundedString(value, MAX_REASON_BYTES, true) &&
    !/[\u0000-\u001f\u007f]/.test(value)
  );
}

function targetMatchesMutation(
  mutation: ExternalWorkerMutation,
  target: ExternalWorkerTarget | undefined,
): boolean {
  if (mutation === "launch") return target === undefined;
  if (target === undefined) return false;
  return mutation === "cancel" ? target.externalRunId !== undefined : target.externalRunId === undefined;
}

/**
 * Parse a host-minted admission.
 *
 * Shape validity is not authority: the server revalidates every field against
 * its own mint ledger. This only stops an obviously unusable ticket from being
 * echoed back and from rendering as if it granted something.
 */
export function parseExternalWorkerAdmission(value: unknown): ExternalWorkerAdmission | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set([
    "contract", "admissionId", "nonce", "requestId", "scope", "mutation", "provider", "providerId",
    "capabilityRevision", "issuedAtMs", "expiresAtMs", "payloadDigest", "target",
  ]))) return null;
  const scope = parseScope(value.scope);
  const target = value.target === undefined ? undefined : parseTarget(value.target);
  if (
    value.contract !== EXTERNAL_WORKER_CONTRACT ||
    !identity(value.admissionId) ||
    !identity(value.nonce) ||
    !identity(value.requestId) ||
    scope === null ||
    typeof value.mutation !== "string" ||
    !MUTATIONS.has(value.mutation as ExternalWorkerMutation) ||
    typeof value.provider !== "string" ||
    !PROVIDERS.has(value.provider as ExternalWorkerProvider) ||
    (value.providerId !== undefined && !identity(value.providerId)) ||
    (value.provider === "custom" && value.providerId === undefined) ||
    !nonNegativeInteger(value.capabilityRevision) ||
    !nonNegativeInteger(value.issuedAtMs) ||
    !nonNegativeInteger(value.expiresAtMs) ||
    value.expiresAtMs <= value.issuedAtMs ||
    value.expiresAtMs - value.issuedAtMs > MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS ||
    typeof value.payloadDigest !== "string" ||
    !SHA256_DIGEST.test(value.payloadDigest) ||
    (value.target !== undefined && target === null) ||
    !targetMatchesMutation(value.mutation as ExternalWorkerMutation, target ?? undefined)
  ) return null;
  return value as ExternalWorkerAdmission;
}

/** Whether an admission is still inside its minted lifetime at `nowMs`. */
export function isExternalWorkerAdmissionLive(
  admission: ExternalWorkerAdmission,
  nowMs: number,
): boolean {
  return nowMs >= admission.issuedAtMs && nowMs < admission.expiresAtMs;
}

/** Parse a redacted durable mutation receipt. */
export function parseExternalWorkerReceipt(value: unknown): ExternalWorkerReceipt | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set([
    "contract", "requestId", "admissionId", "mutation", "scope", "provider", "providerId",
    "providerRequestId", "attempt", "state", "target", "payloadDigest", "reason",
    "createdAtMs", "updatedAtMs",
  ]))) return null;
  const scope = parseScope(value.scope);
  const target = value.target === undefined ? undefined : parseTarget(value.target);
  if (
    value.contract !== EXTERNAL_WORKER_CONTRACT ||
    !identity(value.requestId) ||
    !identity(value.admissionId) ||
    !identity(value.providerRequestId) ||
    typeof value.mutation !== "string" ||
    !MUTATIONS.has(value.mutation as ExternalWorkerMutation) ||
    scope === null ||
    typeof value.provider !== "string" ||
    !PROVIDERS.has(value.provider as ExternalWorkerProvider) ||
    (value.providerId !== undefined && !identity(value.providerId)) ||
    (value.provider === "custom" && value.providerId === undefined) ||
    typeof value.attempt !== "number" ||
    !Number.isInteger(value.attempt) ||
    value.attempt < 1 ||
    typeof value.state !== "string" ||
    !RECEIPT_STATES.has(value.state as ExternalWorkerReceiptState) ||
    (value.target !== undefined && target === null) ||
    (value.state === "accepted" && target === undefined) ||
    typeof value.payloadDigest !== "string" ||
    !SHA256_DIGEST.test(value.payloadDigest) ||
    !redactedReason(value.reason) ||
    !nonNegativeInteger(value.createdAtMs) ||
    !nonNegativeInteger(value.updatedAtMs) ||
    value.updatedAtMs < value.createdAtMs
  ) return null;
  return value as ExternalWorkerReceipt;
}

/** Whether a receipt state forbids another attempt on the same request. */
export function externalWorkerReceiptBlocksRetry(state: ExternalWorkerReceiptState): boolean {
  return state !== "rejected";
}

/** Whether every advertisement gate holds for one provider identity. */
export function externalWorkerCapabilityAvailable(
  status: ExternalWorkerCapabilityStatus,
): boolean {
  return status.registered && status.reachable && status.versionCompatible && status.policyAllowed;
}

/** Parse one advertised external-worker capability status. */
export function parseExternalWorkerCapabilityStatus(
  value: unknown,
): ExternalWorkerCapabilityStatus | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set([
    "provider", "providerId", "registered", "reachable", "versionCompatible", "policyAllowed",
    "capabilityRevision", "reason",
  ]))) return null;
  if (
    typeof value.provider !== "string" ||
    !PROVIDERS.has(value.provider as ExternalWorkerProvider) ||
    (value.providerId !== undefined && !identity(value.providerId)) ||
    (value.provider === "custom" && value.providerId === undefined) ||
    typeof value.registered !== "boolean" ||
    typeof value.reachable !== "boolean" ||
    typeof value.versionCompatible !== "boolean" ||
    typeof value.policyAllowed !== "boolean" ||
    !nonNegativeInteger(value.capabilityRevision) ||
    (value.reason !== undefined && !redactedReason(value.reason))
  ) return null;
  const status = value as ExternalWorkerCapabilityStatus;
  if (!externalWorkerCapabilityAvailable(status) && status.reason === undefined) return null;
  return status;
}

/**
 * Whether an admission may be presented for this exact mutation.
 *
 * The browser check is deliberately narrow and local: it stops a ticket from
 * being sent against the wrong scope, mutation, or target. Expiry, single use,
 * capability revision, and payload binding are the server's to enforce.
 */
export function externalWorkerAdmissionCovers(
  admission: ExternalWorkerAdmission,
  mutation: ExternalWorkerMutation,
  scope: ExternalWorkerScope,
  requestId: string,
  target?: ExternalWorkerTarget,
): boolean {
  return (
    admission.mutation === mutation &&
    admission.requestId === requestId &&
    admission.scope.principalId === scope.principalId &&
    admission.scope.sessionId === scope.sessionId &&
    admission.scope.workspace === scope.workspace &&
    admission.scope.runId === scope.runId &&
    admission.target?.externalAgentId === target?.externalAgentId &&
    admission.target?.externalRunId === target?.externalRunId
  );
}
