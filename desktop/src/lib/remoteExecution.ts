import type {
  LaneSummary,
  RemoteSessionTarget,
  RemoteServiceStatus,
  RemoteTaskSubmission,
  RuntimeConnectionState,
  RuntimeTarget,
} from "./protocol";

export type ExecutionTargetChoice =
  | { kind: "local" }
  | { kind: "remote"; sessionId: string };

/** Stable value used by the composer target picker. */
export function executionTargetValue(choice: ExecutionTargetChoice): string {
  return choice.kind === "local" ? "local" : `remote:${choice.sessionId}`;
}

/** Parse picker values without allowing malformed remote ids into a request. */
export function parseExecutionTargetValue(value: string): ExecutionTargetChoice | null {
  if (value === "local") return { kind: "local" };
  if (!value.startsWith("remote:")) return null;
  const sessionId = value.slice("remote:".length).trim();
  return sessionId ? { kind: "remote", sessionId } : null;
}

/** User-facing acknowledgement for a remote submission. */
export function remoteSubmissionMessage(
  session: RemoteSessionTarget,
  submission: RemoteTaskSubmission,
): string {
  const label = session.title || session.workspace;
  return `Submitted to ${label}. Run ${submission.runId} is ${submission.state}.`;
}

function runtimeTargetForServiceUrl(baseUrl?: string | null): RuntimeTarget {
  if (!baseUrl) return "hosted_service";
  try {
    const url = new URL(baseUrl);
    return ["localhost", "127.0.0.1", "::1"].includes(url.hostname)
      ? "local_service"
      : "hosted_service";
  } catch {
    return "hosted_service";
  }
}

function runtimeConnectionForService(
  status: RemoteServiceStatus,
  session: RemoteSessionTarget,
): RuntimeConnectionState {
  if (status.connectionState === "error") return "error";
  if (!status.connected && status.connectionState) return status.connectionState;
  if (session.runtimeConnection) return session.runtimeConnection;
  if (status.connectionState) return status.connectionState;
  return status.connected ? "connected" : "disconnected";
}

/** Project service-owned sessions into the same Lane contract as local work. */
export function projectRemoteSessionAsLane(
  session: RemoteSessionTarget,
  status: RemoteServiceStatus,
  agentId?: string | null,
): LaneSummary {
  return {
    id: session.sessionId,
    session_id: session.sessionId,
    agent_id: agentId ?? null,
    title: session.title,
    cwd: session.workspace,
    created_at: session.updatedAt,
    updated_at: session.updatedAt,
    message_count: 0,
    archived: false,
    archived_at: null,
    kind: "build",
    execution_mode: "shared",
    workspace_status: "ready",
    runtime_target:
      session.runtimeTarget ?? status.runtimeTarget ?? runtimeTargetForServiceUrl(status.baseUrl),
    runtime_connection: runtimeConnectionForService(status, session),
  };
}

/** Merge local and service projections without allowing a stale remote copy to win. */
export function mergeLaneProjections(
  localLanes: LaneSummary[],
  remoteLanes: LaneSummary[],
): LaneSummary[] {
  const byId = new Map(localLanes.map((lane) => [lane.id, lane]));
  for (const lane of remoteLanes) {
    if (!byId.has(lane.id)) byId.set(lane.id, lane);
  }
  return [...byId.values()].sort((a, b) =>
    b.updated_at.localeCompare(a.updated_at),
  );
}

/** Resolve the product Lane for old and future Run payloads. */
export function laneIdForRun(run: { laneId?: string | null; sessionId: string }): string {
  return run.laneId ?? run.sessionId;
}
