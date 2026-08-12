import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DurableRun, RunReview } from "../lib/protocol";
import { RunInspector } from "./RunInspector";

afterEach(cleanup);

function run(overrides: Partial<DurableRun> = {}): DurableRun {
  return {
    runId: "desktop-run-1",
    sessionId: "session-1",
    workspace: "/tmp/demo",
    requestId: "desktop-turn-1",
    clientId: "desktop",
    state: "completed",
    bounds: { maxPromptBytes: 1000, maxRounds: 8, maxDurationMs: 1000 },
    promptPreview: "Fix the failing test",
    startSeq: 1,
    endSeq: 8,
    createdAt: "2026-08-11T12:00:00Z",
    updatedAt: "2026-08-11T12:01:00Z",
    terminalResult: "completed",
    finalResponse: "Changed src/lib.rs; cargo test passed.",
    errorCode: null,
    aggregates: {
      changes: [{ path: "src/lib.rs", summary: "updated" }],
      tests: [{ callId: "test-1", command: "cargo test", status: "ended", exitCode: 0, cancelled: false }],
      permissionsRequested: 0,
      permissionsGranted: 0,
      permissionsDenied: 0,
      usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3, requests: 1 },
      verification: {
        status: "verified",
        stopReason: "completed",
        interrupted: false,
        claims: { present: true, mentionsChanges: true, mentionsTests: true, mentionsVerification: true },
        observations: {
          changedFiles: 1,
          testsObserved: 1,
          testsPassed: 1,
          testsFailed: 0,
          testsIncomplete: 0,
          permissionsRequested: 0,
          permissionsGranted: 0,
          permissionsDenied: 0,
          permissionsUnresolved: 0,
        },
        usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3, requests: 1 },
      },
    },
    progress: null,
    ...overrides,
  };
}

const review: RunReview = {
  changedFiles: [{ path: "src/lib.rs", summary: "changed in isolated run" }],
  diff: "diff --git a/src/lib.rs b/src/lib.rs\n+changed",
  diffTruncated: false,
  fingerprint: "abc123",
};

const actions = {
  onReview: vi.fn(async () => review),
  onPromote: vi.fn(async () => undefined),
  onDiscard: vi.fn(async () => undefined),
};

describe("RunInspector", () => {
  it("shows durable status, evidence, and bounded handoff", () => {
    render(<RunInspector runs={[run()]} onRefresh={vi.fn()} {...actions} />);

    expect(screen.getByText("Completed")).toBeTruthy();
    expect(screen.getByText("Fix the failing test")).toBeTruthy();
    expect(screen.getByText("1 files")).toBeTruthy();
    expect(screen.getByText("1/1 tests passed")).toBeTruthy();
    expect(screen.getByText("Verification: verified")).toBeTruthy();
    expect(screen.getByText("Handoff")).toBeTruthy();
    expect(screen.getByText("Desktop", { selector: ".run-origin" })).toBeTruthy();
    expect(screen.getByText("Shared workspace", { selector: ".run-execution-mode" })).toBeTruthy();
  });

  it("labels coordinator-owned runs", () => {
    render(
      <RunInspector
        runs={[run({ clientId: "mcp" })]}
        onRefresh={vi.fn()}
        {...actions}
      />,
    );

    expect(screen.getByText("MCP coordinator", { selector: ".run-origin" })).toBeTruthy();
  });

  it("filters by origin and controls live watching", () => {
    const onWatchingChange = vi.fn();
    render(
      <RunInspector
        runs={[
          run({ runId: "desktop-run", promptPreview: "Desktop task", clientId: "desktop" }),
          run({ runId: "mcp-run", promptPreview: "Coordinator task", clientId: "mcp" }),
        ]}
        watching
        onWatchingChange={onWatchingChange}
        onRefresh={vi.fn()}
        {...actions}
      />,
    );

    fireEvent.change(screen.getByLabelText("Filter task runs by source"), {
      target: { value: "mcp" },
    });
    expect(screen.queryByText("Desktop task")).toBeNull();
    expect(screen.getByText("Coordinator task")).toBeTruthy();
    fireEvent.click(screen.getByLabelText("Watch live updates"));
    expect(onWatchingChange).toHaveBeenCalledWith(false);
  });

  it("makes an interrupted run actionable and refreshable", () => {
    const onRefresh = vi.fn();
    render(
      <RunInspector
        runs={[run({ state: "interrupted", errorCode: "interrupted" })]}
        onRefresh={onRefresh}
        {...actions}
      />,
    );

    expect(screen.getByText("Interrupted")).toBeTruthy();
    expect(screen.getByText(/stopped after restart/i)).toBeTruthy();
    fireEvent.click(screen.getByLabelText("Refresh task runs"));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });

  it("reviews an isolated run before enabling promotion", async () => {
    const isolated = run({
      execution: {
        mode: "isolated_worktree",
        sourceWorkspace: "/tmp/demo",
        executionWorkspace: "/tmp/demo/.grokptah/worktrees/runs/run-1",
        baseRevision: "base",
        sourceFingerprint: "source",
        finalFingerprint: "abc123",
        promotionState: "ready",
        promotedAt: null,
      },
    });
    render(<RunInspector runs={[isolated]} onRefresh={vi.fn()} {...actions} />);

    expect(screen.getByText(/Isolated · ready/)).toBeTruthy();
    expect(screen.getByText("Isolated worktree", { selector: ".run-execution-mode" })).toBeTruthy();
    expect(screen.queryByText("Promote reviewed changes")).toBeNull();
    fireEvent.click(screen.getByText("Review diff"));
    expect(await screen.findByText(/1 changed files/)).toBeTruthy();
    expect(screen.getByText("Promote reviewed changes")).toBeTruthy();
    expect(screen.getByText(/diff --git/)).toBeTruthy();
  });
});
