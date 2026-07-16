import { useEffect, useState } from "react";
import type { ActivityState } from "../lib/activity";
import { stalledDetail } from "../lib/activity";

/**
 * Always-visible turn status: live pulse while the server is working,
 * clear idle / done when the turn ends.
 */
export function ActivityIndicator({
  activity,
  busy,
}: {
  activity: ActivityState;
  /** Tab busy flag — keeps strip in live mode even if phase lags */
  busy: boolean;
}) {
  const [now, setNow] = useState(Date.now());

  // Tick while live so “still waiting” / elapsed stay accurate.
  useEffect(() => {
    if (!activity.live && !busy) return;
    const id = window.setInterval(() => setNow(Date.now()), 400);
    return () => window.clearInterval(id);
  }, [activity.live, busy]);

  // Brief “done” settles back to quiet ready after a few seconds.
  const [settledDone, setSettledDone] = useState(false);
  useEffect(() => {
    if (activity.phase !== "done") {
      setSettledDone(false);
      return;
    }
    const t = window.setTimeout(() => setSettledDone(true), 3200);
    return () => window.clearTimeout(t);
  }, [activity.phase, activity.lastEventAt]);

  const stalled = stalledDetail(activity, now);
  const live = activity.live || busy;
  const phase =
    live && activity.phase === "idle"
      ? "queued"
      : settledDone && activity.phase === "done"
        ? "idle"
        : activity.phase;

  const displayLabel =
    phase === "idle"
      ? "Ready"
      : phase === "done"
        ? activity.label
        : stalled && phase === "queued"
          ? "Waiting on server"
          : activity.label;

  const displayDetail =
    stalled && live
      ? stalled
      : phase === "idle"
        ? "No active turn"
        : activity.detail;

  const sinceEvent =
    live && now - activity.lastEventAt >= 800
      ? `${Math.floor((now - activity.lastEventAt) / 1000)}s since last event`
      : null;

  return (
    <div
      className={`activity-strip phase-${phase} ${live ? "live" : "idle"}`}
      role="status"
      aria-live="polite"
      aria-busy={live}
      data-testid="activity-indicator"
    >
      <div className="activity-left">
        <span className="activity-glyph" aria-hidden>
          {live ? (
            <span className="activity-orbit">
              <span className="activity-orbit-core" />
            </span>
          ) : phase === "done" ? (
            <span className="activity-check">✓</span>
          ) : phase === "error" ? (
            <span className="activity-err">!</span>
          ) : (
            <span className="activity-idle-dot" />
          )}
        </span>
        <div className="activity-copy">
          <span className="activity-label">{displayLabel}</span>
          {displayDetail && (
            <span className="activity-detail" title={displayDetail}>
              {displayDetail}
            </span>
          )}
        </div>
      </div>
      <div className="activity-right">
        {live && (
          <span className="activity-pulse-bars" aria-hidden>
            <i />
            <i />
            <i />
            <i />
          </span>
        )}
        {sinceEvent && live && (
          <span className="activity-elapsed">{sinceEvent}</span>
        )}
        {!live && phase === "idle" && (
          <span className="activity-ready-tag">idle</span>
        )}
        {!live && phase === "done" && !settledDone && (
          <span className="activity-ready-tag done">complete</span>
        )}
      </div>
      {live && <div className="activity-shimmer" aria-hidden />}
    </div>
  );
}
