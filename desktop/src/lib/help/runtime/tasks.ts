/**
 * Bounded, cancellable task runtime for Help work.
 *
 * Indexing, retrieval, and answering all run through one scheduler so the
 * failure modes are handled once and identically:
 *
 *   - **Every task has a deadline.** A task that never settles is failed by
 *     the scheduler rather than holding a slot forever.
 *   - **Cancellation cannot hang.** `cancel` settles the caller's promise
 *     immediately and stops accounting for the task, whether or not the
 *     underlying work notices its abort signal. Work that ignores its signal
 *     wastes CPU; it must never wedge the queue or the caller.
 *   - **The queue is bounded.** Submitting past capacity rejects with
 *     `queue-full` instead of growing without limit, which is what turns a
 *     burst of keystrokes into unbounded memory.
 *   - **Shutdown drains deterministically.** `shutdown` cancels queued work,
 *     signals running work, and resolves once every task has settled, so a
 *     restart never inherits a half-finished task.
 *
 * Deliberately dependency-free and host-neutral: the desktop, the broker, and
 * a consumer embedding the SDK all get the same behavior.
 */

export type HelpTaskKind = "index" | "retrieve" | "answer";

export type HelpTaskState =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "timed-out"
  | "rejected";

export type HelpTaskFailure =
  | "queue-full"
  | "shutting-down"
  | "deadline-exceeded"
  | "cancelled"
  | "task-error";

export class HelpTaskError extends Error {
  readonly failure: HelpTaskFailure;
  readonly taskId: string;
  constructor(failure: HelpTaskFailure, taskId: string, detail?: string) {
    super(`help task ${taskId}: ${failure}${detail ? ` (${detail})` : ""}`);
    this.name = "HelpTaskError";
    this.failure = failure;
    this.taskId = taskId;
  }
}

export type HelpTaskContext = {
  readonly taskId: string;
  readonly signal: AbortSignal;
  /** Milliseconds remaining before the deadline, at the time of the call. */
  readonly remainingMs: () => number;
};

export type HelpTaskRecord = {
  readonly taskId: string;
  readonly kind: HelpTaskKind;
  readonly state: HelpTaskState;
  readonly failure?: HelpTaskFailure;
};

export type HelpSchedulerOptions = {
  /** Maximum tasks running at once. */
  readonly concurrency?: number;
  /** Maximum tasks waiting to start. */
  readonly queueLimit?: number;
  /** Default deadline for a task, in milliseconds. */
  readonly defaultDeadlineMs?: number;
  /** Injectable clock and timer, so tests need no wall-clock waiting. */
  readonly now?: () => number;
  readonly setTimer?: (handler: () => void, ms: number) => unknown;
  readonly clearTimer?: (handle: unknown) => void;
};

export const HELP_SCHEDULER_DEFAULTS = Object.freeze({
  concurrency: 2,
  queueLimit: 32,
  defaultDeadlineMs: 20_000,
});

type Entry = {
  readonly taskId: string;
  readonly kind: HelpTaskKind;
  readonly deadlineMs: number;
  readonly run: (context: HelpTaskContext) => Promise<unknown>;
  readonly resolve: (value: unknown) => void;
  readonly reject: (error: unknown) => void;
  readonly controller: AbortController;
  state: HelpTaskState;
  failure?: HelpTaskFailure;
  timer?: unknown;
  startedAt?: number;
  /** True once the caller's promise has settled; guards double-settle. */
  settled: boolean;
};

export type HelpTaskScheduler = {
  submit: <T>(
    kind: HelpTaskKind,
    run: (context: HelpTaskContext) => Promise<T>,
    options?: { taskId?: string; deadlineMs?: number },
  ) => Promise<T>;
  cancel: (taskId: string) => boolean;
  cancelKind: (kind: HelpTaskKind) => number;
  shutdown: () => Promise<void>;
  /** Snapshot for status surfaces and restart-recovery assertions. */
  inspect: () => {
    running: readonly HelpTaskRecord[];
    queued: readonly HelpTaskRecord[];
    completed: readonly HelpTaskRecord[];
    shuttingDown: boolean;
  };
};

/**
 * Create a scheduler.
 *
 * Completed tasks are retained as a bounded ring so a restarting UI can
 * reconcile what happened without the history growing without limit.
 */
export function createHelpTaskScheduler(options: HelpSchedulerOptions = {}): HelpTaskScheduler {
  const concurrency = Math.max(1, options.concurrency ?? HELP_SCHEDULER_DEFAULTS.concurrency);
  const queueLimit = Math.max(1, options.queueLimit ?? HELP_SCHEDULER_DEFAULTS.queueLimit);
  const defaultDeadlineMs = Math.max(
    1,
    options.defaultDeadlineMs ?? HELP_SCHEDULER_DEFAULTS.defaultDeadlineMs,
  );
  const now = options.now ?? (() => Date.now());
  const setTimer = options.setTimer ?? ((handler, ms) => setTimeout(handler, ms));
  const clearTimer = options.clearTimer ?? ((handle) => clearTimeout(handle as never));

  const queue: Entry[] = [];
  const running = new Map<string, Entry>();
  const completed: HelpTaskRecord[] = [];
  const COMPLETED_LIMIT = 64;
  let shuttingDown = false;
  let sequence = 0;
  /** Resolvers waiting for the scheduler to become idle. */
  let idleWaiters: Array<() => void> = [];

  const record = (entry: Entry): HelpTaskRecord =>
    Object.freeze({
      taskId: entry.taskId,
      kind: entry.kind,
      state: entry.state,
      failure: entry.failure,
    });

  function remember(entry: Entry): void {
    completed.push(record(entry));
    if (completed.length > COMPLETED_LIMIT) completed.shift();
  }

  function maybeIdle(): void {
    if (running.size === 0 && queue.length === 0) {
      const waiters = idleWaiters;
      idleWaiters = [];
      for (const waiter of waiters) waiter();
    }
  }

  /**
   * Settle a task exactly once.
   *
   * The guard matters: a timeout, a cancellation, and the task's own
   * completion can all race. Without it a cancelled task that later resolves
   * would settle the caller twice and corrupt the accounting that `shutdown`
   * waits on.
   */
  function settle(entry: Entry, state: HelpTaskState, value: unknown, failure?: HelpTaskFailure): void {
    if (entry.settled) return;
    entry.settled = true;
    entry.state = state;
    entry.failure = failure;
    if (entry.timer !== undefined) {
      clearTimer(entry.timer);
      entry.timer = undefined;
    }
    running.delete(entry.taskId);
    remember(entry);
    if (state === "succeeded") entry.resolve(value);
    else entry.reject(value);
    pump();
    maybeIdle();
  }

  function start(entry: Entry): void {
    entry.state = "running";
    entry.startedAt = now();
    running.set(entry.taskId, entry);

    entry.timer = setTimer(() => {
      // Signal the work, then settle regardless: a task that ignores its
      // signal must not be able to hold the caller or the queue.
      entry.controller.abort();
      settle(
        entry,
        "timed-out",
        new HelpTaskError("deadline-exceeded", entry.taskId, `${entry.deadlineMs}ms`),
        "deadline-exceeded",
      );
    }, entry.deadlineMs);

    const context: HelpTaskContext = {
      taskId: entry.taskId,
      signal: entry.controller.signal,
      remainingMs: () => Math.max(0, entry.deadlineMs - (now() - (entry.startedAt ?? now()))),
    };

    Promise.resolve()
      .then(() => {
        // A task cancelled between being started and reaching this microtask
        // must not run at all: the caller has already been settled, so the
        // work would be pure waste and its result would be discarded.
        if (entry.settled) return undefined;
        return entry.run(context);
      })
      .then(
        (value) => settle(entry, "succeeded", value),
        (error) => settle(entry, "failed", error, "task-error"),
      );
  }

  function pump(): void {
    while (running.size < concurrency && queue.length > 0) {
      const entry = queue.shift();
      if (!entry) break;
      if (entry.settled) continue;
      start(entry);
    }
  }

  return {
    submit(kind, run, submitOptions = {}) {
      const taskId = submitOptions.taskId ?? `${kind}-${(sequence += 1)}`;
      if (shuttingDown) {
        return Promise.reject(new HelpTaskError("shutting-down", taskId));
      }
      if (queue.length >= queueLimit) {
        // Bounded: refuse rather than accumulate.
        return Promise.reject(new HelpTaskError("queue-full", taskId, `limit ${queueLimit}`));
      }
      const deadlineMs = Math.max(1, submitOptions.deadlineMs ?? defaultDeadlineMs);
      return new Promise((resolve, reject) => {
        const entry: Entry = {
          taskId,
          kind,
          deadlineMs,
          run: run as (context: HelpTaskContext) => Promise<unknown>,
          resolve: resolve as (value: unknown) => void,
          reject,
          controller: new AbortController(),
          state: "queued",
          settled: false,
        };
        queue.push(entry);
        pump();
      });
    },

    cancel(taskId) {
      const runningEntry = running.get(taskId);
      if (runningEntry) {
        runningEntry.controller.abort();
        settle(
          runningEntry,
          "cancelled",
          new HelpTaskError("cancelled", taskId),
          "cancelled",
        );
        return true;
      }
      const index = queue.findIndex((entry) => entry.taskId === taskId);
      if (index >= 0) {
        const [entry] = queue.splice(index, 1);
        if (entry) {
          entry.controller.abort();
          settle(entry, "cancelled", new HelpTaskError("cancelled", taskId), "cancelled");
        }
        return true;
      }
      return false;
    },

    cancelKind(kind) {
      const targets = [
        ...queue.filter((entry) => entry.kind === kind).map((entry) => entry.taskId),
        ...[...running.values()].filter((entry) => entry.kind === kind).map((entry) => entry.taskId),
      ];
      let cancelled = 0;
      for (const taskId of targets) if (this.cancel(taskId)) cancelled += 1;
      return cancelled;
    },

    async shutdown() {
      shuttingDown = true;
      // Queued work never starts; running work is signalled and settled, so
      // shutdown completes in bounded time even if a task ignores its signal.
      for (const entry of [...queue]) {
        entry.controller.abort();
        const index = queue.indexOf(entry);
        if (index >= 0) queue.splice(index, 1);
        settle(entry, "cancelled", new HelpTaskError("shutting-down", entry.taskId), "shutting-down");
      }
      for (const entry of [...running.values()]) {
        entry.controller.abort();
        settle(entry, "cancelled", new HelpTaskError("shutting-down", entry.taskId), "shutting-down");
      }
      if (running.size === 0 && queue.length === 0) return;
      await new Promise<void>((resolve) => {
        idleWaiters.push(resolve);
      });
    },

    inspect() {
      return {
        running: Object.freeze([...running.values()].map(record)),
        queued: Object.freeze(queue.map(record)),
        completed: Object.freeze([...completed]),
        shuttingDown,
      };
    },
  };
}
