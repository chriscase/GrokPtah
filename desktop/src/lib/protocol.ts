/** Typed client mirror of bridge SessionUpdate + Tauri commands. */

import type { ActivityState } from "./activity";
import type { PromptQueueEntry } from "./promptQueue";

export type ToolCallKind = "read" | "edit" | "search" | "execute" | "think" | "other";
export type ToolCallStatus =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "denied";

export type CompletionStatus = "verified" | "unverified" | "failed" | "incomplete";

export interface CompletionEvidence {
  status: CompletionStatus;
  stopReason: string;
  interrupted: boolean;
  claims: {
    present: boolean;
    mentionsChanges: boolean;
    mentionsTests: boolean;
    mentionsVerification: boolean;
  };
  observations: {
    changedFiles: number;
    testsObserved: number;
    testsPassed: number;
    testsFailed: number;
    testsIncomplete: number;
    permissionsRequested: number;
    permissionsGranted: number;
    permissionsDenied: number;
    permissionsUnresolved: number;
  };
  usage: {
    promptTokens: number;
    completionTokens: number;
    totalTokens: number;
    requests: number;
  };
}

export interface SessionCompletionRecord {
  turn_id: string;
  completed_at: string;
  evidence: CompletionEvidence;
}

export type DurableRunState =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "limit_reached";

export type DurableRunStopCause =
  | "completed"
  | "round_limit"
  | "duration_limit"
  | "token_ceiling"
  | "token_accounting_unavailable"
  | "token_accounting_overflow"
  | "stationarity"
  | "recovery_exhausted"
  | "cancelled"
  | "interrupted"
  | "failed";

export type PersistentAgentState =
  | "active"
  | "waiting"
  | "interrupted"
  | "failed"
  | "completed";

export type RuntimeTarget = "local_desktop" | "local_service" | "hosted_service";
export type RuntimeConnectionState =
  | "connected"
  | "reconnecting"
  | "disconnected"
  | "error";

export interface PersistentAgentRunBounds {
  maxPromptBytes: number;
  maxRounds: number;
  maxDurationMs: number;
  maxTotalTokens?: number | null;
}

export interface PersistentAgentAuthorityPolicy {
  sandboxProfile: string;
  bypassPermissions: boolean;
  allowedTools: string[];
  allowedMcpServers: string[];
  computerUseAllowed: boolean;
  autoAllowedTools: string[];
  allowRules: string[];
  denyRules: string[];
}

export interface PersistentAgentSpec {
  schemaVersion: number;
  revision: number;
  previousRevision?: number | null;
  displayName: string;
  role: string;
  sourceWorkspace: string;
  model: {
    selectionKey: string;
    providerId: string;
    modelId: string;
  };
  defaultRunBounds: PersistentAgentRunBounds;
  authority: PersistentAgentAuthorityPolicy;
  memory: {
    projectScope: boolean;
    agentPrivateScope: boolean;
    teamIds: string[];
  };
  createdAt: string;
  createdBy: string;
  managedExecution?: {
    enabled: boolean;
    allowedWorkKinds?: string[];
    allowedSourceRoutineIds?: string[];
    maxConcurrentRuns?: number;
    retryEligible?: boolean;
    requiresApprovalBeforeExecution?: boolean;
  };
}

export interface PersistentAgentLaneAssociation {
  laneId: string;
  sourceWorkspace: string;
  attachedAt: string;
  attachedBy: string;
  detachedAt?: string | null;
  detachedBy?: string | null;
}

/** User-facing Lane projection; `id === session_id` during migration. */
export interface LaneSummary {
  id: string;
  session_id: string;
  agent_id?: string | null;
  title: string;
  cwd: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  archived?: boolean;
  archived_at?: string | null;
  kind?: SessionKind;
  execution_mode?: RunExecutionMode;
  workspace_status?: WorkspaceStatus;
  runtime_target?: RuntimeTarget;
  runtime_connection?: RuntimeConnectionState;
}

export interface PersistentAgent {
  agentId: string;
  sessionId: string;
  /** All durable Lanes attached to this Agent; old records expose the primary Lane. */
  laneIds?: string[];
  laneAssociations?: PersistentAgentLaneAssociation[];
  workspace: string;
  model: string;
  spec?: PersistentAgentSpec;
  state: PersistentAgentState;
  currentRunId?: string | null;
  lastRunId?: string | null;
  lastLaneId?: string | null;
  latestCheckpointId?: string | null;
  continuationOrdinal: number;
  createdAt: string;
  updatedAt: string;
}

export interface PersistentAgentCheckpoint {
  checkpointId: string;
  agentId: string;
  sessionId: string;
  runId: string;
  agentSpecRevision?: number | null;
  parentCheckpointId?: string | null;
  ordinal: number;
  workspace: string;
  contextSummary: string;
  contextHash: string;
  eventSeq: number;
  reason: string;
  createdAt: string;
}

export interface PersistentAgentResumePlan {
  agent: PersistentAgent;
  checkpoint: PersistentAgentCheckpoint;
  parentRunId: string;
}

export interface RemoteServiceStatus {
  connected: boolean;
  baseUrl?: string | null;
  runtimeTarget?: RuntimeTarget | null;
  connectionState?: RuntimeConnectionState;
  lastError?: string | null;
}

export interface RemoteSessionTarget {
  sessionId: string;
  title: string;
  workspace: string;
  updatedAt: string;
  busy: boolean;
  runtimeTarget?: RuntimeTarget;
  runtimeConnection?: RuntimeConnectionState;
}

export interface RemoteTaskSubmission {
  runId: string;
  sessionId: string;
  state: DurableRunState;
  requestId: string;
  executionMode: RunExecutionMode;
  queuedPosition?: number | null;
}

export type DurableWorkState =
  | "queued"
  | "blocked"
  | "leased"
  | "running"
  | "awaiting_input"
  | "awaiting_approval"
  | "review"
  | "succeeded"
  | "failed"
  | "cancelled";

export type DurableWorkAttemptState =
  | "leased"
  | "running"
  | "awaiting_input"
  | "awaiting_approval"
  | "review"
  | "released"
  | "expired"
  | "succeeded"
  | "failed"
  | "cancelled";

export interface DurableWorkRetryPolicy {
  maxAttempts: number;
  retryFailed: boolean;
  retryExpired: boolean;
  backoffMs: number;
}

export interface DurableWorkPolicy {
  bounds: PersistentAgentRunBounds;
  retry: DurableWorkRetryPolicy;
  requiresApproval: boolean;
  maxConcurrentAttempts: number;
}

export interface DurableWorkDependency {
  workId: string;
  requiredState: DurableWorkState;
}

export interface DurableWorkProgress {
  summary: string;
  percent?: number | null;
  updatedAt: string;
}

export interface DurableWorkArtifactRef {
  artifactId: string;
  kind: string;
  label: string;
  digest?: string | null;
  retainedUntil?: string | null;
}

export interface DurableWorkResult {
  summary: string;
  evidence: string[];
  artifacts: DurableWorkArtifactRef[];
  failure?: string | null;
  cancellationReason?: string | null;
  completedAt: string;
}

export interface DurableWorkApproval {
  reviewerId: string;
  note?: string | null;
  approvedAt: string;
}

export interface DurableWorkItem {
  schemaVersion: number;
  workId: string;
  kind: string;
  objective: string;
  sessionId: string;
  workspace: string;
  createdBy: string;
  assignedAgentId?: string | null;
  priority: number;
  deadline?: string | null;
  parentWorkId?: string | null;
  dependencies: DurableWorkDependency[];
  policy: DurableWorkPolicy;
  state: DurableWorkState;
  revision: number;
  attemptCount: number;
  progress?: DurableWorkProgress | null;
  result?: DurableWorkResult | null;
  approval?: DurableWorkApproval | null;
  sourceRoutineId?: string | null;
  sourceActivationId?: string | null;
  createdAt: string;
  updatedAt: string;
}

/** Desktop-safe attempt projection; lease credentials are never included. */
export interface DurableWorkAttempt {
  schemaVersion: number;
  attemptId: string;
  workId: string;
  attemptNumber: number;
  claimantId: string;
  acquiredAt: string;
  leaseExpiresAt: string;
  lastHeartbeatAt: string;
  state: DurableWorkAttemptState;
  linkedRunIds: string[];
  progress?: DurableWorkProgress | null;
  result?: DurableWorkResult | null;
  terminalReason?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface RemoteWorkSnapshot {
  work: DurableWorkItem;
  attempts: DurableWorkAttempt[];
}

export type RoutineLifecycle = "enabled" | "paused" | "disabled";
export type MissedRunPolicy = "skip" | "coalesce" | "catch_up";
export type ActivationDisposition =
  | "created_work"
  | "deduplicated"
  | "skipped_paused"
  | "skipped_disabled"
  | "skipped_overlap"
  | "skipped_missed"
  | "skipped_expired"
  | "rejected"
  | "backoff"
  | "circuit_open";

export interface DurableRoutine {
  schemaVersion: number;
  routineId: string;
  name: string;
  agentId: string;
  sessionId: string;
  workspace: string;
  lifecycle: RoutineLifecycle;
  trigger: { kind: string; [key: string]: unknown };
  workTemplate: {
    kind: string;
    objective: string;
    priority: number;
    policy: DurableWorkPolicy;
  };
  missedRunPolicy: MissedRunPolicy;
  concurrency: { maxInFlight: number; overlap: string };
  retry: { backoffMs: number; maxBackoffMs: number; circuitFailures: number };
  revision: number;
  nextFireAt?: string | null;
  lastFireAt?: string | null;
  consecutiveFailures: number;
  circuitOpen: boolean;
  createdBy: string;
  createdAt: string;
  updatedAt: string;
}

export interface DurableActivation {
  schemaVersion: number;
  activationId: string;
  routineId: string;
  routineRevision: number;
  triggerKind: string;
  dedupeKey: string;
  scheduledAt: string;
  receivedAt: string;
  firedAt: string;
  workId?: string | null;
  disposition: ActivationDisposition;
  error?: string | null;
  capturedPolicy: {
    routineRevision: number;
    agentId?: string | null;
    agentSpecRevision?: number | null;
    computerUseAllowed: boolean;
    autoApproveTools: boolean;
  };
  createdBy: string;
}

export interface RemoteRoutineSnapshot {
  routine: DurableRoutine;
  activations: DurableActivation[];
}

export interface RemoteRunScope {
  sessionId: string;
  workspace: string;
  runId: string;
}

export interface RemoteRunEvent extends DurableRunEvent {
  runId: string;
  sessionId: string;
  workspace: string;
}

export interface RemoteRunRecovery {
  runId: string;
  sessionId: string;
  workspace: string;
  afterSeq: number;
  reason: string;
}

export type RunExecutionMode = "shared" | "isolated_worktree";
export type PromotionState =
  | "not_applicable"
  | "preparing"
  | "ready"
  | "promoted"
  | "conflicted"
  | "discarded";

export interface RunExecution {
  mode: RunExecutionMode;
  sourceWorkspace: string;
  executionWorkspace: string;
  baseRevision: string;
  sourceFingerprint: string;
  finalFingerprint?: string | null;
  promotionState: PromotionState;
  promotedAt?: string | null;
}

export interface RunReview {
  changedFiles: Array<{ path: string; summary: string }>;
  diff: string;
  diffTruncated: boolean;
  fingerprint: string;
}

export interface RunApproval {
  approvalId: string;
  runId: string;
  sessionId: string;
  workspace: string;
  sourceFingerprint: string;
  finalFingerprint: string;
  changedFiles: Array<{ path: string; summary: string }>;
  issuedAt: string;
  expiresAt: string;
}

export interface DurableRunEvent {
  seq: number;
  ts: string;
  update: SessionUpdate;
}

export interface DurableRunEventPage {
  entries: DurableRunEvent[];
  nextCursor?: number | null;
  cursorExpired: boolean;
}

/**
 * Local in-process host Build-run record (`run_list` / `run_get`).
 * Remote `grokptah.public-run.v1` documents are a different contract; parse
 * them with `parsePublicRunV1` / `parseRemotePublicRun` and stamp
 * session/workspace from the request. Do not map public DTOs onto this type.
 */
export interface DurableRun {
  runId: string;
  sessionId: string;
  /** Compatibility projection; old payloads derive this from sessionId. */
  laneId?: string | null;
  workspace: string;
  requestId: string;
  clientId?: string | null;
  state: DurableRunState;
  agentId?: string | null;
  /** Source run for an explicit post-restart replacement. */
  retryOf?: string | null;
  /** Verified continuation source, distinct from explicit retry replacement. */
  parentRunId?: string | null;
  /** Immutable Agent specification revision frozen for this finite Run. */
  agentSpecRevision?: number | null;
  /** Verified checkpoint used for an explicit continuation. */
  checkpointId?: string | null;
  continuationContextId?: string | null;
  continuationContextHash?: string | null;
  continuationFidelity?: "complete" | "degraded" | null;
  /** One-based position while waiting in the bounded MCP admission queue. */
  queuePosition?: number | null;
  bounds: {
    maxPromptBytes: number;
    maxRounds: number;
    maxDurationMs: number;
    maxTotalTokens?: number | null;
  };
  promptPreview: string;
  startSeq?: number | null;
  endSeq?: number | null;
  createdAt: string;
  updatedAt: string;
  terminalResult?: string | null;
  finalResponse?: string | null;
  errorCode?: string | null;
  stopCause?: DurableRunStopCause | null;
  aggregates: {
    changes: Array<{ path: string; summary: string }>;
    tests: Array<{
      callId: string;
      command?: string | null;
      status: string;
      exitCode?: number | null;
      cancelled?: boolean | null;
    }>;
    permissionsRequested: number;
    permissionsGranted: number;
    permissionsDenied: number;
    usage: CompletionEvidence["usage"];
    usageComplete?: boolean;
    usagePendingRequests?: number;
    verification?: CompletionEvidence | null;
  };
  progress?: {
    round: number;
    maxRounds: number;
    lastTool?: string | null;
    detail: string;
    updatedAt: string;
  } | null;
  execution?: RunExecution | null;
  approval?: RunApproval | null;
}

export type SessionUpdate =
  | { type: "agent_message_chunk"; session_id: string; text: string }
  | { type: "agent_thought_chunk"; session_id: string; text: string }
  | {
      type: "tool_call";
      session_id: string;
      call_id: string;
      title: string;
      kind: ToolCallKind;
      status: ToolCallStatus;
      input: unknown;
    }
  | {
      type: "tool_call_update";
      session_id: string;
      call_id: string;
      status: ToolCallStatus;
      output?: string | null;
    }
  | {
      type: "plan";
      session_id: string;
      steps: string[];
      status: string;
    }
  | {
      type: "permission_required";
      session_id: string;
      request: PermissionRequest;
    }
  | { type: "turn_complete"; session_id: string; cancelled: boolean }
  | { type: "turn_started"; session_id: string; turn_id: string }
  | {
      type: "completion_evidence";
      session_id: string;
      turn_id: string;
      evidence: CompletionEvidence;
    }
  | { type: "error"; session_id: string; message: string }
  | {
      type: "subagent_spawned";
      session_id: string;
      subagent_id: string;
      kind: string;
      title: string;
    }
  | {
      type: "subagent_update";
      session_id: string;
      subagent_id: string;
      status: string;
      detail?: string | null;
    }
  | {
      type: "background_task";
      session_id?: string | null;
      task_id: string;
      title: string;
      status: string;
    }
  | {
      type: "shell_session_started";
      session_id: string;
      call_id: string;
      command: string;
    }
  | {
      type: "shell_output";
      session_id: string;
      call_id: string;
      data: string;
    }
  | {
      type: "shell_session_ended";
      session_id: string;
      call_id: string;
      exit_code?: number | null;
      cancelled: boolean;
    }
  | {
      type: "file_edit";
      session_id: string;
      path: string;
      summary: string;
      unified_diff: string;
    }
  | {
      type: "agent_progress";
      session_id: string;
      round: number;
      max_rounds: number;
      last_tool?: string | null;
      detail: string;
    }
  | {
      type: "rate_limited";
      session_id: string;
      message: string;
      retry_after_ms?: number | null;
    }
  | {
      type: "prompt_queue_changed";
      session_id: string;
      /**
       * Per-session commit sequence stamped under the bridge's mutation lock.
       * Publishing happens after that lock is released, so snapshots can
       * arrive out of commit order — apply one only if its revision is
       * greater than the newest already applied.
       */
      revision: number;
      entries: PromptQueueEntry[];
      action: string;
      origin: string;
      changed_entry?: PromptQueueEntry | null;
      disposition?: "pending" | "queued" | null;
    }
  | {
      type: "steering_injected";
      session_id: string;
      steering_id: string;
      text: string;
    };

export interface PermissionRequest {
  id: string;
  session_id: string;
  run_id?: string | null;
  tool_name: string;
  summary: string;
  detail: unknown;
}

export type SessionKind = "build" | "chat";
export type RunOrigin = "desktop" | "mcp" | "other";
export type WorkspaceStatus =
  | "ready"
  | "missing"
  | "inaccessible"
  | "not_directory";

export interface SessionSummary {
  id: string;
  agent_id?: string | null;
  title: string;
  cwd: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  forked_from?: string | null;
  folder?: string | null;
  tags?: string[];
  archived?: boolean;
  archived_at?: string | null;
  kind?: SessionKind;
  execution_mode?: RunExecutionMode;
  workspace_status?: WorkspaceStatus;
}

export interface SearchHit {
  session_id: string;
  title: string;
  kind: SessionKind;
  folder?: string | null;
  tags: string[];
  archived: boolean;
  score: number;
  keyword_score: number;
  semantic_score: number;
  snippet: string;
  match_field: string;
  message_index?: number | null;
  updated_at: string;
}

/** Restored from `~/.grokptah/workspace.json` on app launch. */
export interface WorkspaceUiState {
  project_cwd?: string | null;
  active_session?: string | null;
  /** Explicit Lane context; equal to active_session during compatibility projection. */
  active_lane_id?: string | null;
  open_tab_ids: string[];
  model: string;
  effort: string;
  sessions: SessionSummary[];
  lanes?: LaneSummary[];
}

/** Attention badge for tabs/sidebar when this session isn't focused. */
export type AttentionKind = "none" | "unseen" | "permission";

/** Client-side open workspace (Claude Code–style concurrent session tab). */
export interface SessionTab {
  id: string;
  title: string;
  /** Stable client-side identity even when the sidebar list is mode-filtered. */
  kind: SessionKind;
  /** Last known working directory for this tab, including when hidden by the mode filter. */
  cwd?: string;
  /** Backend-owned workspace health; non-ready states require explicit recovery. */
  workspaceStatus?: WorkspaceStatus;
  transcript: TranscriptItem[];
  busy: boolean;
  plan: { steps: string[]; status: string } | null;
  /** Live turn indicator (server activity vs idle/done). */
  activity: ActivityState;
  /** Agent loop round (from AgentProgress). */
  agentRound?: number | null;
  /** Last tool name for fleet strip. */
  lastTool?: string | null;
  /** Unread activity while the user was on another tab. */
  unseen: boolean;
  /** Distinct “needs your button” state (permission / plan accept). */
  needsPermission: boolean;
  /** #174 fleet: running subagents for this session */
  runningSubagents?: number;
  /** #174 fleet: total tokens this session (when known) */
  totalTokens?: number;
  /** Last authoritative completion summary for this session. */
  completionEvidence: CompletionEvidence | null;
  /** Turn identity associated with the live or restored evidence. */
  completionTurnId: string | null;
  /** Optional source of the currently active run, when known. */
  runOrigin?: RunOrigin | null;
}

export type TranscriptItem =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; streaming?: boolean }
  | { kind: "thought"; text: string; streaming?: boolean }
  | {
      kind: "tool";
      callId: string;
      title: string;
      status: string;
      output?: string;
    }
  | { kind: "plan"; steps: string[]; status: string }
  | { kind: "error"; text: string };

export interface ModelInfo {
  id: string;
  display_name: string;
  provider_id?: string;
  provider_label?: string;
  wire_model_id?: string;
  supports_tools?: boolean;
  supports_stream?: boolean;
  supports_image_input?: boolean;
  computer_use_tier?: ComputerUseTier;
  computer_capability_source?: "declared" | "measured" | "unknown" | string;
  capability_source?: "declared" | "measured" | "unknown" | string;
  supports_effort: boolean;
  effort_options?: string[];
}

export interface ProviderModelSummary {
  id: string;
  displayName: string;
  supportsTools: boolean;
  supportsStream: boolean;
  supportsImageInput: boolean;
  computerUseTier: ComputerUseTier;
  computerCapabilitySource: "declared" | "measured" | "unknown";
  effortOptions: string[];
  capabilitySource: "declared" | "measured" | "unknown";
}

export interface ProviderProfileSummary {
  id: string;
  label: string;
  baseUrl: string;
  deadlineClass: "interactive" | "standard" | "extended";
  credentialSet: boolean;
  managedByEnv: boolean;
  models: ProviderModelSummary[];
}

export interface QualificationCheck {
  status: "pass" | "degraded" | "fail";
  detail: string;
}

export interface ProviderQualificationReport {
  providerId: string;
  modelId: string;
  basicGeneration: QualificationCheck;
  nativeToolCall: QualificationCheck;
  toolResultContinuation: QualificationCheck;
  streaming: QualificationCheck;
  codingReady: boolean;
  semanticObservation: QualificationCheck;
  staleObservationRecovery: QualificationCheck;
  computerUseTier: ComputerUseTier;
}

export type ComputerUseTier =
  | "none"
  | "observe"
  | "semantic_act"
  | "visual_fallback_act";

export interface AuthState {
  signed_in: boolean;
  display_name?: string | null;
  method?: string | null;
}

export type ComputerPermissionStatus =
  | "unsupported"
  | "missing"
  | "prompt_pending"
  | "denied"
  | "granted"
  | "revoked"
  | "restricted";

export interface ComputerPlatformStatus {
  platformId: string;
  available: boolean;
  minimumOsVersion?: string | null;
  screenRecording: ComputerPermissionStatus;
  accessibility: ComputerPermissionStatus;
  detail?: string | null;
}

export interface ComputerTargetCandidate {
  selectionToken: string;
  target: {
    appId: string;
    windowId: string;
    generation: number;
    displayName: string;
    sensitivity: string;
  };
  geometry: {
    x: number;
    y: number;
    width: number;
    height: number;
    scaleFactor: number;
  };
  onScreen: boolean;
  active: boolean;
  minimized: boolean;
}

export interface ComputerObservationPreview {
  observation: {
    observationId: string;
    sequence: number;
    target: ComputerTargetCandidate["target"];
    elements: Array<{
      elementId: string;
      role: string;
      label?: string | null;
      value?: string | null;
      sensitivity: string;
    }>;
    elementsTruncated: boolean;
    screenshot?: {
      contentSha256: string;
      byteLen: number;
      width: number;
      height: number;
      redacted: boolean;
      assetId: string;
    } | null;
  };
  imageDataUrl?: string | null;
}

export type ComputerAction =
  | { type: "activate_target" }
  | { type: "invoke"; element_id: string }
  | { type: "set_value"; element_id: string; text: string }
  | { type: "select"; element_id: string }
  | {
      type: "scroll";
      element_id: string;
      delta_x: number;
      delta_y: number;
    };

export interface ComputerSemanticElement {
  elementId: string;
  role: string;
  label?: string | null;
  value?: string | null;
  enabled: boolean;
  focused: boolean;
  sensitivity: string;
  actions: string[];
}

export interface ComputerRun {
  runId: string;
  ownerSessionId: string;
  target: ComputerTargetCandidate["target"];
  state: string;
  controlDisposition?:
    | "agent_owned"
    | "paused"
    | "operator_takeover"
    | "stopped"
    | "interrupted"
    | "uncertain_outcome"
    | string;
  controlEpoch?: number;
  version: number;
  createdAt: string;
  updatedAt: string;
  startedAt?: string | null;
  endedAt?: string | null;
  limits: {
    maxActions: number;
    maxDurationSecs: number;
  };
  actionCount: number;
  currentObservation?: {
    observationId: string;
    sequence: number;
    capturedAt: string;
    target: ComputerTargetCandidate["target"];
    elements: ComputerSemanticElement[];
    elementsTruncated: boolean;
  } | null;
  grant?: {
    grantId: string;
    expiresAt: string;
    usesRemaining?: number | null;
    revokedAt?: string | null;
    actionClasses: string[];
  } | null;
  lastOutcome?: {
    summary: string;
    expectedPostconditionMet?: boolean | null;
  } | null;
  lastError?: { code: string; message: string } | null;
  audit: Array<{
    sequence: number;
    at: string;
    operation: string;
    disposition: string;
    actionClass?: string | null;
    observationId?: string | null;
    errorCode?: string | null;
  }>;
}

export interface PendingComputerApproval {
  approvalId: string;
  ownerSessionId: string;
  runId: string;
  runVersion: number;
  observationId: string;
  targetLabel: string;
  action: ComputerAction;
  actionSummary: string;
  risk: string;
  createdAt: string;
}

export type ComputerControlDisposition =
  | "agent_owned"
  | "paused"
  | "operator_takeover"
  | "stopped"
  | "interrupted"
  | "uncertain_outcome";

/**
 * Authoritative Computer Run view. This mirrors the Rust
 * `ComputerRunProjection` exactly: it is the same serialized payload an
 * external coordinator receives, so the cockpit cannot disagree with an
 * outside observer about run state, control disposition, or epoch.
 *
 * Observed element labels, values, and evidence tokens are deliberately
 * absent — those stay on `ComputerRun`, which is local-only. Action summary
 * text and error messages are likewise local-only; the projection carries
 * only structured last-outcome / last-error summaries.
 */
export interface ComputerRunProjection {
  runId: string;
  ownerSessionId: string;
  parentRunId?: string | null;
  campaignId?: string | null;
  target: {
    appId: string;
    windowId: string;
    generation: number;
    displayName: string;
    sensitivity: string;
  };
  state: string;
  controlDisposition: ComputerControlDisposition | string;
  controlEpoch: number;
  version: number;
  /** True only while the backend is mid-observation or mid-action. */
  agentActive: boolean;
  terminal: boolean;
  createdAt: string;
  updatedAt: string;
  startedAt?: string | null;
  endedAt?: string | null;
  progress: {
    actionCount: number;
    maxActions: number;
    evidenceBytes: number;
    maxEvidenceBytes: number;
    elapsedMillis?: number | null;
    maxDurationSecs: number;
    durationExceeded: boolean;
  };
  grant?: {
    grantId: string;
    actionClasses: string[];
    issuedBy: string;
    issuedAt: string;
    expiresAt: string;
    usesRemaining?: number | null;
    revoked: boolean;
    expired: boolean;
  } | null;
  observation?: {
    observationId: string;
    sequence: number;
    capturedAt: string;
    elementCount: number;
    elementsTruncated: boolean;
    sensitivity: string;
    hasScreenshot: boolean;
    screenshotRedacted?: boolean | null;
    stale: boolean;
  } | null;
  lastOutcome?: {
    expectedPostconditionMet?: boolean | null;
  } | null;
  lastError?: { code: string } | null;
  eventRange?: { startSeq: number; endSeq: number } | null;
  /**
   * Redacted result of the most recent adaptive planner/executor review.
   *
   * Present only for actions dispatched with a planner claim; absent for every
   * run driven through the plain action path. Dispositions, one closed reason,
   * and integer confidence only — the same payload a coordinator receives, so
   * the cockpit cannot show a decision an outside observer cannot.
   *
   * `admitted: false` means the review refused an action the policy layer had
   * already authorized; the run's audit journal carries the matching
   * `errorCode`.
   */
  adaptive?: AdaptiveDecisionSummary | null;
}

/** Rung of the adaptive disposition ladder, least to most conservative. */
export type AdaptiveDisposition =
  | "commit"
  | "disambiguate"
  | "request_approval"
  | "escalate"
  | "refuse";

/** Explicitly selected efficiency profile. There is no implicit default. */
export type AdaptiveProfile = "economy" | "balanced" | "high_assurance";

/**
 * Redacted view of one adaptive review. Mirrors the Rust
 * `AdaptiveDecisionSummary` exactly.
 */
export interface AdaptiveDecisionSummary {
  profile: AdaptiveProfile | string;
  planner: AdaptiveDisposition | string;
  executor: AdaptiveDisposition | string;
  resolved: AdaptiveDisposition | string;
  reason: string;
  /** True when planner and executor did not reach the same rung. */
  disagreed: boolean;
  /** True when the review left the already-authorized action in place. */
  admitted: boolean;
  actionClass: string;
  topConfidenceBps: number;
  marginBps: number;
  candidateCount: number;
  /** Staleness bound actually enforced, after clamping to the run's own. */
  appliedAgeBoundMillis: number;
  controlEpoch: number;
  decidedAt: string;
}

export interface ComputerCockpitSnapshot {
  backend: {
    backendId: string;
    observe: boolean;
    semanticActions: boolean;
    textEntry: boolean;
    keyChords: boolean;
    pointerFallback: boolean;
  };
  origin: "desktop" | "mcp" | string;
  /** Authoritative projection; prefer this over `run` for status rendering. */
  projection?: ComputerRunProjection | null;
  /** Local-only detail retained for approval rendering. */
  run?: ComputerRun | null;
  pendingApproval?: PendingComputerApproval | null;
}

export interface ComputerAgentEligibility {
  model: string;
  tier: ComputerUseTier;
  source: string;
}

export interface ComputerAgentProposalResult {
  snapshot: ComputerCockpitSnapshot;
  summary: string;
  completed: boolean;
}

export type SubagentExecutionMode =
  | "unknown"
  | "worktree"
  | "project_copy"
  | "shared_read_only"
  | "shared_mutating"
  | "isolation_failed";

export interface SubagentInfo {
  id: string;
  kind: string;
  title: string;
  status: string;
  session_id?: string | null;
  summary?: string | null;
  last_tool?: string | null;
  cwd?: string | null;
  execution_mode: SubagentExecutionMode;
}

export interface AgentStatus {
  running: boolean;
  project_cwd?: string | null;
  active_session?: string | null;
  always_approve: boolean;
  model: string;
  effort: string;
  sandbox_profile: string;
  appearance: string;
  auto_update_enabled: boolean;
}

/** Normalize serde externally-tagged / snake_case payloads from Rust. */
export function normalizeSessionUpdate(raw: unknown): SessionUpdate | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  // serde externally tagged: { "agent_message_chunk": { ... } } OR internally tagged with type
  if (typeof o.type === "string") {
    return o as unknown as SessionUpdate;
  }
  const keys = Object.keys(o);
  if (keys.length === 1) {
    const type = keys[0];
    const body = o[type];
    if (body && typeof body === "object") {
      return { type, ...(body as object) } as SessionUpdate;
    }
  }
  return null;
}

export {
  PUBLIC_RUN_SCHEMA_VERSION,
  PublicRunDtoError,
  parsePublicRunHandoffV1,
  parsePublicRunListV1,
  parsePublicRunProgressV1,
  parsePublicRunV1,
  parseRemotePublicRun,
  parseRemotePublicRunList,
} from "./publicRun";
export type {
  PublicRunDtoErrorKind,
  PublicRunHandoffV1,
  PublicRunListV1,
  PublicRunProgressV1,
  PublicRunState,
  PublicRunV1,
  RemotePublicRun,
  RemotePublicRunList,
} from "./publicRun";

export const SLASH_COMMANDS = [
  { cmd: "/help", desc: "Show commands" },
  { cmd: "/plan", desc: "Propose a plan (accept starts execution)" },
  { cmd: "/yolo", desc: "Always approve tools" },
  { cmd: "/explore", desc: "Spawn explore subagent" },
  { cmd: "/compact", desc: "Shrink server context (keeps full local history)" },
  { cmd: "/model", desc: "Show or set model id" },
  { cmd: "/effort", desc: "Show or set effort level" },
  { cmd: "/clear", desc: "Clear session transcript" },
  { cmd: "/context", desc: "Context / compact window stats" },
  { cmd: "/mcp", desc: "List MCP servers + doctor" },
  { cmd: "/skills", desc: "List discovered skills" },
  { cmd: "/sandbox", desc: "Show or set tool safety profile (not an OS sandbox)" },
  { cmd: "/resume", desc: "Open session browser to resume history" },
  { cmd: "/continue", desc: "Continue the most recently updated other session" },
  { cmd: "/fork", desc: "Fork the focused session (new id + history copy)" },
  { cmd: "/rename", desc: "Rename the focused session: /rename <title>" },
  { cmd: "/export", desc: "Export focused session transcript to clipboard" },
  { cmd: "/cd", desc: "Set project/session working directory: /cd <path>" },
] as const;
