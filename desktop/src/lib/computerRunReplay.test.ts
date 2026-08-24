import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  advanceComputerRunReplay,
  computerRunReplayKey,
  loadComputerRunCursor,
  loadComputerRunReplay,
  saveComputerRunCursor,
} from "./computerRunReplay";

beforeEach(() => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
    clear: () => values.clear(),
    key: (index: number) => [...values.keys()][index] ?? null,
    get length() {
      return values.size;
    },
  });
});

describe("Computer Run replay cursors", () => {
  it("persists an exact session and Run binding across reloads", () => {
    saveComputerRunCursor("session-a", "run-a", 42, true);
    expect(loadComputerRunCursor("session-a", "run-a")).toBe(42);
    expect(loadComputerRunReplay("session-a", "run-a")).toEqual({
      cursor: 42,
      gapDetected: true,
    });
    expect(loadComputerRunCursor("session-a", "run-b")).toBeNull();
    expect(loadComputerRunCursor("session-b", "run-a")).toBeNull();
  });

  it("never clears a persisted history gap for the same Run", () => {
    saveComputerRunCursor("session-a", "run-a", 4, true);
    saveComputerRunCursor("session-a", "run-a", 7);
    expect(loadComputerRunReplay("session-a", "run-a")).toEqual({
      cursor: 7,
      gapDetected: true,
    });
  });

  it("rejects malformed and unsafe cursor values", () => {
    saveComputerRunCursor("session-a", "run-a", -1);
    saveComputerRunCursor("session-a", "run-a", Number.MAX_SAFE_INTEGER + 1);
    expect(loadComputerRunCursor("session-a", "run-a")).toBeNull();

    localStorage.setItem(
      "grokptah.computer-run-event-cursors.v1",
      JSON.stringify({
        [computerRunReplayKey("session-a", "run-a")]: {
          cursor: "42",
          updatedAt: Date.now(),
        },
      }),
    );
    expect(loadComputerRunCursor("session-a", "run-a")).toBeNull();
  });

  it("recovers from an expired cursor while keeping the gap explicit", () => {
    const expired = advanceComputerRunReplay("run-a", 2, undefined, {
      runId: "run-a",
      entries: [],
      nextCursor: null,
      cursorExpired: true,
      range: { startSeq: 9, endSeq: 11 },
    });
    expect(expired).toEqual({
      runId: "run-a",
      cursor: 8,
      gapDetected: true,
      replayedEntries: 0,
      lastEvent: null,
    });

    const retained = advanceComputerRunReplay("run-a", 8, expired, {
      runId: "run-a",
      entries: [
        {
          sequence: 9,
          at: "2026-08-23T00:00:00Z",
          surfaceEvent: "permission_revoked",
          operation: "pause",
          disposition: "ok",
        },
      ],
      nextCursor: null,
      cursorExpired: false,
      range: { startSeq: 9, endSeq: 9 },
    });
    expect(retained).toMatchObject({
      cursor: 9,
      gapDetected: true,
      replayedEntries: 1,
      lastEvent: "permission_revoked",
    });
  });

  it("marks an initially truncated or rolled-back journal as incomplete", () => {
    const initial = advanceComputerRunReplay("run-a", null, undefined, {
      runId: "run-a",
      entries: [
        {
          sequence: 9,
          at: "2026-08-23T00:00:00Z",
          surfaceEvent: "paused",
          operation: "pause",
          disposition: "paused",
        },
      ],
      nextCursor: null,
      cursorExpired: false,
      range: { startSeq: 9, endSeq: 9 },
    });
    expect(initial).toMatchObject({ cursor: 9, gapDetected: true });

    const rolledBack = advanceComputerRunReplay("run-a", 42, undefined, {
      runId: "run-a",
      entries: [],
      nextCursor: null,
      cursorExpired: false,
      range: { startSeq: 9, endSeq: 11 },
    });
    expect(rolledBack).toMatchObject({ cursor: 8, gapDetected: true });
  });

  it("rejects a replay page for another Run", () => {
    expect(() =>
      advanceComputerRunReplay("run-a", null, undefined, {
        runId: "run-b",
        entries: [],
        nextCursor: null,
        cursorExpired: false,
        range: null,
      }),
    ).toThrow(/different Run identity/);
  });
});
