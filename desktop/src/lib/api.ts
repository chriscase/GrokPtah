import { invoke } from "@tauri-apps/api/core";
import type {
  AgentStatus,
  AuthState,
  ModelInfo,
  SessionSummary,
  WorkspaceUiState,
} from "./protocol";

export const api = {
  agentStart: () => invoke<void>("agent_start"),
  agentStop: () => invoke<void>("agent_stop"),
  agentStatus: () => invoke<AgentStatus>("agent_status"),
  setProjectCwd: (path: string) => invoke<string>("set_project_cwd", { path }),
  pickProjectFolder: () => invoke<string | null>("pick_project_folder"),
  sessionNew: () => invoke<SessionSummary>("session_new"),
  sessionLoad: (id: string) => invoke<SessionSummary>("session_load", { id }),
  sessionList: () => invoke<SessionSummary[]>("session_list"),
  /** Full workspace restore (sessions + open tabs + project). */
  workspaceState: () => invoke<WorkspaceUiState>("workspace_state"),
  setOpenTabs: (tabIds: string[], activeId?: string | null) =>
    invoke<void>("set_open_tabs", {
      tabIds,
      activeId: activeId ?? null,
    }),
  sessionPrompt: (sessionId: string, prompt: string) =>
    invoke<string>("session_prompt", { sessionId, prompt }),
  /** Cancel one session's turn, or all active turns when sessionId omitted. */
  sessionCancel: (sessionId?: string | null) =>
    invoke<void>("session_cancel", {
      sessionId: sessionId ?? null,
    }),
  sessionTranscript: (sessionId: string) =>
    invoke<{ role: string; text: string }[]>("session_transcript", {
      sessionId,
    }),
  sessionFork: (sourceId: string) =>
    invoke<SessionSummary>("session_fork", { sourceId }),
  sessionRewind: (sessionId: string, keepMessages: number) =>
    invoke<SessionSummary>("session_rewind", {
      sessionId,
      keepMessages,
    }),
  sessionCompact: (sessionId: string) =>
    invoke<SessionSummary>("session_compact", { sessionId }),
  permissionRespond: (requestId: string, decision: string) =>
    invoke<void>("permission_respond", { requestId, decision }),
  listModels: () => invoke<ModelInfo[]>("list_models"),
  setModel: (model: string) => invoke<void>("set_model", { model }),
  setEffort: (effort: string) => invoke<void>("set_effort", { effort }),
  setAlwaysApprove: (value: boolean) =>
    invoke<void>("set_always_approve", { value }),
  authState: () => invoke<AuthState>("auth_state"),
  signInLocal: (displayName: string) =>
    invoke<AuthState>("sign_in_local", { displayName }),
  signOut: () => invoke<AuthState>("sign_out"),
  authSetApiKey: (apiKey: string, displayName: string) =>
    invoke<AuthState>("auth_set_api_key", { apiKey, displayName }),
  authOpenLogin: () => invoke<string>("auth_open_login"),
  fileTree: () => invoke<string[]>("file_tree"),
  fuzzyOpen: (query: string) => invoke<string[]>("fuzzy_open", { query }),
  gitStatus: () => invoke<string>("git_status"),
  gitDiff: () => invoke<string>("git_diff"),
  agentEditDiffs: () => invoke<string>("agent_edit_diffs"),
  gitStageAll: () => invoke<string>("git_stage_all"),
  gitCommit: (message: string) => invoke<string>("git_commit", { message }),
  listWorktrees: () => invoke<string>("list_worktrees"),
  mcpList: () => invoke<unknown[]>("mcp_list"),
  mcpSetEnabled: (name: string, enabled: boolean) =>
    invoke("mcp_set_enabled", { name, enabled }),
  mcpDoctor: () => invoke<string[]>("mcp_doctor"),
  mcpAddStdio: (name: string, command: string, args: string[]) =>
    invoke<void>("mcp_add_stdio", { name, command, args }),
  pluginsList: () => invoke<unknown[]>("plugins_list"),
  pluginInstall: (id: string) => invoke("plugin_install", { id }),
  skillsList: () => invoke<unknown[]>("skills_list"),
  hooksConfig: () => invoke<string>("hooks_config"),
  subagentsList: () => invoke<unknown[]>("subagents_list"),
  backgroundTasks: () => invoke<unknown[]>("background_tasks"),
  cancelBackgroundTask: (id: string) =>
    invoke<void>("cancel_background_task", { id }),
  scheduleBackgroundTask: (title: string) =>
    invoke("schedule_background_task", { title }),
  settingsSnapshot: () => invoke<Record<string, unknown>>("settings_snapshot"),
  setSandbox: (profile: string) => invoke<void>("set_sandbox", { profile }),
  setAppearance: (appearance: string) =>
    invoke<void>("set_appearance", { appearance }),
  setPermissionMode: (mode: string) =>
    invoke<void>("set_permission_mode", { mode }),
  setAllowDenyRules: (allow: string[], deny: string[]) =>
    invoke<void>("set_allow_deny_rules", { allow, deny }),
  projectRules: () => invoke<string[]>("project_rules"),
  setPlanMode: (sessionId: string, enabled: boolean) =>
    invoke<void>("set_plan_mode", { sessionId, enabled }),
  acceptPlan: (sessionId: string) =>
    invoke<void>("accept_plan", { sessionId }),
  rejectPlan: (sessionId: string) =>
    invoke<void>("reject_plan", { sessionId }),
  productInfo: () =>
    invoke<{ name: string; bridgeVersion: string; autoUpdateEnabled: boolean }>(
      "product_info",
    ),
  ptyCreate: (cols: number, rows: number) =>
    invoke<string>("pty_create", { cols, rows }),
  ptyCreateCommand: (command: string, cols: number, rows: number) =>
    invoke<string>("pty_create_command", { command, cols, rows }),
  ptyWrite: (id: string, data: string) =>
    invoke<void>("pty_write", { id, data }),
  ptyResize: (id: string, cols: number, rows: number) =>
    invoke<void>("pty_resize", { id, cols, rows }),
  ptyKill: (id: string) => invoke<void>("pty_kill", { id }),
  ptyList: () => invoke<string[]>("pty_list"),
  ptyBacklog: (id: string) => invoke<string>("pty_backlog", { id }),
};
