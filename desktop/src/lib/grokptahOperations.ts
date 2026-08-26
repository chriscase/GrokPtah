import {
  capabilityActionState,
  findCapability,
  type CapabilitySet,
} from "./capabilities";
import {
  GrokPtahClient,
  type GrokPtahCallResult,
  GrokPtahRemoteError,
  type GrokPtahRunScope,
} from "./grokptahClient";

/** The identity that fences every run-scoped operation. */
export type GrokPtahScope = Omit<GrokPtahRunScope, "runId">;

export type GrokPtahExecutionMode = "shared" | "isolated_worktree";

export type GrokPtahBounds = {
  maxPromptBytes?: number;
  maxRounds?: number;
  maxDurationMs?: number;
};

/** The public contract caps model rounds even when a host has a lower ceiling. */
export const GROKPTAH_MAX_ROUNDS = 24;

/**
 * Validate caller-provided bounds before they cross an adapter boundary.
 *
 * The service performs the authoritative merge against its own ceilings; this
 * client-side check prevents malformed values from reaching transport and
 * keeps browser/desktop consumers aligned with the published schema.
 */
export function validateGrokPtahBounds(bounds: GrokPtahBounds): GrokPtahBounds {
  const fields: Array<[keyof GrokPtahBounds, string]> = [
    ["maxPromptBytes", "maxPromptBytes"],
    ["maxRounds", "maxRounds"],
    ["maxDurationMs", "maxDurationMs"],
  ];
  for (const [field, label] of fields) {
    const value = bounds[field];
    if (value === undefined) continue;
    if (!Number.isSafeInteger(value) || value <= 0) {
      throw new Error(`GrokPtah ${label} must be a positive safe integer`);
    }
  }
  if (bounds.maxRounds !== undefined && bounds.maxRounds > GROKPTAH_MAX_ROUNDS) {
    throw new Error(`GrokPtah maxRounds must be at most ${GROKPTAH_MAX_ROUNDS}`);
  }
  return bounds;
}

function optionalRounds(value: number | undefined): number | undefined {
  if (value === undefined) return undefined;
  validateGrokPtahBounds({ maxRounds: value });
  return value;
}

export type GrokPtahOperationResult<T = unknown> = {
  /** The structured result when supplied by the server, otherwise raw content. */
  value: T;
  /** The unmodified MCP result for consumers that need protocol details. */
  raw: unknown;
};

/** Raised when an adapter tries to use a capability that is not advertised or approved. */
export class GrokPtahCapabilityError extends Error {
  readonly capabilityId: string;
  readonly state: "unavailable" | "requires_gate";

  constructor(capabilityId: string, state: "unavailable" | "requires_gate") {
    super(
      state === "requires_gate"
        ? `GrokPtah capability ${capabilityId} requires an explicit approval`
        : `GrokPtah capability ${capabilityId} is unavailable`,
    );
    this.name = "GrokPtahCapabilityError";
    this.capabilityId = capabilityId;
    this.state = state;
  }
}

/**
 * Typed, scope-fenced operation helpers for desktop adapters and trusted
 * brokers. The transport client remains available for future tools, while
 * these methods keep common ContextDesk integrations from hand-building
 * snake_case payloads or accidentally dropping an identity fence.
 */
export class GrokPtahOperations {
  constructor(private readonly client: GrokPtahClient) {}

  get capabilities(): CapabilitySet | null {
    return this.client.capabilities;
  }

  async listSessions<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("session.observe");
    return this.invoke<T>("ptah_list_sessions", {});
  }

  async getCapacity<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("session.observe");
    return this.invoke<T>("ptah_get_capacity", {});
  }

  async getPersistentAgent<T = unknown>(
    scope: GrokPtahScope,
    agentId: string,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("agent.continuity");
    return this.invoke<T>("ptah_get_persistent_agent", {
      ...scopeArgs(scope),
      agent_id: nonEmpty(agentId, "agentId"),
    });
  }

  async listPersistentAgents<T = unknown>(
    scope: GrokPtahScope,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("agent.continuity");
    // The list endpoint is intentionally allowlist-wide and has an EmptyArgs
    // wire schema.  The scope is still required by this facade so callers must
    // present an authenticated run context, but it must not be serialized into
    // the request or the strict server parser rejects it as invalid_request.
    void scopeArgs(scope);
    return this.invoke<T>("ptah_list_persistent_agents", {});
  }

  async resumePersistentAgent<T = unknown>(
    scope: GrokPtahScope,
    agentId: string,
    requestId: string,
    prompt: string,
    maxRounds?: number,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("agent.resume");
    return this.invoke<T>("ptah_resume_persistent_agent", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      agent_id: nonEmpty(agentId, "agentId"),
      prompt: nonEmpty(prompt, "prompt"),
      ...(maxRounds === undefined ? {} : { max_rounds: optionalRounds(maxRounds) }),
    });
  }

  async getRun<T = unknown>(scope: GrokPtahRunScope): Promise<GrokPtahOperationResult<T>> {
    scopeArgs(scope);
    this.requireAvailable("run.review");
    return this.scoped<T>("ptah_get_run", scope);
  }

  async getProgress<T = unknown>(
    scope: GrokPtahRunScope,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.review");
    return this.scoped<T>("ptah_get_progress", scope);
  }

  async getChanges<T = unknown>(scope: GrokPtahRunScope): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.review");
    return this.scoped<T>("ptah_get_changes", scope);
  }

  async getTestResults<T = unknown>(
    scope: GrokPtahRunScope,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.review");
    return this.scoped<T>("ptah_get_test_results", scope);
  }

  async getHandoff<T = unknown>(scope: GrokPtahRunScope): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.review");
    return this.scoped<T>("ptah_get_handoff", scope);
  }

  async reviewRun<T = unknown>(scope: GrokPtahRunScope): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.review");
    return this.scoped<T>("ptah_review_run", scope);
  }

  async getEvents<T = unknown>(
    scope: GrokPtahRunScope,
    options: { afterSeq?: number; limit?: number } = {},
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.review");
    return this.invoke<T>("ptah_get_events", {
      ...scopeArgs(scope),
      ...(options.afterSeq === undefined ? {} : { after_seq: options.afterSeq }),
      ...(options.limit === undefined ? {} : { limit: options.limit }),
    });
  }

  async submitTask<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    prompt: string,
    options: {
      bounds?: GrokPtahBounds;
      executionMode?: GrokPtahExecutionMode;
      allowQueue?: boolean;
    } = {},
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.execute");
    return this.invoke<T>("ptah_submit_task", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      prompt: nonEmpty(prompt, "prompt"),
      ...(options.bounds ? { bounds: validateGrokPtahBounds(options.bounds) } : {}),
      ...(options.executionMode ? { execution_mode: options.executionMode } : {}),
      ...(options.allowQueue === undefined ? {} : { allow_queue: options.allowQueue }),
    });
  }

  async retryRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    prompt: string,
    options: {
      bounds?: GrokPtahBounds;
      executionMode?: GrokPtahExecutionMode;
      allowQueue?: boolean;
    } = {},
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.execute");
    return this.invoke<T>("ptah_retry_run", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      prompt: nonEmpty(prompt, "prompt"),
      ...(options.bounds ? { bounds: validateGrokPtahBounds(options.bounds) } : {}),
      ...(options.executionMode ? { execution_mode: options.executionMode } : {}),
      ...(options.allowQueue === undefined ? {} : { allow_queue: options.allowQueue }),
    });
  }

  async cancelRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.execute");
    return this.invoke<T>("ptah_cancel", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
    });
  }

  async getQueue<T = unknown>(scope: GrokPtahScope): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_get_queue", scopeArgs(scope));
  }

  async queuePrompt<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    prompt: string,
    priority?: boolean,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_queue_prompt", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      prompt: nonEmpty(prompt, "prompt"),
      ...(priority === undefined ? {} : { priority }),
    });
  }

  async steer<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    text: string,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_steer", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      text: nonEmpty(text, "text"),
    });
  }

  async editQueue<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    entryId: string,
    version: number,
    text: string,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_edit_queue", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      entry_id: nonEmpty(entryId, "entryId"),
      version,
      text: nonEmpty(text, "text"),
    });
  }

  async removeQueue<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    entryId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_remove_queue", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      entry_id: nonEmpty(entryId, "entryId"),
      expected_version: expectedVersion,
    });
  }

  async reorderQueue<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    entryId: string,
    toIndex: number,
    expectedVersion: number,
    expectedRevision: number,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_reorder_queue", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      entry_id: nonEmpty(entryId, "entryId"),
      to_index: toIndex,
      expected_version: expectedVersion,
      expected_revision: expectedRevision,
    });
  }

  async clearQueue<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_clear_queue", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
    });
  }

  async runNext<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    entryId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_run_next", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      entry_id: nonEmpty(entryId, "entryId"),
      expected_version: expectedVersion,
    });
  }

  async steerQueued<T = unknown>(
    scope: GrokPtahScope,
    requestId: string,
    entryId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("run.queue");
    return this.invoke<T>("ptah_steer_queued", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      entry_id: nonEmpty(entryId, "entryId"),
      expected_version: expectedVersion,
    });
  }

  async approveRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    sourceFingerprint: string,
    finalFingerprint: string,
    changedFiles: Array<{ path: string; summary: string }>,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireGated("run.promote", gateSatisfied);
    return this.invoke<T>("ptah_approve_run", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      source_fingerprint: nonEmpty(sourceFingerprint, "sourceFingerprint"),
      final_fingerprint: nonEmpty(finalFingerprint, "finalFingerprint"),
      changed_files: changedFiles,
    });
  }

  async promoteRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    approvalId: string,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireGated("run.promote", gateSatisfied);
    return this.invoke<T>("ptah_promote_run", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      approval_id: nonEmpty(approvalId, "approvalId"),
    });
  }

  async discardRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireGated("run.promote", gateSatisfied);
    return this.invoke<T>("ptah_discard_run", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
    });
  }

  async listComputerRuns<T = unknown>(
    scope: GrokPtahScope,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("computer.observe");
    return this.invoke<T>("ptah_list_computer_runs", scopeArgs(scope));
  }

  async getComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("computer.observe");
    return this.scoped<T>("ptah_get_computer_run", scope);
  }

  async getComputerRunEvents<T = unknown>(
    scope: GrokPtahRunScope,
    options: { afterSeq?: number; limit?: number } = {},
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("computer.observe");
    return this.invoke<T>("ptah_get_computer_run_events", {
      ...scopeArgs(scope),
      ...(options.afterSeq === undefined ? {} : { after_seq: options.afterSeq }),
      ...(options.limit === undefined ? {} : { limit: options.limit }),
    });
  }

  async getComputerCapacity<T = unknown>(
    scope: GrokPtahScope,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAvailable("computer.observe");
    return this.invoke<T>("ptah_get_computer_capacity", scopeArgs(scope));
  }

  /**
   * Ask a human at the trusted host to approve one exact control intent.
   *
   * This grants nothing. It returns a pending approval id plus the one-time
   * nonce that identifies this caller as the requester; the host serves that
   * nonce exactly once, here.
   */
  async requestComputerApproval<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
    lease: GrokPtahComputerLease,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAdvertised("computer.control");
    return this.invoke<T>("ptah_request_computer_approval", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      expected_version: expectedVersion,
      ...validateComputerLease(lease),
    });
  }

  /** Poll the lifecycle of an approval this same caller requested. */
  async getComputerApproval<T = unknown>(
    scope: GrokPtahScope,
    approvalId: string,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAdvertised("computer.control");
    return this.invoke<T>("ptah_get_computer_approval", {
      ...scopeArgs(scope),
      approval_id: nonEmpty(approvalId, "approvalId"),
    });
  }

  /**
   * Take Computer Use control by spending a host-issued approval receipt.
   *
   * The receipt is a required argument, not an optional assurance. There is
   * deliberately no Boolean here: a caller asserting "the gate is satisfied"
   * is not evidence of anything, and the authority decision belongs to the
   * host, which re-validates every binding and consumes the receipt exactly
   * once. The requested lease may narrow what the human approved; it can
   * never widen it.
   */
  async authorizeComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
    lease: GrokPtahComputerLease,
    receipt: GrokPtahApprovalReceipt,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAdvertised("computer.control");
    return this.invoke<T>("ptah_authorize_computer_run", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      expected_version: expectedVersion,
      ...validateComputerLease(lease),
      approval_id: nonEmpty(receipt.approvalId, "receipt.approvalId"),
      approval_nonce: nonEmpty(receipt.nonce, "receipt.nonce"),
    });
  }

  /**
   * De-escalating safety controls.
   *
   * These take no receipt by design. They only ever reduce agent authority,
   * and making it harder to stop a running agent than to start one would be a
   * safety regression. The host still applies the lease, revision, and
   * stale-observation fences, and revokes any outstanding receipt for the run.
   */
  async pauseComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.computerControl<T>("ptah_pause_computer_run", scope, requestId, expectedVersion);
  }

  async takeOverComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.computerControl<T>("ptah_take_over_computer_run", scope, requestId, expectedVersion);
  }

  async cancelComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.computerControl<T>("ptah_cancel_computer_run", scope, requestId, expectedVersion);
  }

  private async computerControl<T>(
    tool: string,
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireAdvertised("computer.control");
    return this.invoke<T>(tool, {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      expected_version: expectedVersion,
    });
  }

  private async scoped<T>(tool: string, scope: GrokPtahRunScope) {
    return this.invoke<T>(tool, scopeArgs(scope));
  }

  private async invoke<T>(
    tool: string,
    args: Record<string, unknown>,
  ): Promise<GrokPtahOperationResult<T>> {
    const result: GrokPtahCallResult = await this.client.callTool(tool, args);
    if (result.isError) {
      if (result.error) throw new GrokPtahRemoteError(result.error);
      throw new Error(`GrokPtah ${tool} returned an error`);
    }
    return {
      value: (result.structuredContent ?? result.content ?? result.raw) as T,
      raw: result.raw,
    };
  }

  private requireAvailable(id: string): void {
    const capability = findCapability(this.client.capabilities, id);
    if (capability && capabilityActionState(capability) === "ready") return;
    throw new GrokPtahCapabilityError(id, "unavailable");
  }

  private requireGated(id: string, gateSatisfied: boolean): void {
    const capability = findCapability(this.client.capabilities, id);
    const state = capabilityActionState(capability, gateSatisfied);
    if (state === "ready") return;
    throw new GrokPtahCapabilityError(id, state === "requires_gate" ? "requires_gate" : "unavailable");
  }

  /**
   * Assert only that the host advertises the capability.
   *
   * Used by the Computer Use control path, where the human gate is proven to
   * the *server* by a host-issued receipt. A client-side Boolean would add
   * nothing an attacker could not also set, so there is none: this check
   * exists to give a clear local error when a host does not expose the
   * capability at all, never to decide authority.
   */
  private requireAdvertised(id: string): void {
    const capability = findCapability(this.client.capabilities, id);
    if (capability && capability.availability !== "unavailable") return;
    throw new GrokPtahCapabilityError(id, "unavailable");
  }
}

/** Bounded action lease a human is asked to approve, or a caller narrows to. */
export type GrokPtahComputerLease = {
  actionClasses: Array<"semantic" | "text_entry">;
  ttlMs: number;
  /** Finite action budget. A human approves a bounded number of actions. */
  usesRemaining: number;
};

/** Host-issued receipt material, returned once by requestComputerApproval. */
export type GrokPtahApprovalReceipt = {
  approvalId: string;
  nonce: string;
};

function validateComputerLease(lease: GrokPtahComputerLease): Record<string, unknown> {
  if (!Array.isArray(lease.actionClasses) || lease.actionClasses.length === 0) {
    throw new Error("GrokPtah actionClasses must name at least one class");
  }
  for (const value of [lease.ttlMs, lease.usesRemaining]) {
    if (!Number.isSafeInteger(value) || value <= 0) {
      throw new Error("GrokPtah computer lease bounds must be positive safe integers");
    }
  }
  return {
    action_classes: lease.actionClasses,
    ttl_ms: lease.ttlMs,
    uses_remaining: lease.usesRemaining,
  };
}

function scopeArgs(scope: GrokPtahScope | GrokPtahRunScope): Record<string, unknown> {
  return {
    session_id: nonEmpty(scope.sessionId, "sessionId"),
    workspace: nonEmpty(scope.workspace, "workspace"),
    ...( "runId" in scope ? { run_id: nonEmpty(scope.runId, "runId") } : {}),
  };
}

function nonEmpty(value: string, field: string): string {
  if (!value.trim()) throw new Error(`GrokPtah ${field} must not be empty`);
  return value;
}
