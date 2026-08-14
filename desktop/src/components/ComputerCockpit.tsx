import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import type {
  ComputerAction,
  ComputerCockpitSnapshot,
  ComputerRun,
  ComputerSemanticElement,
} from "../lib/protocol";

const SIMULATOR_APP_ID = "com.grokptah.computer-use-simulator";

type ComputerCockpitProps = {
  sessionId: string | null;
  sessionTitle?: string;
  model: string;
  effort: string;
  sessionBusy: boolean;
  onClose: () => void;
  onSteer: (text: string) => Promise<string>;
  onRunState?: (state: string | null) => void;
};

function titleCase(value: string) {
  return value.replaceAll("_", " ").replace(/\b\w/g, (char) => char.toUpperCase());
}

function isTerminal(run: ComputerRun) {
  return ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(
    run.state,
  );
}

function elementByRole(
  run: ComputerRun,
  role: string,
): ComputerSemanticElement | undefined {
  return run.currentObservation?.elements.find((element) => element.role === role);
}

function actionText(action: ComputerAction) {
  return action.type === "set_value" ? action.text : "Submit";
}

export function ComputerCockpit({
  sessionId,
  sessionTitle,
  model,
  effort,
  sessionBusy,
  onClose,
  onSteer,
  onRunState,
}: ComputerCockpitProps) {
  const [snapshot, setSnapshot] = useState<ComputerCockpitSnapshot | null>(null);
  const [scopeReviewed, setScopeReviewed] = useState(false);
  const [name, setName] = useState("Ada Lovelace");
  const [steerText, setSteerText] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const requestEpoch = useRef(0);
  const proposalFocus = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    const epoch = ++requestEpoch.current;
    setSnapshot(null);
    setScopeReviewed(false);
    setError(null);
    setNotice(null);
    setBusy(false);
    if (!sessionId) {
      onRunState?.(null);
      return;
    }
    void api
      .computerUseCockpitSnapshot(sessionId)
      .then((next) => {
        if (requestEpoch.current !== epoch) return;
        setSnapshot(next);
        onRunState?.(next.run?.state ?? null);
      })
      .catch((reason) => {
        if (requestEpoch.current !== epoch) return;
        setError(String(reason));
        onRunState?.(null);
      });
  }, [sessionId, onRunState]);

  const apply = async (
    mutation: () => Promise<ComputerCockpitSnapshot>,
    success?: string,
  ) => {
    const epoch = requestEpoch.current;
    setBusy(true);
    setError(null);
    try {
      const next = await mutation();
      if (requestEpoch.current !== epoch) return;
      setSnapshot(next);
      setNotice(success ?? null);
      onRunState?.(next.run?.state ?? null);
    } catch (reason) {
      if (requestEpoch.current === epoch) setError(String(reason));
    } finally {
      if (requestEpoch.current === epoch) setBusy(false);
    }
  };

  const run = snapshot?.run ?? null;
  const observation = run?.currentObservation ?? null;
  const nameElement = run ? elementByRole(run, "text_field") : undefined;
  const submitElement = run ? elementByRole(run, "button") : undefined;
  const statusElement = run ? elementByRole(run, "status") : undefined;
  const approval = snapshot?.pendingApproval ?? null;
  const grantActive = Boolean(run?.grant && !run.grant.revokedAt);
  const runActive = Boolean(run && !isTerminal(run));
  const timeline = useMemo(() => run?.audit.slice(-12).reverse() ?? [], [run]);

  const stage = async (action: ComputerAction) => {
    if (!sessionId || !run || !observation) return;
    await apply(() =>
      api.computerUseCockpitStageAction(
        sessionId,
        run.runId,
        run.version,
        observation.observationId,
        action,
      ),
    );
  };

  return (
    <section className="computer-cockpit" aria-label="Computer Run cockpit">
      <header className="computer-cockpit-header">
        <div>
          <div className="computer-eyebrow">Computer Run</div>
          <h1>{run?.target.displayName ?? "Simulator"}</h1>
          <div className="computer-owner">
            {sessionTitle ? `Owned by ${sessionTitle}` : "Select a session to continue"}
          </div>
        </div>
        <div className="computer-header-actions">
          {runActive && (
            <span className={`computer-live-indicator state-${run?.state}`} role="status">
              <span aria-hidden />
              {titleCase(run?.state ?? "idle")}
            </span>
          )}
          <button type="button" onClick={onClose} aria-label="Close Computer Run cockpit">
            Close
          </button>
        </div>
      </header>

      {!sessionId && (
        <div className="computer-empty" role="status">
          Open a build or chat session before starting a Computer Run.
        </div>
      )}

      {sessionId && !snapshot && !error && (
        <div className="computer-empty" role="status">Loading Computer Run…</div>
      )}

      {error && (
        <div className="computer-alert is-error" role="alert">
          <strong>Computer Run needs attention</strong>
          <span>{error}</span>
        </div>
      )}
      {notice && !error && (
        <div className="computer-alert" role="status">{notice}</div>
      )}

      {sessionId && snapshot && !run && (
        <div className="computer-scope">
          <div className="computer-scope-title">
            <div>
              <span className="computer-section-label">Scope review</span>
              <h2>Computer Use Simulator</h2>
            </div>
            <span className="computer-origin">Local simulator</span>
          </div>
          <dl className="computer-scope-grid">
            <div><dt>Exact target</dt><dd>{SIMULATOR_APP_ID}</dd></div>
            <div><dt>Allowed input</dt><dd>Semantic invoke and visible text entry</dd></div>
            <div><dt>Grant</dt><dd>One action, then pause</dd></div>
            <div><dt>Evidence</dt><dd>Semantic demo data; no screen capture</dd></div>
          </dl>
          <label className="computer-scope-check">
            <input
              type="checkbox"
              checked={scopeReviewed}
              onChange={(event) => setScopeReviewed(event.target.checked)}
            />
            I reviewed this exact target and one-action scope
          </label>
          <button
            type="button"
            className="primary"
            disabled={!scopeReviewed || busy}
            title={!scopeReviewed ? "Review and accept the exact scope first" : undefined}
            onClick={() =>
              void apply(
                () => api.computerUseCockpitStartSimulator(sessionId, SIMULATOR_APP_ID),
                "Simulator observed. No action has run.",
              )
            }
          >
            Start Computer Run
          </button>
        </div>
      )}

      {sessionId && snapshot && run && (
        <>
          <div className="computer-control-bar" aria-label="Computer Run controls">
            <div className="computer-control-status">
              <strong>{titleCase(run.state)}</strong>
              <span>{run.actionCount} / {run.limits.maxActions} actions</span>
              <span>{grantActive ? "One action authorized" : "No active grant"}</span>
            </div>
            <div className="computer-control-actions">
              <button
                type="button"
                disabled={busy || run.state !== "ready"}
                title={run.state !== "ready" ? "Pause is available while the run is ready" : undefined}
                onClick={() =>
                  void apply(() =>
                    api.computerUseCockpitPause(sessionId, run.runId, run.version),
                  )
                }
              >
                Pause
              </button>
              <button
                type="button"
                disabled={busy || isTerminal(run) || run.state === "paused"}
                onClick={() =>
                  void apply(
                    () => api.computerUseCockpitTakeOver(sessionId, run.runId, run.version),
                    "You have control. All Computer Use authority was revoked.",
                  )
                }
              >
                Take over
              </button>
              <button
                type="button"
                className="danger"
                disabled={busy || isTerminal(run)}
                onClick={() =>
                  void apply(
                    () => api.computerUseCockpitStop(sessionId, run.runId),
                    "Computer Run stopped.",
                  )
                }
              >
                Stop
              </button>
            </div>
          </div>

          <div className="computer-cockpit-grid">
            <div className="computer-observation-column">
              <div className="computer-section-heading">
                <div><span className="computer-section-label">Observation</span><h2>Demo form</h2></div>
                <span>{observation ? `Frame ${observation.sequence}` : "No live frame"}</span>
              </div>
              <div className={`computer-demo-surface ${observation ? "is-observed" : ""}`}>
                <label>
                  Name
                  <input value={nameElement?.value ?? ""} readOnly aria-label="Observed demo name" />
                </label>
                <button type="button" disabled={!submitElement?.enabled}>Submit</button>
                <output>{statusElement?.label ?? "Observation unavailable"}</output>
              </div>
              <div className="computer-semantic-strip">
                {(observation?.elements ?? []).map((element) => (
                  <span key={element.elementId} className={element.enabled ? "" : "is-disabled"}>
                    {element.label ?? element.role}
                  </span>
                ))}
              </div>
            </div>

            <aside className="computer-run-details" aria-label="Computer Run details">
              <span className="computer-section-label">Run details</span>
              <dl>
                <div><dt>Target</dt><dd>{run.target.displayName}</dd></div>
                <div><dt>Backend</dt><dd>{snapshot.backend.backendId}</dd></div>
                <div><dt>Origin</dt><dd>{titleCase(snapshot.origin)}</dd></div>
                <div><dt>Agent model</dt><dd>{model} · {effort}</dd></div>
                <div><dt>Grant expires</dt><dd>{grantActive && run.grant ? new Date(run.grant.expiresAt).toLocaleTimeString() : "Revoked"}</dd></div>
                <div><dt>Pointer fallback</dt><dd>Disabled</dd></div>
              </dl>
              {run.lastOutcome && <div className="computer-outcome">{run.lastOutcome.summary}</div>}
              {run.lastError && (
                <div className="computer-alert is-error" role="alert">
                  <strong>{titleCase(run.lastError.code)}</strong>
                  <span>{run.lastError.message}</span>
                </div>
              )}
            </aside>
          </div>

          {run.state === "ready" && observation && !approval && (
            <div className="computer-proposal">
              <div><span className="computer-section-label">Next action</span><h2>Stage for approval</h2></div>
              <label>
                Visible text
                <input value={name} maxLength={256} onChange={(event) => setName(event.target.value)} />
              </label>
              <div className="computer-proposal-actions">
                <button
                  ref={proposalFocus}
                  type="button"
                  disabled={busy || !name.trim() || !nameElement?.enabled}
                  onClick={() =>
                    void stage({
                      type: "set_value",
                      element_id: nameElement?.elementId ?? "",
                      text: name.trim(),
                    })
                  }
                >
                  Stage text entry
                </button>
                <button
                  type="button"
                  disabled={busy || !submitElement?.enabled}
                  title={!submitElement?.enabled ? "Enter and approve a name first" : undefined}
                  onClick={() =>
                    void stage({ type: "invoke", element_id: submitElement?.elementId ?? "" })
                  }
                >
                  Stage Submit
                </button>
              </div>
            </div>
          )}

          {approval && (
            <div className="computer-approval" role="dialog" aria-label="Approve Computer Use action">
              <div className="computer-approval-title">
                <div><span className="computer-section-label">Approval required</span><h2>{approval.actionSummary}</h2></div>
                <span className="computer-risk">{approval.risk}</span>
              </div>
              <dl>
                <div><dt>Target</dt><dd>{approval.targetLabel}</dd></div>
                <div><dt>Will send</dt><dd>{actionText(approval.action)}</dd></div>
                <div><dt>Bound to</dt><dd>Frame {observation?.sequence ?? "expired"} · one use</dd></div>
              </dl>
              <div className="computer-approval-actions">
                <button
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void apply(() => api.computerUseCockpitDiscardApproval(sessionId)).finally(() =>
                      proposalFocus.current?.focus(),
                    )
                  }
                >
                  Reject
                </button>
                <button
                  type="button"
                  className="primary"
                  disabled={busy}
                  autoFocus
                  onClick={() =>
                    void apply(
                      () =>
                        api.computerUseCockpitApprove(
                          sessionId,
                          approval.approvalId,
                          crypto.randomUUID(),
                        ),
                      "Action completed. Reauthorization is required.",
                    )
                  }
                >
                  Approve once
                </button>
              </div>
            </div>
          )}

          {run.state === "paused" && (
            <div className="computer-paused">
              <div><span className="computer-section-label">Authority revoked</span><h2>Paused</h2></div>
              <button
                type="button"
                className="primary"
                disabled={busy}
                onClick={() =>
                  void apply(
                    () => api.computerUseCockpitRefresh(sessionId, run.runId, run.version),
                    "Fresh observation ready with a new one-action grant.",
                  )
                }
              >
                Reauthorize and observe
              </button>
            </div>
          )}

          <div className="computer-lower-grid">
            <div className="computer-steer">
              <span className="computer-section-label">Steer owning session</span>
              <textarea
                rows={3}
                value={steerText}
                placeholder="Guide the agent at its next safe step"
                onChange={(event) => setSteerText(event.target.value)}
              />
              <button
                type="button"
                disabled={busy || !steerText.trim()}
                onClick={() => {
                  const text = steerText.trim();
                  const epoch = requestEpoch.current;
                  setBusy(true);
                  setError(null);
                  void onSteer(text)
                    .then((message) => {
                      if (requestEpoch.current !== epoch) return;
                      setSteerText("");
                      setNotice(message);
                    })
                    .catch((reason) => {
                      if (requestEpoch.current === epoch) setError(String(reason));
                    })
                    .finally(() => {
                      if (requestEpoch.current === epoch) setBusy(false);
                    });
                }}
              >
                {sessionBusy ? "Steer now" : "Queue priority prompt"}
              </button>
            </div>
            <div className="computer-timeline">
              <span className="computer-section-label">Audit timeline</span>
              <ol>
                {timeline.map((entry) => (
                  <li key={entry.sequence}>
                    <span>{entry.sequence}</span>
                    <div><strong>{titleCase(entry.operation)}</strong><small>{titleCase(entry.disposition)}</small></div>
                  </li>
                ))}
              </ol>
            </div>
          </div>
        </>
      )}
    </section>
  );
}
