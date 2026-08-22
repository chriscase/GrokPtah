import { invoke } from "@tauri-apps/api/core";
import type {
  AgentStatus,
  AuthState,
  ComputerObservationPreview,
  ComputerAgentEligibility,
  ComputerAgentProposalResult,
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
  ProviderQualificationReport,
  NativeCodingReadinessProjection,
  PersistentAgent,
  PersistentAgentResumePlan,
  LaneSummary,
  RemoteSessionTarget,
  RemoteServiceStatus,
  RemoteRunScope,
  RemoteTaskSubmission,
  DurableWorkItem,
  DurableRoutine,
  RemoteWorkSnapshot,
  RemoteRoutineSnapshot,
  DurableActivation,
} from "./protocol";
import type {
  PromptQueueEntry,
  PromptQueueRunNextResult,
  PromptQueueSnapshot,
  PromptQueueTakeResult,
  SteeringReceipt,
} from "./promptQueue";

export const api = {
  agentStart: () => invoke<void>("agent_start"),
  agentStop: () => invoke<void>("agent_stop"),
  agentStatus: () => invoke<AgentStatus>("agent_status"),
  remoteServiceConnect: (baseUrl: string, token: string) =>
    invoke<RemoteServiceStatus>("remote_service_connect", { baseUrl, token }),
  remoteServiceDisconnect: () => invoke<void>("remote_service_disconnect"),
  remoteServiceStatus: () =>
    invoke<RemoteServiceStatus>("remote_service_status"),
  remoteServiceSessionList: () =>
    invoke<RemoteSessionTarget[]>("remote_service_session_list"),
  remoteServiceSessionCreate: (workspace: string, title?: string) =>
    invoke<RemoteSessionTarget>("remote_service_session_create", {
      workspace,
      title: title ?? null,
    }),
  remoteServiceTaskSubmit: (
    sessionId: string,
    workspace: string,
    prompt: string,
    executionMode: RunExecutionMode = "shared",
    allowQueue = true,
  ) =>
    invoke<RemoteTaskSubmission>("remote_service_task_submit", {
      sessionId,
      workspace,
      prompt,
      executionMode,
      allowQueue,
    }),
  remoteServiceRunList: () =>
    invoke<DurableRun[]>("remote_service_run_list"),
  remoteServiceWorkList: (sessionId: string, workspace: string) =>
    invoke<DurableWorkItem[]>("remote_service_work_list", {
      sessionId,
      workspace,
    }),
  remoteServiceWorkGet: (
    sessionId: string,
    workspace: string,
    workId: string,
  ) =>
    invoke<RemoteWorkSnapshot>("remote_service_work_get", {
      sessionId,
      workspace,
      workId,
    }),
  workList: (sessionId: string) =>
    invoke<DurableWorkItem[]>("work_list", { sessionId }),
  workGet: (sessionId: string, workId: string) =>
    invoke<RemoteWorkSnapshot | null>("work_get", { sessionId, workId }),
  workCreate: (
    sessionId: string,
    kind: string,
    objective: string,
    priority = 0,
    requiresApproval = false,
  ) =>
    invoke<DurableWorkItem>("work_create", {
      sessionId,
      kind,
      objective,
      priority,
      requiresApproval,
    }),
  workAssign: (
    sessionId: string,
    workId: string,
    assignedAgentId: string | null,
    expectedRevision?: number,
  ) =>
    invoke<DurableWorkItem>("work_assign", {
      sessionId,
      workId,
      assignedAgentId,
      expectedRevision: expectedRevision ?? null,
    }),
  workRetry: (sessionId: string, workId: string, reason: string, expectedRevision?: number) =>
    invoke<DurableWorkItem>("work_retry", {
      sessionId,
      workId,
      reason,
      expectedRevision: expectedRevision ?? null,
    }),
  workApprove: (sessionId: string, workId: string, note?: string, expectedRevision?: number) =>
    invoke<DurableWorkItem>("work_approve", {
      sessionId,
      workId,
      note: note ?? null,
      expectedRevision: expectedRevision ?? null,
    }),
  workCancel: (sessionId: string, workId: string, reason: string, expectedRevision?: number) =>
    invoke<DurableWorkItem>("work_cancel", {
      sessionId,
      workId,
      reason,
      expectedRevision: expectedRevision ?? null,
    }),
  remoteServiceWorkCreate: (
    sessionId: string,
    workspace: string,
    kind: string,
    objective: string,
    priority = 0,
    requiresApproval = false,
  ) =>
    invoke<DurableWorkItem>("remote_service_work_create", {
      sessionId,
      workspace,
      kind,
      objective,
      priority,
      requiresApproval,
    }),
  remoteServiceWorkAssign: (
    sessionId: string,
    workspace: string,
    workId: string,
    assignedAgentId: string | null,
    expectedRevision?: number,
  ) =>
    invoke<DurableWorkItem>("remote_service_work_assign", {
      sessionId,
      workspace,
      workId,
      assignedAgentId,
      expectedRevision: expectedRevision ?? null,
    }),
  remoteServiceWorkRetry: (
    sessionId: string,
    workspace: string,
    workId: string,
    reason: string,
    expectedRevision?: number,
  ) =>
    invoke<DurableWorkItem>("remote_service_work_retry", {
      sessionId,
      workspace,
      workId,
      reason,
      expectedRevision: expectedRevision ?? null,
    }),
  remoteServiceWorkApprove: (
    sessionId: string,
    workspace: string,
    workId: string,
    note?: string,
    expectedRevision?: number,
  ) =>
    invoke<DurableWorkItem>("remote_service_work_approve", {
      sessionId,
      workspace,
      workId,
      note: note ?? null,
      expectedRevision: expectedRevision ?? null,
    }),
  remoteServiceWorkCancel: (
    sessionId: string,
    workspace: string,
    workId: string,
    reason: string,
    expectedRevision?: number,
  ) =>
    invoke<DurableWorkItem>("remote_service_work_cancel", {
      sessionId,
      workspace,
      workId,
      reason,
      expectedRevision: expectedRevision ?? null,
    }),
  routineList: (sessionId: string) =>
    invoke<DurableRoutine[]>("routine_list", { sessionId }),
  routineGet: (sessionId: string, routineId: string) =>
    invoke<RemoteRoutineSnapshot | null>("routine_get", { sessionId, routineId }),
  routineCreate: (sessionId: string, name: string, agentId: string, objective: string) =>
    invoke<DurableRoutine>("routine_create", { sessionId, name, agentId, objective }),
  routineSetLifecycle: (
    sessionId: string,
    routineId: string,
    lifecycle: "enabled" | "paused" | "disabled",
    expectedRevision?: number,
  ) =>
    invoke<DurableRoutine>("routine_set_lifecycle", {
      sessionId,
      routineId,
      lifecycle,
      expectedRevision: expectedRevision ?? null,
    }),
  routineFire: (sessionId: string, routineId: string) =>
    invoke<DurableActivation>("routine_fire", { sessionId, routineId }),
  remoteServiceRoutineList: (sessionId: string, workspace: string) =>
    invoke<DurableRoutine[]>("remote_service_routine_list", { sessionId, workspace }),
  remoteServiceRoutineGet: (sessionId: string, workspace: string, routineId: string) =>
    invoke<RemoteRoutineSnapshot>("remote_service_routine_get", {
      sessionId,
      workspace,
      routineId,
    }),
  remoteServiceRoutineCreate: (
    sessionId: string,
    workspace: string,
    name: string,
    agentId: string,
    objective: string,
  ) =>
    invoke<DurableRoutine>("remote_service_routine_create", {
      sessionId,
      workspace,
      name,
      agentId,
      objective,
    }),
  remoteServiceRoutineSetLifecycle: (
    sessionId: string,
    workspace: string,
    routineId: string,
    lifecycle: "enabled" | "paused" | "disabled",
    expectedRevision?: number,
  ) =>
    invoke<DurableRoutine>("remote_service_routine_set_lifecycle", {
      sessionId,
      workspace,
      routineId,
      lifecycle,
      expectedRevision: expectedRevision ?? null,
    }),
  remoteServiceRoutineFire: (sessionId: string, workspace: string, routineId: string) =>
    invoke<DurableActivation>("remote_service_routine_fire", {
      sessionId,
      workspace,
      routineId,
    }),
  remoteServiceRunGet: (sessionId: string, workspace: string, runId: string) =>
    invoke<DurableRun>("remote_service_run_get", { sessionId, workspace, runId }),
  remoteServiceRunEvents: (
    sessionId: string,
    workspace: string,
    runId: string,
    afterSeq = 0,
    limit = 80,
  ) =>
    invoke<DurableRunEventPage>("remote_service_run_events", {
      sessionId,
      workspace,
      runId,
      afterSeq,
      limit,
    }),
  remoteServiceRunSteer: (sessionId: string, workspace: string, text: string) =>
    invoke<void>("remote_service_run_steer", { sessionId, workspace, text }),
  remoteServiceRunCancel: (sessionId: string, workspace: string, runId: string) =>
    invoke<void>("remote_service_run_cancel", { sessionId, workspace, runId }),
  remoteServiceWatchRuns: (scopes: RemoteRunScope[]) =>
    invoke<void>("remote_service_watch_runs", { scopes }),
  persistentAgentList: () =>
    invoke<PersistentAgent[]>("persistent_agent_list"),
  persistentAgentGet: (agentId: string) =>
    invoke<PersistentAgent | null>("persistent_agent_get", { agentId }),
  persistentAgentSetManagedExecution: (agentId: string, enabled: boolean) =>
    invoke<PersistentAgent | null>("persistent_agent_set_managed_execution", {
      agentId,
      enabled,
    }),
  persistentAgentAttachSession: (sessionId: string, agentId: string) =>
    invoke<PersistentAgent>("persistent_agent_attach_session", {
      sessionId,
      agentId,
    }),
  laneList: (includeArchived = false) =>
    invoke<LaneSummary[]>("lane_list", { includeArchived }),
  persistentAgentResumePlan: (sessionId: string) =>
    invoke<PersistentAgentResumePlan>("persistent_agent_resume_plan", {
      sessionId,
    }),
  persistentAgentResume: (
    sessionId: string,
    prompt: string,
    maxRounds?: number,
    requestId?: string,
  ) =>
    invoke<string>("persistent_agent_resume", {
      sessionId,
      prompt,
      maxRounds: maxRounds ?? null,
      requestId: requestId ?? null,
    }),
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
  computerUseCockpitAgentEligibility: (sessionId: string) =>
    invoke<ComputerAgentEligibility>("computer_use_cockpit_agent_eligibility", {
      sessionId,
    }),
  computerUseCockpitQualifyAgent: (sessionId: string) =>
    invoke<ComputerAgentEligibility>("computer_use_cockpit_qualify_agent", {
      sessionId,
    }),
  computerUseCockpitProposeAgentAction: (
    sessionId: string,
    runId: string,
    expectedVersion: number,
    observationId: string,
    objective: string,
  ) =>
    invoke<ComputerAgentProposalResult>("computer_use_cockpit_propose_agent_action", {
      sessionId,
      runId,
      expectedVersion,
      observationId,
      objective,
    }),
  computerUseCockpitCancelAgent: (sessionId: string) =>
    invoke<boolean>("computer_use_cockpit_cancel_agent", { sessionId }),
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
  sessionInspect: (id: string) =>
    invoke<SessionSummary>("session_inspect", { id }),
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
  /**
   * `reservation` must be the value a queue drain returned, so the drained
   * prompt starts the turn its drain reserved rather than racing for a new one.
   */
  sessionPrompt: (
    sessionId: string,
    prompt: string,
    reservation?: string | null,
  ) =>
    invoke<string>("session_prompt", {
      sessionId,
      prompt,
      reservation: reservation ?? null,
    }),
  /** Returns entries plus the revision they were read at (see the reducer). */
  sessionQueueList: (sessionId: string) =>
    invoke<PromptQueueSnapshot>("session_queue_list", { sessionId }),
  /** Give back a drained batch whose turn never started. */
  sessionQueueRestoreDrain: (
    sessionId: string,
    reservation: string | null | undefined,
    entries: PromptQueueEntry[],
  ) =>
    invoke<PromptQueueEntry[]>("session_queue_restore_drain", {
      sessionId,
      reservation: reservation ?? null,
      entries,
    }),
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
  /**
   * Every queue mutator is compare-and-set. `expectedVersion` is required
   * because the desktop is only one of two writers — an MCP coordinator can
   * mutate the same queue — so a mutation without a version is last-write-wins.
   * Pass the `version` of the entry as this client last saw it; a stale one is
   * rejected with a conflict and the caller should refetch.
   */
  sessionQueueRemove: (
    sessionId: string,
    entryId: string,
    expectedVersion: number,
  ) =>
    invoke<PromptQueueEntry[]>("session_queue_remove", {
      sessionId,
      entryId,
      expectedVersion,
    }),
  sessionQueueClear: (sessionId: string) =>
    invoke<PromptQueueEntry[]>("session_queue_clear", { sessionId }),
  /**
   * Reorder is fenced on the queue revision as well as the entry version:
   * `toIndex` is absolute, so it only means something against the ordering it
   * was computed from. Pass the revision this client last applied.
   */
  sessionQueueMove: (
    sessionId: string,
    entryId: string,
    toIndex: number,
    expectedVersion: number,
    expectedRevision: number,
  ) =>
    invoke<PromptQueueSnapshot>("session_queue_move", {
      sessionId,
      entryId,
      toIndex,
      expectedVersion,
      expectedRevision,
    }),
  sessionQueueTakeNext: (sessionId: string) =>
    invoke<PromptQueueTakeResult>("session_queue_take_next", { sessionId }),
  sessionQueueRunNext: (
    sessionId: string,
    entryId: string,
    expectedVersion: number,
  ) =>
    invoke<PromptQueueRunNextResult>("session_queue_run_next", {
      sessionId,
      entryId,
      expectedVersion,
    }),
  sessionQueueSteerEntry: (
    sessionId: string,
    entryId: string,
    expectedVersion: number,
  ) =>
    invoke<SteeringReceipt>("session_queue_steer_entry", {
      sessionId,
      entryId,
      expectedVersion,
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
  memoryList: (
    sessionId: string,
    scope:
      | { kind: "project" }
      | { kind: "agent_private"; agent_id: string }
      | { kind: "team"; team_id: string },
  ) =>
    invoke<{ id: string; text: string; tags: string[]; updated_at: string }[]>(
      "memory_list",
      { sessionId, scope },
    ),
  memoryRemember: (
    sessionId: string,
    scope:
      | { kind: "project" }
      | { kind: "agent_private"; agent_id: string }
      | { kind: "team"; team_id: string },
    text: string,
  ) => invoke<string>("memory_remember", { sessionId, scope, text }),
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
  nativeCodingReadiness: (providerId: string, modelId: string) =>
    invoke<NativeCodingReadinessProjection>("native_coding_readiness", {
      providerId,
      modelId,
    }),
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
  upsertProviderProfile: (
    providerId: string,
    label: string,
    baseUrl: string,
    modelId: string,
    deadlineClass: "interactive" | "standard" | "extended",
    effortOptions: string[],
    apiKey?: string | null,
  ) =>
    invoke<void>("upsert_provider_profile", {
      providerId,
      label,
      baseUrl,
      modelId,
      deadlineClass,
      effortOptions,
      apiKey: apiKey ?? null,
    }),
  discoverProviderModels: (providerId: string) =>
    invoke<ModelInfo[]>("discover_provider_models", { providerId }),
  qualifyProviderModel: (providerId: string, modelId: string) =>
    invoke<ProviderQualificationReport>("qualify_provider_model", {
      providerId,
      modelId,
    }),
  deleteProviderProfile: (providerId: string) =>
    invoke<void>("delete_provider_profile", { providerId }),
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
