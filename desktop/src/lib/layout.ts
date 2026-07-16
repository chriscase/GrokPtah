import { useEffect, useState } from "react";

/** Ultrawide / multi-pane density ladder. */
export type LayoutDensity = "compact" | "normal" | "wide" | "ultrawide";

export function densityFromWidth(w: number): LayoutDensity {
  if (w >= 2560) return "ultrawide";
  if (w >= 1440) return "wide";
  if (w >= 900) return "normal";
  return "compact";
}

/** True when the shell can host a second session pane. */
export function canSplitSessions(d: LayoutDensity): boolean {
  return d === "wide" || d === "ultrawide";
}

export function useLayoutDensity(): LayoutDensity {
  const [density, setDensity] = useState<LayoutDensity>(() =>
    typeof window !== "undefined"
      ? densityFromWidth(window.innerWidth)
      : "normal",
  );

  useEffect(() => {
    const onResize = () => setDensity(densityFromWidth(window.innerWidth));
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  return density;
}
