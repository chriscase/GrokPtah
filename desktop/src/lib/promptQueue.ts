export type PromptQueueKind = "prompt" | "command";
export type PromptQueueSource =
  | "composer"
  | "control"
  | "steer_now"
  | "steering_deferred"
  | "steering_delivery_recovery"
  | "desktop";

export type PromptQueueEntry = {
  id: string;
  version: number;
  text: string;
  kind: PromptQueueKind;
  source: PromptQueueSource;
  owner?: string | null;
  created_at: string;
  priority: boolean;
};

export type PromptQueueState = {
  entries: Record<string, PromptQueueEntry[]>;
  /**
   * Newest bridge `revision` applied per session. The bridge stamps a commit
   * sequence under its mutation lock but publishes after releasing it, so
   * snapshots can arrive out of commit order; anything at or below the
   * watermark is a stale view of the queue and must be dropped.
   */
  revisions: Record<string, number>;
};

export const emptyPromptQueueState: PromptQueueState = {
  entries: {},
  revisions: {},
};

/**
 * Newest revision this client has applied for a session.
 *
 * This is what a revision-fenced write must send: it names the ordering the
 * UI is actually showing, so a write computed against a view the bridge has
 * since moved past fails closed instead of landing on a queue nobody read.
 */
export function queueRevisionFor(
  state: PromptQueueState,
  sessionId: string | null | undefined,
): number {
  if (!sessionId) return 0;
  return state.revisions[sessionId] ?? 0;
}

/** Entries the UI should render for a session. */
export function queueEntriesFor(
  state: PromptQueueState,
  sessionId: string | null | undefined,
): PromptQueueEntry[] {
  if (!sessionId) return [];
  return state.entries[sessionId] ?? [];
}

export type PromptQueueAction =
  | {
      type: "replace";
      sessionId: string;
      entries: PromptQueueEntry[];
      /**
       * Present for bridge `prompt_queue_changed` snapshots and revisioned
       * refetches. Mutation receipts omit it; once a watermark exists they
       * must not overwrite that projection.
       */
      revision?: number;
    }
  | { type: "add"; sessionId: string; entry: PromptQueueEntry }
  | {
      type: "edit";
      sessionId: string;
      entryId: string;
      text: string;
    }
  | { type: "remove"; sessionId: string; entryId: string }
  | { type: "clear"; sessionId: string }
  | {
      type: "move";
      sessionId: string;
      entryId: string;
      toIndex: number;
    };

function queueId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `queue-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

export function queueKind(text: string): PromptQueueKind {
  const trimmed = text.trimStart();
  return trimmed.startsWith("/") || trimmed.startsWith("!")
    ? "command"
    : "prompt";
}

export function createPromptQueueEntry(
  text: string,
  overrides: Partial<PromptQueueEntry> = {},
): PromptQueueEntry {
  return {
    id: overrides.id ?? queueId(),
    version: overrides.version ?? 0,
    text,
    kind: overrides.kind ?? queueKind(text),
    source: overrides.source ?? "composer",
    owner: overrides.owner ?? "desktop",
    created_at: overrides.created_at ?? new Date().toISOString(),
    priority: overrides.priority ?? false,
  };
}

function withSession(
  state: PromptQueueState,
  sessionId: string,
  entries: PromptQueueEntry[],
  revision?: number,
): PromptQueueState {
  const revisions =
    revision === undefined
      ? state.revisions
      : { ...state.revisions, [sessionId]: revision };
  if (entries.length === 0) {
    if (!(sessionId in state.entries)) {
      return revisions === state.revisions ? state : { ...state, revisions };
    }
    const nextEntries = { ...state.entries };
    delete nextEntries[sessionId];
    return { entries: nextEntries, revisions };
  }
  return { entries: { ...state.entries, [sessionId]: entries }, revisions };
}

export function promptQueueReducer(
  state: PromptQueueState,
  action: PromptQueueAction,
): PromptQueueState {
  const entries = state.entries[action.sessionId] ?? [];
  switch (action.type) {
    case "replace": {
      // Bridge snapshots can be published out of commit order. Applying an
      // older one would silently regress the queue (drop a just-added entry,
      // resurrect a removed one), so only ever move the watermark forward.
      // Revisionless receipts are request-ordered only: once an event or
      // revisioned refetch has established a watermark, that projection wins.
      const applied = state.revisions[action.sessionId];
      if (action.revision !== undefined) {
        if (applied !== undefined && action.revision <= applied) return state;
      } else if (applied !== undefined) {
        return state;
      }
      return withSession(
        state,
        action.sessionId,
        [...action.entries],
        action.revision,
      );
    }
    case "add":
      return withSession(state, action.sessionId, [...entries, action.entry]);
    case "edit":
      return withSession(
        state,
        action.sessionId,
        entries.map((entry) =>
          entry.id === action.entryId
            ? {
                ...entry,
                text: action.text,
                kind: queueKind(action.text),
                version: entry.version + 1,
              }
            : entry,
        ),
      );
    case "remove":
      return withSession(
        state,
        action.sessionId,
        entries.filter((entry) => entry.id !== action.entryId),
      );
    case "clear":
      return withSession(state, action.sessionId, []);
    case "move": {
      const fromIndex = entries.findIndex(
        (entry) => entry.id === action.entryId,
      );
      if (fromIndex < 0 || entries.length < 2) return state;
      const toIndex = Math.max(
        0,
        Math.min(action.toIndex, entries.length - 1),
      );
      if (fromIndex === toIndex) return state;
      const next = [...entries];
      const [entry] = next.splice(fromIndex, 1);
      next.splice(toIndex, 0, entry);
      return withSession(state, action.sessionId, next);
    }
  }
}

export type DrainedPromptQueue = {
  entries: PromptQueueEntry[];
  text: string;
  remaining: PromptQueueEntry[];
};

export type PromptQueueBatch = {
  entries: PromptQueueEntry[];
  text: string;
};

export type PromptQueueTakeResult = {
  batch?: PromptQueueBatch | null;
  entries: PromptQueueEntry[];
  /**
   * Turn slot claimed for this batch under the same lock that removed it.
   * Present only when a batch was drained. Present it when starting the turn,
   * or hand the batch back — a drain whose turn never starts otherwise loses
   * the prompt outright.
   */
  reservation?: string | null;
};

/** A queue read stamped with the revision it was taken at. */
export type PromptQueueSnapshot = {
  entries: PromptQueueEntry[];
  revision: number;
};

export type PromptQueueRunNextResult = {
  entries: PromptQueueEntry[];
  cancelled_active: boolean;
  changed_entry?: PromptQueueEntry;
};

export type SteeringReceipt = {
  entry: PromptQueueEntry;
  disposition: "pending" | "queued";
  entries: PromptQueueEntry[];
};

export function drainPromptQueuePrefix(
  entries: PromptQueueEntry[],
): DrainedPromptQueue | null {
  if (entries.length === 0) return null;
  let count = 1;
  if (entries[0].kind === "prompt" && !entries[0].priority) {
    while (
      count < entries.length &&
      entries[count].kind === "prompt" &&
      !entries[count].priority
    ) {
      count += 1;
    }
  }
  const drained = entries.slice(0, count);
  return {
    entries: drained,
    text: drained.map((entry) => entry.text).join("\n\n"),
    remaining: entries.slice(count),
  };
}

export type HelpCommandIntercept = {
  handled: boolean;
  openHelp: boolean;
  /**
   * Queued /help already left the queue when the drain reserved the turn.
   * Release that reservation with an empty restore so the slot is free and
   * /help is not sent or put back on the queue.
   */
  releaseReservationWithoutRestore: boolean;
};

/** Local /help handling that must not reach sessionPrompt or re-queue. */
export function interceptHelpCommand(input: {
  prompt: string;
  fromQueue?: boolean;
  reservation?: string | null;
}): HelpCommandIntercept {
  if (input.prompt !== "/help") {
    return {
      handled: false,
      openHelp: false,
      releaseReservationWithoutRestore: false,
    };
  }
  return {
    handled: true,
    openHelp: true,
    releaseReservationWithoutRestore: Boolean(input.fromQueue || input.reservation),
  };
}
