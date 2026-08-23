import type { LaneSummary, RuntimeConnectionState, RuntimeTarget } from "../lib/protocol";
import { StateCard } from "./StateCard";
import "./LaneContextHeader.css";

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

type LaneContextStateKey =
  | "archived"
  | "error"
  | "disconnected"
  | "reconnecting"
  | "running"
  | "ready";

/* The mark is a redundant non-color cue next to the state text, matching the
 * StateCard glyph vocabulary. */
function contextState(
  lane: LaneSummary | null,
  runLane: LaneSummary | null | undefined,
  runLive: boolean,
): { label: string; state: LaneContextStateKey; mark: string } {
  if (lane?.archived) return { label: "Archived", state: "archived", mark: "◌" };
  const connection = runLane?.runtime_connection ?? lane?.runtime_connection;
  if (connection === "error") return { label: "Unavailable", state: "error", mark: "!" };
  if (connection === "disconnected")
    return { label: "Disconnected", state: "disconnected", mark: "⌁" };
  if (connection === "reconnecting")
    return { label: "Reconnecting", state: "reconnecting", mark: "↻" };
  if (runLive) return { label: "Running", state: "running", mark: "◉" };
  return { label: "Ready", state: "ready", mark: "○" };
}

/**
 * Product-facing ownership header for the selected Lane.
 *
 * A remote execution target remains a distinct Run Lane; it is never rendered
 * as though the selected local Lane changed Runtime ownership. When present it
 * appears as a subordinate, explicitly "Remote" row beneath the Lane's own
 * facts, and the Lane's Runtime fact always reflects the selected Lane alone.
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
  const laneRuntime = `${runtimeTargetLabel(lane?.runtime_target)} · ${runtimeConnectionLabel(lane?.runtime_connection)}`;

  const remoteRunLane = runLane && runLane.id !== lane?.id ? runLane : null;
  const runLaneName = remoteRunLane
    ? remoteRunLane.title || workspaceDisplayName(remoteRunLane.cwd)
    : "";
  const runLaneRuntime = remoteRunLane
    ? `${runtimeTargetLabel(remoteRunLane.runtime_target)} · ${runtimeConnectionLabel(remoteRunLane.runtime_connection)}`
    : "";
  const runLaneEvidence = remoteRunLane
    ? [runLaneName, runLaneRuntime, remoteRunLane.cwd].filter(Boolean).join(" · ")
    : undefined;

  return (
    <section
      className="lane-context-header"
      aria-labelledby={headingId}
      data-lane-id={lane?.id ?? undefined}
      data-run-lane-id={runLane?.id ?? lane?.id ?? undefined}
      data-state={state.state}
    >
      <div className="lane-context-heading-row">
        <div className="lane-context-heading">
          <span className="lane-context-eyebrow">Selected Lane</span>
          <h1 id={headingId} title={title}>
            {title}
          </h1>
        </div>
        <span
          className={`lane-context-state is-${state.state}`}
          data-state={state.state}
          role="status"
        >
          <span className="lane-context-state-mark" aria-hidden>
            {state.mark}
          </span>
          {/* "Work status", not "Lane status": with a remote Run Lane the pill
              reflects the effective work connection, while the Runtime fact
              below stays the selected Lane's own. */}
          <span className="lane-context-sr-only">Work status: </span>
          {state.label}
        </span>
      </div>

      <dl className="lane-context-facts">
        <div className="lane-context-fact">
          <dt>Agent</dt>
          <dd title={agentId || undefined}>
            <span>{agentId ? agentId : "Ad hoc"}</span>
          </dd>
        </div>
        <div className="lane-context-fact">
          <dt>Runtime</dt>
          <dd title={laneRuntime}>
            <span>
              {runtimeTargetLabel(lane?.runtime_target)}
              <span aria-hidden> · </span>
              {runtimeConnectionLabel(lane?.runtime_connection)}
            </span>
          </dd>
        </div>
        <div className="lane-context-fact lane-context-workspace">
          <dt>Workspace</dt>
          <dd title={workspace || undefined}>
            <span>{workspaceDisplayName(workspace)}</span>
            {onChangeWorkspace && !isArchived && (
              <button type="button" aria-label="Change workspace" onClick={onChangeWorkspace}>
                Change
              </button>
            )}
          </dd>
        </div>
        <div className="lane-context-fact">
          <dt>Run</dt>
          <dd title={runLabel || undefined}>
            <span>{runLabel}</span>
          </dd>
        </div>
        {remoteRunLane && (
          <div
            className="lane-context-fact lane-context-run-lane"
            data-run-lane-target={remoteRunLane.runtime_target ?? "local_desktop"}
            data-run-lane-connection={remoteRunLane.runtime_connection ?? "connected"}
          >
            <dt>
              Run Lane
              <span
                className="lane-context-remote-flag"
                title="Remote execution target — selected Lane ownership is unchanged"
              >
                Remote
              </span>
              {/* The title tooltip is mouse-only; keyboard and screen-reader
                  users need the ownership boundary in text. */}
              <span className="lane-context-sr-only">
                Remote execution target; selected Lane ownership is unchanged.
              </span>
            </dt>
            <dd title={runLaneEvidence}>
              <span>
                {runLaneName}
                <span aria-hidden> · </span>
                {runtimeTargetLabel(remoteRunLane.runtime_target)}
                <span aria-hidden> · </span>
                {runtimeConnectionLabel(remoteRunLane.runtime_connection)}
              </span>
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
