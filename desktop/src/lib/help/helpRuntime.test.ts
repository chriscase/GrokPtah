import { describe, expect, it } from "vitest";
import {
  HELP_SCHEDULER_DEFAULTS,
  HelpTaskError,
  createHelpTaskScheduler,
  type HelpTaskContext,
} from "./runtime/tasks";

/** A controllable clock and timer, so no test waits on wall time. */
function harness() {
  let clock = 0;
  const timers = new Map<number, { at: number; handler: () => void }>();
  let nextHandle = 1;
  const scheduler = createHelpTaskScheduler({
    concurrency: 1,
    queueLimit: 3,
    defaultDeadlineMs: 1_000,
    now: () => clock,
    setTimer: (handler, ms) => {
      const handle = nextHandle++;
      timers.set(handle, { at: clock + ms, handler });
      return handle;
    },
    clearTimer: (handle) => {
      timers.delete(handle as number);
    },
  });
  const advance = (ms: number) => {
    clock += ms;
    for (const [handle, timer] of [...timers]) {
      if (timer.at <= clock) {
        timers.delete(handle);
        timer.handler();
      }
    }
  };
  return { scheduler, advance, pendingTimers: () => timers.size };
}

const never = () => new Promise<never>(() => {});

describe("Help task scheduler", () => {
  it("runs a task and returns its value", async () => {
    const { scheduler } = harness();
    await expect(scheduler.submit("retrieve", async () => "ok")).resolves.toBe("ok");
    expect(scheduler.inspect().completed.at(-1)?.state).toBe("succeeded");
  });

  it("fails a task that exceeds its deadline", async () => {
    const { scheduler, advance } = harness();
    const pending = scheduler.submit("answer", never, { taskId: "slow", deadlineMs: 500 });
    const assertion = expect(pending).rejects.toMatchObject({ failure: "deadline-exceeded" });
    advance(500);
    await assertion;
    expect(scheduler.inspect().completed.at(-1)?.state).toBe("timed-out");
  });

  it("settles the caller on cancel even when the task ignores its signal", async () => {
    // The property that matters: a task that never observes its abort signal
    // must not be able to wedge the caller or the queue.
    const { scheduler } = harness();
    const pending = scheduler.submit("index", never, { taskId: "stubborn" });
    await Promise.resolve();
    expect(scheduler.cancel("stubborn")).toBe(true);
    await expect(pending).rejects.toMatchObject({ failure: "cancelled" });
    expect(scheduler.inspect().running).toHaveLength(0);
  });

  it("signals cooperative work through its context", async () => {
    const { scheduler } = harness();
    const observed: boolean[] = [];
    const pending = scheduler.submit(
      "retrieve",
      (context: HelpTaskContext) =>
        new Promise((_resolve, reject) => {
          context.signal.addEventListener("abort", () => {
            observed.push(true);
            reject(new Error("aborted"));
          });
        }),
      { taskId: "cooperative" },
    );
    // Let the task actually start; cancelling before it runs is covered by
    // the "queued task" case, and there is no listener to signal yet.
    await Promise.resolve();
    scheduler.cancel("cooperative");
    await expect(pending).rejects.toMatchObject({ failure: "cancelled" });
    expect(observed).toEqual([true]);
  });

  it("cancels a queued task without ever starting it", async () => {
    const { scheduler } = harness();
    const first = scheduler.submit("retrieve", never, { taskId: "first" });
    let started = false;
    const second = scheduler.submit(
      "retrieve",
      async () => {
        started = true;
      },
      { taskId: "second" },
    );
    expect(scheduler.inspect().queued.map((task) => task.taskId)).toEqual(["second"]);
    expect(scheduler.cancel("second")).toBe(true);
    await expect(second).rejects.toMatchObject({ failure: "cancelled" });
    expect(started).toBe(false);
    scheduler.cancel("first");
    await expect(first).rejects.toBeInstanceOf(HelpTaskError);
  });

  it("rejects submissions past the queue bound instead of growing", async () => {
    const { scheduler } = harness();
    const held = [
      scheduler.submit("index", never, { taskId: "run" }),
      scheduler.submit("index", never, { taskId: "q1" }),
      scheduler.submit("index", never, { taskId: "q2" }),
      scheduler.submit("index", never, { taskId: "q3" }),
    ];
    await expect(scheduler.submit("index", never, { taskId: "overflow" })).rejects.toMatchObject({
      failure: "queue-full",
    });
    await scheduler.shutdown();
    await Promise.allSettled(held);
  });

  it("cancels every task of one kind, leaving others alone", async () => {
    const { scheduler } = harness();
    const retrieval = scheduler.submit("retrieve", never, { taskId: "r1" });
    const indexing = scheduler.submit("index", never, { taskId: "i1" });
    expect(scheduler.cancelKind("retrieve")).toBe(1);
    await expect(retrieval).rejects.toMatchObject({ failure: "cancelled" });
    const live = [...scheduler.inspect().queued, ...scheduler.inspect().running];
    expect(live.map((task) => task.taskId)).toContain("i1");
    await scheduler.shutdown();
    await Promise.allSettled([indexing]);
  });

  it("drains on shutdown in bounded time and refuses new work", async () => {
    const { scheduler } = harness();
    const held = [
      scheduler.submit("index", never, { taskId: "a" }),
      scheduler.submit("index", never, { taskId: "b" }),
    ];
    // Resolves even though neither task cooperates.
    await scheduler.shutdown();
    const inspection = scheduler.inspect();
    expect(inspection.running).toHaveLength(0);
    expect(inspection.queued).toHaveLength(0);
    expect(inspection.shuttingDown).toBe(true);
    await expect(scheduler.submit("index", async () => 1)).rejects.toMatchObject({
      failure: "shutting-down",
    });
    await Promise.allSettled(held);
  });

  it("leaves no pending timers behind after settling", async () => {
    const { scheduler, advance, pendingTimers } = harness();
    await scheduler.submit("retrieve", async () => "done");
    expect(pendingTimers()).toBe(0);
    const cancelled = scheduler.submit("retrieve", never, { taskId: "c" });
    scheduler.cancel("c");
    await expect(cancelled).rejects.toBeInstanceOf(HelpTaskError);
    expect(pendingTimers()).toBe(0);
    advance(10_000);
  });

  it("records a bounded, restart-readable history", async () => {
    const { scheduler } = harness();
    for (let index = 0; index < 100; index += 1) {
      await scheduler.submit("retrieve", async () => index, { taskId: `t${index}` });
    }
    const { completed } = scheduler.inspect();
    // Retained for restart reconciliation but must not grow without limit;
    // the newest entries are the ones kept.
    expect(completed.length).toBeLessThanOrEqual(64);
    expect(completed.at(-1)?.taskId).toBe("t99");
  });

  it("does not settle a caller twice when cancel races completion", async () => {
    const { scheduler } = harness();
    let release: (value: string) => void = () => {};
    const pending = scheduler.submit(
      "answer",
      () =>
        new Promise<string>((resolve) => {
          release = resolve;
        }),
      { taskId: "racy" },
    );
    scheduler.cancel("racy");
    release("late value");
    await expect(pending).rejects.toMatchObject({ failure: "cancelled" });
    const records = scheduler.inspect().completed.filter((task) => task.taskId === "racy");
    expect(records).toHaveLength(1);
  });

  it("exposes a shrinking deadline to the running task", async () => {
    const { scheduler } = harness();
    const seen: number[] = [];
    const pending = scheduler.submit(
      "index",
      async (context: HelpTaskContext) => {
        seen.push(context.remainingMs());
        return "ok";
      },
      { deadlineMs: 800 },
    );
    await pending;
    expect(seen[0]).toBeLessThanOrEqual(800);
    expect(seen[0]).toBeGreaterThan(0);
  });

  it("ships conservative defaults", () => {
    expect(HELP_SCHEDULER_DEFAULTS.concurrency).toBeGreaterThan(0);
    expect(HELP_SCHEDULER_DEFAULTS.queueLimit).toBeGreaterThan(0);
    expect(HELP_SCHEDULER_DEFAULTS.defaultDeadlineMs).toBeGreaterThan(0);
  });
});

describe("Help task scheduler cancellation before start", () => {
  it("never invokes work that was cancelled before its first tick", async () => {
    const { scheduler } = harness();
    let invoked = false;
    const pending = scheduler.submit(
      "retrieve",
      async () => {
        invoked = true;
        return "should not run";
      },
      { taskId: "pre-cancelled" },
    );
    // Cancelled synchronously, before the microtask that would invoke it.
    scheduler.cancel("pre-cancelled");
    await expect(pending).rejects.toMatchObject({ failure: "cancelled" });
    await Promise.resolve();
    await Promise.resolve();
    expect(invoked).toBe(false);
  });
});
