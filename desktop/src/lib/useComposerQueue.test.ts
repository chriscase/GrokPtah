import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { createPromptQueueEntry } from "./promptQueue";
import { useComposerQueue } from "./useComposerQueue";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("useComposerQueue request ordering", () => {
  it("ignores a stale hydration response after a newer request", async () => {
    const { result } = renderHook(() => useComposerQueue("session-a"));
    const first = deferred<ReturnType<typeof createPromptQueueEntry>[]>();
    const second = deferred<ReturnType<typeof createPromptQueueEntry>[]>();
    const oldEntry = createPromptQueueEntry("old", { id: "old" });
    const currentEntry = createPromptQueueEntry("current", { id: "current" });
    let firstRun!: Promise<unknown>;
    let secondRun!: Promise<unknown>;

    act(() => {
      firstRun = result.current.syncQueue("session-a", () => first.promise);
      secondRun = result.current.syncQueue("session-a", () => second.promise);
    });

    await act(async () => {
      second.resolve([currentEntry]);
      await secondRun;
    });
    expect(result.current.queueFor("session-a")).toEqual([currentEntry]);

    await act(async () => {
      first.resolve([oldEntry]);
      await firstRun;
    });
    expect(result.current.queueFor("session-a")).toEqual([currentEntry]);
  });
});
