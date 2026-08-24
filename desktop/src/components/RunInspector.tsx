import { useEffect, useState } from "react";
import type {
  DurableRun,
  DurableRunEvent,
  DurableRunEventPage,
  RunReview,
  SessionUpdate,
} from "../lib/protocol";
import { LaneScopeLine, type LaneScope } from "./LaneScopeLine";
import { StateCard } from "./StateCard";

type RunInspectorProps = {
  runs: DurableRun[];
  totalCount?: number;
  truncated?: boolean;
  laneTitle?: string | null;
  runtimeLabel?: string | null;
  scope?: LaneScope;
  error?: string | null;
  busy?: boolean;
  remote?: boolean;
  liveEvents?: Record<string, DurableRunEvent[]>;
  watching?: boolean;
  onWatchingChange?: (watching: boolean) => void;
  onRefresh: () => void;
  onReview: (runId: string) => Promise<RunReview>;
  onApprove: (runId: string) => Promise<void>;
  onPromote: (runId: string) => Promise<void>;
  onDiscard: (runId: string) => Promise<void>;
  onRetry: (runId: string, prompt: string) => Promise<void>;
  onSteer: (runId: string, text: string) => Promise<void>;
  onCancel: (runId: string) => Promise<void>;
  onEvents: (runId: string, afterSeq?: number, limit?: number) => Promise<DurableRunEventPage>;
};

const stateLabels: Record<DurableRun["state"], string> = {
  queued: "Queued",
  running: "Running",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
  limit_reached: "Limit reached",
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

function testLabel(run: DurableRun): string {
  const tests = run.aggregates.tests;
  if (!tests.length) return "No tests observed";
  const passed = tests.filter((test) => test.exitCode === 0).length;
  return `${passed}/${tests.length} tests passed`;
}

function tokenLabel(run: DurableRun): string | null {
  const usage = run.aggregates.usage;
  const ceiling = run.bounds.maxTotalTokens;
  if (!usage.requests && ceiling == null && run.aggregates.usageComplete !== false) return null;
  const measured = run.aggregates.usagePendingRequests
    ? "measuring"
    : run.aggregates.usageComplete === false
      ? "partial"
      : "measured";
  if (ceiling != null) {
    return `${usage.totalTokens.toLocaleString()} / ${ceiling.toLocaleString()} tokens · ${measured}`;
  }
  return `${usage.totalTokens.toLocaleString()} tokens · ${measured}`;
}

function stopCauseLabel(run: DurableRun): string | null {
  if (!run.stopCause) return null;
  return run.stopCause.replace(/_/g, " ");
}

function runOriginLabel(run: DurableRun): string {
  if (run.clientId === "mcp") return "MCP coordinator";
  if (run.clientId === "desktop") return "Desktop";
  return run.clientId || "Unknown origin";
}

function runExecutionLabel(run: DurableRun): string {
  if (run.execution?.mode === "isolated_worktree") return "Isolated worktree";
  return "Shared workspace";
}

function compactEventText(value: string, maxLength = 220): string {
  const text = value.replace(/\s+/g, " ").trim();
  return text.length > maxLength ? `${text.slice(0, maxLength - 1)}…` : text;
}

function eventLabel(update: SessionUpdate): string {
  switch (update.type) {
    case "agent_message_chunk":
      return `Assistant · ${compactEventText(update.text)}`;
    case "agent_thought_chunk":
      return `Thought · ${compactEventText(update.text)}`;
    case "tool_call":
      return `${compactEventText(update.title, 120)} · ${update.status}`;
    case "tool_call_update":
      return `${update.call_id} · ${update.status}${update.output ? ` · ${compactEventText(update.output, 140)}` : ""}`;
    case "plan":
      return `Plan · ${update.status} · ${update.steps.length} steps`;
    case "permission_required":
      return `Permission requested · ${compactEventText(update.request.summary, 160)}`;
    case "turn_started":
      return `Turn started · ${update.turn_id}`;
    case "turn_complete":
      return update.cancelled ? "Turn cancelled" : "Turn complete";
    case "completion_evidence":
      return `Verification · ${update.evidence.status}`;
    case "error":
      return `Error · ${compactEventText(update.message)}`;
    case "subagent_spawned":
      return `Subagent started · ${compactEventText(update.title, 140)}`;
    case "subagent_update":
      return `Subagent ${update.subagent_id} · ${update.status}${update.detail ? ` · ${compactEventText(update.detail, 140)}` : ""}`;
    case "background_task":
      return `Background task · ${compactEventText(update.title, 140)} · ${update.status}`;
    case "shell_session_started":
      return `Shell started · ${compactEventText(update.command, 160)}`;
    case "shell_output":
      return `Shell output · ${compactEventText(update.data)}`;
    case "shell_session_ended":
      return update.cancelled
        ? `Shell cancelled · ${update.call_id}`
        : `Shell ended · ${update.call_id} · exit ${update.exit_code ?? "?"}`;
    case "file_edit":
      return `${compactEventText(update.path, 140)} · ${compactEventText(update.summary, 140)}`;
    case "agent_progress":
      return `Round ${update.round}/${update.max_rounds} · ${compactEventText(update.last_tool || "model step", 100)} · ${compactEventText(update.detail, 120)}`;
    case "rate_limited":
      return `Rate limited · ${compactEventText(update.message)}`;
    case "steering_injected":
      return `Steering delivered · ${compactEventText(update.text)}`;
    case "prompt_queue_changed":
      return `${update.origin === "mcp" ? "MCP" : "Desktop"} queue · ${update.action.replace(/_/g, " ")}${update.disposition ? ` · ${update.disposition}` : ""}`;
  }
}

export function RunInspector({
  runs,
  totalCount,
  truncated = false,
  laneTitle = null,
  runtimeLabel = null,
  scope,
  error,
  busy,
  remote = false,
  liveEvents,
  watching,
  onWatchingChange,
  onRefresh,
  onReview,
  onApprove,
  onPromote,
  onDiscard,
  onRetry,
  onSteer,
  onCancel,
  onEvents,
}: RunInspectorProps) {
  const [originFilter, setOriginFilter] = useState<"all" | "desktop" | "mcp" | "other">("all");
  const [localWatching, setLocalWatching] = useState(true);
  const [reviewing, setReviewing] = useState<string | null>(null);
  const [reviews, setReviews] = useState<Record<string, RunReview>>({});
  const [actionError, setActionError] = useState<string | null>(null);
  const [eventPages, setEventPages] = useState<Record<string, DurableRunEventPage>>({});
  const [eventLoading, setEventLoading] = useState<string | null>(null);
  const [eventErrors, setEventErrors] = useState<Record<string, string>>({});
  const [retryPrompts, setRetryPrompts] = useState<Record<string, string>>({});
  const [steerPrompts, setSteerPrompts] = useState<Record<string, string>>({});
  const watchValue = watching ?? localWatching;

  useEffect(() => {
    if (!liveEvents) return;
    setEventPages((current) => {
      let changed = false;
      const next = { ...current };
      for (const [runId, events] of Object.entries(liveEvents)) {
        if (!events.length) continue;
        const previous = current[runId];
        const entries = [...(previous?.entries ?? []), ...events].filter(
          (entry, index, all) => all.findIndex((candidate) => candidate.seq === entry.seq) === index,
        ).sort((a, b) => a.seq - b.seq);
        if (!previous || entries.length !== previous.entries.length) {
          next[runId] = {
            entries,
            nextCursor: entries.at(-1)?.seq ?? previous?.nextCursor ?? null,
            cursorExpired: previous?.cursorExpired ?? false,
          };
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [liveEvents]);

  function setWatchValue(next: boolean) {
    setLocalWatching(next);
    onWatchingChange?.(next);
  }

  const visibleRuns = runs.filter((run) => {
    if (originFilter === "all") return true;
    if (originFilter === "mcp") return run.clientId === "mcp";
    if (originFilter === "desktop") return run.clientId === "desktop";
    return run.clientId !== "mcp" && run.clientId !== "desktop";
  });

  async function review(runId: string) {
    setReviewing(runId);
    setActionError(null);
    try {
      const result = await onReview(runId);
      setReviews((current) => ({ ...current, [runId]: result }));
    } catch (error) {
      setActionError(String(error));
    } finally {
      setReviewing(null);
    }
  }

  async function promote(runId: string) {
    setReviewing(runId);
    setActionError(null);
    try {
      await onPromote(runId);
      setReviews((current) => {
        const next = { ...current };
        delete next[runId];
        return next;
      });
      onRefresh();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setReviewing(null);
    }
  }

  async function approve(runId: string) {
    setReviewing(runId);
    setActionError(null);
    try {
      await onApprove(runId);
      onRefresh();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setReviewing(null);
    }
  }

  async function discard(runId: string) {
    if (!window.confirm("Discard this isolated run and its unpromoted changes?")) {
      return;
    }
    setReviewing(runId);
    setActionError(null);
    try {
      await onDiscard(runId);
      onRefresh();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setReviewing(null);
    }
  }

  async function retry(runId: string) {
    const prompt = retryPrompts[runId]?.trim() ?? "";
    if (!prompt) return;
    setReviewing(runId);
    setActionError(null);
    try {
      await onRetry(runId, prompt);
      setRetryPrompts((current) => ({ ...current, [runId]: "" }));
      onRefresh();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setReviewing(null);
    }
  }

  async function steer(runId: string) {
    const text = steerPrompts[runId]?.trim() ?? "";
    if (!text) return;
    setReviewing(runId);
    setActionError(null);
    try {
      await onSteer(runId, text);
      setSteerPrompts((current) => ({ ...current, [runId]: "" }));
      onRefresh();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setReviewing(null);
    }
  }

  async function cancel(runId: string) {
    if (!window.confirm("Cancel this MCP-owned run?")) return;
    setReviewing(runId);
    setActionError(null);
    try {
      await onCancel(runId);
      onRefresh();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setReviewing(null);
    }
  }

  async function loadEvents(runId: string, afterSeq = 0) {
    setEventLoading(runId);
    setEventErrors((current) => {
      const next = { ...current };
      delete next[runId];
      return next;
    });
    try {
      const page = await onEvents(runId, afterSeq, 80);
      setEventPages((current) => {
        const previous = afterSeq === 0 ? undefined : current[runId];
        const entries = [...(previous?.entries ?? []), ...page.entries].filter(
          (entry, index, all) => all.findIndex((candidate) => candidate.seq === entry.seq) === index,
        );
        return {
          ...current,
          [runId]: { ...page, entries },
        };
      });
    } catch (error) {
      setEventErrors((current) => ({ ...current, [runId]: String(error) }));
    } finally {
      setEventLoading(null);
    }
  }

  return (
    <section className="run-inspector" aria-label="Durable task runs">
      <div className="run-inspector-header">
        <div>
          <div className="section-title">Task runs</div>
          <p className="run-inspector-subtitle">
            Durable progress and verification from desktop and MCP activity.
          </p>
          {scope ? (
            <LaneScopeLine scope={scope} compact />
          ) : laneTitle ? (
            <div className="panel-context-scope">
              Lane: {laneTitle}{runtimeLabel ? ` · ${runtimeLabel}` : ""}
            </div>
          ) : null}
        </div>
        <button
          type="button"
          className="icon-btn run-inspector-refresh"
          aria-label="Refresh task runs"
          title="Refresh task runs"
          onClick={onRefresh}
          disabled={busy}
        >
          ↻
        </button>
      </div>

      <div className="run-inspector-controls">
        <label className="run-inspector-filter">
          <span>Source</span>
          <select
            aria-label="Filter task runs by source"
            value={originFilter}
            onChange={(event) =>
              setOriginFilter(event.target.value as typeof originFilter)
            }
          >
            <option value="all">All sources</option>
            <option value="desktop">Desktop</option>
            <option value="mcp">MCP coordinator</option>
            <option value="other">Other</option>
          </select>
        </label>
        <label className="run-inspector-watch">
          <input
            type="checkbox"
            aria-label="Watch live updates"
            checked={watchValue}
            onChange={(event) => setWatchValue(event.target.checked)}
          />
          <span>Watch live</span>
        </label>
      </div>

      {truncated ? (
        <p className="run-inspector-truncated" role="status">
          Showing {runs.length} of {totalCount ?? runs.length} durable Runs.
          Older Runs are omitted from this page.
        </p>
      ) : null}

      {error ? (
        <StateCard
          variant="error"
          title={remote ? "Remote task history is unavailable" : "Task history could not be refreshed"}
          description={
            remote
              ? "The service-owned Lane is still identified, but its durable Runs could not be read. Retry when the service is available."
              : "Your durable Run history is unchanged. Retry the refresh, or open the technical details if support needs the diagnostic."
          }
          actionLabel="Retry refresh"
          onAction={onRefresh}
          technicalDetail={error}
        />
      ) : runs.length === 0 ? (
        <StateCard
          variant="empty"
          title="No durable Build runs yet"
          description={
            remote
              ? "Submit work to the selected service-owned Lane to see its queued and completed Runs here."
              : "Submit a Build prompt to see its queued, running, and completed Runs here."
          }
        />
      ) : visibleRuns.length === 0 ? (
        <StateCard
          variant="empty"
          title="No Runs match this filter"
          description="Choose another Run source or clear the filter to see the available history."
        />
      ) : (
        <div className="run-list">
          {visibleRuns.map((run) => {
            const verification = run.aggregates.verification;
            const eventPage = eventPages[run.runId];
            const eventError = eventErrors[run.runId];
            const approvalExpiry = run.approval ? Date.parse(run.approval.expiresAt) : NaN;
            const approvalActive = Number.isFinite(approvalExpiry) && approvalExpiry > Date.now();
            const requiresDurableApproval = run.clientId === "mcp";
            const tokens = tokenLabel(run);
            const stopCause = stopCauseLabel(run);
            return (
              <article className={`run-card state-${run.state}`} key={run.runId}>
                <div className="run-card-heading">
                  <span className="run-state">
                    <span className="run-state-dot" aria-hidden />
                    {stateLabels[run.state]}
                  </span>
                  <span
                    className="run-origin"
                    title={`Run submitted by ${remote ? "Remote service" : runOriginLabel(run)}`}
                  >
                    {remote ? "Remote service" : runOriginLabel(run)}
                  </span>
                  <span
                    className="run-execution-mode"
                    title={`Execution mode: ${runExecutionLabel(run)}`}
                  >
                    {runExecutionLabel(run)}
                  </span>
                  <time dateTime={run.updatedAt}>{timeLabel(run.updatedAt)}</time>
                </div>
                <div className="run-prompt" title={run.promptPreview}>
                  {run.promptPreview || "Untitled Build run"}
                </div>
                {run.progress && (
                  <div className="run-progress" aria-label="Run progress">
                    <div className="run-progress-label">
                      <span>
                        Round {run.progress.round}/{run.progress.maxRounds}
                      </span>
                      <span>{run.progress.lastTool || "model step"}</span>
                    </div>
                    <div className="run-progress-track" aria-hidden>
                      <span
                        style={{
                          width: `${Math.min(100, (run.progress.round / Math.max(1, run.progress.maxRounds)) * 100)}%`,
                        }}
                      />
                    </div>
                    <div className="run-progress-detail">{run.progress.detail}</div>
                  </div>
                )}
                {run.state === "queued" && (
                  <div className="run-queue-callout" role="status">
                    Waiting for an execution slot
                    {run.queuePosition != null ? ` · position ${run.queuePosition}` : ""}
                  </div>
                )}
                <div className="run-metrics">
                  <span>{run.aggregates.changes.length} files</span>
                  <span>{testLabel(run)}</span>
                  {verification && <span>Verification: {verification.status}</span>}
                  {tokens && <span>{tokens}</span>}
                </div>
                {stopCause && (
                  <div className="run-callout" role="status">
                    Stop cause: {stopCause}
                  </div>
                )}
                {run.state === "interrupted" && (
                  <div className="run-callout" role="status">
                    This run stopped after restart. Review it before starting a linked retry.
                  </div>
                )}
                {!remote && run.state === "interrupted" && run.clientId === "mcp" && (
                  <div className="run-retry">
                    <label htmlFor={`retry-prompt-${run.runId}`}>Fresh recovery prompt</label>
                    <textarea
                      id={`retry-prompt-${run.runId}`}
                      value={retryPrompts[run.runId] ?? ""}
                      onChange={(event) =>
                        setRetryPrompts((current) => ({
                          ...current,
                          [run.runId]: event.target.value,
                        }))
                      }
                      placeholder="Describe what the replacement run should do next"
                      rows={2}
                      disabled={reviewing === run.runId}
                    />
                    <button
                      type="button"
                      className="composer-chip"
                      onClick={() => void retry(run.runId)}
                      disabled={reviewing === run.runId || !(retryPrompts[run.runId] ?? "").trim()}
                    >
                      {reviewing === run.runId ? "Retrying…" : "Retry interrupted run"}
                    </button>
                  </div>
                )}
                {run.clientId === "mcp" &&
                  (run.state === "running" || run.state === "queued") && (
                    <div className="run-control">
                      <label htmlFor={`steer-prompt-${run.runId}`}>Steering prompt</label>
                      <textarea
                        id={`steer-prompt-${run.runId}`}
                        value={steerPrompts[run.runId] ?? ""}
                        onChange={(event) =>
                          setSteerPrompts((current) => ({
                            ...current,
                            [run.runId]: event.target.value,
                          }))
                        }
                        placeholder="Guide the current turn at its next safe boundary"
                        rows={2}
                        disabled={reviewing === run.runId}
                      />
                      <div className="run-actions">
                        <button
                          type="button"
                          className="composer-chip"
                          onClick={() => void steer(run.runId)}
                          disabled={reviewing === run.runId || !(steerPrompts[run.runId] ?? "").trim()}
                        >
                          {reviewing === run.runId ? "Sending…" : "Steer now"}
                        </button>
                        <button
                          type="button"
                          className="composer-chip quiet"
                          onClick={() => void cancel(run.runId)}
                          disabled={reviewing === run.runId}
                        >
                          Cancel run
                        </button>
                      </div>
                    </div>
                  )}
                {run.retryOf && (
                  <div className="run-callout" role="status">
                    Explicit retry of interrupted run {run.retryOf}
                  </div>
                )}
                {run.errorCode && run.state !== "interrupted" && (
                  <div className="run-error" role="status">
                    {run.errorCode.replaceAll("_", " ")}
                  </div>
                )}
                <div className="run-actions run-event-actions">
                  <button
                    type="button"
                    className="composer-chip quiet"
                    onClick={() => void loadEvents(run.runId)}
                    disabled={eventLoading === run.runId}
                  >
                    {eventLoading === run.runId
                      ? "Loading events…"
                      : eventPage
                        ? "Refresh events"
                        : "Replay events"}
                  </button>
                </div>
                {eventError && (
                  <div className="run-error" role="alert">
                    Could not load the event timeline: {eventError}
                  </div>
                )}
                {eventPage && (
                  <details className="run-events" open>
                    <summary>Run timeline · {eventPage.entries.length} events</summary>
                    {eventPage.cursorExpired && (
                      <div className="run-callout" role="status">
                        Older events are no longer retained. Durable progress, changes, tests,
                        and handoff remain available.
                      </div>
                    )}
                    {eventPage.entries.length > 0 ? (
                      <ol className="run-event-list">
                        {eventPage.entries.map((entry) => (
                          <li className="run-event-row" key={entry.seq}>
                            <time dateTime={entry.ts}>{timeLabel(entry.ts)}</time>
                            <span className="run-event-detail">{eventLabel(entry.update)}</span>
                          </li>
                        ))}
                      </ol>
                    ) : (
                      <div className="run-event-empty">No retained events in this page.</div>
                    )}
                    {!eventPage.cursorExpired && eventPage.nextCursor != null && (
                      <button
                        type="button"
                        className="composer-chip quiet"
                        onClick={() => void loadEvents(run.runId, eventPage.nextCursor ?? 0)}
                        disabled={eventLoading === run.runId}
                      >
                        Load more events
                      </button>
                    )}
                  </details>
                )}
                {!remote && run.execution?.mode === "isolated_worktree" &&
                  run.state === "completed" && (
                    <div className="run-promotion" aria-label="Isolated run actions">
                      <div className="run-promotion-status">
                        Isolated · {run.execution.promotionState.replaceAll("_", " ")}
                      </div>
                      {run.approval && (
                        <div
                          className={`run-approval ${approvalActive ? "is-active" : "is-expired"}`}
                          role="status"
                        >
                          {approvalActive
                            ? `Approval active until ${timeLabel(run.approval.expiresAt)}`
                            : "Approval expired · review and approve again"}
                        </div>
                      )}
                      {run.execution.promotionState === "conflicted" && (
                        <div className="run-callout run-promotion-conflict" role="alert">
                          Promotion blocked because the isolated workspace changed after review.
                          Discard this run and start a fresh isolated attempt.
                        </div>
                      )}
                      <div className="run-actions">
                        {run.execution.promotionState === "ready" && (
                          <button
                            type="button"
                            className="composer-chip"
                            onClick={() => void review(run.runId)}
                            disabled={reviewing === run.runId}
                          >
                            {reviewing === run.runId ? "Reviewing…" : "Review diff"}
                          </button>
                        )}
                        {requiresDurableApproval &&
                          run.execution.promotionState === "ready" &&
                          reviews[run.runId] &&
                          !approvalActive && (
                            <button
                              type="button"
                              className="composer-chip on"
                              onClick={() => void approve(run.runId)}
                              disabled={reviewing === run.runId}
                            >
                              {reviewing === run.runId
                                ? "Approving…"
                                : "Approve for promotion"}
                            </button>
                          )}
                        {run.execution.promotionState === "ready" &&
                          reviews[run.runId] &&
                          (!requiresDurableApproval || approvalActive) && (
                            <button
                              type="button"
                              className="composer-chip on"
                              onClick={() => void promote(run.runId)}
                              disabled={reviewing === run.runId}
                            >
                              Promote reviewed changes
                            </button>
                          )}
                        {run.execution.promotionState !== "promoted" &&
                          run.execution.promotionState !== "discarded" && (
                            <button
                              type="button"
                              className="composer-chip quiet"
                              onClick={() => void discard(run.runId)}
                              disabled={reviewing === run.runId}
                            >
                              Discard
                            </button>
                          )}
                      </div>
                      {reviews[run.runId] && (
                        <details className="run-review" open>
                          <summary>
                            {reviews[run.runId].changedFiles.length} changed files
                            {reviews[run.runId].diffTruncated ? " · diff truncated" : ""}
                          </summary>
                          <pre>{reviews[run.runId].diff || "No changes"}</pre>
                        </details>
                      )}
                    </div>
                  )}
                {run.finalResponse && (
                  <details className="run-handoff">
                    <summary>Handoff</summary>
                    <div>{run.finalResponse}</div>
                  </details>
                )}
              </article>
            );
          })}
        </div>
      )}
      {actionError && (
        <div className="run-error" role="alert">
          {actionError}
        </div>
      )}
    </section>
  );
}
