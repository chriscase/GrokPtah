import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WorkBoard } from "../components/WorkBoard";
import type { DurableWorkAttempt, RemoteWorkSnapshot, WorkListEntry } from "./protocol";
import { WORK_BOARD_POLL_MS, type WorkBoardPage, type WorkBoardScope } from "./workBoardController";
import { useWorkBoardPoll } from "./useWorkBoardPoll";

/**
 * Same wiring App uses: Refresh and traversal Retry call restart, detail Retry
 * calls refreshDetail, and selecting an id loads its detail.
 */
function WiredBoard({
  client,
  scope,
  enabled = true,
  api,
}: {
  client: {
    list: (scope: WorkBoardScope, cursor: string | null) => Promise<WorkBoardPage>;
    get: (scope: WorkBoardScope, workId: string) => Promise<RemoteWorkSnapshot | null>;
  };
  scope: WorkBoardScope;
  enabled?: boolean;
  api?: { current: ReturnType<typeof useWorkBoardPoll> | null };
}) {
  const board = useWorkBoardPoll({ enabled, scope, client });
  if (api) api.current = board;
  const selectedWorkId = board.view.selectedWorkId;
  const { dismissDetail, refreshDetail } = board;
  useEffect(() => {
    if (!enabled || !selectedWorkId) {
      if (!selectedWorkId) dismissDetail();
      return;
    }
    void refreshDetail(selectedWorkId);
  }, [dismissDetail, enabled, refreshDetail, selectedWorkId]);
  return (
    <WorkBoard
      items={board.view.items}
      selectedWorkId={selectedWorkId}
      snapshot={board.view.snapshot}
      detailStatus={board.view.detailStatus}
      detailError={board.view.detailError}
      scope={{
        laneId: scope.sessionId,
        laneTitle: "Lane",
        runtimeTarget: "local_desktop",
        runtimeConnection: "connected",
        workspacePath: scope.workspace,
        runLabel: "Ready",
      }}
      busy={board.view.busy}
      error={board.view.error}
      traversalStatus={board.view.traversal}
      pagesHeld={board.view.freshness === "held"}
      mutationsEnabled
      onRefresh={() => void board.restart()}
      onRetryTraversal={() => void board.restart()}
      onRetryDetail={() => {
        if (selectedWorkId) void board.refreshDetail(selectedWorkId);
      }}
      onSelect={(workId) => board.select(workId)}
      onLoadMore={() => void board.loadMore()}
      onApprove={async () => undefined}
      onOpenRun={() => undefined}
    />
  );
}

function scope(overrides: Partial<WorkBoardScope> = {}): WorkBoardScope {
  return {
    sessionId: "lane-a",
    workspace: "/fixture",
    remote: false,
    remoteConnected: false,
    remoteLanePresent: true,
    ...overrides,
  };
}

function entry(workId: string, revision: number, state: WorkListEntry["state"]): WorkListEntry {
  return {
    workId,
    kind: "test",
    state,
    priority: 0,
    revision,
    attemptCount: 1,
    createdAt: "2026-10-12T00:00:00Z",
    updatedAt: `2026-10-12T00:00:0${revision}Z`,
    objective: workId,
  };
}

function attempt(heartbeat: string, runId: string): DurableWorkAttempt {
  return {
    schemaVersion: 1,
    attemptId: "attempt-1",
    workId: "work-a",
    attemptNumber: 1,
    claimantId: "worker",
    acquiredAt: "2026-10-12T00:00:00Z",
    leaseExpiresAt: "2026-10-12T01:00:00Z",
    lastHeartbeatAt: heartbeat,
    state: "running",
    linkedRunIds: [runId],
    progress: null,
    result: null,
    terminalReason: null,
    createdAt: "2026-10-12T00:00:00Z",
    updatedAt: heartbeat,
  };
}

function snapshotFor(
  revision: number,
  state: RemoteWorkSnapshot["work"]["state"],
  attempts: DurableWorkAttempt[] = [],
): RemoteWorkSnapshot {
  return {
    work: {
      schemaVersion: 1,
      workId: "work-a",
      kind: "test",
      objective: `objective-rev-${revision}`,
      sessionId: "lane-a",
      workspace: "/fixture",
      createdBy: "operator",
      assignedAgentId: null,
      priority: 0,
      deadline: null,
      parentWorkId: null,
      dependencies: [],
      policy: {
        bounds: { maxPromptBytes: 0, maxRounds: 0, maxDurationMs: 0 },
        retry: { maxAttempts: 3, retryFailed: true, retryExpired: false, backoffMs: 0 },
        requiresApproval: true,
        maxConcurrentAttempts: 1,
      },
      state,
      revision,
      attemptCount: 1,
      progress: null,
      result: null,
      approval: null,
      createdAt: "2026-10-12T00:00:00Z",
      updatedAt: `2026-10-12T00:00:0${revision}Z`,
    },
    attempts,
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
  cleanup();
  vi.useRealTimers();
});

async function flush() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

describe("production Work Board detail refresh", () => {
  it("refreshes the same selected id from running revision 1 to awaiting_approval revision 2", async () => {
    let revision = 1;
    let state: WorkListEntry["state"] = "running";
    const list = vi.fn(async () => ({
      items: [entry("work-a", revision, state)],
      complete: true,
      continuation: null,
    }));
    const get = vi.fn(async () => snapshotFor(revision, state));
    render(<WiredBoard client={{ list, get }} scope={scope()} />);
    await flush();
    expect(screen.getByRole("heading", { name: "objective-rev-1" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve completion" })).toBeNull();

    revision = 2;
    state = "awaiting_approval";
    fireEvent.click(screen.getByRole("button", { name: "↻" }));
    await flush();
    expect(screen.getByRole("heading", { name: "objective-rev-2" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve completion" })).toBeInTheDocument();
    expect(get).toHaveBeenCalledTimes(2);

    revision = 3;
    state = "succeeded";
    fireEvent.click(screen.getByRole("button", { name: "↻" }));
    await flush();
    expect(screen.getByRole("heading", { name: "objective-rev-3" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve completion" })).toBeNull();
  });

  it("re-reads attempt heartbeat and run link when the Work revision stays the same", async () => {
    let heartbeat = "2026-10-12T00:10:00Z";
    let runId = "run-old0001";
    const list = vi.fn(async () => ({
      items: [entry("work-a", 4, "running")],
      complete: true,
      continuation: null,
    }));
    const get = vi.fn(async () => snapshotFor(4, "running", [attempt(heartbeat, runId)]));
    render(<WiredBoard client={{ list, get }} scope={scope()} />);
    await flush();
    expect(screen.getByRole("button", { name: "Inspect Run run-old0" })).toBeInTheDocument();

    heartbeat = "2026-10-12T00:20:00Z";
    runId = "run-new9999";
    fireEvent.click(screen.getByRole("button", { name: "↻" }));
    await flush();
    expect(screen.getByRole("button", { name: "Inspect Run run-new9" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Inspect Run run-old0" })).toBeNull();
    expect(get.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("keeps the newer detail when an older success lands last", async () => {
    const first = deferred<RemoteWorkSnapshot | null>();
    const newer = deferred<RemoteWorkSnapshot | null>();
    let gets = 0;
    const get = vi.fn(() => {
      gets += 1;
      return gets === 1 ? first.promise : newer.promise;
    });
    let revision = 1;
    const list = vi.fn(async () => ({
      items: [entry("work-a", revision, revision === 1 ? "running" : "awaiting_approval")],
      complete: true,
      continuation: null,
    }));
    render(<WiredBoard client={{ list, get }} scope={scope()} />);
    await flush();
    revision = 2;
    fireEvent.click(screen.getByRole("button", { name: "↻" }));
    await flush();
    newer.resolve(snapshotFor(2, "awaiting_approval"));
    await flush();
    first.resolve(snapshotFor(1, "running"));
    await flush();
    expect(screen.getByRole("heading", { name: "objective-rev-2" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "objective-rev-1" })).toBeNull();
    expect(screen.getByRole("button", { name: "Approve completion" })).toBeInTheDocument();
  });

  it("does not let an older rejection clear the newer snapshot", async () => {
    const first = deferred<RemoteWorkSnapshot | null>();
    const newer = deferred<RemoteWorkSnapshot | null>();
    let gets = 0;
    const get = vi.fn(() => {
      gets += 1;
      return gets === 1 ? first.promise : newer.promise;
    });
    const list = vi.fn(async () => ({
      items: [entry("work-a", 2, "awaiting_approval")],
      complete: true,
      continuation: null,
    }));
    render(<WiredBoard client={{ list, get }} scope={scope()} />);
    await flush();
    fireEvent.click(screen.getByRole("button", { name: "↻" }));
    await flush();
    newer.resolve(snapshotFor(2, "awaiting_approval"));
    await flush();
    first.reject(new Error("stale failure"));
    await flush();
    expect(screen.getByRole("heading", { name: "objective-rev-2" })).toBeInTheDocument();
    expect(screen.queryByText(/stale failure/)).toBeNull();
    expect(screen.queryByText("Work detail could not be refreshed")).toBeNull();
  });

  it("invalidates detail when Refresh is queued behind Load more and then settles", async () => {
    const api: { current: ReturnType<typeof useWorkBoardPoll> | null } = { current: null };
    const pageTwo = deferred<WorkBoardPage>();
    const staleDetail = deferred<RemoteWorkSnapshot | null>();
    const freshDetail = deferred<RemoteWorkSnapshot | null>();
    let gets = 0;
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      if (cursor == null) {
        return Promise.resolve({
          items: [entry("work-a", 1, "running"), entry("other", 1, "queued")],
          complete: false,
          continuation: "cursor-1",
        });
      }
      return pageTwo.promise;
    });
    const get = vi.fn(() => {
      gets += 1;
      return gets === 1 ? staleDetail.promise : freshDetail.promise;
    });
    render(<WiredBoard api={api} client={{ list, get }} scope={scope()} />);
    await flush();
    await act(async () => {
      void api.current?.loadMore();
    });
    const queued = act(async () => {
      await api.current?.restart();
    });
    freshDetail.resolve(snapshotFor(2, "awaiting_approval"));
    staleDetail.resolve(snapshotFor(1, "running"));
    pageTwo.resolve({
      items: [entry("second", 1, "queued")],
      complete: true,
      continuation: null,
    });
    await queued;
    await flush();
    expect(screen.getByRole("heading", { name: "objective-rev-2" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "objective-rev-1" })).toBeNull();
    expect(screen.getByRole("button", { name: "Approve completion" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "↻" })).toBeEnabled();
  });

  it("bounds detail reads across repeated Refresh and poll ticks and keeps the newest result", async () => {
    let revision = 1;
    let hold = false;
    const pending: Array<ReturnType<typeof deferred<RemoteWorkSnapshot | null>>> = [];
    const list = vi.fn(async () => ({
      items: [entry("work-a", revision, "running")],
      complete: true,
      continuation: null,
    }));
    const get = vi.fn(() => {
      if (!hold) return Promise.resolve(snapshotFor(revision, "running"));
      const slot = deferred<RemoteWorkSnapshot | null>();
      pending.push(slot);
      return slot.promise;
    });
    render(<WiredBoard client={{ list, get }} scope={scope()} />);
    await flush();
    hold = true;
    for (let i = 0; i < 6; i += 1) {
      revision += 1;
      fireEvent.click(screen.getByRole("button", { name: "↻" }));
      await flush();
    }
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 4);
    });
    expect(get.mock.calls.length).toBeLessThanOrEqual(8);
    expect(get.mock.calls.length).toBeGreaterThan(1);
    const newest = revision;
    while (pending.length > 0) {
      pending.shift()?.resolve(snapshotFor(newest, "running"));
      await flush();
    }
    expect(screen.getByRole("heading", { name: `objective-rev-${newest}` })).toBeInTheDocument();
  });

  it("reconciles an unpaged background row change and still holds a paged selection", async () => {
    let revision = 1;
    let state: WorkListEntry["state"] = "running";
    const calls: Array<string | null> = [];
    const list = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      calls.push(cursor);
      if (cursor == null) {
        return Promise.resolve({
          items: [entry("work-a", revision, state)],
          complete: true,
          continuation: null,
        });
      }
      return Promise.resolve({
        items: [entry("ignored", 1, "queued")],
        complete: true,
        continuation: null,
      });
    });
    const get = vi.fn(async () => snapshotFor(revision, state));
    const { rerender } = render(<WiredBoard client={{ list, get }} scope={scope()} />);
    await flush();
    expect(screen.queryByRole("button", { name: "Approve completion" })).toBeNull();
    revision = 2;
    state = "awaiting_approval";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS);
    });
    expect(screen.getByRole("heading", { name: "objective-rev-2" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve completion" })).toBeInTheDocument();

    const pagedCalls: Array<string | null> = [];
    const pagedList = vi.fn(async (_scope: WorkBoardScope, cursor: string | null) => {
      pagedCalls.push(cursor);
      if (cursor == null) {
        return {
          items: [entry("first", 1, "queued")],
          complete: false,
          continuation: "cursor-1",
        };
      }
      return {
        items: [entry("beyond-page-one", 1, "running")],
        complete: true,
        continuation: null,
      };
    });
    rerender(
      <WiredBoard
        client={{ list: pagedList, get: async () => snapshotFor(1, "running") }}
        scope={scope({ sessionId: "lane-paged" })}
      />,
    );
    await flush();
    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await flush();
    fireEvent.click(screen.getByRole("button", { name: /^beyond-page-one/ }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 3);
    });
    expect(pagedCalls.filter((cursor) => cursor == null).length).toBe(1);
    const crossing = deferred<WorkBoardPage>();
    const crossingList = vi.fn((_scope: WorkBoardScope, cursor: string | null) => {
      if (cursor == null) {
        return Promise.resolve({
          items: [entry("first", 1, "queued")],
          complete: false,
          continuation: "cursor-1",
        });
      }
      return crossing.promise;
    });
    rerender(
      <WiredBoard
        client={{ list: crossingList, get: async () => snapshotFor(1, "queued") }}
        scope={scope({ sessionId: "lane-cross" })}
      />,
    );
    await flush();
    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS);
    });
    crossing.resolve({
      items: [entry("beyond-page-one", 1, "running")],
      complete: true,
      continuation: null,
    });
    await flush();
    fireEvent.click(screen.getByRole("button", { name: /^beyond-page-one/ }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WORK_BOARD_POLL_MS * 3);
    });
    expect(screen.getByRole("button", { name: /^first/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^beyond-page-one/, pressed: true })).toBeInTheDocument();
  });

  it("drops a late detail after the selection, scope, disconnect, or view goes away", async () => {
    const late = deferred<RemoteWorkSnapshot | null>();
    let hold = false;
    const listFor = (ids: string[]) =>
      vi.fn(async () => ({
        items: ids.map((id) => entry(id, 1, "running")),
        complete: true,
        continuation: null,
      }));
    const get = vi.fn(() => (hold ? late.promise : Promise.resolve(snapshotFor(1, "running"))));
    const api: { current: ReturnType<typeof useWorkBoardPoll> | null } = { current: null };
    const { rerender, unmount } = render(
      <WiredBoard api={api} client={{ list: listFor(["work-a", "work-b"]), get }} scope={scope()} />,
    );
    await flush();
    hold = true;
    fireEvent.click(screen.getByRole("button", { name: /^work-b/ }));
    await flush();
    late.resolve(snapshotFor(9, "succeeded"));
    await flush();
    expect(screen.queryByRole("heading", { name: "objective-rev-9" })).toBeNull();
    expect(screen.getByRole("button", { name: /^work-b/, pressed: true })).toBeInTheDocument();

    rerender(
      <WiredBoard
        api={api}
        client={{ list: listFor(["lane-b-item"]), get: async () => snapshotFor(1, "queued") }}
        scope={scope({ sessionId: "lane-b" })}
      />,
    );
    await flush();
    expect(screen.queryByRole("heading", { name: "objective-rev-9" })).toBeNull();
    expect(screen.getByRole("button", { name: /^lane-b-item/ })).toBeInTheDocument();

    rerender(
      <WiredBoard
        api={api}
        client={{ list: listFor(["work-a"]), get: async () => snapshotFor(1, "running") }}
        scope={scope({ remote: true, remoteConnected: false, remoteLanePresent: true, sessionId: "lane-remote" })}
      />,
    );
    await flush();
    expect(screen.getByText(/disconnected/i)).toBeInTheDocument();

    rerender(
      <WiredBoard
        api={api}
        client={{ list: listFor(["work-a"]), get: async () => snapshotFor(1, "running") }}
        scope={scope({ sessionId: null, workspace: null })}
      />,
    );
    await flush();
    expect(screen.getByText("No durable Work Items in this Lane")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "objective-rev-9" })).toBeNull();
    unmount();
  });

  it("retries a failed detail read without dropping another loaded page", async () => {
    let fail = false;
    const list = vi.fn(async (_scope: WorkBoardScope, cursor: string | null) => {
      if (cursor == null) {
        return {
          items: [entry("work-a", 2, "awaiting_approval")],
          complete: false,
          continuation: "cursor-1",
        };
      }
      return {
        items: [entry("other-page", 1, "queued")],
        complete: true,
        continuation: null,
      };
    });
    const get = vi.fn(async () => {
      if (fail) throw new Error("detail unavailable");
      return snapshotFor(2, "awaiting_approval");
    });
    const apiFail: { current: ReturnType<typeof useWorkBoardPoll> | null } = { current: null };
    render(<WiredBoard api={apiFail} client={{ list, get }} scope={scope()} />);
    await flush();
    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await flush();
    expect(screen.getByRole("button", { name: "Approve completion" })).toBeInTheDocument();
    fail = true;
    await act(async () => {
      await apiFail.current?.refreshDetail("work-a");
    });
    expect(screen.getByText("Work detail could not be refreshed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve completion" })).toBeNull();
    expect(screen.getByRole("button", { name: /^other-page/ })).toBeInTheDocument();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await flush();
    expect(screen.getByRole("button", { name: "Approve completion" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^other-page/ })).toBeInTheDocument();
    expect(screen.queryByText("Work detail could not be refreshed")).toBeNull();
  });
});
