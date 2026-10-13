import type {
  DurableWorkItem,
  RemoteWorkSnapshot,
  WorkListEntry,
  WorkTraversalStatus,
} from "./protocol";
import { workListEntryToItem } from "./protocol";
import { acceptWorkPage, workTraversalFromError } from "./workTraversal";

/** Production Work Board poll. Background observation uses this interval. */
export const WORK_BOARD_POLL_MS = 2500;

export interface WorkBoardScope {
  sessionId: string | null;
  workspace: string | null;
  remote: boolean;
  remoteConnected: boolean;
  remoteLanePresent: boolean;
}

/** List page plus the optional lane fence sealed into a traversal. */
export interface WorkBoardPage {
  items: WorkListEntry[];
  complete: boolean;
  continuation?: string | null;
  fence?: string | null;
}

/** Whether the selected snapshot is safe to drive detail actions. */
export type WorkDetailStatus = "current" | "loading" | "error";

export interface WorkBoardView {
  items: DurableWorkItem[];
  selectedWorkId: string | null;
  snapshot: RemoteWorkSnapshot | null;
  error: string | null;
  busy: boolean;
  traversal: WorkTraversalStatus;
  continuation: string | null;
  freshness: "polling" | "held" | "refresh-required";
  /** `loading` and `error` must not let an older snapshot drive actions. */
  detailStatus: WorkDetailStatus;
  detailError: string | null;
}

type ListKind = "observe" | "restart" | "loadMore";

interface QueuedList {
  kind: ListKind;
  scope: WorkBoardScope;
  resolve: () => void;
}

const DISCONNECTED = "The remote service is disconnected. The last Work snapshot remains available.";

/**
 * Lane identity for a traversal. Connection is not part of the key: a disconnect
 * keeps the loaded pages and only drops responses that arrive after it.
 */
export function workBoardScopeKey(scope: WorkBoardScope): string {
  return [scope.remote ? "remote" : "local", scope.sessionId ?? "", scope.workspace ?? ""].join("\n");
}

function rank(kind: ListKind): number {
  if (kind === "restart") return 3;
  if (kind === "loadMore") return 2;
  return 1;
}

function emptyView(): WorkBoardView {
  return {
    items: [],
    selectedWorkId: null,
    snapshot: null,
    error: null,
    busy: false,
    traversal: "complete",
    continuation: null,
    freshness: "polling",
    detailStatus: "current",
    detailError: null,
  };
}

/**
 * Operation-local Work Board traversal.
 * Background observation does not bump the list generation and does not replace
 * a held continuation. An explicit restart does, and it also reloads the
 * selected detail. One list request may run, and one follow-up may wait.
 * Detail reads keep the in-flight response plus one newer read; anything
 * older is dropped when it lands.
 */
export class WorkBoardController {
  private view: WorkBoardView = emptyView();
  private listGen = 0;
  private detailGen = 0;
  private scopeKey = "";
  private paged = false;
  private fence: string | null = null;
  private inflight = false;
  private queued: QueuedList | null = null;
  private listener: ((view: WorkBoardView) => void) | null = null;
  private detailActive = 0;
  private detailQueued: { scope: WorkBoardScope; workId: string; gen: number } | null = null;
  private detailWaiters = new Map<number, () => void>();

  constructor(
    private readonly client: {
      list: (scope: WorkBoardScope, cursor: string | null) => Promise<WorkBoardPage>;
      get: (scope: WorkBoardScope, workId: string) => Promise<RemoteWorkSnapshot | null>;
    },
  ) {}

  setListener(listener: ((view: WorkBoardView) => void) | null) {
    this.listener = listener;
  }

  snapshot(): WorkBoardView {
    return { ...this.view, items: this.view.items.slice() };
  }

  /** Drop list and detail responses that have not landed yet. */
  invalidate() {
    this.listGen += 1;
    this.detailGen += 1;
    this.dropDetailReads();
    if (this.view.detailStatus === "loading") {
      this.view.snapshot = null;
      this.view.detailStatus = "current";
      this.view.detailError = null;
    }
    const wasBusy = this.view.busy;
    this.view.busy = false;
    const queued = this.queued;
    this.queued = null;
    queued?.resolve();
    if (wasBusy) this.emit();
  }

  select(workId: string | null) {
    if (workId === null) {
      this.detailGen += 1;
      this.dropDetailReads();
      this.view.selectedWorkId = null;
      this.view.snapshot = null;
      this.view.detailStatus = "current";
      this.view.detailError = null;
      this.emit();
      return;
    }
    if (this.view.selectedWorkId === workId) return;
    this.detailGen += 1;
    this.dropDetailReads();
    this.view.selectedWorkId = workId;
    this.view.snapshot = null;
    this.view.detailStatus = "loading";
    this.view.detailError = null;
    this.emit();
  }

  dismissDetail() {
    this.detailGen += 1;
    this.dropDetailReads();
    if (this.view.snapshot == null && this.view.detailStatus === "current") return;
    this.view.snapshot = null;
    this.view.detailStatus = "current";
    this.view.detailError = null;
    this.emit();
  }

  async observe(scope: WorkBoardScope): Promise<WorkBoardView> {
    if (this.refuseUnavailable(scope)) return this.snapshot();
    const key = workBoardScopeKey(scope);
    if (this.scopeKey && key !== this.scopeKey) {
      this.beginScope(key);
      await this.request("restart", scope);
      return this.snapshot();
    }
    // A held traversal stays put. Do not bump the generation: an in-flight
    // Load more for this scope must still be able to land.
    if (this.paged) return this.snapshot();
    await this.request("observe", scope);
    return this.snapshot();
  }

  async restart(scope: WorkBoardScope): Promise<WorkBoardView> {
    if (this.refuseUnavailable(scope)) return this.snapshot();
    const key = workBoardScopeKey(scope);
    if (this.scopeKey && key !== this.scopeKey) this.beginScope(key);
    // Fence detail before the list returns, including while this restart waits
    // behind another list request. The same work id still needs a new read.
    this.armDetailRefresh();
    await this.request("restart", scope);
    return this.snapshot();
  }

  async loadMore(scope: WorkBoardScope): Promise<WorkBoardView> {
    if (this.refuseUnavailable(scope)) return this.snapshot();
    const key = workBoardScopeKey(scope);
    if (this.scopeKey && key !== this.scopeKey) return this.snapshot();
    if (!this.view.continuation || this.view.traversal !== "partial") return this.snapshot();
    await this.request("loadMore", scope);
    return this.snapshot();
  }

  async refreshDetail(scope: WorkBoardScope, workId: string): Promise<void> {
    if (this.refuseUnavailable(scope)) return;
    const key = workBoardScopeKey(scope);
    if (this.scopeKey && key !== this.scopeKey) {
      this.detailGen += 1;
      this.dropDetailReads();
      this.listGen += 1;
      return;
    }
    const gen = ++this.detailGen;
    this.view.detailStatus = "loading";
    this.view.detailError = null;
    this.emit();
    const done = new Promise<void>((resolve) => {
      this.detailWaiters.set(gen, resolve);
    });
    this.startDetail({ scope, workId, gen });
    await done;
  }

  private beginScope(key: string) {
    this.listGen += 1;
    this.detailGen += 1;
    this.dropDetailReads();
    this.scopeKey = key;
    this.paged = false;
    this.fence = null;
    this.view.items = [];
    this.view.selectedWorkId = null;
    this.view.snapshot = null;
    this.view.continuation = null;
    this.view.error = null;
    this.view.detailStatus = "current";
    this.view.detailError = null;
    this.view.traversal = "loading";
    this.view.freshness = "polling";
    this.view.busy = true;
    this.emit();
  }

  /** @returns true when the caller must not start a list or detail read. */
  private refuseUnavailable(scope: WorkBoardScope): boolean {
    const missing = !scope.sessionId || !scope.workspace;
    const remoteDown = scope.remote && (!scope.remoteLanePresent || !scope.remoteConnected);
    if (!missing && !remoteDown) return false;
    this.listGen += 1;
    this.detailGen += 1;
    this.dropDetailReads();
    const queued = this.queued;
    this.queued = null;
    queued?.resolve();
    if (missing) {
      const already =
        this.scopeKey === "" &&
        this.view.items.length === 0 &&
        this.view.selectedWorkId == null &&
        this.view.snapshot == null &&
        this.view.error == null &&
        this.view.busy === false &&
        this.view.continuation == null &&
        this.view.traversal === "complete";
      this.scopeKey = "";
      this.paged = false;
      this.fence = null;
      this.view = emptyView();
      if (!already) this.emit();
      return true;
    }
    const key = workBoardScopeKey(scope);
    const switched = this.scopeKey !== key;
    if (switched) {
      this.view.items = [];
      this.view.selectedWorkId = null;
      this.view.snapshot = null;
      this.view.continuation = null;
      this.paged = false;
      this.fence = null;
    }
    this.scopeKey = key;
    const unchanged =
      !switched &&
      this.view.busy === false &&
      this.view.error === DISCONNECTED &&
      this.view.traversal === "error";
    this.view.busy = false;
    this.view.error = DISCONNECTED;
    this.view.traversal = "error";
    if (!unchanged) this.emit();
    return true;
  }

  private async request(kind: ListKind, scope: WorkBoardScope): Promise<void> {
    if (this.inflight) {
      if (kind === "restart") this.listGen += 1;
      if (this.queued && rank(kind) < rank(this.queued.kind)) return;
      const replaced = this.queued;
      this.queued = null;
      replaced?.resolve();
      await new Promise<void>((resolve) => {
        this.queued = { kind, scope, resolve };
      });
      return;
    }
    this.inflight = true;
    try {
      await this.run(kind, scope);
    } finally {
      this.inflight = false;
      const next = this.queued;
      this.queued = null;
      if (next) void this.follow(next);
    }
  }

  private follow(next: QueuedList) {
    const op =
      next.kind === "restart"
        ? this.restart(next.scope)
        : next.kind === "loadMore"
          ? this.loadMore(next.scope)
          : this.observe(next.scope);
    void op.finally(next.resolve);
  }

  private async run(kind: ListKind, scope: WorkBoardScope): Promise<void> {
    const key = workBoardScopeKey(scope);
    this.scopeKey = key;
    const gen = kind === "loadMore" ? this.listGen : ++this.listGen;
    const cursor = kind === "loadMore" ? this.view.continuation : null;
    this.view.busy = true;
    if (kind !== "loadMore") this.view.traversal = "loading";
    this.emit();
    try {
      const page = await this.client.list(scope, cursor);
      if (this.listGen !== gen || this.scopeKey !== key) return;
      if (kind === "loadMore" && page.fence && this.fence && page.fence !== this.fence) {
        this.holdForRefresh("Could not load the next work page: restart_required");
        return;
      }
      const sessionId = scope.sessionId ?? "";
      const workspace = scope.workspace ?? "";
      const incoming = page.items.map((entry) => workListEntryToItem(entry, sessionId, workspace));
      const priorId = this.view.selectedWorkId;
      const priorRow = priorId ? this.view.items.find((item) => item.workId === priorId) ?? null : null;
      if (kind === "loadMore") {
        const accepted = acceptWorkPage(this.listGen, gen, this.view.items, incoming, true);
        if (!accepted) return;
        this.view.items = accepted;
        this.paged = true;
      } else {
        const accepted = acceptWorkPage(this.listGen, gen, [], incoming, false);
        if (!accepted) return;
        this.view.items = accepted;
        const kept =
          priorId != null && accepted.some((item) => item.workId === priorId) ? priorId : null;
        this.view.selectedWorkId = kept ?? accepted[0]?.workId ?? null;
        if (this.view.selectedWorkId !== priorId) this.view.snapshot = null;
        this.paged = !page.complete && Boolean(page.continuation);
        this.fence = page.fence ?? null;
      }
      this.view.continuation = page.complete ? null : page.continuation ?? null;
      this.view.traversal = page.complete ? "complete" : "partial";
      this.view.error = null;
      this.view.freshness = this.paged ? "held" : "polling";
      this.view.busy = false;
      this.emit();
      const selectedId = this.view.selectedWorkId;
      const nextRow = selectedId ? this.view.items.find((item) => item.workId === selectedId) ?? null : null;
      const rowChanged = Boolean(priorRow && nextRow && workRowChanged(priorRow, nextRow));
      if (!selectedId) {
        this.view.snapshot = null;
        this.view.detailStatus = "current";
        this.view.detailError = null;
        this.emit();
      } else if (kind === "restart" || (kind === "observe" && rowChanged)) {
        await this.refreshDetail(scope, selectedId);
      }
    } catch (error) {
      if (this.listGen !== gen || this.scopeKey !== key) return;
      const status = workTraversalFromError(error);
      this.view.busy = false;
      this.view.traversal = status;
      if (kind === "loadMore") {
        if (status === "expired") this.holdForRefresh(`Could not load the next work page: ${String(error)}`);
        else {
          this.view.error = `Could not load the next work page: ${String(error)}`;
          this.emit();
        }
        return;
      }
      if (this.paged) {
        this.view.error = `Could not refresh durable Work Items: ${String(error)}`;
        if (status === "expired") this.view.freshness = "refresh-required";
        this.emit();
        return;
      }
      this.view.continuation = null;
      this.view.error = `Could not refresh durable Work Items: ${String(error)}`;
      this.emit();
    }
  }

  private holdForRefresh(message: string) {
    this.view.continuation = null;
    this.view.traversal = "expired";
    this.view.freshness = "refresh-required";
    this.view.error = message;
    this.view.busy = false;
    this.paged = true;
    this.emit();
  }

  /** Invalidate detail as soon as Refresh is requested, before its list returns. */
  private armDetailRefresh() {
    this.detailGen += 1;
    this.dropDetailReads();
    this.view.detailStatus = "loading";
    this.view.detailError = null;
    this.emit();
  }

  private dropDetailReads() {
    const queued = this.detailQueued;
    this.detailQueued = null;
    if (queued) this.releaseDetail(queued.gen);
    for (const gen of [...this.detailWaiters.keys()]) this.releaseDetail(gen);
  }

  private releaseDetail(gen: number) {
    const done = this.detailWaiters.get(gen);
    if (!done) return;
    this.detailWaiters.delete(gen);
    done();
  }

  /**
   * Keep the read already in flight and at most one newer read beside it.
   * Further refresh requests replace that newer slot instead of piling up.
   */
  private startDetail(job: { scope: WorkBoardScope; workId: string; gen: number }) {
    if (this.detailActive >= 2) {
      if (this.detailQueued) this.releaseDetail(this.detailQueued.gen);
      this.detailQueued = job;
      return;
    }
    this.launchDetail(job);
  }

  private launchDetail(job: { scope: WorkBoardScope; workId: string; gen: number }) {
    this.detailActive += 1;
    void (async () => {
      try {
        if (job.gen === this.detailGen && this.view.selectedWorkId === job.workId) {
          await this.readDetailOnce(job.scope, job.workId, job.gen);
        }
      } finally {
        this.releaseDetail(job.gen);
        this.detailActive = Math.max(0, this.detailActive - 1);
        const next = this.detailQueued;
        this.detailQueued = null;
        if (next) this.startDetail(next);
      }
    })();
  }

  private async readDetailOnce(scope: WorkBoardScope, workId: string, gen: number) {
    try {
      const snapshot = await this.client.get(scope, workId);
      if (gen !== this.detailGen || this.view.selectedWorkId !== workId) return;
      if (this.scopeKey && this.scopeKey !== workBoardScopeKey(scope)) return;
      this.view.snapshot = snapshot;
      this.view.detailStatus = "current";
      this.view.detailError = null;
      if (this.view.error?.startsWith("Could not open durable Work Item:")) this.view.error = null;
      this.emit();
    } catch (error) {
      if (gen !== this.detailGen || this.view.selectedWorkId !== workId) return;
      this.view.snapshot = null;
      this.view.detailStatus = "error";
      this.view.detailError = `Could not open durable Work Item: ${String(error)}`;
      this.emit();
    }
  }

  private emit() {
    this.listener?.(this.snapshot());
  }
}

function workRowChanged(previous: DurableWorkItem, next: DurableWorkItem): boolean {
  return (
    previous.revision !== next.revision ||
    previous.state !== next.state ||
    previous.updatedAt !== next.updatedAt ||
    previous.attemptCount !== next.attemptCount
  );
}
