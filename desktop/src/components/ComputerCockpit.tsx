import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import {
  computerActivityAnnouncement,
  computerActivityState,
} from "../lib/computerActivity";
import type {
  ComputerAction,
  ComputerAgentEligibility,
  ComputerCockpitSnapshot,
  ComputerPermissionStatus,
  ComputerPlatformStatus,
  ComputerRun,
  ComputerSemanticElement,
  ComputerTargetCandidate,
} from "../lib/protocol";
import { LaneScopeLine, type LaneScope } from "./LaneScopeLine";

const SIMULATOR_APP_ID = "com.grokptah.computer-use-simulator";

type ComputerCockpitProps = {
  sessionId: string | null;
  sessionTitle?: string;
  scope?: LaneScope;
  model: string;
  effort: string;
  computerUseTier?: string;
  computerCapabilitySource?: string;
  sessionBusy: boolean;
  onClose: () => void;
  onSteer: (text: string) => Promise<string>;
  onRunState?: (state: string | null) => void;
};

const DEFAULT_AGENT_OBJECTIVE =
  "Enter Ada Lovelace in the Name field, then submit the form.";

function titleCase(value: string) {
  return value.replaceAll("_", " ").replace(/\b\w/g, (char) => char.toUpperCase());
}

function isTerminal(run: ComputerRun) {
  return ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(
    run.state,
  );
}

function hasOperatorTakeover(run: ComputerRun) {
  return run.controlDisposition === "operator_takeover";
}

function elementByAction(
  run: ComputerRun,
  action: string,
): ComputerSemanticElement | undefined {
  return run.currentObservation?.elements.find((element) =>
    element.actions.includes(action),
  );
}

function actionText(action: ComputerAction) {
  switch (action.type) {
    case "set_value":
      return action.text;
    case "activate_target":
      return "Activate the authorized application";
    case "invoke":
      return "Invoke the selected element";
    case "select":
      return "Select the chosen element";
    case "scroll":
      return "Scroll the chosen element into view";
  }
}

export function ComputerCockpit({
  sessionId,
  sessionTitle,
  scope,
  model,
  effort,
  computerUseTier = "none",
  computerCapabilitySource = "unknown",
  sessionBusy,
  onClose,
  onSteer,
  onRunState,
}: ComputerCockpitProps) {
  const [snapshot, setSnapshot] = useState<ComputerCockpitSnapshot | null>(null);
  const [launchMode, setLaunchMode] = useState<"simulator" | "macos">("simulator");
  const [nativeTargets, setNativeTargets] = useState<ComputerTargetCandidate[]>([]);
  const [selectedNativeToken, setSelectedNativeToken] = useState<string | null>(null);
  const [platformStatus, setPlatformStatus] = useState<ComputerPlatformStatus | null>(null);
  const [scopeReviewed, setScopeReviewed] = useState(false);
  const [name, setName] = useState("Ada Lovelace");
  const [objective, setObjective] = useState(DEFAULT_AGENT_OBJECTIVE);
  const [agentEligibility, setAgentEligibility] = useState<ComputerAgentEligibility | null>(null);
  const [agentBusy, setAgentBusy] = useState(false);
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
    setNativeTargets([]);
    setSelectedNativeToken(null);
    setPlatformStatus(null);
    setError(null);
    setNotice(null);
    setBusy(false);
    setAgentBusy(false);
    setAgentEligibility(null);
    setObjective(DEFAULT_AGENT_OBJECTIVE);
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
    void api
      .computerUseCockpitAgentEligibility(sessionId)
      .then((eligibility) => {
        if (requestEpoch.current === epoch) setAgentEligibility(eligibility);
      })
      .catch(() => {
        // The durable model capability passed as props remains authoritative.
      });
    return () => {
      void api.computerUseCockpitCancelAgent(sessionId).catch(() => {});
    };
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
  // Status rendering reads the authoritative projection, which is the same
  // payload an external coordinator observes. `run` is kept only for the
  // local-only observation detail an approval needs.
  const projection = snapshot?.projection ?? null;
  const activity = projection ? computerActivityState(projection) : null;
  const observation = run?.currentObservation ?? null;
  const nameElement = run ? elementByAction(run, "set_value") : undefined;
  const submitElement = run ? elementByAction(run, "invoke") : undefined;
  const statusElement = observation?.elements.find((element) =>
    ["status", "AXStaticText"].includes(element.role),
  );
  const approval = snapshot?.pendingApproval ?? null;
  const grantActive = Boolean(run?.grant && !run.grant.revokedAt);
  const timeline = useMemo(() => run?.audit.slice(-12).reverse() ?? [], [run]);
  const selectedNativeTarget = nativeTargets.find(
    (candidate) => candidate.selectionToken === selectedNativeToken,
  );
  const simulatorRun = run?.target.appId === SIMULATOR_APP_ID;
  const nativePermissionsReady =
    platformStatus?.screenRecording === "granted" &&
    platformStatus.accessibility === "granted";
  const effectiveAgentTier =
    agentEligibility?.model === model ? agentEligibility.tier : computerUseTier;
  const effectiveAgentSource =
    agentEligibility?.model === model ? agentEligibility.source : computerCapabilitySource;

  const qualifyAgent = async () => {
    if (!sessionId) return;
    const epoch = requestEpoch.current;
    setAgentBusy(true);
    setError(null);
    setNotice(null);
    try {
      const eligibility = await api.computerUseCockpitQualifyAgent(sessionId);
      if (requestEpoch.current !== epoch) return;
      setAgentEligibility(eligibility);
      setNotice(`${eligibility.model} passed the semantic simulator check.`);
    } catch (reason) {
      if (requestEpoch.current === epoch) setError(String(reason));
    } finally {
      if (requestEpoch.current === epoch) setAgentBusy(false);
    }
  };

  const proposeAgentAction = async () => {
    if (!sessionId || !run || !observation || !objective.trim()) return;
    const epoch = requestEpoch.current;
    setAgentBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await api.computerUseCockpitProposeAgentAction(
        sessionId,
        run.runId,
        run.version,
        observation.observationId,
        objective.trim(),
      );
      if (requestEpoch.current !== epoch) return;
      setSnapshot(result.snapshot);
      onRunState?.(result.snapshot.run?.state ?? null);
      setNotice(
        result.completed
          ? `Model marked the run complete: ${result.summary}`
          : `Model proposal ready for review: ${result.summary}`,
      );
    } catch (reason) {
      if (requestEpoch.current === epoch) setError(String(reason));
    } finally {
      if (requestEpoch.current === epoch) setAgentBusy(false);
    }
  };

  const refreshNativeStatus = async () => {
    const epoch = requestEpoch.current;
    setBusy(true);
    setError(null);
    try {
      const status = await api.computerUseStatus();
      if (requestEpoch.current === epoch) setPlatformStatus(status);
    } catch (reason) {
      if (requestEpoch.current === epoch) setError(String(reason));
    } finally {
      if (requestEpoch.current === epoch) setBusy(false);
    }
  };

  const requestNativePermission = async (
    permission: "screen_recording" | "accessibility",
  ) => {
    const epoch = requestEpoch.current;
    setBusy(true);
    setError(null);
    try {
      await api.computerUseRequestPermission(permission);
      const status = await api.computerUseStatus();
      if (requestEpoch.current !== epoch) return;
      setPlatformStatus(status);
      setNotice("Permission status refreshed.");
    } catch (reason) {
      if (requestEpoch.current === epoch) setError(String(reason));
    } finally {
      if (requestEpoch.current === epoch) setBusy(false);
    }
  };

  const findNativeTargets = async () => {
    const epoch = requestEpoch.current;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const targets = await api.computerUseListTargets();
      if (requestEpoch.current !== epoch) return;
      const eligible = targets.filter(
        (candidate) => candidate.onScreen && !candidate.minimized,
      );
      setNativeTargets(eligible);
      setSelectedNativeToken(null);
      setScopeReviewed(false);
      setNotice(
        eligible.length ? "Choose one exact macOS window." : "No eligible windows found.",
      );
    } catch (reason) {
      if (requestEpoch.current === epoch) setError(String(reason));
    } finally {
      if (requestEpoch.current === epoch) setBusy(false);
    }
  };

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
          <h1>
            {run?.target.displayName ??
              selectedNativeTarget?.target.displayName ??
              (launchMode === "macos" ? "macOS application" : "Simulator")}
          </h1>
          <div className="computer-owner">
            {sessionTitle ? `Owned by ${sessionTitle}` : "Select a session to continue"}
          </div>
          {scope && <LaneScopeLine scope={scope} compact />}
        </div>
        <div className="computer-header-actions">
          {activity && projection && (
            <span
              className={`computer-live-indicator tone-${activity.tone} state-${projection.state}`}
              data-activity={activity.id}
              title={activity.detail}
            >
              <span aria-hidden />
              {activity.label}
            </span>
          )}
          <button type="button" onClick={onClose} aria-label="Close Computer Run cockpit">
            Close
          </button>
        </div>
      </header>

      {/* Single authoritative announcement of who owns the run, so assistive
          technology hears takeover, stop, interruption, and uncertain outcomes
          rather than only a lifecycle state name. */}
      <p className="computer-visually-hidden" role="status" aria-live="polite">
        {projection ? computerActivityAnnouncement(projection) : ""}
      </p>

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
              <h2>
                {launchMode === "simulator"
                  ? "Computer Use Simulator"
                  : selectedNativeTarget?.target.displayName ?? "macOS application"}
              </h2>
            </div>
            <span className="computer-origin">
              {launchMode === "simulator" ? "Local simulator" : "Native Accessibility"}
            </span>
          </div>
          <div className="computer-launch-mode" role="group" aria-label="Computer Run target type">
            <button
              type="button"
              className={launchMode === "simulator" ? "active" : ""}
              aria-pressed={launchMode === "simulator"}
              onClick={() => {
                setLaunchMode("simulator");
                setScopeReviewed(false);
              }}
            >
              Simulator
            </button>
            <button
              type="button"
              className={launchMode === "macos" ? "active" : ""}
              aria-pressed={launchMode === "macos"}
              onClick={() => {
                setLaunchMode("macos");
                setScopeReviewed(false);
                if (!platformStatus) void refreshNativeStatus();
              }}
            >
              macOS app
            </button>
          </div>
          {launchMode === "macos" && (
            <div className="computer-native-picker">
              <div className="computer-native-permissions" aria-label="macOS Computer Use permissions">
                {(["screenRecording", "accessibility"] as const).map((key) => {
                  const permission = key === "screenRecording" ? "screen_recording" : "accessibility";
                  const value: ComputerPermissionStatus | undefined = platformStatus?.[key];
                  return (
                    <div key={key}>
                      <span>{key === "screenRecording" ? "Screen Recording" : "Accessibility"}</span>
                      <strong>{value ? titleCase(value) : "Checking"}</strong>
                      {value && value !== "granted" && value !== "unsupported" && (
                        <button
                          type="button"
                          aria-label={`Request ${key === "screenRecording" ? "Screen Recording" : "Accessibility"} permission`}
                          disabled={busy}
                          onClick={() => void requestNativePermission(permission)}
                        >
                          Request
                        </button>
                      )}
                    </div>
                  );
                })}
                <button
                  type="button"
                  aria-label="Refresh macOS permission status"
                  disabled={busy}
                  onClick={() => void refreshNativeStatus()}
                >
                  Refresh
                </button>
              </div>
              {platformStatus && !nativePermissionsReady && (
                <p className="settings-hint is-warning">
                  These grants apply to this GrokPtah installation. Codex Computer Use and Terminal
                  grants do not grant GrokPtah access; enable both for GrokPtah in macOS Privacy &amp;
                  Security, then restart GrokPtah and refresh.
                </p>
              )}
              <button
                type="button"
                disabled={busy || !nativePermissionsReady}
                onClick={() => void findNativeTargets()}
              >
                Find eligible windows
              </button>
              {nativeTargets.length > 0 && (
                <div className="computer-native-targets" role="radiogroup" aria-label="macOS target window">
                  {nativeTargets.map((candidate) => (
                    <label key={candidate.selectionToken}>
                      <input
                        type="radio"
                        name="computer-native-target"
                        value={candidate.selectionToken}
                        checked={selectedNativeToken === candidate.selectionToken}
                        onChange={() => {
                          setSelectedNativeToken(candidate.selectionToken);
                          setScopeReviewed(false);
                          setNotice(null);
                        }}
                      />
                      <span>
                        <strong>{candidate.target.displayName}</strong>
                        <small>
                          {Math.round(candidate.geometry.width)} x {Math.round(candidate.geometry.height)}
                          {candidate.active ? " · active" : ""}
                        </small>
                      </span>
                    </label>
                  ))}
                </div>
              )}
            </div>
          )}
          <dl className="computer-scope-grid">
            <div>
              <dt>Exact target</dt>
              <dd>
                {launchMode === "simulator"
                  ? SIMULATOR_APP_ID
                  : selectedNativeTarget?.target.appId ?? "Choose a window above"}
              </dd>
            </div>
            <div><dt>Allowed input</dt><dd>Semantic invoke and visible text entry</dd></div>
            <div><dt>Grant</dt><dd>One action, then pause</dd></div>
            <div>
              <dt>Evidence</dt>
              <dd>
                {launchMode === "simulator"
                  ? "Semantic demo data; no screen capture"
                  : "Redacted window capture and bounded Accessibility tree"}
              </dd>
            </div>
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
            disabled={
              !scopeReviewed ||
              busy ||
              (launchMode === "macos" && !selectedNativeTarget)
            }
            title={
              !scopeReviewed
                ? "Review and accept the exact scope first"
                : launchMode === "macos" && !selectedNativeTarget
                  ? "Choose an exact macOS window first"
                  : undefined
            }
            onClick={() =>
              void apply(
                () =>
                  launchMode === "simulator"
                    ? api.computerUseCockpitStartSimulator(sessionId, SIMULATOR_APP_ID)
                    : api.computerUseCockpitStartNative(
                        sessionId,
                        selectedNativeTarget?.selectionToken ?? "",
                        selectedNativeTarget?.target.appId ?? "",
                      ),
                launchMode === "simulator"
                  ? "Simulator observed. No action has run."
                  : "macOS target observed. No action has run.",
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
                <div>
                  <span className="computer-section-label">Observation</span>
                  <h2>{simulatorRun ? "Demo form" : "Semantic snapshot"}</h2>
                </div>
                <span>{observation ? `Frame ${observation.sequence}` : "No live frame"}</span>
              </div>
              {simulatorRun ? (
                <div className={`computer-demo-surface ${observation ? "is-observed" : ""}`}>
                  <label>
                    Name
                    <input value={nameElement?.value ?? ""} readOnly aria-label="Observed demo name" />
                  </label>
                  <button type="button" disabled={!submitElement?.enabled}>Submit</button>
                  <output>{statusElement?.label ?? "Observation unavailable"}</output>
                </div>
              ) : (
                <div className="computer-native-observation" aria-label="Observed macOS elements">
                  {(observation?.elements ?? []).slice(0, 48).map((element) => (
                    <div key={element.elementId}>
                      <span>{element.role.replace(/^AX/, "")}</span>
                      <strong>{element.label ?? element.value ?? "Unlabelled element"}</strong>
                      <small>{element.actions.join(" · ") || "read only"}</small>
                    </div>
                  ))}
                  {!observation?.elements.length && <span>No safe semantic elements exposed.</span>}
                </div>
              )}
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
              {activity && (
                <div
                  className={`computer-activity-state tone-${activity.tone}`}
                  data-activity={activity.id}
                >
                  <strong>{activity.label}</strong>
                  <span>{activity.detail}</span>
                </div>
              )}
              <dl>
                <div><dt>Target</dt><dd>{run.target.displayName}</dd></div>
                <div><dt>Backend</dt><dd>{snapshot.backend.backendId}</dd></div>
                <div><dt>Origin</dt><dd>{titleCase(snapshot.origin)}</dd></div>
                {projection && (
                  <div>
                    <dt>Control epoch</dt>
                    <dd>{projection.controlEpoch}</dd>
                  </div>
                )}
                <div><dt>Agent model</dt><dd>{model} · {effort}</dd></div>
                <div>
                  <dt>Agent access</dt>
                  <dd>
                    {effectiveAgentTier === "none"
                      ? "Manual only · not qualified"
                      : `${titleCase(effectiveAgentTier)} · ${titleCase(effectiveAgentSource)}`}
                  </dd>
                </div>
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
              <div className="computer-agent-proposal">
                <div>
                  <span className="computer-section-label">Agent objective</span>
                  <h2>Propose one action</h2>
                </div>
                <label>
                  Objective
                  <textarea
                    rows={2}
                    maxLength={4096}
                    value={objective}
                    onChange={(event) => setObjective(event.target.value)}
                  />
                </label>
                <div className="computer-agent-actions">
                  {effectiveAgentTier === "semantic_act" ||
                  effectiveAgentTier === "visual_fallback_act" ? (
                    <button
                      type="button"
                      className="primary"
                      disabled={busy || agentBusy || !objective.trim()}
                      onClick={() => void proposeAgentAction()}
                    >
                      {agentBusy ? "Waiting for model" : "Propose next action"}
                    </button>
                  ) : (
                    <button
                      type="button"
                      disabled={busy || agentBusy}
                      onClick={() => void qualifyAgent()}
                    >
                      {agentBusy ? "Checking model" : "Verify model for this session"}
                    </button>
                  )}
                </div>
              </div>
              <div><span className="computer-section-label">Manual action</span><h2>Stage for approval</h2></div>
              {nameElement && (
                <label>
                  Visible text for {nameElement.label ?? nameElement.role}
                  <input value={name} maxLength={256} onChange={(event) => setName(event.target.value)} />
                </label>
              )}
              <div className="computer-proposal-actions">
                {!simulatorRun && (
                  <button
                    ref={proposalFocus}
                    type="button"
                    disabled={busy}
                    onClick={() => void stage({ type: "activate_target" })}
                  >
                    Stage activation
                  </button>
                )}
                {nameElement && (
                  <button
                    ref={simulatorRun ? proposalFocus : undefined}
                    type="button"
                    disabled={busy || !name.trim() || !nameElement.enabled}
                    onClick={() =>
                      void stage({
                        type: "set_value",
                        element_id: nameElement.elementId,
                        text: name.trim(),
                      })
                    }
                  >
                    Stage text entry
                  </button>
                )}
                <button
                  type="button"
                  disabled={busy || !submitElement?.enabled}
                  title={!submitElement?.enabled ? "Enter and approve a name first" : undefined}
                  onClick={() =>
                    void stage({ type: "invoke", element_id: submitElement?.elementId ?? "" })
                  }
                >
                  Stage {submitElement?.label ?? "Invoke"}
                </button>
                {!simulatorRun && observation.elements.some((element) => element.actions.includes("select")) && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => {
                      const element = observation.elements.find((item) => item.actions.includes("select"));
                      if (element) void stage({ type: "select", element_id: element.elementId });
                    }}
                  >
                    Stage Select
                  </button>
                )}
                {!simulatorRun && observation.elements.some((element) => element.actions.includes("scroll")) && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => {
                      const element = observation.elements.find((item) => item.actions.includes("scroll"));
                      if (element) {
                        void stage({
                          type: "scroll",
                          element_id: element.elementId,
                          delta_x: 0,
                          delta_y: 480,
                        });
                      }
                    }}
                  >
                    Stage Scroll
                  </button>
                )}
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

          {run.state === "paused" && hasOperatorTakeover(run) && (
            <div className="computer-paused">
              <div>
                <span className="computer-section-label">Local operator control</span>
                <h2>Take over active</h2>
              </div>
              <p>
                This run cannot be reauthorized after takeover. Start a new Computer Run to return
                control to the agent.
              </p>
            </div>
          )}

          {run.state === "paused" && !hasOperatorTakeover(run) && (
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
