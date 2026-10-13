import { describe, expect, it } from "vitest";
import type { DurableWorkItem } from "./protocol";
import { acceptWorkPage, mergeWorkPages, workTraversalFromError } from "./workTraversal";

function item(workId: string, revision: number): DurableWorkItem {
  return {
    schemaVersion: 1,
    workId,
    kind: "implementation",
    objective: workId,
    sessionId: "lane-1",
    workspace: "/tmp/project",
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
    revision,
    attemptCount: 0,
    progress: null,
    result: null,
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
  };
}

describe("work page traversal", () => {
  it("deduplicates a repeated id and revision and keeps a new revision", () => {
    const merged = mergeWorkPages(
      [item("a", 1), item("b", 1)],
      [item("b", 1), item("b", 2), item("c", 1)],
    );
    expect(merged.map((entry) => `${entry.workId}:${entry.revision}`)).toEqual([
      "a:1",
      "b:1",
      "b:2",
      "c:1",
    ]);
  });

  it("drops a stale generation instead of appending it to the current lane", () => {
    const current = [item("lane-a", 1)];
    expect(acceptWorkPage(2, 1, current, [item("lane-b", 1)], true)).toBeNull();
    expect(acceptWorkPage(2, 2, current, [item("lane-a", 1), item("lane-c", 4)], true)?.map(
      (entry) => entry.workId,
    )).toEqual(["lane-a", "lane-c"]);
  });

  it("replaces the lane on a refresh and does not keep the previous page", () => {
    const accepted = acceptWorkPage(3, 3, [item("old", 1)], [item("new", 2)], false);
    expect(accepted?.map((entry) => entry.workId)).toEqual(["new"]);
  });

  it("distinguishes an expired continuation from a transport failure", () => {
    expect(workTraversalFromError(new Error("MCP remote error: restart_required"))).toBe("expired");
    expect(workTraversalFromError("cursor_expired")).toBe("expired");
    expect(workTraversalFromError(new Error("connection reset"))).toBe("error");
  });
});
