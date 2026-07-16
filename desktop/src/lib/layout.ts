import { useEffect, useState, type RefObject } from "react";

/**
 * Ultrawide / multi-pane density ladder + dock capacity.
 *
 * **Docks** = visible session columns on the stage.
 * Capacity prefers **stage width** (after chrome collapse) when available;
 * falls back to viewport width.
 *
 * Tuned for ~480–520 CSS-px min column width so a 3840 ultrawide
 * (rails collapsed) can reach 6 zones without feeling crushed.
 */

export type LayoutDensity = "compact" | "normal" | "wide" | "ultrawide";

/** Absolute max session columns (power-user ceiling). */
export const MAX_DOCKS = 6;

/** Min CSS px for density "wide" (2-dock friendly window). */
export const SPLIT_MIN_WIDTH = 1180;
/** Min CSS px for density "ultrawide". */
export const ULTRAWIDE_MIN_WIDTH = 2000;

/**
 * Stage-width thresholds → max session docks (1–6).
 * Roughly ≥480px per additional column after the first.
 */
export const DOCK_STAGE_2 = 640;
export const DOCK_STAGE_3 = 1080;
export const DOCK_STAGE_4 = 1560;
export const DOCK_STAGE_5 = 2040;
export const DOCK_STAGE_6 = 2520;

export function densityFromWidth(w: number): LayoutDensity {
  if (w >= ULTRAWIDE_MIN_WIDTH) return "ultrawide";
  if (w >= SPLIT_MIN_WIDTH) return "wide";
  if (w >= 900) return "normal";
  return "compact";
}

/** Max docks from measured stage (main column) width. */
export function maxDocksFromStageWidth(stageWidth: number): number {
  if (stageWidth >= DOCK_STAGE_6) return 6;
  if (stageWidth >= DOCK_STAGE_5) return 5;
  if (stageWidth >= DOCK_STAGE_4) return 4;
  if (stageWidth >= DOCK_STAGE_3) return 3;
  if (stageWidth >= DOCK_STAGE_2) return 2;
  return 1;
}

/** Fallback when stage not measured: density ladder. */
export function maxDocksForDensity(d: LayoutDensity): number {
  switch (d) {
    case "ultrawide":
      return 5;
    case "wide":
      return 3;
    default:
      return 1;
  }
}

/** @deprecated use maxDocks — kept for call sites during migration */
export function canSplitSessions(d: LayoutDensity): boolean {
  return maxDocksForDensity(d) >= 2;
}

function readViewportWidth(): number {
  if (typeof window === "undefined") return 1280;
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

/**
 * Observe stage element width and return max docks (1–MAX_DOCKS).
 * Falls back to density-based max when ref not mounted / width 0.
 */
export function useMaxDocks(
  stageRef: RefObject<HTMLElement | null>,
  density: LayoutDensity,
): number {
  const [stageW, setStageW] = useState(0);

  useEffect(() => {
    const el = stageRef.current;
    if (!el || typeof ResizeObserver === "undefined") {
      setStageW(0);
      return;
    }
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width ?? 0;
      setStageW(Math.round(w));
    });
    ro.observe(el);
    setStageW(Math.round(el.getBoundingClientRect().width));
    return () => ro.disconnect();
  }, [stageRef]);

  if (stageW > 0) {
    return Math.min(MAX_DOCKS, maxDocksFromStageWidth(stageW));
  }
  return Math.min(MAX_DOCKS, maxDocksForDensity(density));
}

/**
 * Clamp dock list when capacity shrinks.
 * Keeps focused session; fills remaining slots left-to-right from previous order.
 */
export function clampDocks(
  docks: string[],
  max: number,
  focusedId: string | null,
): string[] {
  if (max < 1) return [];
  if (docks.length <= max) {
    // Deduplicate while preserving order
    const seen = new Set<string>();
    return docks.filter((id) => {
      if (seen.has(id)) return false;
      seen.add(id);
      return true;
    });
  }
  const unique: string[] = [];
  const seen = new Set<string>();
  for (const id of docks) {
    if (seen.has(id)) continue;
    seen.add(id);
    unique.push(id);
  }
  if (focusedId && unique.includes(focusedId)) {
    const rest = unique.filter((id) => id !== focusedId);
    return [focusedId, ...rest].slice(0, max);
  }
  return unique.slice(0, max);
}
