import { useState } from "react";
import type { DurableRun, RunReview } from "../lib/protocol";

type RunInspectorProps = {
  runs: DurableRun[];
  busy?: boolean;
  watching?: boolean;
  onWatchingChange?: (watching: boolean) => void;
  onRefresh: () => void;
  onReview: (runId: string) => Promise<RunReview>;
  onPromote: (runId: string) => Promise<void>;
  onDiscard: (runId: string) => Promise<void>;
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

function runOriginLabel(run: DurableRun): string {
  if (run.clientId === "mcp") return "MCP coordinator";
  if (run.clientId === "desktop") return "Desktop";
  return run.clientId || "Unknown origin";
}

function runExecutionLabel(run: DurableRun): string {
  if (run.execution?.mode === "isolated_worktree") return "Isolated worktree";
  return "Shared workspace";
}

export function RunInspector({
  runs,
  busy,
  watching,
  onWatchingChange,
  onRefresh,
  onReview,
  onPromote,
  onDiscard,
}: RunInspectorProps) {
  const [originFilter, setOriginFilter] = useState<"all" | "desktop" | "mcp" | "other">("all");
  const [localWatching, setLocalWatching] = useState(true);
  const [reviewing, setReviewing] = useState<string | null>(null);
  const [reviews, setReviews] = useState<Record<string, RunReview>>({});
  const [actionError, setActionError] = useState<string | null>(null);
  const watchValue = watching ?? localWatching;

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

  return (
    <section className="run-inspector" aria-label="Durable task runs">
      <div className="run-inspector-header">
        <div>
          <div className="section-title">Task runs</div>
          <p className="run-inspector-subtitle">
            Durable progress and verification from desktop and MCP activity.
          </p>
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

      {runs.length === 0 ? (
        <div className="panel-block run-inspector-empty">
          No durable Build runs for this session yet.
        </div>
      ) : visibleRuns.length === 0 ? (
        <div className="panel-block run-inspector-empty">
          No runs match this source filter.
        </div>
      ) : (
        <div className="run-list">
          {visibleRuns.map((run) => {
            const verification = run.aggregates.verification;
            return (
              <article className={`run-card state-${run.state}`} key={run.runId}>
                <div className="run-card-heading">
                  <span className="run-state">
                    <span className="run-state-dot" aria-hidden />
                    {stateLabels[run.state]}
                  </span>
                  <span
                    className="run-origin"
                    title={`Run submitted by ${runOriginLabel(run)}`}
                  >
                    {runOriginLabel(run)}
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
                <div className="run-metrics">
                  <span>{run.aggregates.changes.length} files</span>
                  <span>{testLabel(run)}</span>
                  {verification && <span>Verification: {verification.status}</span>}
                </div>
                {run.state === "interrupted" && (
                  <div className="run-callout" role="status">
                    This run stopped after restart. Review it before starting a linked retry.
                  </div>
                )}
                {run.errorCode && run.state !== "interrupted" && (
                  <div className="run-error" role="status">
                    {run.errorCode.replaceAll("_", " ")}
                  </div>
                )}
                {run.execution?.mode === "isolated_worktree" &&
                  run.state === "completed" && (
                    <div className="run-promotion" aria-label="Isolated run actions">
                      <div className="run-promotion-status">
                        Isolated · {run.execution.promotionState.replaceAll("_", " ")}
                      </div>
                      <div className="run-actions">
                        <button
                          type="button"
                          className="composer-chip"
                          onClick={() => void review(run.runId)}
                          disabled={reviewing === run.runId}
                        >
                          {reviewing === run.runId ? "Reviewing…" : "Review diff"}
                        </button>
                        {run.execution.promotionState === "ready" &&
                          reviews[run.runId] && (
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
