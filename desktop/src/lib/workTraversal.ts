import type { DurableWorkItem, WorkTraversalStatus } from "./protocol";

/** Map a page-read failure onto the status the Work Board can announce. */
export function workTraversalFromError(error: unknown): WorkTraversalStatus {
  const text = String(error);
  if (text.includes("restart_required") || text.includes("cursor_expired")) return "expired";
  return "error";
}

/**
 * Append a page without hiding a revision change.
 * The key is work id plus revision. A newer revision stays visible.
 */
export function mergeWorkPages(
  current: DurableWorkItem[],
  incoming: DurableWorkItem[],
): DurableWorkItem[] {
  const seen = new Set(current.map((item) => `${item.workId}:${item.revision}`));
  const next = [...current];
  for (const item of incoming) {
    const key = `${item.workId}:${item.revision}`;
    if (seen.has(key)) continue;
    seen.add(key);
    next.push(item);
  }
  return next;
}

/**
 * Accept a page only when it belongs to the fetch that is still current.
 * A newer refresh, a lane switch, or a disposed request returns null.
 * Append merges. A refresh replaces. Neither path invents a lane total.
 */
export function acceptWorkPage(
  currentGeneration: number,
  generation: number,
  current: DurableWorkItem[],
  incoming: DurableWorkItem[],
  append: boolean,
): DurableWorkItem[] | null {
  if (currentGeneration !== generation) return null;
  return append ? mergeWorkPages(current, incoming) : incoming;
}
