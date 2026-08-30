/**
 * Fail-closed parser for the allowlisted `grokptah.public-run.v1` document.
 *
 * Mirrors `orchestration::public_run` (deny unknown fields / unknown version).
 * Session and workspace scope stay on the request; this parser never copies
 * them from a remote body. Local host records stay on `DurableRun`.
 *
 * `parsePublicRunV1` is the MCP document. `parseRemotePublicRun` consumes the
 * additive Tauri `remote_service_public_run_*` wrapper, which flattens
 * request-stamped `sessionId`/`workspace` onto that document.
 */

export const PUBLIC_RUN_SCHEMA_VERSION = "grokptah.public-run.v1" as const;

const PUBLIC_RUN_STATES = [
  "queued",
  "running",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "limit_reached",
] as const;

export type PublicRunState = (typeof PUBLIC_RUN_STATES)[number];

export type PublicRunDtoErrorKind = "unknown_schema_version" | "decode";

/** Redacted client parse failure. Display never includes raw field values. */
export class PublicRunDtoError extends Error {
  readonly kind: PublicRunDtoErrorKind;
  readonly schemaVersion?: string;
  readonly problem?: string;

  constructor(
    kind: PublicRunDtoErrorKind,
    options: { schemaVersion?: string; problem?: string } = {},
  ) {
    const message =
      kind === "unknown_schema_version"
        ? `unknown public-run schema version: ${options.schemaVersion ?? ""}`
        : "public-run dto decode failed";
    super(message);
    this.name = "PublicRunDtoError";
    this.kind = kind;
    this.schemaVersion = options.schemaVersion;
    this.problem = options.problem;
  }
}

export interface PublicRunV1 {
  schemaVersion: typeof PUBLIC_RUN_SCHEMA_VERSION;
  runId: string;
  state: PublicRunState;
  createdAt: string;
  updatedAt: string;
  queuePosition: number | null;
  eventStartSeq: number | null;
  eventEndSeq: number | null;
  changeCount: number;
  testCount: number;
  permissionRequestedCount: number;
  permissionGrantedCount: number;
  permissionDeniedCount: number;
  usagePromptTokens: number;
  usageCompletionTokens: number;
  usageTotalTokens: number;
  usageRequestCount: number;
  usageComplete: boolean;
  usagePendingRequestCount: number;
  progressRound: number | null;
  progressMaxRounds: number | null;
}

export interface PublicRunListV1 {
  schemaVersion: typeof PUBLIC_RUN_SCHEMA_VERSION;
  runs: PublicRunV1[];
}

export interface PublicRunProgressV1 {
  schemaVersion: typeof PUBLIC_RUN_SCHEMA_VERSION;
  runId: string;
  state: PublicRunState;
  busy: boolean;
  createdAt: string;
  updatedAt: string;
  queuePosition: number | null;
  eventStartSeq: number | null;
  eventEndSeq: number | null;
  progressRound: number | null;
  progressMaxRounds: number | null;
}

export interface PublicRunHandoffV1 {
  schemaVersion: typeof PUBLIC_RUN_SCHEMA_VERSION;
  runId: string;
  state: PublicRunState;
  createdAt: string;
  updatedAt: string;
  eventStartSeq: number | null;
  eventEndSeq: number | null;
  changeCount: number;
  testCount: number;
  usagePromptTokens: number;
  usageCompletionTokens: number;
  usageTotalTokens: number;
  usageRequestCount: number;
  usageComplete: boolean;
  usagePendingRequestCount: number;
}

/**
 * Request-stamped public-run row from `remote_service_public_run_get` /
 * nested `remote_service_public_run_list` items. Scope fields are not part of
 * the remote document.
 */
export interface RemotePublicRun extends PublicRunV1 {
  sessionId: string;
  workspace: string;
}

/** Request-stamped public-run list for one session/workspace. */
export interface RemotePublicRunList {
  schemaVersion: typeof PUBLIC_RUN_SCHEMA_VERSION;
  sessionId: string;
  workspace: string;
  runs: RemotePublicRun[];
}

type JsonObject = Record<string, unknown>;

const RFC3339 =
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;

const U32_MAX = 0xffff_ffff;

const PUBLIC_RUN_REQUIRED = [
  "schemaVersion",
  "runId",
  "state",
  "createdAt",
  "updatedAt",
  "changeCount",
  "testCount",
  "permissionRequestedCount",
  "permissionGrantedCount",
  "permissionDeniedCount",
  "usagePromptTokens",
  "usageCompletionTokens",
  "usageTotalTokens",
  "usageRequestCount",
  "usageComplete",
  "usagePendingRequestCount",
] as const;

const PUBLIC_RUN_OPTIONAL = [
  "queuePosition",
  "eventStartSeq",
  "eventEndSeq",
  "progressRound",
  "progressMaxRounds",
] as const;

const LIST_REQUIRED = ["schemaVersion", "runs"] as const;

const PROGRESS_REQUIRED = [
  "schemaVersion",
  "runId",
  "state",
  "busy",
  "createdAt",
  "updatedAt",
] as const;

const PROGRESS_OPTIONAL = [
  "queuePosition",
  "eventStartSeq",
  "eventEndSeq",
  "progressRound",
  "progressMaxRounds",
] as const;

const HANDOFF_REQUIRED = [
  "schemaVersion",
  "runId",
  "state",
  "createdAt",
  "updatedAt",
  "changeCount",
  "testCount",
  "usagePromptTokens",
  "usageCompletionTokens",
  "usageTotalTokens",
  "usageRequestCount",
  "usageComplete",
  "usagePendingRequestCount",
] as const;

const HANDOFF_OPTIONAL = ["eventStartSeq", "eventEndSeq"] as const;

function decode(problem: string): PublicRunDtoError {
  return new PublicRunDtoError("decode", { problem });
}

function closedObject(
  value: unknown,
  required: readonly string[],
  optional: readonly string[],
): JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw decode("expected an object");
  }
  const record = value as JsonObject;
  const allowed = new Set<string>([...required, ...optional]);
  for (const key of Object.keys(record)) {
    if (!allowed.has(key)) throw decode("unknown field");
  }
  for (const key of required) {
    if (!Object.hasOwn(record, key)) throw decode("missing required field");
  }
  return record;
}

function stringValue(value: unknown): string {
  if (typeof value !== "string") throw decode("expected a string");
  return value;
}

function booleanValue(value: unknown): boolean {
  if (typeof value !== "boolean") throw decode("expected a boolean");
  return value;
}

function unsignedValue(value: unknown, max: number): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0 || value > max) {
    throw decode("expected an unsigned integer");
  }
  return value;
}

function u64Value(value: unknown): number {
  return unsignedValue(value, Number.MAX_SAFE_INTEGER);
}

function u32Value(value: unknown): number {
  return unsignedValue(value, U32_MAX);
}

function optionalUnsigned(
  record: JsonObject,
  key: string,
  read: (value: unknown) => number,
): number | null {
  if (!Object.hasOwn(record, key) || record[key] === null) return null;
  return read(record[key]);
}

function timestampValue(value: unknown): string {
  const text = stringValue(value);
  if (!RFC3339.test(text)) throw decode("expected an rfc3339 timestamp");
  const parsed = Date.parse(text);
  if (Number.isNaN(parsed)) throw decode("expected an rfc3339 timestamp");
  return text;
}

function stateValue(value: unknown): PublicRunState {
  const text = stringValue(value);
  if (!(PUBLIC_RUN_STATES as readonly string[]).includes(text)) {
    throw decode("expected a public-run state");
  }
  return text as PublicRunState;
}

function requireKnownVersion(version: string): typeof PUBLIC_RUN_SCHEMA_VERSION {
  if (version !== PUBLIC_RUN_SCHEMA_VERSION) {
    throw new PublicRunDtoError("unknown_schema_version", { schemaVersion: version });
  }
  return PUBLIC_RUN_SCHEMA_VERSION;
}

function schemaVersionValue(value: unknown): typeof PUBLIC_RUN_SCHEMA_VERSION {
  return requireKnownVersion(stringValue(value));
}

function parseRunObject(record: JsonObject): PublicRunV1 {
  return {
    schemaVersion: schemaVersionValue(record.schemaVersion),
    runId: stringValue(record.runId),
    state: stateValue(record.state),
    createdAt: timestampValue(record.createdAt),
    updatedAt: timestampValue(record.updatedAt),
    queuePosition: optionalUnsigned(record, "queuePosition", u64Value),
    eventStartSeq: optionalUnsigned(record, "eventStartSeq", u64Value),
    eventEndSeq: optionalUnsigned(record, "eventEndSeq", u64Value),
    changeCount: u64Value(record.changeCount),
    testCount: u64Value(record.testCount),
    permissionRequestedCount: u64Value(record.permissionRequestedCount),
    permissionGrantedCount: u64Value(record.permissionGrantedCount),
    permissionDeniedCount: u64Value(record.permissionDeniedCount),
    usagePromptTokens: u64Value(record.usagePromptTokens),
    usageCompletionTokens: u64Value(record.usageCompletionTokens),
    usageTotalTokens: u64Value(record.usageTotalTokens),
    usageRequestCount: u64Value(record.usageRequestCount),
    usageComplete: booleanValue(record.usageComplete),
    usagePendingRequestCount: u64Value(record.usagePendingRequestCount),
    progressRound: optionalUnsigned(record, "progressRound", u32Value),
    progressMaxRounds: optionalUnsigned(record, "progressMaxRounds", u32Value),
  };
}

/** Parse one `ptah_get_run` public-run.v1 document. */
export function parsePublicRunV1(value: unknown): PublicRunV1 {
  return parseRunObject(closedObject(value, PUBLIC_RUN_REQUIRED, PUBLIC_RUN_OPTIONAL));
}

/** Parse one `ptah_list_runs` public-run.v1 envelope. */
export function parsePublicRunListV1(value: unknown): PublicRunListV1 {
  const record = closedObject(value, LIST_REQUIRED, []);
  const schemaVersion = schemaVersionValue(record.schemaVersion);
  if (!Array.isArray(record.runs)) throw decode("expected an array");
  const runs = record.runs.map((item) => parsePublicRunV1(item));
  return { schemaVersion, runs };
}

/** Parse one `ptah_get_progress` public-run.v1 document. */
export function parsePublicRunProgressV1(value: unknown): PublicRunProgressV1 {
  const record = closedObject(value, PROGRESS_REQUIRED, PROGRESS_OPTIONAL);
  return {
    schemaVersion: schemaVersionValue(record.schemaVersion),
    runId: stringValue(record.runId),
    state: stateValue(record.state),
    busy: booleanValue(record.busy),
    createdAt: timestampValue(record.createdAt),
    updatedAt: timestampValue(record.updatedAt),
    queuePosition: optionalUnsigned(record, "queuePosition", u64Value),
    eventStartSeq: optionalUnsigned(record, "eventStartSeq", u64Value),
    eventEndSeq: optionalUnsigned(record, "eventEndSeq", u64Value),
    progressRound: optionalUnsigned(record, "progressRound", u32Value),
    progressMaxRounds: optionalUnsigned(record, "progressMaxRounds", u32Value),
  };
}

/** Parse one `ptah_get_handoff` public-run.v1 document. */
export function parsePublicRunHandoffV1(value: unknown): PublicRunHandoffV1 {
  const record = closedObject(value, HANDOFF_REQUIRED, HANDOFF_OPTIONAL);
  return {
    schemaVersion: schemaVersionValue(record.schemaVersion),
    runId: stringValue(record.runId),
    state: stateValue(record.state),
    createdAt: timestampValue(record.createdAt),
    updatedAt: timestampValue(record.updatedAt),
    eventStartSeq: optionalUnsigned(record, "eventStartSeq", u64Value),
    eventEndSeq: optionalUnsigned(record, "eventEndSeq", u64Value),
    changeCount: u64Value(record.changeCount),
    testCount: u64Value(record.testCount),
    usagePromptTokens: u64Value(record.usagePromptTokens),
    usageCompletionTokens: u64Value(record.usageCompletionTokens),
    usageTotalTokens: u64Value(record.usageTotalTokens),
    usageRequestCount: u64Value(record.usageRequestCount),
    usageComplete: booleanValue(record.usageComplete),
    usagePendingRequestCount: u64Value(record.usagePendingRequestCount),
  };
}

/**
 * Drop request-wrapper keys without using their values. Additive Tauri
 * commands flatten `sessionId`/`workspace` onto the public document; MCP
 * bodies omit them. Either way the stamped result comes from `sessionId` /
 * `workspace` arguments, never from `value`.
 */
function omitRequestScope(value: unknown): unknown {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return value;
  }
  const rest: JsonObject = { ...(value as JsonObject) };
  delete rest.sessionId;
  delete rest.workspace;
  return rest;
}

function stampRemotePublicRun(
  document: PublicRunV1,
  sessionId: string,
  workspace: string,
): RemotePublicRun {
  return { ...document, sessionId, workspace };
}

/** Parse one additive Tauri public-run get payload and stamp request scope. */
export function parseRemotePublicRun(
  value: unknown,
  sessionId: string,
  workspace: string,
): RemotePublicRun {
  return stampRemotePublicRun(parsePublicRunV1(omitRequestScope(value)), sessionId, workspace);
}

/** Parse one additive Tauri public-run list payload and stamp request scope. */
export function parseRemotePublicRunList(
  value: unknown,
  sessionId: string,
  workspace: string,
): RemotePublicRunList {
  const envelope = omitRequestScope(value);
  if (envelope === null || typeof envelope !== "object" || Array.isArray(envelope)) {
    throw decode("expected an object");
  }
  const record = envelope as JsonObject;
  if (Array.isArray(record.runs)) {
    record.runs = record.runs.map(omitRequestScope);
  }
  const parsed = parsePublicRunListV1(record);
  return {
    schemaVersion: parsed.schemaVersion,
    sessionId,
    workspace,
    runs: parsed.runs.map((document) => stampRemotePublicRun(document, sessionId, workspace)),
  };
}
