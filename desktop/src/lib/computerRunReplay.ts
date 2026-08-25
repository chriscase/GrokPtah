import type {
  ComputerRunEventPage,
  ComputerRunReplayStatus,
} from "./protocol";

const STORAGE_KEY = "grokptah.computer-run-event-cursors.v1";
const MAX_STORED_CURSORS = 128;

export type StoredComputerRunReplay = {
  cursor: number;
  gapDetected: boolean;
  updatedAt: number;
};

type StoredCursors = Record<string, StoredComputerRunReplay>;

function storage(): Storage | null {
  try {
    if (typeof localStorage === "undefined") return null;
    return localStorage;
  } catch {
    return null;
  }
}

function readAll(): StoredCursors {
  try {
    const raw = storage()?.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as StoredCursors;
    return Object.fromEntries(
      Object.entries(parsed)
        .filter(
          ([key, value]) =>
            key.length <= 600 &&
            Number.isSafeInteger(value?.cursor) &&
            value.cursor >= 0 &&
            (value.gapDetected === undefined ||
              typeof value.gapDetected === "boolean") &&
            Number.isFinite(value.updatedAt),
        )
        .map(([key, value]) => [
          key,
          { ...value, gapDetected: value.gapDetected === true },
        ]),
    );
  } catch {
    return {};
  }
}

export function computerRunReplayKey(sessionId: string, runId: string) {
  return `${sessionId}:${runId}`;
}

export function loadComputerRunCursor(sessionId: string, runId: string): number | null {
  return readAll()[computerRunReplayKey(sessionId, runId)]?.cursor ?? null;
}

export function loadComputerRunReplay(
  sessionId: string,
  runId: string,
): Pick<StoredComputerRunReplay, "cursor" | "gapDetected"> | null {
  const replay = readAll()[computerRunReplayKey(sessionId, runId)];
  return replay
    ? { cursor: replay.cursor, gapDetected: replay.gapDetected }
    : null;
}

export function saveComputerRunCursor(
  sessionId: string,
  runId: string,
  cursor: number,
  gapDetected = false,
) {
  if (!Number.isSafeInteger(cursor) || cursor < 0) return;
  const target = storage();
  if (!target) return;
  const current = readAll();
  const key = computerRunReplayKey(sessionId, runId);
  const entries = Object.entries({
    ...current,
    [key]: {
      cursor,
      gapDetected: gapDetected || current[key]?.gapDetected === true,
      updatedAt: Date.now(),
    },
  })
    .sort(([, left], [, right]) => right.updatedAt - left.updatedAt)
    .slice(0, MAX_STORED_CURSORS);
  try {
    target.setItem(STORAGE_KEY, JSON.stringify(Object.fromEntries(entries)));
  } catch {
    // Cursor persistence is recovery evidence, never a reason to break Stop.
  }
}

/**
 * Advance one exact Run replay without ever healing a known gap. An expired
 * cursor is moved to immediately before the retained window; the next poll can
 * then consume that tail while the sticky gap remains visible.
 */
export function advanceComputerRunReplay(
  runId: string,
  requestedCursor: number | null,
  previous: ComputerRunReplayStatus | undefined,
  page: ComputerRunEventPage,
): ComputerRunReplayStatus {
  if (page.runId !== runId) {
    throw new Error("Computer Run replay returned a different Run identity");
  }
  const prior = previous?.runId === runId ? previous : undefined;
  const initialWindowGap =
    requestedCursor === null && Boolean(page.range && page.range.startSeq > 1);
  const cursorAhead = Boolean(
    requestedCursor !== null &&
      page.range &&
      requestedCursor > page.range.endSeq,
  );
  const gapDetected =
    prior?.gapDetected === true ||
    page.cursorExpired ||
    initialWindowGap ||
    cursorAhead;
  const lastEntry = page.entries.at(-1);
  const recoveryCursor = (page.cursorExpired || cursorAhead) && page.range
    ? Math.max(0, page.range.startSeq - 1)
    : null;
  const cursor = lastEntry?.sequence ?? recoveryCursor ?? requestedCursor;

  return {
    runId,
    cursor,
    gapDetected,
    replayedEntries: (prior?.replayedEntries ?? 0) + page.entries.length,
    lastEvent: lastEntry?.surfaceEvent ?? prior?.lastEvent ?? null,
  };
}
