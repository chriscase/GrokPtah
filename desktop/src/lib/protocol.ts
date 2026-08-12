/** Typed client mirror of bridge SessionUpdate + Tauri commands. */

import type { ActivityState } from "./activity";

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

export interface DurableRun {
  runId: string;
  sessionId: string;
  workspace: string;
  requestId: string;
  clientId?: string | null;
  state: DurableRunState;
  bounds: {
    maxPromptBytes: number;
    maxRounds: number;
    maxDurationMs: number;
  };
  promptPreview: string;
  startSeq?: number | null;
  endSeq?: number | null;
  createdAt: string;
  updatedAt: string;
  terminalResult?: string | null;
  finalResponse?: string | null;
  errorCode?: string | null;
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
      type: "steering_injected";
      session_id: string;
      steering_id: string;
      text: string;
    };

export interface PermissionRequest {
  id: string;
  session_id: string;
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
  open_tab_ids: string[];
  model: string;
  effort: string;
  sessions: SessionSummary[];
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
  supports_effort: boolean;
  effort_options?: string[];
}

export interface AuthState {
  signed_in: boolean;
  display_name?: string | null;
  method?: string | null;
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
