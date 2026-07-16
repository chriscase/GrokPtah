import type { SessionTab } from "../lib/protocol";

export type FleetStripProps = {
  tabs: SessionTab[];
  activeSessionId: string | null;
  sideSessionId: string | null;
  onFocus: (id: string) => void;
  onOpenBeside: (id: string) => void;
  canSplit: boolean;
};

/**
 * Ultrawide glance rail: live / attention sessions at a glance.
 */
export function FleetStrip({
  tabs,
  activeSessionId,
  sideSessionId,
  onFocus,
  onOpenBeside,
  canSplit,
}: FleetStripProps) {
  if (tabs.length === 0) return null;

  const ranked = [...tabs].sort((a, b) => {
    const score = (t: SessionTab) =>
      (t.needsPermission ? 100 : 0) +
      (t.busy ? 50 : 0) +
      (t.unseen ? 20 : 0);
    return score(b) - score(a);
  });

  return (
    <div className="fleet-strip" role="region" aria-label="Session fleet">
      <div className="fleet-strip-label">Fleet</div>
      <div className="fleet-cards">
        {ranked.map((t) => {
          const isPrimary = t.id === activeSessionId;
          const isSide = t.id === sideSessionId;
          return (
            <button
              key={t.id}
              type="button"
              className={`fleet-card ${t.busy ? "busy" : ""} ${t.needsPermission ? "permission" : ""} ${isPrimary ? "primary" : ""} ${isSide ? "side" : ""}`}
              title={
                canSplit
                  ? `${t.title}\nClick: focus · Alt-click: open beside`
                  : t.title
              }
              onClick={(e) => {
                if (e.altKey && canSplit) {
                  onOpenBeside(t.id);
                } else {
                  onFocus(t.id);
                }
              }}
              onDoubleClick={() => {
                if (canSplit) onOpenBeside(t.id);
              }}
            >
              <span className="fleet-card-status">
                {t.needsPermission
                  ? "!"
                  : t.busy
                    ? "●"
                    : t.unseen
                      ? "○"
                      : "·"}
              </span>
              <span className="fleet-card-title">{t.title}</span>
              <span className="fleet-card-phase">
                {t.needsPermission
                  ? "needs you"
                  : t.busy
                    ? [
                        t.agentRound != null ? `r${t.agentRound}` : null,
                        t.lastTool || t.activity.detail || t.activity.label,
                      ]
                        .filter(Boolean)
                        .join(" · ")
                    : t.unseen
                      ? "unseen"
                      : "idle"}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
