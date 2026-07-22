import { memo } from "react";
import type { SessionTab } from "../lib/protocol";

export type FleetStripProps = {
  tabs: SessionTab[];
  activeSessionId: string | null;
  /** Session ids currently docked as stage columns */
  zoneIds: string[];
  canSplit: boolean;
  onFocus: (id: string) => void;
  onOpenBeside: (id: string) => void;
  onHide?: () => void;
};

function phaseLabel(t: SessionTab): string {
  if (t.needsPermission) return "needs you";
  const fleetBits: string[] = [];
  if (t.runningSubagents && t.runningSubagents > 0) {
    fleetBits.push(
      t.runningSubagents === 1
        ? "1 subagent"
        : `${t.runningSubagents} subagents`,
    );
  }
  if (t.totalTokens && t.totalTokens > 0) {
    fleetBits.push(`${t.totalTokens} tok`);
  }
  if (t.busy) {
    const bits = [
      t.agentRound != null ? `r${t.agentRound}` : null,
      t.lastTool || t.activity.detail || t.activity.label || "working",
      ...fleetBits,
    ].filter(Boolean);
    return bits.join(" · ");
  }
  if (fleetBits.length) {
    return fleetBits.join(" · ");
  }
  if (t.unseen) return "unseen";
  return "idle";
}

/**
 * Live attention rail — open-session switcher.
 * Order matches the tab strip (stable; never reorders on focus click).
 * Cards size to the title, not the full ultrawide width.
 */
export const FleetStrip = memo(function FleetStrip({
  tabs,
  activeSessionId,
  zoneIds,
  canSplit,
  onFocus,
  onOpenBeside,
  onHide,
}: FleetStripProps) {
  if (tabs.length === 0) return null;

  return (
    <div className="fleet-strip" role="region" aria-label="Live sessions">
      <div
        className="fleet-strip-label"
        title="Open sessions — click to focus, Alt-click to dock"
      >
        Live
        <span className="fleet-strip-count">{tabs.length}</span>
      </div>
      <div className="fleet-cards">
        {tabs.map((t) => {
          const isPrimary = t.id === activeSessionId;
          const zoneIndex = zoneIds.indexOf(t.id);
          const inZone = zoneIndex >= 0;
          const status = t.needsPermission
            ? "permission"
            : t.busy
              ? "busy"
              : t.unseen
                ? "unseen"
                : "idle";
          return (
            <button
              key={t.id}
              type="button"
              className={`fleet-card status-${status} ${isPrimary ? "primary" : ""} ${inZone ? "in-zone" : ""}`}
              data-testid="fleet-card"
              data-session-id={t.id}
              data-running-subagents={t.runningSubagents ?? 0}
              data-total-tokens={t.totalTokens ?? 0}
              aria-pressed={isPrimary}
              aria-current={isPrimary ? "true" : undefined}
              aria-label={`${t.title}: ${phaseLabel(t)}${
                t.runningSubagents
                  ? `, ${t.runningSubagents} subagent${t.runningSubagents === 1 ? "" : "s"}`
                  : ""
              }${t.totalTokens ? `, ${t.totalTokens} tokens` : ""}`}
              title={
                canSplit
                  ? `${t.title}\nClick: focus · Alt-click / double-click: dock beside`
                  : `${t.title}\nClick: focus`
              }
              onClick={(e) => {
                if (e.altKey && canSplit) {
                  onOpenBeside(t.id);
                } else {
                  onFocus(t.id);
                }
              }}
              onDoubleClick={(e) => {
                e.preventDefault();
                if (canSplit) onOpenBeside(t.id);
              }}
            >
              <span className="fleet-card-rail" aria-hidden />
              <span className="fleet-card-top">
                <span className="fleet-card-status" aria-hidden>
                  {t.needsPermission
                    ? "!"
                    : t.busy
                      ? "●"
                      : t.unseen
                        ? "○"
                        : "·"}
                </span>
                <span className="fleet-card-title">{t.title}</span>
                {inZone ? (
                  <span className="fleet-card-zone" title={`Zone ${zoneIndex + 1}`}>
                    Z{zoneIndex + 1}
                  </span>
                ) : canSplit ? (
                  <span className="fleet-card-zone is-ghost" title="Not docked">
                    —
                  </span>
                ) : null}
              </span>
              <span className="fleet-card-phase">{phaseLabel(t)}</span>
            </button>
          );
        })}
      </div>
      {onHide && (
        <button
          type="button"
          className="fleet-strip-hide"
          title="Hide Live rail (⌘⇧L)"
          onClick={onHide}
        >
          ×
        </button>
      )}
    </div>
  );
});
