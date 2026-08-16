import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import {
  createPromptQueueEntry,
  drainPromptQueuePrefix,
  emptyPromptQueueState,
  promptQueueReducer,
  queueEntriesFor,
  type PromptQueueState,
} from "./promptQueue";

const sessionId = "session-a";
const root = dirname(fileURLToPath(import.meta.url));

function entry(id: string, text: string, priority = false) {
  return createPromptQueueEntry(text, {
    id,
    created_at: "2026-07-27T00:00:00Z",
    priority,
  });
}

describe("prompt queue reducer", () => {
  it("adds, edits, and removes stable versioned entries", () => {
    let state: PromptQueueState = emptyPromptQueueState;
    state = promptQueueReducer(state, {
      type: "add",
      sessionId,
      entry: entry("a", "first"),
    });
    state = promptQueueReducer(state, {
      type: "edit",
      sessionId,
      entryId: "a",
      text: "/help",
    });
    expect(state.entries[sessionId][0]).toMatchObject({
      id: "a",
      version: 1,
      text: "/help",
      kind: "command",
    });

    state = promptQueueReducer(state, {
      type: "remove",
      sessionId,
      entryId: "a",
    });
    expect(state.entries).toEqual({});
  });

  it("reorders and clears one session without touching another", () => {
    let state: PromptQueueState = {
      entries: {
        [sessionId]: [
          entry("a", "one"),
          entry("b", "two"),
          entry("c", "three"),
        ],
        other: [entry("z", "other")],
      },
      revisions: {},
    };
    state = promptQueueReducer(state, {
      type: "move",
      sessionId,
      entryId: "c",
      toIndex: 0,
    });
    expect(queueEntriesFor(state, sessionId).map((item) => item.id)).toEqual([
      "c",
      "a",
      "b",
    ]);
    expect(queueEntriesFor(state, "other").map((item) => item.id)).toEqual([
      "z",
    ]);

    state = promptQueueReducer(state, { type: "clear", sessionId });
    expect(state.entries[sessionId]).toBeUndefined();
    expect(queueEntriesFor(state, "other")).toHaveLength(1);
  });

  // S2: bridge snapshots are published after the mutation lock is released,
  // so commit order and publish order can differ. Before the revision gate an
  // ungated `replace` let the older snapshot win and silently regress the
  // queue — this is the case that reproduces it.
  it("drops a queue snapshot published out of commit order", () => {
    let state: PromptQueueState = emptyPromptQueueState;
    const first = entry("a", "one");
    const second = entry("b", "two");

    // Commit A (revision 1) added `a`; commit B (revision 2) added `b`.
    // B's publish wins the race and lands first.
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [first, second],
      revision: 2,
    });
    expect(queueEntriesFor(state, sessionId).map((item) => item.id)).toEqual([
      "a",
      "b",
    ]);

    const afterNewer = state;
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [first],
      revision: 1,
    });
    expect(state).toBe(afterNewer);
    expect(queueEntriesFor(state, sessionId).map((item) => item.id)).toEqual([
      "a",
      "b",
    ]);

    // A re-delivery of the newest revision is also stale, not newer.
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [],
      revision: 2,
    });
    expect(queueEntriesFor(state, sessionId)).toHaveLength(2);

    // A genuinely newer revision still applies, including an empty clear.
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [],
      revision: 3,
    });
    expect(queueEntriesFor(state, sessionId)).toEqual([]);
    expect(state.revisions[sessionId]).toBe(3);
  });

  it("keeps per-session revision watermarks independent", () => {
    let state: PromptQueueState = emptyPromptQueueState;
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [entry("a", "one")],
      revision: 5,
    });
    // A different session's revision counter is its own; revision 1 here is
    // new, not stale.
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId: "other",
      entries: [entry("z", "other")],
      revision: 1,
    });
    expect(queueEntriesFor(state, "other")).toHaveLength(1);
    expect(state.revisions).toEqual({ [sessionId]: 5, other: 1 });
  });

  it("drops a refetch that was overtaken by a newer event", () => {
    let state: PromptQueueState = emptyPromptQueueState;
    // A refetch reads the queue at revision 4...
    const refetch = {
      type: "replace" as const,
      sessionId,
      entries: [entry("a", "one")],
      revision: 4,
    };
    // ...but a mutation commits and its event lands first.
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [entry("a", "one"), entry("b", "two")],
      revision: 5,
    });
    // The slow read must not restore the older membership.
    state = promptQueueReducer(state, refetch);
    expect(queueEntriesFor(state, sessionId)).toHaveLength(2);
    expect(state.revisions[sessionId]).toBe(5);
  });

  it("applies a refetch that is newer than every event seen", () => {
    let state: PromptQueueState = emptyPromptQueueState;
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [entry("a", "one")],
      revision: 2,
    });
    // A refetch can legitimately be ahead of the event stream: it reads
    // committed state directly, and its event may not have arrived yet.
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [entry("a", "one"), entry("b", "two")],
      revision: 3,
    });
    expect(queueEntriesFor(state, sessionId)).toHaveLength(2);
    expect(state.revisions[sessionId]).toBe(3);
  });

  it("applies a revisionless mutation receipt without moving the watermark", () => {
    let state: PromptQueueState = emptyPromptQueueState;
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [entry("a", "one")],
      revision: 4,
    });
    // A mutation receipt carries no revision and is ordered by request
    // alone; it must still apply.
    state = promptQueueReducer(state, {
      type: "replace",
      sessionId,
      entries: [entry("a", "one"), entry("b", "two")],
    });
    expect(queueEntriesFor(state, sessionId)).toHaveLength(2);
    expect(state.revisions[sessionId]).toBe(4);
  });
});

describe("prompt queue event wiring", () => {
  // The reducer can only drop stale snapshots if App.tsx actually forwards
  // the bridge revision. Dropping this argument would silently restore the
  // ungated replace that S2 describes.
  it("forwards the bridge revision from prompt_queue_changed", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/revision: queueUpdate\.revision/);
  });
});

describe("prompt queue drain", () => {
  it("combines the eligible plain prefix and preserves the rest", () => {
    const drained = drainPromptQueuePrefix([
      entry("a", "one"),
      entry("b", "two"),
      entry("c", "/help"),
      entry("d", "four"),
    ]);
    expect(drained?.entries.map((item) => item.id)).toEqual(["a", "b"]);
    expect(drained?.text).toBe("one\n\ntwo");
    expect(drained?.remaining.map((item) => item.id)).toEqual(["c", "d"]);
  });

  it("drains a priority entry alone", () => {
    const drained = drainPromptQueuePrefix([
      entry("a", "urgent", true),
      entry("b", "later"),
    ]);
    expect(drained?.entries.map((item) => item.id)).toEqual(["a"]);
    expect(drained?.remaining.map((item) => item.id)).toEqual(["b"]);
  });

  it("returns null for an empty queue", () => {
    expect(drainPromptQueuePrefix([])).toBeNull();
  });
});

describe("background tasks panel (#52)", () => {
  it("exposes schedule shell and cancel/adopt for long-running work", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/Schedule scan/);
    expect(app).toMatch(/Schedule shell/);
    expect(app).toMatch(/Open session/);
    expect(app).toMatch(/Background \/ scheduled/);
    expect(app).toMatch(/background_task/);
  });
});

describe("multi-agent panel (#152)", () => {
  it("shows cancel-one-child and subagent summary fields", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    const card = readFileSync(
      join(root, "..", "components", "SubagentCard.tsx"),
      "utf8",
    );
    expect(card).toMatch(/Cancel child/);
    expect(app).toMatch(/cancelSubagent/);
    expect(card).toMatch(/subagent-card/);
    expect(card).toMatch(/agent\.summary/);
    expect(app).toMatch(/subagent_spawned|subagent_update/);
    expect(app).toMatch(/setSubagents\(await api\.subagentsList\(\)\)/);
  });
});

describe("terminal design system (#129)", () => {
  it("uses design tokens and Tab N labels, not raw green PTY banners", () => {
    const term = readFileSync(
      join(root, "..", "components", "TerminalPane.tsx"),
      "utf8",
    );
    expect(term).toMatch(/--surface-deep/);
    expect(term).toMatch(/--accent/);
    expect(term).toMatch(/Tab \{i \+ 1\}/);
    expect(term).not.toMatch(/\\x1b\[32mGrokPtah terminal/);
  });
});
