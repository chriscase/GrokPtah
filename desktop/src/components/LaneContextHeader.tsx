import type { LaneSummary, RuntimeConnectionState, RuntimeTarget } from "../lib/protocol";
import { StateCard } from "./StateCard";

export type LaneContextHeaderProps = {
  lane: LaneSummary | null;
  fallbackTitle: string;
  agentId?: string | null;
  runLane?: LaneSummary | null;
  runLabel: string;
  runLive: boolean;
  scopeError?: string | null;
  onChangeWorkspace?: () => void;
  onRestore?: () => void;
};

export function runtimeTargetLabel(value?: RuntimeTarget): string {
  switch (value) {
    case "local_service":
      return "Local service / VM";
    case "hosted_service":
      return "Hosted service";
    default:
      return "Local desktop";
  }
}

export function runtimeConnectionLabel(value?: RuntimeConnectionState): string {
  switch (value) {
    case "reconnecting":
      return "Reconnecting";
    case "disconnected":
      return "Disconnected";
    case "error":
      return "Unavailable";
    default:
      return "Connected";
  }
}

export function workspaceDisplayName(path?: string | null): string {
  if (!path) return "No workspace selected";
  const parts = path.split(/[/\\]/).filter(Boolean);
  return parts.slice(-3).join(" / ") || path;
}

function contextState(
  lane: LaneSummary | null,
  runLane: LaneSummary | null | undefined,
  runLive: boolean,
): { label: string; state: string } {
  if (lane?.archived) return { label: "Archived", state: "archived" };
  const connection = runLane?.runtime_connection ?? lane?.runtime_connection;
  if (connection === "error") return { label: "Unavailable", state: "error" };
  if (connection === "disconnected") return { label: "Disconnected", state: "disconnected" };
  if (connection === "reconnecting") return { label: "Reconnecting", state: "reconnecting" };
  if (runLive) return { label: "Running", state: "running" };
  return { label: "Ready", state: "ready" };
}

/**
 * Product-facing ownership header for the selected Lane.
 *
 * A remote execution target remains a distinct Run Lane; it is never rendered
 * as though the selected local Lane changed Runtime ownership.
 */
export function LaneContextHeader({
  lane,
  fallbackTitle,
  agentId,
  runLane,
  runLabel,
  runLive,
  scopeError,
  onChangeWorkspace,
  onRestore,
}: LaneContextHeaderProps) {
  const title = lane?.title || fallbackTitle || "Current work";
  const state = contextState(lane, runLane, runLive);
  const workspace = lane?.cwd;
  const isArchived = Boolean(lane?.archived);
  const headingId = `lane-context-${lane?.id ?? "current"}`;

  return (
    <section
      className="lane-context-header"
      aria-labelledby={headingId}
      data-lane-id={lane?.id ?? undefined}
      data-run-lane-id={runLane?.id ?? lane?.id ?? undefined}
    >
      <div className="lane-context-heading-row">
        <div className="lane-context-heading">
          <span className="lane-context-eyebrow">Current Lane</span>
          <h1 id={headingId}>{title}</h1>
        </div>
        <span className={`lane-context-state is-${state.state}`}>
          {state.label}
        </span>
      </div>

      <dl className="lane-context-facts">
        <div>
          <dt>Agent</dt>
          <dd>{agentId ? agentId : "Ad hoc"}</dd>
        </div>
        <div>
          <dt>Runtime</dt>
          <dd>
            {runtimeTargetLabel(lane?.runtime_target)}
            <span aria-hidden> · </span>
            {runtimeConnectionLabel(lane?.runtime_connection)}
          </dd>
        </div>
        <div className="lane-context-workspace">
          <dt>Workspace</dt>
          <dd title={workspace || undefined}>
            <span>{workspaceDisplayName(workspace)}</span>
            {onChangeWorkspace && !isArchived && (
              <button type="button" onClick={onChangeWorkspace}>
                Change
              </button>
            )}
          </dd>
        </div>
        <div>
          <dt>Run</dt>
          <dd>{runLabel}</dd>
        </div>
        {runLane && runLane.id !== lane?.id && (
          <div className="lane-context-run-lane">
            <dt>Run Lane</dt>
            <dd title={runLane.cwd || undefined}>
              {runLane.title || workspaceDisplayName(runLane.cwd)}
              <span aria-hidden> · </span>
              {runtimeTargetLabel(runLane.runtime_target)}
              <span aria-hidden> · </span>
              {runtimeConnectionLabel(runLane.runtime_connection)}
            </dd>
          </div>
        )}
      </dl>

      {isArchived && (
        <StateCard
          variant="archived"
          title="Archived Lane — inspection only"
          description="Transcript, Runs, evidence, and workspace details remain available. Restore this Lane before starting or steering work."
          actionLabel={onRestore ? "Restore Lane" : undefined}
          onAction={onRestore}
        />
      )}
      {!isArchived && scopeError && (
        <StateCard
          variant="blocked"
          title="Lane workspace unavailable"
          description={scopeError}
        />
      )}
    </section>
  );
}
