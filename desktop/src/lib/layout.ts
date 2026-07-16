import { useEffect, useState } from "react";

/**
 * Ultrawide / multi-pane density ladder.
 *
 * Thresholds use CSS viewport width (`innerWidth`). Default Tauri window is
 * ~1600px so split is available without maximize; 1180 still works on a
 * modestly resized laptop window (sidebar + main + optional right rail).
 */
export type LayoutDensity = "compact" | "normal" | "wide" | "ultrawide";

/** Min CSS px for two-pane session split. */
export const SPLIT_MIN_WIDTH = 1180;
/** Min CSS px for ultrawide density (fleet + room for future 3-pane). */
export const ULTRAWIDE_MIN_WIDTH = 2000;

export function densityFromWidth(w: number): LayoutDensity {
  if (w >= ULTRAWIDE_MIN_WIDTH) return "ultrawide";
  if (w >= SPLIT_MIN_WIDTH) return "wide";
  if (w >= 900) return "normal";
  return "compact";
}

/** True when the shell can host a second session pane. */
export function canSplitSessions(d: LayoutDensity): boolean {
  return d === "wide" || d === "ultrawide";
}

function readViewportWidth(): number {
  if (typeof window === "undefined") return 1280;
  // Prefer visualViewport when present (handles some zoom/chrome cases).
  const vv = window.visualViewport?.width;
  if (typeof vv === "number" && vv > 0) return Math.round(vv);
  return window.innerWidth || document.documentElement.clientWidth || 1280;
}

export function useLayoutDensity(): LayoutDensity {
  const [density, setDensity] = useState<LayoutDensity>(() =>
    densityFromWidth(readViewportWidth()),
  );

  useEffect(() => {
    const onResize = () => setDensity(densityFromWidth(readViewportWidth()));
    onResize();
    window.addEventListener("resize", onResize);
    window.visualViewport?.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      window.visualViewport?.removeEventListener("resize", onResize);
    };
  }, []);

  return density;
}
