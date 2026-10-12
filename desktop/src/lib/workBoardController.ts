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

export interface WorkBoardView {
  items: DurableWorkItem[];
  selectedWorkId: string | null;
  snapshot: RemoteWorkSnapshot | null;
  error: string | null;
  busy: boolean;
  traversal: WorkTraversalStatus;
  continuation: string | null;
  freshness: "polling" | "held" | "refresh-required";
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
  };
}

/**
 * Operation-local Work Board traversal.
 * Background observation does not bump the list generation and does not replace
 * a held continuation. An explicit restart does. One list request may run, and
 * one follow-up may wait, so a slow response cannot discard every later poll.
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
      this.view.selectedWorkId = null;
      this.view.snapshot = null;
      this.emit();
      return;
    }
    if (this.view.selectedWorkId === workId) return;
    this.detailGen += 1;
    this.view.selectedWorkId = workId;
    this.view.snapshot = null;
    this.emit();
  }

  dismissDetail() {
    this.detailGen += 1;
    if (this.view.snapshot == null) return;
    this.view.snapshot = null;
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
      this.listGen += 1;
      return;
    }
    const gen = ++this.detailGen;
    try {
      const snapshot = await this.client.get(scope, workId);
      if (gen !== this.detailGen) return;
      if (this.scopeKey && this.scopeKey !== key) return;
      this.view.snapshot = snapshot;
      if (this.view.error?.startsWith("Could not open durable Work Item:")) this.view.error = null;
      this.emit();
    } catch (error) {
      if (gen !== this.detailGen) return;
      this.view.snapshot = null;
      this.view.error = `Could not open durable Work Item: ${String(error)}`;
      this.emit();
    }
  }

  private beginScope(key: string) {
    this.listGen += 1;
    this.detailGen += 1;
    this.scopeKey = key;
    this.paged = false;
    this.fence = null;
    this.view.items = [];
    this.view.selectedWorkId = null;
    this.view.snapshot = null;
    this.view.continuation = null;
    this.view.error = null;
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
      if (kind === "loadMore") {
        const accepted = acceptWorkPage(this.listGen, gen, this.view.items, incoming, true);
        if (!accepted) return;
        this.view.items = accepted;
        this.paged = true;
      } else {
        const accepted = acceptWorkPage(this.listGen, gen, [], incoming, false);
        if (!accepted) return;
        this.view.items = accepted;
        this.view.selectedWorkId =
          this.view.selectedWorkId && accepted.some((item) => item.workId === this.view.selectedWorkId)
            ? this.view.selectedWorkId
            : accepted[0]?.workId ?? null;
        this.paged = !page.complete && Boolean(page.continuation);
        this.fence = page.fence ?? null;
      }
      this.view.continuation = page.complete ? null : page.continuation ?? null;
      this.view.traversal = page.complete ? "complete" : "partial";
      this.view.error = null;
      this.view.freshness = this.paged ? "held" : "polling";
      this.view.busy = false;
      this.emit();
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

  private emit() {
    this.listener?.(this.snapshot());
  }
}
