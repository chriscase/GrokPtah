import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import {
  createPromptQueueEntry,
  drainPromptQueuePrefix,
  promptQueueReducer,
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
    let state: PromptQueueState = {};
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
    expect(state[sessionId][0]).toMatchObject({
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
    expect(state).toEqual({});
  });

  it("reorders and clears one session without touching another", () => {
    let state: PromptQueueState = {
      [sessionId]: [entry("a", "one"), entry("b", "two"), entry("c", "three")],
      other: [entry("z", "other")],
    };
    state = promptQueueReducer(state, {
      type: "move",
      sessionId,
      entryId: "c",
      toIndex: 0,
    });
    expect(state[sessionId].map((item) => item.id)).toEqual(["c", "a", "b"]);
    expect(state.other.map((item) => item.id)).toEqual(["z"]);

    state = promptQueueReducer(state, { type: "clear", sessionId });
    expect(state[sessionId]).toBeUndefined();
    expect(state.other).toHaveLength(1);
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
    expect(app).toMatch(/Cancel child/);
    expect(app).toMatch(/cancelSubagent/);
    expect(app).toMatch(/subagent-card/);
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
