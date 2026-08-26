import {
  HELP_AUTHORITY_LIMITS,
  createHelpAuthorityCleanupReceipt,
  parseHelpAuthorityResponse,
  validateHelpAuthorityRequest,
  type HelpAuthorityCleanupReceipt,
  type HelpAuthorityRequest,
  type HelpAuthorityResponse,
} from "./contract";

export type HelpAuthorityTransport = (
  request: HelpAuthorityRequest,
  signal: AbortSignal,
) => Promise<unknown>;

export type HelpAuthorityExecutionFailure =
  | "capacity"
  | "cancelled"
  | "deadline"
  | "transport-error"
  | "rejected";

export type HelpAuthorityExecution =
  | {
      readonly ok: true;
      readonly response: HelpAuthorityResponse;
      readonly cleanup: HelpAuthorityCleanupReceipt;
    }
  | {
      readonly ok: false;
      readonly failure: HelpAuthorityExecutionFailure;
      readonly detail: string;
      readonly cleanup: HelpAuthorityCleanupReceipt;
    };

export type HelpAuthorityExecutorOptions = {
  readonly maxConcurrent?: number;
  readonly maxQueued?: number;
  readonly transport: HelpAuthorityTransport;
};

type Waiter = {
  readonly resolve: () => void;
  readonly timer: ReturnType<typeof setTimeout>;
};

function safeErrorName(error: unknown): string {
  return error instanceof Error ? error.name : "unknown";
}

/**
 * The only execution primitive for Help answers.
 *
 * Admission is bounded before a provider task is created. A timeout/cancel
 * aborts the task and then awaits the same promise before releasing the slot;
 * a transport that ignores AbortSignal therefore cannot leak a live provider
 * task past its typed finalization receipt.
 */
export class HelpAuthorityExecutor {
  private readonly maxConcurrent: number;
  private readonly maxQueued: number;
  private readonly transport: HelpAuthorityTransport;
  private active = 0;
  private readonly waiters: Waiter[] = [];

  constructor(options: HelpAuthorityExecutorOptions) {
    this.maxConcurrent = Math.max(1, Math.min(options.maxConcurrent ?? 1, 8));
    this.maxQueued = Math.max(0, Math.min(options.maxQueued ?? 8, 32));
    this.transport = options.transport;
  }

  get activeCount(): number {
    return this.active;
  }

  get queuedCount(): number {
    return this.waiters.length;
  }

  private release(): void {
    this.active -= 1;
    const next = this.waiters.shift();
    if (next) {
      clearTimeout(next.timer);
      next.resolve();
    }
  }

  private async acquire(deadlineAt: string): Promise<"acquired" | "capacity" | "deadline"> {
    if (Date.parse(deadlineAt) <= Date.now()) return "deadline";
    if (this.active < this.maxConcurrent) {
      this.active += 1;
      return "acquired";
    }
    if (this.waiters.length >= this.maxQueued) return "capacity";
    const remaining = Date.parse(deadlineAt) - Date.now();
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        const index = this.waiters.findIndex((waiter) => waiter.timer === timer);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(new Error("deadline"));
      }, Math.max(1, remaining));
      this.waiters.push({ resolve, timer });
    }).catch((error: unknown) => {
      if (error instanceof Error && error.message === "deadline") return;
      throw error;
    });
    if (this.active >= this.maxConcurrent) {
      // A release always wakes exactly one waiter. This guard is defensive:
      // it turns an impossible admission race into a closed failure.
      return "deadline";
    }
    this.active += 1;
    return "acquired";
  }

  private static cleanup(
    requestId: string,
    status: HelpAuthorityCleanupReceipt["status"],
    providerTask: HelpAuthorityCleanupReceipt["providerTask"],
    abortRequested: boolean,
    queueSlot: HelpAuthorityCleanupReceipt["queueSlot"],
  ): HelpAuthorityCleanupReceipt {
    return createHelpAuthorityCleanupReceipt(
      requestId,
      status,
      providerTask,
      abortRequested,
      queueSlot,
    );
  }

  async execute(
    request: HelpAuthorityRequest,
    signal?: AbortSignal,
  ): Promise<HelpAuthorityExecution> {
    const requestValidation = validateHelpAuthorityRequest(request);
    if (!requestValidation.accepted) {
      return {
        ok: false,
        failure: "rejected",
        detail: `${requestValidation.reason}: ${requestValidation.detail}`,
        cleanup: HelpAuthorityExecutor.cleanup(
          request.requestId,
          "finalized",
          "joined",
          false,
          "released",
        ),
      };
    }

    const admission = await this.acquire(request.deadline.deadlineAt);
    if (admission !== "acquired") {
      return {
        ok: false,
        failure: admission === "capacity" ? "capacity" : "deadline",
        detail: admission === "capacity" ? "Help authority queue is full" : "Help authority deadline expired in queue",
        cleanup: HelpAuthorityExecutor.cleanup(
          request.requestId,
          "finalized",
          "joined",
          false,
          "released",
        ),
      };
    }

    const controller = new AbortController();
    const abortFromCaller = () => controller.abort();
    signal?.addEventListener("abort", abortFromCaller, { once: true });
    let abortRequested = Boolean(signal?.aborted);
    let timer: ReturnType<typeof setTimeout> | undefined;
    let deadlineReached = false;
    let providerTaskJoined = false;
    let raw: unknown;
    let transportError: unknown;
    let failure: "cancelled" | "deadline" | null = abortRequested ? "cancelled" : null;

    // Start exactly one provider task. There is no session, transcript, tool,
    // workspace, fallback, or inherited host authority in this closure.
    const providerTask = Promise.resolve().then(() => this.transport(request, controller.signal));
    const remaining = Math.max(1, Date.parse(request.deadline.deadlineAt) - Date.now());
    const deadline = new Promise<"deadline">((resolve) => {
      timer = setTimeout(() => {
        deadlineReached = true;
        resolve("deadline");
      }, remaining);
    });
    const cancelled = new Promise<"cancelled">((resolve) => {
      if (signal?.aborted) resolve("cancelled");
      else signal?.addEventListener("abort", () => resolve("cancelled"), { once: true });
    });

    try {
      if (failure === null) {
        type RaceOutcome =
          | { readonly type: "provider"; readonly value: unknown }
          | { readonly type: "error"; readonly error: unknown }
          | { readonly type: "deadline" }
          | { readonly type: "cancelled" };
        const providerOutcome: Promise<RaceOutcome> = providerTask.then(
          (value): RaceOutcome => ({ type: "provider", value }),
          (error: unknown): RaceOutcome => ({ type: "error", error }),
        );
        const outcome = await Promise.race([
          providerOutcome,
          deadline.then((): RaceOutcome => ({ type: "deadline" })),
          cancelled.then((): RaceOutcome => ({ type: "cancelled" })),
        ]);
        if (outcome.type === "provider") raw = outcome.value;
        else if (outcome.type === "error") transportError = outcome.error;
        else failure = outcome.type;
      }
    } finally {
      if (failure !== null || deadlineReached) {
        abortRequested = true;
        controller.abort();
      }
      // Promise.race above can settle before a cancellation-ignoring
      // transport. Await the provider promise itself before cleanup; this is
      // the crucial no-leaked-task guarantee.
      try {
        raw = await providerTask;
      } catch (error) {
        transportError = error;
      } finally {
        providerTaskJoined = true;
      }
      if (timer !== undefined) clearTimeout(timer);
      signal?.removeEventListener("abort", abortFromCaller);
      this.release();
    }

    const cleanup = HelpAuthorityExecutor.cleanup(
      request.requestId,
      providerTaskJoined ? "finalized" : "uncertain",
      providerTaskJoined ? "joined" : "not_joined",
      abortRequested,
      "released",
    );
    if (!providerTaskJoined) {
      return {
        ok: false,
        failure: "rejected",
        detail: "provider cleanup was uncertain",
        cleanup,
      };
    }
    if (failure === "cancelled" || signal?.aborted) {
      return { ok: false, failure: "cancelled", detail: "cancelled in flight", cleanup };
    }
    if (failure === "deadline" || deadlineReached) {
      return {
        ok: false,
        failure: "deadline",
        detail: `deadline exceeded ${request.deadline.maxDurationMs}ms`,
        cleanup,
      };
    }
    if (transportError !== undefined) {
      return {
        ok: false,
        failure: "transport-error",
        detail: safeErrorName(transportError),
        cleanup,
      };
    }
    if (raw === undefined) {
      return { ok: false, failure: "rejected", detail: "provider returned no response", cleanup };
    }

    // The host owns finalization. A provider may return the response core
    // without cleanup; the executor adds only its own typed receipt. If a
    // broker already supplied a receipt, strict response validation checks it.
    const candidate =
      typeof raw === "object" && raw !== null && !Array.isArray(raw) && !("cleanup" in raw)
        ? { ...(raw as Record<string, unknown>), cleanup }
        : raw;
    const response = parseHelpAuthorityResponse(candidate, request);
    if (!response) {
      return { ok: false, failure: "rejected", detail: "provider response failed strict validation", cleanup };
    }
    return { ok: true, response, cleanup };
  }
}

export const HELP_AUTHORITY_DEFAULT_QUEUE_LIMITS = Object.freeze({
  maxConcurrent: 1,
  maxQueued: 8,
});

export function createHelpAuthorityExecutor(
  transport: HelpAuthorityTransport,
  options: Omit<HelpAuthorityExecutorOptions, "transport"> = {},
): HelpAuthorityExecutor {
  return new HelpAuthorityExecutor({
    ...HELP_AUTHORITY_DEFAULT_QUEUE_LIMITS,
    ...options,
    transport,
  });
}
