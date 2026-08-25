/**
 * Tauri-free external-worker contracts for desktop and War Room consumers.
 *
 * These values mirror `grokptah-agent-sdk::external_worker` and the
 * `grokptah-external-worker.v1` schema. The parser is intentionally strict:
 * browser code sees only redacted provider projections, never credentials,
 * host paths, raw tool output, or a provider's private response shape.
 */

export const EXTERNAL_WORKER_CONTRACT = "grokptah.external-workers.v1" as const;
/** v1 does not claim a sequenced provider event stream. lastSeq must be null. */
export const EXTERNAL_WORKER_STREAMING_SUPPORTED = false;

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
  stream: "unsupported";
  lastSeq: number | null;
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
  runId: string;
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
/** Maximum bytes a single external worker artifact may report. */
export const MAX_EXTERNAL_WORKER_ARTIFACT_BYTES = 8 * 1024 * 1024;
/** Maximum artifacts accepted in one run listing. */
export const MAX_EXTERNAL_WORKER_ARTIFACTS = 256;
/** The only content-digest algorithm this contract accepts. */
const SHA256_DIGEST = /^sha256:[0-9a-f]{64}$/;
/**
 * One conservative portable grammar for artifact paths, identical to the Rust
 * contract and the published `grokptah-external-worker.v1` schema: ASCII
 * alphanumerics, dot, underscore and hyphen per segment, never ending in a
 * dot, never a Windows reserved device name. That single shape also refuses
 * every absolute form, traversal, empty and dot segments, separators,
 * query/fragment cloaking, and all non-ASCII — so a path that resolves here
 * resolves the same way on every filesystem.
 *
 * Held as a string, not a literal, so a test can assert it is byte-identical
 * to the schema's own `artifactPath` pattern.
 */
export const ARTIFACT_PATH_PATTERN = "^(?!(?:[Cc][Oo][Nn]|[Pp][Rr][Nn]|[Aa][Uu][Xx]|[Nn][Uu][Ll]|[Cc][Oo][Mm][1-9]|[Ll][Pp][Tt][1-9])(?:\\.|/|$))[A-Za-z0-9._-]*[A-Za-z0-9_-](?:/(?!(?:[Cc][Oo][Nn]|[Pp][Rr][Nn]|[Aa][Uu][Xx]|[Nn][Uu][Ll]|[Cc][Oo][Mm][1-9]|[Ll][Pp][Tt][1-9])(?:\\.|/|$))[A-Za-z0-9._-]*[A-Za-z0-9_-])*$";
const ARTIFACT_PATH = new RegExp(ARTIFACT_PATH_PATTERN);
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

/**
 * Artifact paths are stricter than refs: they name a file a consumer may
 * materialize under a containment root, so every form that can leave that root
 * or make two strings name one file is refused. `relativeRef` alone accepts a
 * Windows drive path, which carries neither a leading slash nor a backslash.
 */
function artifactPath(value: unknown): value is string {
  return (
    typeof value === "string" &&
    new TextEncoder().encode(value).byteLength <= MAX_REF_BYTES &&
    ARTIFACT_PATH.test(value)
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
    "externalAgentId", "externalRunId", "state", "stream", "lastSeq", "terminalResult", "createdAt", "updatedAt",
  ]))) return null;
  if (
    !identity(value.externalAgentId) ||
    !identity(value.externalRunId) ||
    typeof value.state !== "string" ||
    !STATES.has(value.state as ExternalWorkerState) ||
    value.stream !== "unsupported" ||
    value.lastSeq !== null ||
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

/** Parse a bounded, relative, run-attributed artifact reference. */
export function parseExternalWorkerArtifact(value: unknown): ExternalWorkerArtifact | null {
  if (!isRecord(value) || !hasOnlyKeys(value, new Set(["path", "digest", "runId", "sizeBytes"]))) return null;
  if (
    !artifactPath(value.path) ||
    typeof value.digest !== "string" ||
    !SHA256_DIGEST.test(value.digest) ||
    !identity(value.runId) ||
    (value.sizeBytes !== undefined &&
      (!Number.isInteger(value.sizeBytes) ||
        (value.sizeBytes as number) < 0 ||
        (value.sizeBytes as number) > MAX_EXTERNAL_WORKER_ARTIFACT_BYTES))
  ) return null;
  return value as ExternalWorkerArtifact;
}

/**
 * Parse a whole artifact listing against the run it was requested for.
 *
 * Attribution and the item ceiling are properties of the listing, not of one
 * artifact, so a caller that maps `parseExternalWorkerArtifact` over an array
 * cannot enforce them. `null` means the listing is refused as a whole.
 */
export function parseExternalWorkerArtifactListing(
  value: unknown,
  externalRunId: string,
): ExternalWorkerArtifact[] | null {
  if (!Array.isArray(value) || value.length > MAX_EXTERNAL_WORKER_ARTIFACTS) return null;
  const artifacts: ExternalWorkerArtifact[] = [];
  for (const item of value) {
    const artifact = parseExternalWorkerArtifact(item);
    if (!artifact || artifact.runId !== externalRunId) return null;
    artifacts.push(artifact);
  }
  return artifacts;
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
