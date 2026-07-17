import { useEffect, useState } from "react";
import djedUrl from "../assets/grokptah-djed.svg";

export type LaunchSplashProps = {
  /** True when workspace + agent chrome are ready to hand off. */
  ready: boolean;
  onDone?: () => void;
};

/**
 * #150 — Djed-direction launch splash (amber on near-black).
 * Dismisses when `ready` (no fake fixed delay). Honors prefers-reduced-motion.
 */
export function LaunchSplash({ ready, onDone }: LaunchSplashProps) {
  const [visible, setVisible] = useState(true);
  const [exiting, setExiting] = useState(false);
  const reduced =
    typeof window !== "undefined" &&
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

  useEffect(() => {
    if (!ready || !visible) return;
    if (reduced) {
      setVisible(false);
      onDone?.();
      return;
    }
    setExiting(true);
    const t = window.setTimeout(() => {
      setVisible(false);
      onDone?.();
    }, 420);
    return () => window.clearTimeout(t);
  }, [ready, visible, reduced, onDone]);

  if (!visible) return null;

  return (
    <div
      className={`launch-splash ${exiting ? "is-exiting" : ""} ${reduced ? "is-static" : ""}`}
      data-testid="launch-splash"
      role="presentation"
      aria-hidden={exiting}
    >
      <div className="launch-splash-inner">
        <img
          src={djedUrl}
          alt=""
          width={96}
          height={96}
          className="launch-splash-mark"
          draggable={false}
        />
        <div className="launch-splash-word">GrokPtah</div>
        <div className="launch-splash-tag">build, don’t chat</div>
        <div className="launch-splash-beam" aria-hidden />
      </div>
    </div>
  );
}
