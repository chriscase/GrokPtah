import type { VerifiedChangeView } from "../lib/verifiedChange";

export function VerifiedChangePanel({
  view,
  error,
}: {
  view: VerifiedChangeView | null;
  error?: string | null;
}) {
  const readiness = view?.readiness;
  return (
    <section className="verified-change" aria-label="Verified change">
      <strong>Verified change</strong>
      {error ? <p role="alert">{error}</p> : null}
      {!view ? (
        <p>Prepare an assignment to see repository, scope, checks, and readiness.</p>
      ) : (
        <dl>
          <div>
            <dt>Repository</dt>
            <dd>{view.repositoryId}</dd>
          </div>
          <div>
            <dt>Source revision</dt>
            <dd>{view.sourceRevision}</dd>
          </div>
          <div>
            <dt>Agent</dt>
            <dd>{view.agentId}</dd>
          </div>
          <div>
            <dt>Execution host</dt>
            <dd>{view.executionHost}</dd>
          </div>
          <div>
            <dt>Scope</dt>
            <dd>{(view.allowedFiles ?? []).join(", ") || "none"}</dd>
          </div>
          <div>
            <dt>Executor</dt>
            <dd>{view.executor}</dd>
          </div>
          <div>
            <dt>Model</dt>
            <dd>{view.modelSelectionKey || "unset"}</dd>
          </div>
          <div>
            <dt>CLI</dt>
            <dd>
              {view.cliVersion || "unavailable"} {view.cliContract ? `(${view.cliContract})` : ""}
            </dd>
          </div>
          <div>
            <dt>Limits</dt>
            <dd>
              {view.limits?.maxRounds ?? "?"} rounds / {view.limits?.maxDurationMs ?? "?"} ms
            </dd>
          </div>
          <div>
            <dt>Required checks</dt>
            <dd>{(view.requiredChecks ?? []).map((check) => check.checkId).join(", ") || "none"}</dd>
          </div>
          <div>
            <dt>Approval</dt>
            <dd>{view.approvalRequired ? "required before application" : "not required"}</dd>
          </div>
          <div>
            <dt>Readiness</dt>
            <dd>
              {readiness?.ready ? "ready" : "not ready"}
              {readiness?.reasons?.length ? `: ${readiness.reasons.join(" ")}` : ""}
            </dd>
          </div>
          <div>
            <dt>Dispatch</dt>
            <dd>{readiness?.workersDispatched ?? 0} workers, {readiness?.providerInvocations ?? 0} provider calls</dd>
          </div>
          <div>
            <dt>Phases</dt>
            <dd>
              stopped {String(view.phases?.workerStopped === true)}, proposed{" "}
              {String(view.phases?.changeProposed === true)}, checks{" "}
              {String(view.phases?.checksPassed === true)}, approved{" "}
              {String(view.phases?.humanApproved === true)}, applied{" "}
              {String(view.phases?.applied === true)}
            </dd>
          </div>
          <div>
            <dt>Next action</dt>
            <dd>{view.safeAction}</dd>
          </div>
          {view.boundedDiff ? (
            <div>
              <dt>Diff</dt>
              <dd>
                <pre>{view.boundedDiff}</pre>
              </dd>
            </div>
          ) : null}
          {view.checkResults?.length ? (
            <div>
              <dt>Check results</dt>
              <dd>{view.checkResults.map((check) => `${check.checkId}: ${check.outcome}`).join("; ")}</dd>
            </div>
          ) : null}
        </dl>
      )}
    </section>
  );
}
