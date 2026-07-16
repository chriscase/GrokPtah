/** Typed client mirror of bridge SessionUpdate + Tauri commands. */

import type { ActivityState } from "./activity";

export type ToolCallKind = "read" | "edit" | "search" | "execute" | "think" | "other";
export type ToolCallStatus =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "denied";

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
    };

export interface PermissionRequest {
  id: string;
  session_id: string;
  tool_name: string;
  summary: string;
  detail: unknown;
}

export type SessionKind = "build" | "chat";

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
}

export interface AuthState {
  signed_in: boolean;
  display_name?: string | null;
  method?: string | null;
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
  { cmd: "/sandbox", desc: "Show or set sandbox profile" },
] as const;
