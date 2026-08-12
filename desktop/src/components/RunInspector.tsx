import type { DurableRun } from "../lib/protocol";

type RunInspectorProps = {
  runs: DurableRun[];
  busy?: boolean;
  onRefresh: () => void;
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

export function RunInspector({ runs, busy, onRefresh }: RunInspectorProps) {
  return (
    <section className="run-inspector" aria-label="Durable task runs">
      <div className="run-inspector-header">
        <div>
          <div className="section-title">Task runs</div>
          <p className="run-inspector-subtitle">
            Durable progress and verification for this Build session.
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

      {runs.length === 0 ? (
        <div className="panel-block run-inspector-empty">
          No durable Build runs for this session yet.
        </div>
      ) : (
        <div className="run-list">
          {runs.map((run) => {
            const verification = run.aggregates.verification;
            return (
              <article className={`run-card state-${run.state}`} key={run.runId}>
                <div className="run-card-heading">
                  <span className="run-state">
                    <span className="run-state-dot" aria-hidden />
                    {stateLabels[run.state]}
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
    </section>
  );
}
