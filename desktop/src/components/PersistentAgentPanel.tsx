import { useEffect, useMemo, useState } from "react";
import type {
  PersistentAgent,
  PersistentAgentResumePlan,
} from "../lib/protocol";

export type PersistentAgentPanelProps = {
  agents: PersistentAgent[];
  activeSessionId: string | null;
  busy?: boolean;
  error?: string | null;
  onRefresh: () => void;
  onOpenSession: (agent: PersistentAgent) => void;
  onInspect: (sessionId: string) => Promise<PersistentAgentResumePlan>;
  onResume: (agent: PersistentAgent, prompt: string) => Promise<string>;
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
  activeSessionId,
  busy = false,
  error,
  onRefresh,
  onOpenSession,
  onInspect,
  onResume,
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
      </div>

      {error && (
        <div className="panel-block" role="alert">
          {error}
        </div>
      )}
      {!busy && sortedAgents.length === 0 && (
        <div className="panel-block" style={{ color: "var(--muted)" }}>
          No persistent agents yet. Complete a Build turn to create one.
        </div>
      )}

      {sortedAgents.map((agent) => {
        const plan = plans[agent.agentId];
        const isSelected = selectedAgentId === agent.agentId;
        const isCurrent = activeSessionId === agent.sessionId;
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
              {agent.model} · continuation {agent.continuationOrdinal} · updated {timeLabel(agent.updatedAt)}
            </div>
            <div style={{ color: "var(--muted)", fontSize: 11, marginTop: 3, wordBreak: "break-word" }}>
              {agent.workspace}
            </div>
            <div className="worktree-create-actions" style={{ marginTop: 7 }}>
              {!isCurrent && (
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
