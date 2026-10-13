import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RemoteWorkSnapshot, WorkListEntry } from "./protocol";
import { WORK_BOARD_POLL_MS, type WorkBoardPage, type WorkBoardScope } from "./workBoardController";
import { useWorkBoardPoll } from "./useWorkBoardPoll";

function scope(overrides: Partial<WorkBoardScope> = {}): WorkBoardScope {
  return {
    sessionId: "lane-a",
    workspace: "/fixture",
    remote: false,
    remoteConnected: false,
    remoteLanePresent: false,
    ...overrides,
  };
}

function entry(workId: string): WorkListEntry {
  return {
    workId,
    kind: "test",
    state: "queued",
    priority: 0,
    revision: 1,
    attemptCount: 0,
    createdAt: "2026-10-11T00:00:00Z",
    updatedAt: "2026-10-11T00:00:00Z",
  };
}

function page(
  ids: string[],
  complete: boolean,
  continuation: string | null,
  fence?: string | null,
): WorkBoardPage {
  return { items: ids.map(entry), complete, continuation, fence: fence ?? null };
}

function snapshotFor(workId: string): RemoteWorkSnapshot {
  return {
    work: {
      schemaVersion: 1,
      workId,
      kind: "test",
      objective: workId,
      sessionId: "lane-a",
      workspace: "/fixture",
      createdBy: "",
      assignedAgentId: null,
      priority: 0,
      deadline: null,
      parentWorkId: null,
      dependencies: [],
      policy: {
        bounds: { maxPromptBytes: 0, maxRounds: 0, maxDurationMs: 0 },
        retry: { maxAttempts: 1, retryFailed: false, retryExpired: false, backoffMs: 0 },
        requiresApproval: false,
        maxConcurrentAttempts: 1,
      },
      state: "queued",
      revision: 1,
      attemptCount: 0,
      progress: null,
      result: null,
      approval: null,
      createdAt: "2026-10-11T00:00:00Z",
      updatedAt: "2026-10-11T00:00:00Z",
    },
    attempts: [],
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("production work board poll", () => {
  it("keeps loaded pages and a beyond-page-one selection across several ticks", async () => {
    const calls: Array<string | null> = [];
    const list = vi.fn(async (_scope: WorkBoardScope, cursor: string | null) => {
      calls.push(cursor);
      if (cursor == null) return page(["first", "page-one"], false, "cursor-1");
      if (cursor === "cursor-1") return page(["beyond-page-one"], false, "cursor-2");
      return page(["third"], true, null);
    });
    const routines = vi.fn();
    const { result } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
        onRoutines: routines,
      }),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first", "page-one"]);
    expect(result.current.view.traversal).toBe("partial");

    await act(async () => {
      await result.current.loadMore();
    });
    await act(async () => {
      await result.current.loadMore();
    });
    act(() => {
      result.current.select("beyond-page-one");
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual([
      "first",
      "page-one",
      "beyond-page-one",
      "third",
    ]);
    expect(result.current.view.traversal).toBe("complete");
    expect(result.current.view.freshness).toBe("held");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 3);
    });

    expect(result.current.view.items.map((item) => item.workId)).toEqual([
      "first",
      "page-one",
      "beyond-page-one",
      "third",
    ]);
    expect(result.current.view.selectedWorkId).toBe("beyond-page-one");
    expect(result.current.view.traversal).toBe("complete");
    expect(calls).toEqual([null, "cursor-1", "cursor-2"]);
    expect(routines).toHaveBeenCalledTimes(1 + 3);
  });

  it("accepts a load-more response that crosses one tick for the same scope", async () => {
    const calls: Array<string | null> = [];
    const second = deferred<WorkBoardPage>();
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      calls.push(cursor);
      if (cursor == null) return Promise.resolve(page(["first"], false, "cursor-1"));
      return second.promise;
    });
    const { result } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    let loading!: Promise<unknown>;
    act(() => {
      loading = result.current.loadMore();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS);
    });
    await act(async () => {
      second.resolve(page(["second"], true, null));
      await loading;
    });

    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first", "second"]);
    expect(result.current.view.selectedWorkId).toBe("first");
    expect(calls.filter((cursor) => cursor == null)).toEqual([null]);
  });

  it("replaces the traversal on an explicit restart and keeps a failed restart's pages", async () => {
    let failRestart = false;
    const list = vi.fn(async (_scope: WorkBoardScope, cursor: string | null) => {
      if (cursor == null && failRestart) throw new Error("connection reset");
      if (cursor == null) return page(["first"], false, "cursor-1");
      return page(["second"], true, null);
    });
    const { result } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      await result.current.loadMore();
    });
    failRestart = true;
    await act(async () => {
      await result.current.restart();
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first", "second"]);
    expect(result.current.view.traversal).toBe("error");
    expect(result.current.view.error).toContain("connection reset");

    failRestart = false;
    await act(async () => {
      await result.current.restart();
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first"]);
    expect(result.current.view.traversal).toBe("partial");
    expect(result.current.view.continuation).toBe("cursor-1");
  });

  it("shows refresh-required on an expired cursor without mixing in a new first page", async () => {
    const calls: Array<string | null> = [];
    let expire = false;
    const list = vi.fn(async (_scope: WorkBoardScope, cursor: string | null) => {
      calls.push(cursor);
      if (cursor != null && expire) throw new Error("MCP remote error: restart_required");
      if (cursor == null) return page(["first"], false, "cursor-1", "fence-a");
      return page(["second"], true, null, "fence-b");
    });
    const { result } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expire = true;
    await act(async () => {
      await result.current.loadMore();
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first"]);
    expect(result.current.view.traversal).toBe("expired");
    expect(result.current.view.freshness).toBe("refresh-required");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 2);
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first"]);
    expect(calls.filter((cursor) => cursor == null)).toEqual([null]);
  });

  it("treats a changed continuation fence as refresh-required and does not append", async () => {
    const list = vi.fn(async (_scope: WorkBoardScope, cursor: string | null) => {
      if (cursor == null) return page(["first"], false, "cursor-1", "fence-a");
      return page(["second"], false, "cursor-2", "fence-b");
    });
    const { result } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      await result.current.loadMore();
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first"]);
    expect(result.current.view.traversal).toBe("expired");
    expect(result.current.view.freshness).toBe("refresh-required");
  });

  it("drops a stale load-more once an explicit restart has begun", async () => {
    const calls: Array<string | null> = [];
    const second = deferred<WorkBoardPage>();
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      calls.push(cursor);
      if (cursor != null) return second.promise;
      if (calls.filter((item) => item == null).length > 1) return Promise.resolve(page(["replaced"], true, null));
      return Promise.resolve(page(["first"], false, "cursor-1"));
    });
    const { result } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    let loading!: Promise<unknown>;
    act(() => {
      loading = result.current.loadMore();
    });
    let restarted!: Promise<unknown>;
    act(() => {
      restarted = result.current.restart();
    });
    await act(async () => {
      second.resolve(page(["stale-page-two"], true, null));
      await loading;
      await restarted;
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["replaced"]);
    expect(result.current.view.items.some((item) => item.workId === "stale-page-two")).toBe(false);
  });

  it("coalesces a slow first page so later ticks cannot starve the update", async () => {
    const calls: Array<string | null> = [];
    const first = deferred<WorkBoardPage>();
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      calls.push(cursor);
      if (calls.length === 1) return first.promise;
      return Promise.resolve(page([`page-${calls.length}`], true, null));
    });
    const { result } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 4);
    });
    expect(calls).toEqual([null]);
    expect(result.current.view.busy).toBe(true);

    await act(async () => {
      first.resolve(page(["original"], true, null));
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(calls).toEqual([null, null]);
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["page-2"]);
    expect(result.current.view.busy).toBe(false);
  });

  it("polls a single complete page and holds a partial page that has not loaded more", async () => {
    const completeCalls: Array<string | null> = [];
    const complete = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: {
          list: async (_scope, cursor) => {
            completeCalls.push(cursor);
            return page(["only"], true, null);
          },
          get: async () => null,
        },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS);
    });
    expect(completeCalls).toEqual([null, null]);
    expect(complete.result.current.view.freshness).toBe("polling");
    complete.unmount();

    const partialCalls: Array<string | null> = [];
    const partial = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope({ sessionId: "lane-b" }),
        client: {
          list: async (_scope, cursor) => {
            partialCalls.push(cursor);
            return page(["only"], false, "cursor-1");
          },
          get: async () => null,
        },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 2);
    });
    expect(partialCalls).toEqual([null]);
    expect(partial.result.current.view.traversal).toBe("partial");
    expect(partial.result.current.view.continuation).toBe("cursor-1");
    expect(partial.result.current.view.freshness).toBe("held");
  });

  it("does not land a stale page, failure, selection, or detail under another scope", async () => {
    const laneA = scope();
    const laneB = scope({ sessionId: "lane-b", workspace: "/other" });
    const hanging = deferred<WorkBoardPage>();
    const detail = deferred<RemoteWorkSnapshot | null>();
    let phase: "a" | "b" | "hang" = "a";
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      if (phase === "hang") return hanging.promise;
      if (_scope.sessionId === "lane-b") return Promise.resolve(page(["lane-b-item"], true, null));
      if (cursor == null) return Promise.resolve(page(["lane-a-item"], false, "cursor-a"));
      return Promise.resolve(page(["lane-a-more"], true, null));
    });
    const get = vi.fn((_scope: WorkBoardScope, workId: string) => {
      if (workId === "lane-a-more") return detail.promise;
      return Promise.resolve(snapshotFor(workId));
    });
    const { result, rerender } = renderHook(
      (props: { current: WorkBoardScope }) =>
        useWorkBoardPoll({
          enabled: true,
          scope: props.current,
          client: { list, get },
        }),
      { initialProps: { current: laneA } },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      await result.current.loadMore();
    });
    act(() => {
      result.current.select("lane-a-more");
    });
    let detailPromise!: Promise<unknown>;
    act(() => {
      detailPromise = result.current.refreshDetail("lane-a-more");
    });
    phase = "hang";
    let abandoned!: Promise<unknown>;
    act(() => {
      abandoned = result.current.restart();
    });

    phase = "b";
    rerender({ current: laneB });
    await act(async () => {
      detail.resolve(snapshotFor("lane-a-more"));
      hanging.resolve(page(["stale-a"], true, null));
      await detailPromise;
      await abandoned;
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(result.current.view.items.map((item) => item.workId)).toEqual(["lane-b-item"]);
    expect(result.current.view.selectedWorkId).toBe("lane-b-item");
    expect(result.current.view.snapshot?.work.workId ?? null).not.toBe("lane-a-more");
    expect(result.current.view.busy).toBe(false);
    expect(result.current.view.error).toBeNull();
  });

  it("clears a missing lane and ignores the response that was already in flight", async () => {
    const hanging = deferred<WorkBoardPage>();
    let hangNext = false;
    const list = vi.fn((_scope: WorkBoardScope, _cursor: string | null) => {
      if (hangNext) return hanging.promise;
      return Promise.resolve(page(["kept"], true, null));
    });
    const detail = deferred<RemoteWorkSnapshot | null>();
    const { result, rerender } = renderHook(
      (props: { current: WorkBoardScope }) =>
        useWorkBoardPoll({
          enabled: true,
          scope: props.current,
          client: {
            list,
            get: () => detail.promise,
          },
        }),
      { initialProps: { current: scope() } },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    act(() => {
      result.current.select("kept");
    });
    let detailPromise!: Promise<unknown>;
    act(() => {
      detailPromise = result.current.refreshDetail("kept");
    });
    hangNext = true;
    let abandoned!: Promise<unknown>;
    act(() => {
      abandoned = result.current.observe();
    });
    rerender({ current: scope({ sessionId: null, workspace: null }) });
    await act(async () => {
      detail.resolve(snapshotFor("kept"));
      hanging.resolve(page(["late"], true, null));
      await detailPromise;
      await abandoned;
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.view.items).toEqual([]);
    expect(result.current.view.selectedWorkId).toBeNull();
    expect(result.current.view.snapshot).toBeNull();
    expect(result.current.view.busy).toBe(false);
    expect(result.current.view.error).toBeNull();
    expect(result.current.view.traversal).toBe("complete");
  });

  it("keeps the same lane's pages when the remote service disconnects and does not apply a late page", async () => {
    const remote = scope({ remote: true, remoteConnected: true, remoteLanePresent: true });
    const hanging = deferred<WorkBoardPage>();
    let hangCursor = false;
    const calls: Array<string | null> = [];
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      calls.push(cursor);
      if (cursor == null) return Promise.resolve(page(["remote-first"], false, "cursor-1"));
      if (hangCursor) return hanging.promise;
      return Promise.resolve(page(["remote-second"], true, null));
    });
    const { result, rerender } = renderHook(
      (props: { current: WorkBoardScope }) =>
        useWorkBoardPoll({
          enabled: true,
          scope: props.current,
          client: { list, get: async () => snapshotFor("remote-first") },
        }),
      { initialProps: { current: remote } },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      await result.current.loadMore();
    });
    hangCursor = true;
    let abandoned!: Promise<unknown>;
    act(() => {
      abandoned = result.current.restart();
    });
    rerender({ current: { ...remote, remoteConnected: false } });
    const callsAtDisconnect = calls.length;
    await act(async () => {
      hanging.resolve(page(["late-remote"], true, null));
      await abandoned;
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 2);
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual([
      "remote-first",
      "remote-second",
    ]);
    expect(result.current.view.items.some((item) => item.workId === "late-remote")).toBe(false);
    expect(result.current.view.error).toBe(
      "The remote service is disconnected. The last Work snapshot remains available.",
    );
    expect(result.current.view.busy).toBe(false);
    expect(calls.length).toBe(callsAtDisconnect);
  });

  it("does not apply a load-more page after the poll is disposed", async () => {
    const second = deferred<WorkBoardPage>();
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      if (cursor == null) return Promise.resolve(page(["first"], false, "cursor-1"));
      return second.promise;
    });
    const { result, unmount } = renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: { list, get: async () => null },
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    let loading!: Promise<unknown>;
    act(() => {
      loading = result.current.loadMore();
    });
    unmount();
    await act(async () => {
      second.resolve(page(["second"], true, null));
      await loading;
    });
    expect(result.current.view.items.map((item) => item.workId)).toEqual(["first"]);
  });

  it("does not fire the production interval before 2500 ms", async () => {
    const routines = vi.fn();
    renderHook(() =>
      useWorkBoardPoll({
        enabled: true,
        scope: scope(),
        client: {
          list: async () => page(["only"], true, null),
          get: async () => null,
        },
        onRoutines: routines,
      }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(routines).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS - 1);
    });
    expect(routines).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(routines).toHaveBeenCalledTimes(2);
    expect(WORK_BOARD_POLL_MS).toBe(2500);
  });
});
