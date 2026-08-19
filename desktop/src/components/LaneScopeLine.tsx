import type { RuntimeConnectionState, RuntimeTarget } from "../lib/protocol";
import {
  runtimeConnectionLabel,
  runtimeTargetLabel,
  workspaceDisplayName,
} from "./LaneContextHeader";

/** The minimum ownership context a Lane-scoped surface must keep visible. */
export type LaneScope = {
  laneId?: string | null;
  laneTitle?: string | null;
  agentLabel?: string | null;
  runtimeTarget?: RuntimeTarget | null;
  runtimeConnection?: RuntimeConnectionState | null;
  workspacePath?: string | null;
  runLabel?: string | null;
};

export type LaneScopeLineProps = {
  scope: LaneScope;
  className?: string;
  compact?: boolean;
};

function scopeValue(value: string | null | undefined, fallback: string): string {
  return value?.trim() || fallback;
}

/**
 * Compact, reusable ownership line for panels and modals that act on a Lane.
 *
 * The full LaneContextHeader remains the primary workspace header. This line
 * keeps secondary surfaces from silently inheriting scope from tab focus.
 */
export function LaneScopeLine({
  scope,
  className = "",
  compact = false,
}: LaneScopeLineProps) {
  const lane = scopeValue(scope.laneTitle, scope.laneId ? "Selected Lane" : "Current Lane");
  const agent = scopeValue(scope.agentLabel, "Ad hoc");
  const runtime = `${runtimeTargetLabel(scope.runtimeTarget ?? undefined)} · ${runtimeConnectionLabel(scope.runtimeConnection ?? undefined)}`;
  const workspace = workspaceDisplayName(scope.workspacePath);
  const run = scopeValue(scope.runLabel, "No active Run");

  return (
    <div
      className={`lane-scope-line ${compact ? "is-compact" : ""} ${className}`.trim()}
      role="group"
      aria-label="Lane scope"
      data-lane-id={scope.laneId ?? undefined}
    >
      <span className="lane-scope-item">
        <strong>Lane</strong>{" "}
        <span title={scope.laneTitle ?? scope.laneId ?? undefined}>{lane}</span>
      </span>
      <span className="lane-scope-item">
        <strong>Agent</strong>{" "}
        <span>{agent}</span>
      </span>
      <span className="lane-scope-item">
        <strong>Runtime</strong>{" "}
        <span>{runtime}</span>
      </span>
      <span className="lane-scope-item">
        <strong>Workspace</strong>{" "}
        <span title={scope.workspacePath ?? undefined}>{workspace}</span>
      </span>
      <span className="lane-scope-item">
        <strong>Run</strong>{" "}
        <span>{run}</span>
      </span>
    </div>
  );
}
