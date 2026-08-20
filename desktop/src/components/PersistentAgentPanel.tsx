import { useEffect, useMemo, useState } from "react";
import type {
  PersistentAgent,
  PersistentAgentResumePlan,
  LaneSummary,
  RemoteSessionTarget,
  RemoteServiceStatus,
} from "../lib/protocol";
import { api } from "../lib/api";
import { StateCard } from "./StateCard";

export type PersistentAgentPanelProps = {
  agents: PersistentAgent[];
  lanes?: LaneSummary[];
  activeLaneId: string | null;
  busy?: boolean;
  error?: string | null;
  onRefresh: () => void;
  onOpenSession: (agent: PersistentAgent) => void;
  onInspect: (sessionId: string) => Promise<PersistentAgentResumePlan>;
  onResume: (agent: PersistentAgent, prompt: string) => Promise<string>;
  remoteStatus?: RemoteServiceStatus;
  onConnectRemote?: (baseUrl: string, token: string) => Promise<void>;
  onDisconnectRemote?: () => Promise<void>;
  remoteSessions?: RemoteSessionTarget[];
  selectedRemoteSessionId?: string | null;
  onSelectRemoteSession?: (sessionId: string) => void;
  onCreateRemoteSession?: (workspace: string, title?: string) => Promise<RemoteSessionTarget>;
};

function timeLabel(value: string): string {
  const time = new Date(value).getTime();
  if (!Number.isFinite(time)) return "Unknown time";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(time);
}

function stateLabel(state: PersistentAgent["state"]): string {
  return state.replaceAll("_", " ");
}

function canResume(agent: PersistentAgent): boolean {
  return agent.state === "waiting" || agent.state === "interrupted" || agent.state === "failed";
}

export function PersistentAgentPanel({
  agents,
  lanes = [],
  activeLaneId,
  busy = false,
  error,
  onRefresh,
  onOpenSession,
  onInspect,
  onResume,
  remoteStatus = { connected: false },
  onConnectRemote,
  onDisconnectRemote,
  remoteSessions = [],
  selectedRemoteSessionId = null,
  onSelectRemoteSession,
  onCreateRemoteSession,
}: PersistentAgentPanelProps) {
  const sortedAgents = useMemo(
    () =>
      [...agents].sort(
        (a, b) =>
          new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime(),
      ),
    [agents],
  );
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [plans, setPlans] = useState<Record<string, PersistentAgentResumePlan>>({});
  const [planErrors, setPlanErrors] = useState<Record<string, string>>({});
  const [planBusy, setPlanBusy] = useState<string | null>(null);
  const [prompts, setPrompts] = useState<Record<string, string>>({});
  const [resumeBusy, setResumeBusy] = useState<string | null>(null);
  const [responses, setResponses] = useState<Record<string, string>>({});
  const [remoteUrl, setRemoteUrl] = useState("http://127.0.0.1:39200");
  const [remoteToken, setRemoteToken] = useState("");
  const [remoteBusy, setRemoteBusy] = useState(false);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [remoteWorkspace, setRemoteWorkspace] = useState("");
  const [remoteTitle, setRemoteTitle] = useState("");
  const [remoteSessionBusy, setRemoteSessionBusy] = useState(false);

  useEffect(() => {
    if (selectedAgentId && sortedAgents.some((agent) => agent.agentId === selectedAgentId)) {
      return;
    }
    setSelectedAgentId(sortedAgents[0]?.agentId ?? null);
  }, [selectedAgentId, sortedAgents]);

  async function inspect(agent: PersistentAgent) {
    setSelectedAgentId(agent.agentId);
    setPlanBusy(agent.agentId);
    setPlanErrors((current) => ({ ...current, [agent.agentId]: "" }));
    try {
      const plan = await onInspect(agent.sessionId);
      setPlans((current) => ({ ...current, [agent.agentId]: plan }));
    } catch (inspectError) {
      setPlanErrors((current) => ({
        ...current,
        [agent.agentId]: String(inspectError),
      }));
    } finally {
      setPlanBusy(null);
    }
  }

  async function resume(agent: PersistentAgent) {
    const prompt = prompts[agent.agentId]?.trim() ?? "";
    if (!prompt || !canResume(agent)) return;
    if (!window.confirm(`Resume persistent agent ${agent.agentId}?`)) return;
    setResumeBusy(agent.agentId);
    setResponses((current) => ({ ...current, [agent.agentId]: "" }));
    try {
      const response = await onResume(agent, prompt);
      setResponses((current) => ({ ...current, [agent.agentId]: response }));
      setPrompts((current) => ({ ...current, [agent.agentId]: "" }));
      onRefresh();
    } catch (resumeError) {
      setResponses((current) => ({
        ...current,
        [agent.agentId]: `Resume failed: ${String(resumeError)}`,
      }));
    } finally {
      setResumeBusy(null);
    }
  }

  async function connectRemote() {
    if (!onConnectRemote || !remoteUrl.trim() || !remoteToken.trim()) return;
    setRemoteBusy(true);
    setRemoteError(null);
    try {
      await onConnectRemote(remoteUrl.trim(), remoteToken);
      setRemoteToken("");
      onRefresh();
    } catch (connectError) {
      setRemoteError(String(connectError));
    } finally {
      setRemoteBusy(false);
    }
  }

  async function disconnectRemote() {
    if (!onDisconnectRemote) return;
    setRemoteBusy(true);
    setRemoteError(null);
    try {
      await onDisconnectRemote();
      onRefresh();
    } catch (disconnectError) {
      setRemoteError(String(disconnectError));
    } finally {
      setRemoteBusy(false);
    }
  }

  return (
    <section className="persistent-agent-panel" role="region" aria-label="Persistent agents">
      <div className="panel-block">
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 8 }}>
          <strong>Persistent agents</strong>
          <button type="button" onClick={onRefresh} disabled={busy}>
            {busy ? "Refreshing…" : "Refresh"}
          </button>
        </div>
        <p style={{ fontSize: 11, color: "var(--muted)", margin: "0.4rem 0 0" }}>
          Durable Build identities and verified checkpoints. Resume is always an explicit operator action.
        </p>
        {onConnectRemote && (
          <div style={{ marginTop: 10, display: "grid", gap: 6 }}>
            <div style={{ fontSize: 11, color: "var(--muted)" }}>
              {remoteStatus.connected
                ? `${remoteStatus.runtimeTarget === "local_service" ? "Local" : "Hosted"} service connected${remoteStatus.baseUrl ? ` · ${remoteStatus.baseUrl}` : ""}`
                : remoteStatus.connectionState === "error"
                  ? "Remote service unavailable · cached Lanes are marked unavailable"
                  : "No remote service connected"}
            </div>
            {remoteStatus.connected ? (
              <button type="button" onClick={() => void disconnectRemote()} disabled={remoteBusy}>
                {remoteBusy ? "Disconnecting…" : "Disconnect remote service"}
              </button>
            ) : (
              <>
                <input
                  aria-label="Remote service URL"
                  value={remoteUrl}
                  onChange={(event) => setRemoteUrl(event.target.value)}
                  placeholder="https://service.example/mcp"
                />
                <input
                  aria-label="Remote service token"
                  type="password"
                  value={remoteToken}
                  onChange={(event) => setRemoteToken(event.target.value)}
                  placeholder="Bearer token (memory only)"
                />
                <button
                  type="button"
                  onClick={() => void connectRemote()}
                  disabled={remoteBusy || !remoteUrl.trim() || !remoteToken.trim()}
                >
                  {remoteBusy ? "Connecting…" : "Connect remote service"}
                </button>
                <div style={{ fontSize: 10, color: "var(--muted)" }}>
                  The token is held in backend memory and is never displayed or persisted by GrokPtah.
                </div>
              </>
            )}
            {remoteError && <div role="alert">Remote connection failed: {remoteError}</div>}
            {remoteStatus.connected && onSelectRemoteSession && onCreateRemoteSession && (
              <div
                style={{
                  marginTop: 8,
                  paddingTop: 8,
                  borderTop: "1px solid var(--border)",
                  display: "grid",
                  gap: 6,
                }}
              >
                <strong style={{ fontSize: 11 }}>Remote execution target</strong>
                <select
                  aria-label="Remote execution session"
                  value={selectedRemoteSessionId ?? ""}
                  onChange={(event) => onSelectRemoteSession(event.target.value)}
                  disabled={remoteSessionBusy}
                >
                  <option value="" disabled>
                    {remoteSessions.length === 0 ? "Create a service session" : "Choose a service session"}
                  </option>
                  {remoteSessions.map((session) => (
                    <option key={session.sessionId} value={session.sessionId}>
                      {session.title || session.workspace} {session.busy ? "· busy" : ""}
                    </option>
                  ))}
                </select>
                <input
                  aria-label="Remote session workspace"
                  value={remoteWorkspace}
                  onChange={(event) => setRemoteWorkspace(event.target.value)}
                  placeholder="Allowlisted workspace path"
                  disabled={remoteSessionBusy}
                />
                <input
                  aria-label="Remote session title"
                  value={remoteTitle}
                  onChange={(event) => setRemoteTitle(event.target.value)}
                  placeholder="Session title (optional)"
                  disabled={remoteSessionBusy}
                />
                <button
                  type="button"
                  disabled={remoteSessionBusy || !remoteWorkspace.trim()}
                  onClick={() => {
                    setRemoteSessionBusy(true);
                    setRemoteError(null);
                    void onCreateRemoteSession(remoteWorkspace.trim(), remoteTitle.trim() || undefined)
                      .then((session) => {
                        onSelectRemoteSession(session.sessionId);
                        setRemoteWorkspace("");
                        setRemoteTitle("");
                      })
                      .catch((createError) => setRemoteError(`Remote session failed: ${String(createError)}`))
                      .finally(() => setRemoteSessionBusy(false));
                  }}
                >
                  {remoteSessionBusy ? "Creating…" : "Create remote session"}
                </button>
                <div style={{ fontSize: 10, color: "var(--muted)" }}>
                  The workspace must already be allowlisted by the service.
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      {error ? (
        <StateCard
          variant="error"
          title="Persistent Agents could not be refreshed"
          description="Your saved Agent identities are unchanged. Try again, or open the technical details if support needs the diagnostic."
          actionLabel="Refresh"
          onAction={onRefresh}
          technicalDetail={error}
        />
      ) : !busy && sortedAgents.length === 0 ? (
        <StateCard
          variant="empty"
          title="No durable Agents yet"
          description="Complete a Build turn to create an identity that can own Lanes and resume from verified checkpoints."
        />
      ) : null}

      {sortedAgents.map((agent) => {
        const plan = plans[agent.agentId];
        const isSelected = selectedAgentId === agent.agentId;
        const laneIds = agent.laneIds?.length ? agent.laneIds : [agent.sessionId];
        const isCurrent = Boolean(activeLaneId && laneIds.includes(activeLaneId));
        const ownedLanes = lanes.filter((lane) => lane.agent_id === agent.agentId);
        const resumeAllowed = canResume(agent) && Boolean(agent.latestCheckpointId);
        return (
          <article
            key={agent.agentId}
            className={`panel-block persistent-agent-card ${isSelected ? "is-selected" : ""}`}
            data-agent-id={agent.agentId}
          >
            <div style={{ display: "flex", justifyContent: "space-between", gap: 8 }}>
              <strong>{agent.agentId}</strong>
              <span className={`agent-state ${agent.state}`}>{stateLabel(agent.state)}</span>
            </div>
            <div style={{ color: "var(--muted)", fontSize: 11, marginTop: 4 }}>
              {agent.model} · {laneIds.length} {laneIds.length === 1 ? "Lane" : "Lanes"} · continuation {agent.continuationOrdinal} · updated {timeLabel(agent.updatedAt)}
            </div>
            <div style={{ color: "var(--muted)", fontSize: 11, marginTop: 3 }}>
              Managed execution: {agent.spec?.managedExecution?.enabled ? "on" : "off"}
              {!remoteStatus.connected && (
                <>
                  {" "}
                  <button
                    type="button"
                    onClick={() => {
                      void api
                        .persistentAgentSetManagedExecution(
                          agent.agentId,
                          !agent.spec?.managedExecution?.enabled,
                        )
                        .then(() => onRefresh());
                    }}
                  >
                    {agent.spec?.managedExecution?.enabled ? "Disable" : "Enable"}
                  </button>
                </>
              )}
            </div>
            <div style={{ color: "var(--muted)", fontSize: 11, marginTop: 3, wordBreak: "break-word" }}>
              {agent.workspace}
            </div>
            {ownedLanes.length > 0 && (
              <div
                aria-label={`Lanes owned by ${agent.agentId}`}
                style={{ color: "var(--muted)", fontSize: 11, marginTop: 4 }}
              >
                Lanes: {ownedLanes.slice(0, 3).map((lane) => lane.title).join(" · ")}
                {ownedLanes.length > 3 ? ` · +${ownedLanes.length - 3} more` : ""}
              </div>
            )}
            <div className="worktree-create-actions" style={{ marginTop: 7 }}>
              {!isCurrent && !remoteStatus.connected && (
                <button type="button" onClick={() => onOpenSession(agent)}>
                  Open session
                </button>
              )}
              <button
                type="button"
                disabled={planBusy === agent.agentId}
                onClick={() => void inspect(agent)}
              >
                {planBusy === agent.agentId ? "Inspecting…" : "Inspect checkpoint"}
              </button>
            </div>
            {isSelected && plan && (
              <div className="agent-checkpoint" style={{ marginTop: 8 }}>
                <div style={{ fontSize: 11, color: "var(--muted)" }}>
                  Checkpoint {plan.checkpoint.ordinal} · {plan.checkpoint.reason.replaceAll("_", " ")} · parent run {plan.parentRunId}
                </div>
                <pre style={{ whiteSpace: "pre-wrap", maxHeight: 180, overflow: "auto" }}>
                  {plan.checkpoint.contextSummary}
                </pre>
                <textarea
                  aria-label={`Resume prompt for ${agent.agentId}`}
                  placeholder="Instruction for the resumed agent…"
                  value={prompts[agent.agentId] ?? ""}
                  onChange={(event) =>
                    setPrompts((current) => ({
                      ...current,
                      [agent.agentId]: event.target.value,
                    }))
                  }
                  rows={3}
                  disabled={resumeBusy === agent.agentId}
                  style={{ width: "100%", resize: "vertical" }}
                />
                <button
                  type="button"
                  disabled={!resumeAllowed || !prompts[agent.agentId]?.trim() || resumeBusy === agent.agentId}
                  onClick={() => void resume(agent)}
                >
                  {resumeBusy === agent.agentId ? "Resuming…" : "Resume agent"}
                </button>
                {responses[agent.agentId] && (
                  <pre style={{ whiteSpace: "pre-wrap", maxHeight: 180, overflow: "auto" }}>
                    {responses[agent.agentId]}
                  </pre>
                )}
              </div>
            )}
            {isSelected && planErrors[agent.agentId] && (
              <div className="panel-block" role="alert" style={{ marginTop: 8 }}>
                {planErrors[agent.agentId]}
              </div>
            )}
          </article>
        );
      })}
    </section>
  );
}
