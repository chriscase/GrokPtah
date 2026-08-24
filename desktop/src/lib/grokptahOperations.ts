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
    return this.invoke<T>("ptah_list_sessions", {});
  }

  async getCapacity<T = unknown>(): Promise<GrokPtahOperationResult<T>> {
    return this.invoke<T>("ptah_get_capacity", {});
  }

  async getPersistentAgent<T = unknown>(
    scope: GrokPtahScope,
    agentId: string,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.invoke<T>("ptah_get_persistent_agent", {
      ...scopeArgs(scope),
      agent_id: nonEmpty(agentId, "agentId"),
    });
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
      ...(maxRounds === undefined ? {} : { max_rounds: maxRounds }),
    });
  }

  async getRun<T = unknown>(scope: GrokPtahRunScope): Promise<GrokPtahOperationResult<T>> {
    return this.scoped<T>("ptah_get_run", scope);
  }

  async getProgress<T = unknown>(
    scope: GrokPtahRunScope,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.scoped<T>("ptah_get_progress", scope);
  }

  async getChanges<T = unknown>(scope: GrokPtahRunScope): Promise<GrokPtahOperationResult<T>> {
    return this.scoped<T>("ptah_get_changes", scope);
  }

  async getTestResults<T = unknown>(
    scope: GrokPtahRunScope,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.scoped<T>("ptah_get_test_results", scope);
  }

  async getHandoff<T = unknown>(scope: GrokPtahRunScope): Promise<GrokPtahOperationResult<T>> {
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
      ...(options.bounds ? { bounds: options.bounds } : {}),
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
      ...(options.bounds ? { bounds: options.bounds } : {}),
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

  async authorizeComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
    actionClasses: Array<"semantic" | "text_entry">,
    ttlMs: number,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireGated("computer.control", gateSatisfied);
    return this.invoke<T>("ptah_authorize_computer_run", {
      request_id: nonEmpty(requestId, "requestId"),
      ...scopeArgs(scope),
      expected_version: expectedVersion,
      action_classes: actionClasses,
      ttl_ms: ttlMs,
    });
  }

  async pauseComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.computerControl<T>("ptah_pause_computer_run", scope, requestId, expectedVersion, gateSatisfied);
  }

  async takeOverComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.computerControl<T>("ptah_take_over_computer_run", scope, requestId, expectedVersion, gateSatisfied);
  }

  async cancelComputerRun<T = unknown>(
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
    gateSatisfied = false,
  ): Promise<GrokPtahOperationResult<T>> {
    return this.computerControl<T>("ptah_cancel_computer_run", scope, requestId, expectedVersion, gateSatisfied);
  }

  private async computerControl<T>(
    tool: string,
    scope: GrokPtahRunScope,
    requestId: string,
    expectedVersion: number,
    gateSatisfied: boolean,
  ): Promise<GrokPtahOperationResult<T>> {
    this.requireGated("computer.control", gateSatisfied);
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
