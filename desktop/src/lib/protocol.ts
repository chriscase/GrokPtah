/** Typed client mirror of bridge SessionUpdate + Tauri commands. */

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
    };

export interface PermissionRequest {
  id: string;
  session_id: string;
  tool_name: string;
  summary: string;
  detail: unknown;
}

export interface SessionSummary {
  id: string;
  title: string;
  cwd: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  forked_from?: string | null;
}

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
  { cmd: "/plan", desc: "Enter plan mode" },
  { cmd: "/yolo", desc: "Always approve tools" },
  { cmd: "/explore", desc: "Spawn explore subagent" },
  { cmd: "/compact", desc: "Compact conversation (use UI too)" },
] as const;
