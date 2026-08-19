import { useMemo, useState } from "react";
import type {
  DurableWorkAttempt,
  DurableWorkItem,
  RemoteWorkSnapshot,
} from "../lib/protocol";
import type { LaneScope } from "./LaneScopeLine";
import { LaneScopeLine } from "./LaneScopeLine";
import { StateCard } from "./StateCard";

type WorkFilter = "all" | "active" | "attention" | "completed";

export type WorkBoardProps = {
  items: DurableWorkItem[];
  selectedWorkId: string | null;
  snapshot: RemoteWorkSnapshot | null;
  scope: LaneScope;
  busy?: boolean;
  error?: string | null;
  sourceLabel?: string;
  onRefresh: () => void;
  onSelect: (workId: string) => void;
  onOpenLane?: (sessionId: string) => void;
  onOpenRun?: (runId: string) => void;
};

function stateLabel(value: string): string {
  return value.replaceAll("_", " ");
}

function filterLabel(value: WorkFilter): string {
  if (value === "all") return "All";
  if (value === "attention") return "Needs attention";
  return value[0].toUpperCase() + value.slice(1);
}

function isActive(state: DurableWorkItem["state"]): boolean {
  return ["leased", "running", "awaiting_input", "awaiting_approval", "review"].includes(state);
}

function needsAttention(state: DurableWorkItem["state"]): boolean {
  return ["blocked", "awaiting_input", "awaiting_approval", "review", "failed"].includes(state);
}

function isCompleted(state: DurableWorkItem["state"]): boolean {
  return state === "succeeded" || state === "cancelled";
}

function dateLabel(value: string): string {
  const timestamp = new Date(value).getTime();
  if (!Number.isFinite(timestamp)) return "Unknown time";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(timestamp);
}

function progressLabel(item: DurableWorkItem): string {
  if (!item.progress) return "No progress reported";
  if (item.progress.percent == null) return item.progress.summary;
  return `${item.progress.percent}% · ${item.progress.summary}`;
}

function attemptSummary(attempt: DurableWorkAttempt): string {
  const runCount = attempt.linkedRunIds.length;
  return `${stateLabel(attempt.state)} · ${runCount} ${runCount === 1 ? "Run" : "Runs"}`;
}

export function WorkBoard({
  items,
  selectedWorkId,
  snapshot,
  scope,
  busy = false,
  error,
  sourceLabel = "Durable workload",
  onRefresh,
  onSelect,
  onOpenLane,
  onOpenRun,
}: WorkBoardProps) {
  const [filter, setFilter] = useState<WorkFilter>("all");
  const visibleItems = useMemo(
    () =>
      items.filter((item) => {
        if (filter === "active") return isActive(item.state);
        if (filter === "attention") return needsAttention(item.state);
        if (filter === "completed") return isCompleted(item.state);
        return true;
      }),
    [filter, items],
  );
  const selected = snapshot?.work ?? items.find((item) => item.workId === selectedWorkId) ?? null;
  const activeCount = items.filter((item) => isActive(item.state)).length;
  const attentionCount = items.filter((item) => needsAttention(item.state)).length;
  const completedCount = items.filter((item) => isCompleted(item.state)).length;

  return (
    <section className="work-board" aria-label="Durable work">
      <div className="work-board-header">
        <div>
          <div className="run-inspector-header">
            <div>
              <strong>Work</strong>
              <p className="run-inspector-subtitle">
                Durable objectives survive Lane archival and service reconnects.
              </p>
            </div>
            <button type="button" className="run-inspector-refresh" onClick={onRefresh} disabled={busy}>
              {busy ? "…" : "↻"}
            </button>
          </div>
          <LaneScopeLine scope={scope} compact />
        </div>
      </div>

      <div className="work-board-summary" aria-label="Work summary">
        <span>{items.length} total</span>
        <span>{activeCount} active</span>
        <span>{attentionCount} needs attention</span>
        <span>{completedCount} complete</span>
        <span className="work-board-source">{sourceLabel}</span>
      </div>

      <div className="work-board-controls" role="group" aria-label="Work filters">
        {(["all", "active", "attention", "completed"] as WorkFilter[]).map((value) => (
          <button
            key={value}
            type="button"
            className={filter === value ? "is-active" : ""}
            aria-pressed={filter === value}
            onClick={() => setFilter(value)}
          >
            {filterLabel(value)}
          </button>
        ))}
      </div>

      {error && (
        <StateCard
          variant="error"
          title="Work could not be refreshed"
          description="The last durable snapshot remains available when possible. Try again or inspect the technical details."
          actionLabel="Refresh"
          onAction={onRefresh}
          technicalDetail={error}
        />
      )}

      {!busy && !error && items.length === 0 && (
        <StateCard
          variant="empty"
          title="No durable Work Items in this Lane"
          description="Queued prompts and Runs still appear in the regular workflow. Durable Work Items will appear here when a coordinator or service creates them."
        />
      )}

      {items.length > 0 && visibleItems.length === 0 && (
        <StateCard
          variant="empty"
          title="Nothing matches this filter"
          description="Choose another work state to see the rest of this Lane's durable history."
        />
      )}

      {visibleItems.length > 0 && (
        <div className="work-board-layout">
          <div className="work-board-list" role="list" aria-label="Work Items">
            {visibleItems.map((item) => (
              <div
                key={item.workId}
                role="listitem"
              >
                <button
                  type="button"
                  className={`work-card ${selected?.workId === item.workId ? "is-selected" : ""} state-${item.state}`}
                  aria-pressed={selected?.workId === item.workId}
                  onClick={() => onSelect(item.workId)}
                >
                  <span className="work-card-heading">
                    <strong>{item.objective}</strong>
                    <span className="work-card-state">{stateLabel(item.state)}</span>
                  </span>
                  <span className="work-card-meta">
                    {item.kind} · priority {item.priority} · {item.attemptCount} attempts
                  </span>
                  <span className="work-card-progress">{progressLabel(item)}</span>
                  <span className="work-card-meta">Updated {dateLabel(item.updatedAt)}</span>
                </button>
              </div>
            ))}
          </div>

          <div className="work-board-detail" aria-live="polite">
            {!selected ? (
              <StateCard
                variant="empty"
                title="Select a Work Item"
                description="Inspect its durable objective, ownership, attempts, and linked Runs."
              />
            ) : (
              <>
                <div className="work-detail-heading">
                  <div>
                    <span className="work-detail-kicker">{selected.kind}</span>
                    <h2>{selected.objective}</h2>
                  </div>
                  <span className={`work-detail-state state-${selected.state}`}>
                    {stateLabel(selected.state)}
                  </span>
                </div>
                <dl className="work-detail-facts">
                  <div><dt>Agent</dt><dd>{selected.assignedAgentId || "Unassigned"}</dd></div>
                  <div><dt>Created by</dt><dd>{selected.createdBy}</dd></div>
                  <div><dt>Attempts</dt><dd>{selected.attemptCount}</dd></div>
                  <div><dt>Approval</dt><dd>{selected.policy.requiresApproval ? "Required" : "Not required"}</dd></div>
                  <div><dt>Updated</dt><dd>{dateLabel(selected.updatedAt)}</dd></div>
                  <div><dt>Revision</dt><dd>{selected.revision}</dd></div>
                </dl>
                <div className="work-detail-progress">
                  <strong>Progress</strong>
                  <p>{progressLabel(selected)}</p>
                  {selected.progress?.percent != null && (
                    <progress max={100} value={selected.progress.percent} />
                  )}
                </div>
                <div className="work-detail-actions">
                  {onOpenLane && (
                    <button type="button" onClick={() => onOpenLane(selected.sessionId)}>
                      Open Lane
                    </button>
                  )}
                  {selected.result?.evidence.length ? (
                    <details>
                      <summary>Completion evidence</summary>
                      <ul>{selected.result.evidence.map((evidence) => <li key={evidence}>{evidence}</li>)}</ul>
                    </details>
                  ) : null}
                </div>
                <div className="work-attempts">
                  <strong>Attempt history</strong>
                  {snapshot?.attempts.length ? (
                    <ol>
                      {snapshot.attempts.map((attempt) => (
                        <li key={attempt.attemptId}>
                          <div>
                            <strong>Attempt {attempt.attemptNumber}</strong>
                            <span>{attemptSummary(attempt)}</span>
                          </div>
                          <small>
                            {attempt.claimantId} · lease until {dateLabel(attempt.leaseExpiresAt)}
                          </small>
                          {onOpenRun && attempt.linkedRunIds.length > 0 && (
                            <div className="work-attempt-runs">
                              {attempt.linkedRunIds.map((runId) => (
                                <button key={runId} type="button" onClick={() => onOpenRun?.(runId)}>
                                  Inspect Run {runId.slice(0, 8)}
                                </button>
                              ))}
                            </div>
                          )}
                        </li>
                      ))}
                    </ol>
                  ) : (
                    <p className="work-board-muted">No attempt history has been recorded.</p>
                  )}
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
