import { invoke } from "@tauri-apps/api/core";
import type {
  AgentStatus,
  AuthState,
  ComputerObservationPreview,
  ComputerAction,
  ComputerCockpitSnapshot,
  ComputerPermissionStatus,
  ComputerPlatformStatus,
  ComputerTargetCandidate,
  ModelInfo,
  SearchHit,
  SessionCompletionRecord,
  SessionKind,
  SessionSummary,
  SubagentInfo,
  DurableRun,
  DurableRunEventPage,
  RunExecutionMode,
  RunReview,
  WorkspaceUiState,
} from "./protocol";
import type {
  PromptQueueEntry,
  PromptQueueRunNextResult,
  PromptQueueTakeResult,
  SteeringReceipt,
} from "./promptQueue";

export const api = {
  agentStart: () => invoke<void>("agent_start"),
  agentStop: () => invoke<void>("agent_stop"),
  agentStatus: () => invoke<AgentStatus>("agent_status"),
  computerUseStatus: () =>
    invoke<ComputerPlatformStatus>("computer_use_status"),
  computerUseRequestPermission: (
    permission: "screen_recording" | "accessibility",
  ) =>
    invoke<ComputerPermissionStatus>("computer_use_request_permission", {
      permission,
    }),
  computerUseListTargets: () =>
    invoke<ComputerTargetCandidate[]>("computer_use_list_targets"),
  computerUseObserveOnce: (selectionToken: string) =>
    invoke<ComputerObservationPreview>("computer_use_observe_once", {
      selectionToken,
    }),
  computerUseCockpitSnapshot: (sessionId: string) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_snapshot", {
      sessionId,
    }),
  computerUseCockpitStartSimulator: (
    sessionId: string,
    reviewedTargetAppId: string,
  ) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_start_simulator", {
      sessionId,
      reviewedTargetAppId,
    }),
  computerUseCockpitStartNative: (
    sessionId: string,
    selectionToken: string,
    reviewedTargetAppId: string,
  ) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_start_native", {
      sessionId,
      selectionToken,
      reviewedTargetAppId,
    }),
  computerUseCockpitRefresh: (
    sessionId: string,
    runId: string,
    expectedVersion: number,
  ) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_refresh", {
      sessionId,
      runId,
      expectedVersion,
    }),
  computerUseCockpitStageAction: (
    sessionId: string,
    runId: string,
    expectedVersion: number,
    observationId: string,
    action: ComputerAction,
  ) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_stage_action", {
      sessionId,
      runId,
      expectedVersion,
      observationId,
      action,
    }),
  computerUseCockpitApprove: (
    sessionId: string,
    approvalId: string,
    requestId: string,
  ) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_approve", {
      sessionId,
      approvalId,
      requestId,
    }),
  computerUseCockpitDiscardApproval: (sessionId: string) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_discard_approval", {
      sessionId,
    }),
  computerUseCockpitPause: (
    sessionId: string,
    runId: string,
    expectedVersion: number,
  ) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_pause", {
      sessionId,
      runId,
      expectedVersion,
    }),
  computerUseCockpitTakeOver: (
    sessionId: string,
    runId: string,
    expectedVersion: number,
  ) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_take_over", {
      sessionId,
      runId,
      expectedVersion,
    }),
  computerUseCockpitStop: (sessionId: string, runId: string) =>
    invoke<ComputerCockpitSnapshot>("computer_use_cockpit_stop", {
      sessionId,
      runId,
    }),
  setProjectCwd: (path: string) => invoke<string>("set_project_cwd", { path }),
  pickProjectFolder: () => invoke<string | null>("pick_project_folder"),
  sessionNew: () => invoke<SessionSummary>("session_new"),
  sessionNewKind: (kind: SessionKind | string) =>
    invoke<SessionSummary>("session_new_kind", { kind }),
  sessionListByKind: (kind: SessionKind | string, includeArchived = false) =>
    invoke<SessionSummary[]>("session_list_by_kind", {
      kind,
      includeArchived,
    }),
  searchSessions: (opts: {
    query: string;
    mode?: "hybrid" | "keyword" | "semantic" | string;
    kind?: "all" | "chat" | "build" | string;
    includeArchived?: boolean;
    limit?: number;
    folder?: string | null;
    tag?: string | null;
  }) =>
    invoke<SearchHit[]>("search_sessions", {
      query: opts.query,
      mode: opts.mode ?? "hybrid",
      kind: opts.kind ?? "all",
      includeArchived: opts.includeArchived ?? false,
      limit: opts.limit ?? 40,
      folder: opts.folder ?? null,
      tag: opts.tag ?? null,
    }),
  sessionLoad: (id: string) => invoke<SessionSummary>("session_load", { id }),
  sessionList: () => invoke<SessionSummary[]>("session_list"),
  sessionListArchived: () => invoke<SessionSummary[]>("session_list_archived"),
  sessionListAll: () => invoke<SessionSummary[]>("session_list_all"),
  sessionRename: (sessionId: string, title: string) =>
    invoke<SessionSummary>("session_rename", { sessionId, title }),
  sessionDelete: (sessionId: string) =>
    invoke<void>("session_delete", { sessionId }),
  sessionArchive: (sessionId: string, archived: boolean) =>
    invoke<SessionSummary>("session_archive", { sessionId, archived }),
  sessionSetFolder: (sessionId: string, folder: string | null) =>
    invoke<SessionSummary>("session_set_folder", { sessionId, folder }),
  sessionSetCwd: (sessionId: string, path: string) =>
    invoke<SessionSummary>("session_set_cwd", { sessionId, path }),
  sessionSetExecutionMode: (sessionId: string, mode: RunExecutionMode) =>
    invoke<SessionSummary>("session_set_execution_mode", { sessionId, mode }),
  /** Folder picker that sets cwd on one session (build project root). */
  pickSessionFolder: (sessionId: string) =>
    invoke<SessionSummary | null>("pick_session_folder", { sessionId }),
  sessionSetTags: (sessionId: string, tags: string[]) =>
    invoke<SessionSummary>("session_set_tags", { sessionId, tags }),
  sessionListFolders: (includeArchived = false) =>
    invoke<string[]>("session_list_folders", { includeArchived }),
  sessionListTags: (includeArchived = false) =>
    invoke<string[]>("session_list_tags", { includeArchived }),
  /** Full workspace restore (sessions + open tabs + project). */
  workspaceState: () => invoke<WorkspaceUiState>("workspace_state"),
  setOpenTabs: (tabIds: string[], activeId?: string | null) =>
    invoke<void>("set_open_tabs", {
      tabIds,
      activeId: activeId ?? null,
    }),
  sessionPrompt: (sessionId: string, prompt: string) =>
    invoke<string>("session_prompt", { sessionId, prompt }),
  sessionQueueList: (sessionId: string) =>
    invoke<PromptQueueEntry[]>("session_queue_list", { sessionId }),
  sessionQueueAdd: (
    sessionId: string,
    text: string,
    priority = false,
  ) =>
    invoke<PromptQueueEntry[]>("session_queue_add", {
      sessionId,
      text,
      priority,
    }),
  sessionQueueEdit: (
    sessionId: string,
    entryId: string,
    version: number,
    text: string,
  ) =>
    invoke<PromptQueueEntry[]>("session_queue_edit", {
      sessionId,
      entryId,
      version,
      text,
    }),
  sessionQueueRemove: (sessionId: string, entryId: string) =>
    invoke<PromptQueueEntry[]>("session_queue_remove", {
      sessionId,
      entryId,
    }),
  sessionQueueClear: (sessionId: string) =>
    invoke<PromptQueueEntry[]>("session_queue_clear", { sessionId }),
  sessionQueueMove: (
    sessionId: string,
    entryId: string,
    toIndex: number,
  ) =>
    invoke<PromptQueueEntry[]>("session_queue_move", {
      sessionId,
      entryId,
      toIndex,
    }),
  sessionQueueTakeNext: (sessionId: string) =>
    invoke<PromptQueueTakeResult>("session_queue_take_next", { sessionId }),
  sessionQueueRunNext: (sessionId: string, entryId: string) =>
    invoke<PromptQueueRunNextResult>("session_queue_run_next", {
      sessionId,
      entryId,
    }),
  sessionQueueSteerEntry: (sessionId: string, entryId: string) =>
    invoke<SteeringReceipt>("session_queue_steer_entry", {
      sessionId,
      entryId,
    }),
  sessionSteer: (sessionId: string, text: string) =>
    invoke<SteeringReceipt>("session_steer", { sessionId, text }),
  /** Cancel one session's turn, or all active turns when sessionId omitted. */
  sessionCancel: (sessionId?: string | null) =>
    invoke<void>("session_cancel", {
      sessionId: sessionId ?? null,
    }),
  sessionTranscript: (sessionId: string) =>
    invoke<
      {
        role: string;
        text: string;
        tool_call_id?: string | null;
        tool_title?: string | null;
        tool_status?: string | null;
        tool_output?: string | null;
      }[]
    >("session_transcript", { sessionId }),
  sessionCompletionHistory: (sessionId: string) =>
    invoke<SessionCompletionRecord[]>("session_completion_history", {
      sessionId,
    }),
  runList: (sessionId: string) =>
    invoke<DurableRun[]>("run_list", { sessionId }),
  runGet: (sessionId: string, runId: string) =>
    invoke<DurableRun | null>("run_get", { sessionId, runId }),
  runEvents: (sessionId: string, runId: string, afterSeq = 0, limit = 80) =>
    invoke<DurableRunEventPage>("run_events", { sessionId, runId, afterSeq, limit }),
  runReview: (sessionId: string, runId: string) =>
    invoke<RunReview>("run_review", { sessionId, runId }),
  runApprove: (sessionId: string, runId: string, ttlMs?: number) =>
    invoke<DurableRun>("run_approve", { sessionId, runId, ttlMs }),
  runPromote: (sessionId: string, runId: string) =>
    invoke<DurableRun>("run_promote", { sessionId, runId }),
  runDiscard: (sessionId: string, runId: string) =>
    invoke<DurableRun>("run_discard", { sessionId, runId }),
  runRetry: (sessionId: string, runId: string, prompt: string) =>
    invoke<string>("run_retry", { sessionId, runId, prompt }),
  runSteer: (sessionId: string, runId: string, text: string) =>
    invoke<void>("run_steer", { sessionId, runId, text }),
  runCancel: (sessionId: string, runId: string) =>
    invoke<void>("run_cancel", { sessionId, runId }),
  sessionFork: (sourceId: string) =>
    invoke<SessionSummary>("session_fork", { sourceId }),
  sessionRewind: (
    sessionId: string,
    keepMessages: number,
    mode?: "conversation" | "files" | "all" | string,
  ) =>
    invoke<SessionSummary>("session_rewind", {
      mode: mode ?? "conversation",
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
  lastEditedPath: () => invoke<string | null>("last_edited_path"),
  exportTranscript: (sessionId: string) =>
    invoke<string>("export_transcript", { sessionId }),
  memoryList: () =>
    invoke<{ id: string; text: string; tags: string[]; updated_at: string }[]>(
      "memory_list",
    ),
  memoryRemember: (text: string) =>
    invoke<string>("memory_remember", { text }),
  gitStageAll: () => invoke<string>("git_stage_all"),
  gitCommit: (message: string) => invoke<string>("git_commit", { message }),
  listWorktrees: () => invoke<string>("list_worktrees"),
  createWorktree: (path: string, branch?: string | null) =>
    invoke<string>("create_worktree", { path, branch: branch ?? null }),
  removeWorktree: (path: string) =>
    invoke<string>("remove_worktree", { path }),
  mcpList: () => invoke<unknown[]>("mcp_list"),
  mcpProjectTrust: () =>
    invoke<{
      project: string | null;
      has_local_mcp: boolean;
      trusted: boolean;
      decided: boolean;
    }>("mcp_project_trust"),
  mcpSetProjectTrust: (trusted: boolean) =>
    invoke<{
      project: string | null;
      has_local_mcp: boolean;
      trusted: boolean;
      decided: boolean;
    }>("mcp_set_project_trust", { trusted }),
  mcpSetEnabled: (name: string, enabled: boolean) =>
    invoke("mcp_set_enabled", { name, enabled }),
  mcpDoctor: () => invoke<string[]>("mcp_doctor"),
  mcpAddStdio: (name: string, command: string, args: string[]) =>
    invoke<void>("mcp_add_stdio", { name, command, args }),
  pluginsList: () => invoke<unknown[]>("plugins_list"),
  pluginInstall: (id: string) => invoke("plugin_install", { id }),
  skillsList: () => invoke<unknown[]>("skills_list"),
  hooksConfig: () => invoke<string>("hooks_config"),
  subagentsList: () => invoke<SubagentInfo[]>("subagents_list"),
  listAgents: () => invoke<unknown[]>("list_agents"),
  listPersonas: () => invoke<unknown[]>("list_personas"),
  fleetObservability: () =>
    invoke<{
      running_subagents_total?: number;
      sessions?: Array<{
        session_id: string;
        running_subagents?: number;
        total_tokens?: number;
        busy?: boolean;
      }>;
    }>("fleet_observability"),
  cancelSubagent: (id: string) => invoke<void>("cancel_subagent", { id }),
  backgroundTasks: () => invoke<unknown[]>("background_tasks"),
  cancelBackgroundTask: (id: string) =>
    invoke<void>("cancel_background_task", { id }),
  scheduleBackgroundTask: (title: string) =>
    invoke("schedule_background_task", { title }),
  settingsSnapshot: () => invoke<Record<string, unknown>>("settings_snapshot"),
  setSandbox: (profile: string) => invoke<void>("set_sandbox", { profile }),
  setSubagentIsolation: (mode: "worktree" | "shared") =>
    invoke<void>("set_subagent_isolation", { mode }),
  setAppearance: (appearance: string) =>
    invoke<void>("set_appearance", { appearance }),
  setPermissionMode: (mode: string) =>
    invoke<void>("set_permission_mode", { mode }),
  setAllowDenyRules: (allow: string[], deny: string[]) =>
    invoke<void>("set_allow_deny_rules", { allow, deny }),
  setGatewayConfig: (
    providerId: string,
    baseUrl: string,
    apiKey?: string | null,
  ) =>
    invoke<void>("set_gateway_config", {
      providerId,
      baseUrl,
      apiKey: apiKey ?? null,
    }),
  projectRules: () => invoke<string[]>("project_rules"),
  setPlanMode: (sessionId: string, enabled: boolean) =>
    invoke<void>("set_plan_mode", { sessionId, enabled }),
  acceptPlan: (sessionId: string) =>
    invoke<string>("accept_plan", { sessionId }),
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
  /** Backlog + seq watermark so live events don't double-render after tab switch (#138). */
  ptyBacklog: (id: string) => invoke<PtyBacklog>("pty_backlog", { id }),
};

export type PtyBacklog = {
  data: string;
  upToSeq: number;
};
