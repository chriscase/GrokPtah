/**
 * Trusted-host facade for GrokPtah's reusable powers.
 *
 * This module is bearer-capable: it composes `GrokPtahClient`, which puts a
 * desktop MCP token on the wire. It must never reach a browser bundle, so it
 * is published only through the separately named `@grokptah/client/host`
 * seam, which is fenced off under the `browser`/`worker` export conditions.
 *
 * It deliberately introduces no second authorization model. Every gate below
 * re-uses the published capability lattice in `capabilities.ts` and the
 * scope-fenced operation lattice in `grokptahOperations.ts`; this file only
 * validates the identity fence, binds those operations to it, and reports the
 * negotiation outcome. Anything it cannot prove is refused.
 */
import {
  CAPABILITY_CONTRACT,
  capabilityActionState,
  findCapability,
  type CapabilityActionState,
  type CapabilityDescriptor,
  type CapabilitySet,
} from "./capabilities";
import {
  GrokPtahClient,
  type GrokPtahClientOptions,
  type GrokPtahEventNotification,
  type GrokPtahRecoveryNotification,
  type GrokPtahRunNotification,
  type GrokPtahRunScope,
} from "./grokptahClient";
import {
  GrokPtahCapabilityError,
  GrokPtahOperations,
  type GrokPtahBounds,
  type GrokPtahExecutionMode,
  type GrokPtahOperationResult,
  type GrokPtahScope,
} from "./grokptahOperations";

/** Stable marker for the trusted-host seam shape. */
export const GROKPTAH_HOST_CONTRACT = "grokptah.host.v1" as const;

/** The authoritative poll tool named by a run recovery notification. */
export const GROKPTAH_RECOVERY_POLL_TOOL = "ptah_get_events" as const;

const MAX_SCOPE_FIELD_BYTES = 512;
const MAX_MONITOR_EVENTS = 256;
const SCOPE_KEYS: ReadonlySet<string> = new Set(["sessionId", "workspace"]);
const RUN_SCOPE_KEYS: ReadonlySet<string> = new Set(["sessionId", "workspace", "runId"]);

/** Raised when an identity fence is missing, malformed, or over-specified. */
export class GrokPtahScopeError extends Error {
  readonly field: string;

  constructor(field: string, reason: string) {
    super(`GrokPtah scope field ${field} ${reason}`);
    this.name = "GrokPtahScopeError";
    this.field = field;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function hasControlCharacter(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code < 0x20 || code === 0x7f) return true;
  }
  return false;
}

/**
 * Validate one scope field as bounded opaque text.
 *
 * `workspace` is an identifier the service resolves; this seam deliberately
 * does not interpret it as a filesystem path or apply any path policy of its
 * own, so no host path semantics leak into the client contract.
 */
function scopeField(value: unknown, field: string): string {
  if (typeof value !== "string") throw new GrokPtahScopeError(field, "must be a string");
  if (!value.trim()) throw new GrokPtahScopeError(field, "must not be empty");
  if (hasControlCharacter(value)) {
    throw new GrokPtahScopeError(field, "must not contain control characters");
  }
  if (utf8Bytes(value) > MAX_SCOPE_FIELD_BYTES) {
    throw new GrokPtahScopeError(field, `must be at most ${MAX_SCOPE_FIELD_BYTES} UTF-8 bytes`);
  }
  return value;
}

function assertKnownKeys(value: Record<string, unknown>, allowed: ReadonlySet<string>): void {
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) throw new GrokPtahScopeError(key, "is not part of the scope contract");
  }
}

/** Validate a workspace-level identity fence, throwing on anything unproven. */
export function assertGrokPtahScope(value: unknown): GrokPtahScope {
  if (!isRecord(value)) throw new GrokPtahScopeError("scope", "must be an object");
  assertKnownKeys(value, SCOPE_KEYS);
  return {
    sessionId: scopeField(value.sessionId, "sessionId"),
    workspace: scopeField(value.workspace, "workspace"),
  };
}

/** Validate a run-level identity fence, throwing on anything unproven. */
export function assertGrokPtahRunScope(value: unknown): GrokPtahRunScope {
  if (!isRecord(value)) throw new GrokPtahScopeError("scope", "must be an object");
  assertKnownKeys(value, RUN_SCOPE_KEYS);
  return {
    sessionId: scopeField(value.sessionId, "sessionId"),
    workspace: scopeField(value.workspace, "workspace"),
    runId: scopeField(value.runId, "runId"),
  };
}

/** Non-throwing workspace fence parse; `null` means the scope is unusable. */
export function parseGrokPtahScope(value: unknown): GrokPtahScope | null {
  try {
    return assertGrokPtahScope(value);
  } catch {
    return null;
  }
}

/** Non-throwing run fence parse; `null` means the scope is unusable. */
export function parseGrokPtahRunScope(value: unknown): GrokPtahRunScope | null {
  try {
    return assertGrokPtahRunScope(value);
  } catch {
    return null;
  }
}

/** A capability id, optionally paired with the gate the caller already holds. */
export type GrokPtahCapabilityRequest = { id: string; gateSatisfied?: boolean };

export type GrokPtahCapabilityRequirement = string | GrokPtahCapabilityRequest;

export type GrokPtahCapabilityOutcome = {
  id: string;
  state: CapabilityActionState;
  gateSatisfied: boolean;
  descriptor?: CapabilityDescriptor;
};

export type GrokPtahCapabilityReport = {
  contract: typeof CAPABILITY_CONTRACT;
  ready: string[];
  requiresGate: string[];
  unavailable: string[];
  outcomes: GrokPtahCapabilityOutcome[];
};

function normalizeRequirement(
  requirement: GrokPtahCapabilityRequirement,
): GrokPtahCapabilityRequest {
  if (typeof requirement === "string") return { id: requirement };
  if (!isRecord(requirement) || typeof requirement.id !== "string") {
    throw new TypeError("GrokPtah capability requirement must be an id or { id, gateSatisfied }");
  }
  return { id: requirement.id, gateSatisfied: requirement.gateSatisfied === true };
}

/**
 * Report how a negotiated capability set answers a caller's requirements.
 *
 * A missing or unparsed capability set yields `unavailable` for every id, so a
 * host that never completed the handshake cannot act.
 */
export function negotiateGrokPtahCapabilities(
  set: CapabilitySet | null | undefined,
  requirements: readonly GrokPtahCapabilityRequirement[],
): GrokPtahCapabilityReport {
  const outcomes: GrokPtahCapabilityOutcome[] = requirements.map((requirement) => {
    const normalized = normalizeRequirement(requirement);
    const gateSatisfied = normalized.gateSatisfied === true;
    const descriptor = findCapability(set, normalized.id);
    return {
      id: normalized.id,
      state: capabilityActionState(descriptor, gateSatisfied),
      gateSatisfied,
      ...(descriptor ? { descriptor } : {}),
    };
  });
  return {
    contract: CAPABILITY_CONTRACT,
    ready: outcomes.filter((outcome) => outcome.state === "ready").map((outcome) => outcome.id),
    requiresGate: outcomes
      .filter((outcome) => outcome.state === "requires_gate")
      .map((outcome) => outcome.id),
    unavailable: outcomes
      .filter((outcome) => outcome.state === "unavailable")
      .map((outcome) => outcome.id),
    outcomes,
  };
}

/**
 * Negotiate and refuse on the first unmet requirement, reusing the published
 * `GrokPtahCapabilityError` rather than a host-specific failure type.
 */
export function requireGrokPtahCapabilities(
  set: CapabilitySet | null | undefined,
  requirements: readonly GrokPtahCapabilityRequirement[],
): GrokPtahCapabilityReport {
  const report = negotiateGrokPtahCapabilities(set, requirements);
  for (const outcome of report.outcomes) {
    if (outcome.state === "ready") continue;
    throw new GrokPtahCapabilityError(outcome.id, outcome.state);
  }
  return report;
}

export type GrokPtahRunMonitorState = {
  lastSeq: number;
  events: GrokPtahEventNotification[];
  recoveryRequired: boolean;
  recovery: GrokPtahRecoveryNotification | null;
};

/** Create an empty monitor seeded at the cursor the caller is replaying from. */
export function createGrokPtahRunMonitor(afterSeq = 0): GrokPtahRunMonitorState {
  return { lastSeq: afterSeq, events: [], recoveryRequired: false, recovery: null };
}

/**
 * Fold one run notification while enforcing a contiguous cursor.
 *
 * `null` means the notification was stale and must not advance the monitor. A
 * gap does not guess at the missing window; it marks recovery as required so
 * the caller polls the authoritative tool before trusting the stream again.
 */
export function applyGrokPtahRunNotification(
  state: GrokPtahRunMonitorState,
  notification: GrokPtahRunNotification,
): GrokPtahRunMonitorState | null {
  if (notification.kind === "recovery") {
    if (notification.afterSeq < state.lastSeq) return null;
    return { ...state, recoveryRequired: true, recovery: notification };
  }
  if (notification.seq <= state.lastSeq) return null;
  if (notification.seq !== state.lastSeq + 1) {
    return { ...state, recoveryRequired: true };
  }
  return {
    lastSeq: notification.seq,
    events: [...state.events, notification].slice(-MAX_MONITOR_EVENTS),
    recoveryRequired: false,
    recovery: null,
  };
}

export type GrokPtahRunFollowUpdate = {
  notification: GrokPtahRunNotification;
  state: GrokPtahRunMonitorState;
};

export type GrokPtahExecuteOptions = {
  bounds?: GrokPtahBounds;
  executionMode?: GrokPtahExecutionMode;
  allowQueue?: boolean;
};

export type GrokPtahEventPageOptions = { afterSeq?: number; limit?: number };

export type GrokPtahHostOptions = GrokPtahClientOptions & {
  /** Capabilities `connect()` refuses to start without. */
  requiredCapabilities?: readonly GrokPtahCapabilityRequirement[];
};

export type GrokPtahHostInit =
  | GrokPtahHostOptions
  | {
      client: GrokPtahClient;
      requiredCapabilities?: readonly GrokPtahCapabilityRequirement[];
    };

/**
 * A trusted desktop/server host bound to one authenticated GrokPtah service.
 *
 * The transport client stays reachable for tools this facade does not wrap;
 * everything the facade does wrap goes through `GrokPtahOperations` so the
 * operation lattice is never duplicated.
 */
export class GrokPtahHost {
  readonly client: GrokPtahClient;
  readonly operations: GrokPtahOperations;
  private readonly requiredCapabilities: readonly GrokPtahCapabilityRequirement[];

  constructor(init: GrokPtahHostInit) {
    if ("client" in init) {
      this.client = init.client;
      this.requiredCapabilities = [...(init.requiredCapabilities ?? [])];
    } else {
      const { requiredCapabilities = [], ...clientOptions } = init;
      this.client = new GrokPtahClient(clientOptions);
      this.requiredCapabilities = [...requiredCapabilities];
    }
    this.operations = new GrokPtahOperations(this.client);
  }

  get capabilities(): CapabilitySet | null {
    return this.client.capabilities;
  }

  get isConnected(): boolean {
    return this.client.isInitialized;
  }

  /**
   * Complete the MCP handshake and negotiate the required capabilities.
   *
   * If negotiation fails the authenticated transport session is torn down
   * before the error propagates, so a host that is not authorized for its
   * declared work does not keep a live bearer session open.
   */
  async connect(
    requirements: readonly GrokPtahCapabilityRequirement[] = this.requiredCapabilities,
  ): Promise<GrokPtahCapabilityReport> {
    await this.client.initialize();
    try {
      return requireGrokPtahCapabilities(this.client.capabilities, requirements);
    } catch (error) {
      await this.client.close().catch(() => undefined);
      throw error;
    }
  }

  /** Report the current negotiation outcome without refusing. */
  negotiate(
    requirements: readonly GrokPtahCapabilityRequirement[] = this.requiredCapabilities,
  ): GrokPtahCapabilityReport {
    return negotiateGrokPtahCapabilities(this.client.capabilities, requirements);
  }

  /** Refuse unless every requirement is ready under the negotiated contract. */
  require(
    requirements: readonly GrokPtahCapabilityRequirement[] = this.requiredCapabilities,
  ): GrokPtahCapabilityReport {
    return requireGrokPtahCapabilities(this.client.capabilities, requirements);
  }

  /** Bind a validated workspace fence. */
  workspace(scope: unknown): GrokPtahHostWorkspace {
    return new GrokPtahHostWorkspace(this, assertGrokPtahScope(scope));
  }

  /** Bind a validated run fence. */
  run(scope: unknown): GrokPtahHostRun {
    return new GrokPtahHostRun(this, assertGrokPtahRunScope(scope));
  }

  listSessions<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.operations.listSessions<T>();
  }

  getCapacity<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.operations.getCapacity<T>();
  }

  close(): Promise<void> {
    return this.client.close();
  }
}

/**
 * Workspace-fenced powers: execution, the durable prompt queue, and durable
 * agent continuity. Every call re-uses the scope validated at construction, so
 * a caller cannot cross into another session or workspace by accident.
 */
export class GrokPtahHostWorkspace {
  constructor(
    private readonly host: GrokPtahHost,
    readonly scope: GrokPtahScope,
  ) {}

  /** Narrow this fence to one run without re-supplying the workspace identity. */
  run(runId: unknown): GrokPtahHostRun {
    return this.host.run({ ...this.scope, runId });
  }

  submitTask<T = unknown>(
    requestId: string,
    prompt: string,
    options: GrokPtahExecuteOptions = {},
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.submitTask<T>(this.scope, requestId, prompt, options);
  }

  getQueue<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getQueue<T>(this.scope);
  }

  queuePrompt<T = unknown>(
    requestId: string,
    prompt: string,
    priority?: boolean,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.queuePrompt<T>(this.scope, requestId, prompt, priority);
  }

  steer<T = unknown>(requestId: string, text: string): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.steer<T>(this.scope, requestId, text);
  }

  editQueue<T = unknown>(
    requestId: string,
    entryId: string,
    version: number,
    text: string,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.editQueue<T>(this.scope, requestId, entryId, version, text);
  }

  removeQueue<T = unknown>(
    requestId: string,
    entryId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.removeQueue<T>(this.scope, requestId, entryId, expectedVersion);
  }

  reorderQueue<T = unknown>(
    requestId: string,
    entryId: string,
    toIndex: number,
    expectedVersion: number,
    expectedRevision: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.reorderQueue<T>(
      this.scope,
      requestId,
      entryId,
      toIndex,
      expectedVersion,
      expectedRevision,
    );
  }

  clearQueue<T = unknown>(requestId: string): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.clearQueue<T>(this.scope, requestId);
  }

  runNext<T = unknown>(
    requestId: string,
    entryId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.runNext<T>(this.scope, requestId, entryId, expectedVersion);
  }

  steerQueued<T = unknown>(
    requestId: string,
    entryId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.steerQueued<T>(this.scope, requestId, entryId, expectedVersion);
  }

  listPersistentAgents<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.listPersistentAgents<T>(this.scope);
  }

  getPersistentAgent<T = unknown>(agentId: string): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getPersistentAgent<T>(this.scope, agentId);
  }

  resumePersistentAgent<T = unknown>(
    agentId: string,
    requestId: string,
    prompt: string,
    maxRounds?: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.resumePersistentAgent<T>(
      this.scope,
      agentId,
      requestId,
      prompt,
      maxRounds,
    );
  }
}

/**
 * Run-fenced powers: review projections, the approval/promotion gate, and
 * bounded event replay plus live monitoring.
 */
export class GrokPtahHostRun {
  constructor(
    private readonly host: GrokPtahHost,
    readonly scope: GrokPtahRunScope,
  ) {}

  getRun<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getRun<T>(this.scope);
  }

  getProgress<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getProgress<T>(this.scope);
  }

  getChanges<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getChanges<T>(this.scope);
  }

  getTestResults<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getTestResults<T>(this.scope);
  }

  getHandoff<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getHandoff<T>(this.scope);
  }

  review<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.reviewRun<T>(this.scope);
  }

  retry<T = unknown>(
    requestId: string,
    prompt: string,
    options: GrokPtahExecuteOptions = {},
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.retryRun<T>(this.scope, requestId, prompt, options);
  }

  cancel<T = unknown>(requestId: string): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.cancelRun<T>(this.scope, requestId);
  }

  approve<T = unknown>(
    requestId: string,
    sourceFingerprint: string,
    finalFingerprint: string,
    changedFiles: Array<{ path: string; summary: string }>,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.approveRun<T>(
      this.scope,
      requestId,
      sourceFingerprint,
      finalFingerprint,
      changedFiles,
      gateSatisfied,
    );
  }

  promote<T = unknown>(
    requestId: string,
    approvalId: string,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.promoteRun<T>(this.scope, requestId, approvalId, gateSatisfied);
  }

  discard<T = unknown>(
    requestId: string,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.discardRun<T>(this.scope, requestId, gateSatisfied);
  }

  /** One bounded replay page from the authoritative event log. */
  events<T = unknown>(
    options: GrokPtahEventPageOptions = {},
  ): Promise<GrokPtahOperationResult<T>> {
    return this.host.operations.getEvents<T>(this.scope, options);
  }

  /**
   * Replay after a recovery notification.
   *
   * The poll tool named by the server is checked against the published
   * contract first: a stream must not be able to steer a trusted host into
   * calling an arbitrary tool name.
   */
  replayRecovery<T = unknown>(
    recovery: GrokPtahRecoveryNotification,
    options: { limit?: number } = {},
  ): Promise<GrokPtahOperationResult<T>> {
    if (recovery.pollTool !== GROKPTAH_RECOVERY_POLL_TOOL) {
      throw new Error(
        `GrokPtah recovery names an unsupported poll tool: ${recovery.pollTool.slice(0, 128)}`,
      );
    }
    // The recovery cursor is the server's, not the caller's: spread first.
    return this.events<T>({ ...options, afterSeq: recovery.afterSeq });
  }

  /**
   * Follow the bounded SSE channel for this exact run.
   *
   * The capability check happens before the generator is created so an
   * unauthorized caller fails immediately rather than on first pull.
   */
  stream(afterSeq?: number): AsyncGenerator<GrokPtahRunNotification> {
    this.host.require(["run.review"]);
    return this.host.client.streamRunEvents(this.scope, afterSeq);
  }

  /** Stream and fold in one step, refusing stale or out-of-order frames. */
  async *follow(options: { afterSeq?: number } = {}): AsyncGenerator<GrokPtahRunFollowUpdate> {
    const afterSeq = options.afterSeq ?? 0;
    let state = createGrokPtahRunMonitor(afterSeq);
    for await (const notification of this.stream(afterSeq)) {
      const next = applyGrokPtahRunNotification(state, notification);
      if (!next) throw new Error("GrokPtah run notification is stale or out of order");
      state = next;
      yield { notification, state };
      if (state.recoveryRequired) return;
    }
  }
}
