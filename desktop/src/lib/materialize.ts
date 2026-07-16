/**
 * Pace large stream dumps so text *appears* to materialize (Gemini-like),
 * even when the backend sends multi-hundred-char chunks in one event.
 */

/** Split into words keeping trailing whitespace on each token. */
export function tokenizeForMaterialize(s: string): string[] {
  if (!s) return [];
  return s.match(/\S+\s*|\s+/g) ?? [s];
}

/**
 * How many queued tokens to reveal per tick based on backlog.
 * Stay readable when queue is short; catch up when huge.
 */
export function materializeBatchSize(queueLen: number): number {
  if (queueLen > 80) return 10;
  if (queueLen > 40) return 5;
  if (queueLen > 18) return 3;
  if (queueLen > 8) return 2;
  return 1;
}

export const MATERIALIZE_TICK_MS = 22;
